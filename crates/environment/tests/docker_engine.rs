//! The real engine, driven against a shell script instead of a daemon.
//!
//! Every branch here is about the *CLI contract* — what a non-zero exit means,
//! what a killed child means, which containers a sweep may remove — and none of
//! it needs a container runtime to be true. The script stands in for `docker`,
//! records its argv and answers whatever the test told it to, which makes the
//! one case a real daemon cannot be asked to produce on demand (a call that
//! never returns) an ordinary test rather than a manual experiment.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use crate::common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::ErrorKind;
use darkwire_environment::container_pool::{
    CONTROL_TIMEOUT, ContainerEngine, DockerEngineOptions, OWNER_LABEL, START_TIMEOUT,
    docker_engine, owner_tag,
};
use tempfile::TempDir;

struct Fake {
    _temp: TempDir,
    bin: PathBuf,
    log: PathBuf,
}

/// A `docker` that does what `body` says and appends its argv to a log.
fn fake(body: &str) -> Fake {
    let temp = TempDir::new().unwrap();
    let bin = temp.path().join("docker");
    let log = temp.path().join("calls.log");
    common::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{body}\n",
            log.display()
        ),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    Fake {
        _temp: temp,
        bin,
        log,
    }
}

impl Fake {
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn engine(&self) -> Arc<dyn ContainerEngine> {
        self.engine_with(DockerEngineOptions {
            bin: self.bin.to_string_lossy().into_owned(),
            owner: Some("me:1".to_owned()),
            control_timeout: Some(Duration::from_millis(400)),
            start_timeout: Some(Duration::from_millis(800)),
            ..DockerEngineOptions::default()
        })
    }

    fn engine_with(&self, options: DockerEngineOptions) -> Arc<dyn ContainerEngine> {
        docker_engine(DockerEngineOptions {
            bin: self.bin.to_string_lossy().into_owned(),
            ..options
        })
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

#[tokio::test]
async fn probe_asks_the_daemon_for_its_version() {
    let fake = fake("exit 0");
    assert!(fake.engine().probe().await.is_ok());
    assert_eq!(fake.calls(), vec!["version --format {{.Server.Version}}"]);
}

#[tokio::test]
async fn a_non_zero_exit_carries_the_daemons_own_words() {
    // A bare "could not be started" sends the reader to the logs for the one
    // fact that would have told them what to do.
    let fake = fake("echo 'Cannot connect to the Docker daemon' >&2\nexit 1");
    let error = fake.engine().probe().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Tool);
    assert!(
        error.message.contains("Cannot connect"),
        "{}",
        error.message
    );
    assert_eq!(error.details["what"], "version");
}

#[tokio::test]
async fn a_call_that_never_returns_is_a_deadline_rather_than_a_clean_failure() {
    // A CLI talking to a socket whose daemon has gone away does not fail fast:
    // it blocks. A timeout arrives as a killing signal rather than as a status,
    // so checking the status alone would read "killed" as an ordinary refusal.
    let fake = fake("sleep 30");
    let error = fake.engine().probe().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Tool);
    assert!(
        error.message.contains("did not respond within"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn a_call_past_its_deadline_is_killed_rather_than_left_running() {
    let temp = TempDir::new().unwrap();
    let pid_file = temp.path().join("pid");
    let fake = fake(&format!(
        "echo $$ > '{}'\nexec sleep 30",
        pid_file.display()
    ));
    // Long enough for the script to have written its pid on a loaded machine.
    let engine = fake.engine_with(DockerEngineOptions {
        control_timeout: Some(Duration::from_secs(2)),
        ..DockerEngineOptions::default()
    });
    engine.probe().await.unwrap_err();
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let signalled = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None);
    assert_eq!(signalled, Err(nix::errno::Errno::ESRCH));
}

#[tokio::test(flavor = "current_thread")]
async fn a_slow_call_leaves_the_runtime_free_for_other_work() {
    // One runtime thread, so a call that blocked it would hold this sleep
    // until the call's own deadline.
    let fake = fake("sleep 30");
    let engine = fake.engine_with(DockerEngineOptions {
        control_timeout: Some(Duration::from_secs(3)),
        ..DockerEngineOptions::default()
    });
    let probe = tokio::spawn(async move { engine.probe().await });
    let started = std::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(probe.await.unwrap().is_err());
}

#[tokio::test]
async fn a_binary_that_is_not_there_names_itself() {
    let engine = docker_engine(DockerEngineOptions {
        bin: "/nonexistent/docker".to_owned(),
        ..DockerEngineOptions::default()
    });
    let error = engine.probe().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Tool);
    assert!(error.message.contains("Could not run"), "{}", error.message);
    assert_eq!(error.details["bin"], "/nonexistent/docker");
}

#[tokio::test]
async fn stop_gives_a_sandbox_two_seconds_rather_than_ten() {
    // A sandbox holds no state worth a graceful shutdown, and a reap that blocks
    // ten seconds per container is a reap nobody runs.
    let fake = fake("exit 0");
    fake.engine().stop("dw-sbx-1").await.unwrap();
    assert_eq!(fake.calls(), vec!["stop --time 2 dw-sbx-1"]);
}

#[tokio::test]
async fn start_runs_the_argv_it_was_handed() {
    let fake = fake("exit 0");
    fake.engine()
        .start(&argv(&["run", "--detach", "x"]))
        .await
        .unwrap();
    assert_eq!(fake.calls(), vec!["run --detach x"]);
}

#[tokio::test]
async fn start_retries_once_for_a_mount_source_the_daemon_cannot_see_yet() {
    // A desktop daemon's file sharing does not see a directory the instant it is
    // created, and the transcript directory is made microseconds before this.
    let fake = fake(
        "if [ ! -f \"$(dirname \"$0\")/tried\" ]; then\n\
           touch \"$(dirname \"$0\")/tried\"\n\
           echo 'bind source path does not exist: /runs' >&2\n\
           exit 1\n\
         fi\n\
         exit 0",
    );
    assert!(fake.engine().start(&argv(&["run", "x"])).await.is_ok());
    assert_eq!(fake.calls().len(), 2);
}

#[tokio::test]
async fn start_does_not_retry_a_genuinely_absent_path() {
    // Scoped to that exact message, so a path that is really missing fails fast.
    let fake = fake("echo 'no such image' >&2\nexit 1");
    let error = fake.engine().start(&argv(&["run", "x"])).await.unwrap_err();
    assert!(error.message.contains("no such image"), "{}", error.message);
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test]
async fn start_gives_up_when_the_retry_fails_too() {
    let fake = fake("echo 'bind source path does not exist: /runs' >&2\nexit 1");
    assert!(fake.engine().start(&argv(&["run", "x"])).await.is_err());
    assert_eq!(fake.calls().len(), 2);
}

mod reaping {
    use super::*;

    /// A `docker` whose `ps` answers with `rows` and whose `rm` is recorded.
    fn sweeper(rows: &str) -> Fake {
        fake(&format!(
            "case \"$1\" in\n  ps) printf '%b' '{rows}' ;;\n  *) ;;\nesac\nexit 0"
        ))
    }

    fn removed(fake: &Fake) -> Vec<String> {
        fake.calls()
            .into_iter()
            .filter(|call| call.starts_with("rm "))
            .collect()
    }

    fn engine(fake: &Fake, alive: &[&'static str]) -> Arc<dyn ContainerEngine> {
        let alive: Vec<&'static str> = alive.to_vec();
        fake.engine_with(DockerEngineOptions {
            bin: String::new(),
            gateway_image: None,
            owner: Some("me:1".to_owned()),
            is_owner_alive: Some(Arc::new(move |owner: &str| alive.contains(&owner))),
            control_timeout: Some(Duration::from_millis(400)),
            start_timeout: Some(Duration::from_millis(800)),
            pull_timeout: Some(Duration::from_millis(800)),
        })
    }

    #[tokio::test]
    async fn filters_on_the_label_a_container_was_created_with() {
        let fake = sweeper("");
        engine(&fake, &[]).reap_orphans().await.unwrap();
        let ps = fake.calls().remove(0);
        // A label rather than a name prefix: a label cannot drift from whatever
        // this version happens to name things.
        assert!(ps.contains("label=darkwire.session"), "{ps}");
        assert!(ps.contains(OWNER_LABEL), "{ps}");
    }

    #[tokio::test]
    async fn spares_this_processs_own_containers() {
        // Shutdown reaps them, and doing it here would kill the container the
        // turn that triggered this sweep is about to use.
        let fake = sweeper("abc me:1\\n");
        engine(&fake, &[]).reap_orphans().await.unwrap();
        assert!(removed(&fake).is_empty());
    }

    #[tokio::test]
    async fn spares_a_peers_container_while_that_peer_is_running() {
        let fake = sweeper("abc peer:2\\n");
        engine(&fake, &["peer:2"]).reap_orphans().await.unwrap();
        assert!(removed(&fake).is_empty());
    }

    #[tokio::test]
    async fn removes_a_container_whose_owner_is_gone() {
        let fake = sweeper("abc peer:2\\ndef peer:3\\n");
        engine(&fake, &["peer:3"]).reap_orphans().await.unwrap();
        // A force removal, not a stop: these are already unowned, and a stop on
        // a container whose process is gone waits out the timeout for nothing.
        assert_eq!(removed(&fake), vec!["rm --force abc".to_owned()]);
    }

    #[tokio::test]
    async fn removes_an_unlabelled_container_from_before_the_label_existed() {
        // Reaped, which is the behaviour it was created under.
        let fake = sweeper("bare\\n");
        engine(&fake, &[]).reap_orphans().await.unwrap();
        assert_eq!(removed(&fake), vec!["rm --force bare".to_owned()]);
    }

    #[tokio::test]
    async fn ignores_blank_lines_and_a_listing_that_failed() {
        let fake = sweeper("\\n   \\n");
        engine(&fake, &[]).reap_orphans().await.unwrap();
        assert!(removed(&fake).is_empty());

        // A `ps` that exits non-zero yields nothing rather than a refusal: an
        // orphan nobody could list is untidy, never fatal.
        let failing = fake_failing();
        engine(&failing, &[]).reap_orphans().await.unwrap();
        assert!(removed(&failing).is_empty());
    }

    fn fake_failing() -> Fake {
        fake("exit 7")
    }
}

#[test]
fn defaults_to_the_docker_cli_owned_by_this_process() {
    let options = DockerEngineOptions::default();
    assert_eq!(options.bin, "docker");
    assert!(options.owner.is_none());
    // Built without reaching the daemon.
    let _engine = docker_engine(DockerEngineOptions::default());
    assert!(owner_tag().contains(':'));
    assert!(format!("{:?}", DockerEngineOptions::default()).contains("DockerEngineOptions"));
}

#[test]
fn the_shipped_deadlines_are_the_ones_measured_against_a_dead_socket() {
    assert_eq!(CONTROL_TIMEOUT, Duration::from_secs(5));
    assert_eq!(START_TIMEOUT, Duration::from_mins(1));
}

/// Turning what an operator typed into what a definition may pin.
///
/// The whole point is that the digest requirement does not move: every case
/// below either produces a content address or refuses. Four shapes, because
/// each comes from a different place an image can live.
mod resolving_an_image {
    use super::*;

    const REGISTRY_DIGEST: &str =
        "node@sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const LOCAL_ID: &str =
        "sha256:2222222222222222222222222222222222222222222222222222222222222222";

    fn engine(fake: &Fake) -> Arc<dyn ContainerEngine> {
        fake.engine_with(DockerEngineOptions {
            control_timeout: Some(Duration::from_millis(400)),
            pull_timeout: Some(Duration::from_millis(800)),
            ..DockerEngineOptions::default()
        })
    }

    #[tokio::test]
    async fn an_image_already_here_resolves_without_a_pull() {
        // The common case once an operator has the image: instant, and no
        // network. `pulled` says so, because the screen that waited on it is
        // the one that has to explain the difference.
        let fake = fake(&format!(
            "case \"$*\" in\n  *RepoDigests*) echo '{REGISTRY_DIGEST}'; exit 0;;\n  *) exit 1;;\nesac"
        ));

        let resolved = engine(&fake)
            .resolve_image("node:22")
            .await
            .expect("a digest");

        assert_eq!(resolved.image, REGISTRY_DIGEST);
        assert!(!resolved.pulled);
        assert!(
            !fake.calls().iter().any(|call| call.starts_with("pull")),
            "{:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn an_image_that_is_not_here_yet_is_pulled_first() {
        // Two inspects around one pull: the first misses, the pull fetches, the
        // second reads the digest off what arrived.
        let fake = fake(&format!(
            "if [ -f \"$(dirname \"$0\")/pulled\" ]; then\n  case \"$*\" in *RepoDigests*) echo '{REGISTRY_DIGEST}'; exit 0;; esac\nfi\ncase \"$*\" in\n  pull*) touch \"$(dirname \"$0\")/pulled\"; exit 0;;\nesac\nexit 1"
        ));

        let resolved = engine(&fake)
            .resolve_image("node:22")
            .await
            .expect("a digest");

        assert_eq!(resolved.image, REGISTRY_DIGEST);
        assert!(resolved.pulled);
        assert!(
            fake.calls().iter().any(|call| call == "pull node:22"),
            "{:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn an_image_built_here_and_never_pushed_falls_back_to_its_id() {
        // No `RepoDigests` at all, which is what a local `docker build`
        // produces. Its own id is still a content address, so it still pins.
        let fake = fake(&format!(
            "case \"$*\" in\n  *RepoDigests*) exit 1;;\n  *'{{{{.Id}}}}'*) echo '{LOCAL_ID}'; exit 0;;\n  *) exit 1;;\nesac"
        ));

        let resolved = engine(&fake)
            .resolve_image("mine:dev")
            .await
            .expect("an id");

        assert_eq!(resolved.image, LOCAL_ID);
        assert!(!resolved.pulled);
    }

    #[tokio::test]
    async fn a_reference_that_does_not_exist_reports_what_the_engine_said() {
        // The engine knows whether this was a typo, a private registry or no
        // network at all. Any sentence invented here would be a worse guess.
        let fake = fake(
            "case \"$*\" in\n  pull*) echo 'manifest unknown' >&2; exit 1;;\n  *) exit 1;;\nesac",
        );

        let error = engine(&fake)
            .resolve_image("node:nope")
            .await
            .expect_err("a refusal");

        assert_eq!(error.kind, ErrorKind::Tool);
        assert!(error.message.contains("manifest unknown"), "{error:?}");
        assert!(error.message.contains("node:nope"), "{error:?}");
    }
}
