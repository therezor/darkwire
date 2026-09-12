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

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ghostai_core::ErrorKind;
use ghostai_runtime::{
    ContainerEngine, DockerEngineOptions, OWNER_LABEL, docker_engine, owner_tag,
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

#[test]
fn probe_asks_the_daemon_for_its_version() {
    let fake = fake("exit 0");
    assert!(fake.engine().probe().is_ok());
    assert_eq!(fake.calls(), vec!["version --format {{.Server.Version}}"]);
}

#[test]
fn a_non_zero_exit_carries_the_daemons_own_words() {
    // A bare "could not be started" sends the reader to the logs for the one
    // fact that would have told them what to do.
    let fake = fake("echo 'Cannot connect to the Docker daemon' >&2\nexit 1");
    let error = fake.engine().probe().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Tool);
    assert!(
        error.message.contains("Cannot connect"),
        "{}",
        error.message
    );
    assert_eq!(error.details["what"], "version");
}

#[test]
fn a_call_that_never_returns_is_a_deadline_rather_than_a_clean_failure() {
    // A CLI talking to a socket whose daemon has gone away does not fail fast:
    // it blocks. A timeout arrives as a killing signal rather than as a status,
    // so checking the status alone would read "killed" as an ordinary refusal.
    let fake = fake("sleep 30");
    let error = fake.engine().probe().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Tool);
    assert!(
        error.message.contains("did not respond within"),
        "{}",
        error.message
    );
}

#[test]
fn a_binary_that_is_not_there_names_itself() {
    let engine = docker_engine(DockerEngineOptions {
        bin: "/nonexistent/docker".to_owned(),
        ..DockerEngineOptions::default()
    });
    let error = engine.probe().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Tool);
    assert!(error.message.contains("Could not run"), "{}", error.message);
    assert_eq!(error.details["bin"], "/nonexistent/docker");
}

#[test]
fn stop_gives_a_sandbox_two_seconds_rather_than_ten() {
    // A sandbox holds no state worth a graceful shutdown, and a reap that blocks
    // ten seconds per container is a reap nobody runs.
    let fake = fake("exit 0");
    fake.engine().stop("ghost-sbx-1").unwrap();
    assert_eq!(fake.calls(), vec!["stop --time 2 ghost-sbx-1"]);
}

#[test]
fn start_runs_the_argv_it_was_handed() {
    let fake = fake("exit 0");
    fake.engine()
        .start(&argv(&["run", "--detach", "x"]))
        .unwrap();
    assert_eq!(fake.calls(), vec!["run --detach x"]);
}

#[test]
fn start_retries_once_for_a_mount_source_the_daemon_cannot_see_yet() {
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
    assert!(fake.engine().start(&argv(&["run", "x"])).is_ok());
    assert_eq!(fake.calls().len(), 2);
}

#[test]
fn start_does_not_retry_a_genuinely_absent_path() {
    // Scoped to that exact message, so a path that is really missing fails fast.
    let fake = fake("echo 'no such image' >&2\nexit 1");
    let error = fake.engine().start(&argv(&["run", "x"])).unwrap_err();
    assert!(error.message.contains("no such image"), "{}", error.message);
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn start_gives_up_when_the_retry_fails_too() {
    let fake = fake("echo 'bind source path does not exist: /runs' >&2\nexit 1");
    assert!(fake.engine().start(&argv(&["run", "x"])).is_err());
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
            owner: Some("me:1".to_owned()),
            is_owner_alive: Some(Arc::new(move |owner: &str| alive.contains(&owner))),
            control_timeout: Some(Duration::from_millis(400)),
            start_timeout: Some(Duration::from_millis(800)),
        })
    }

    #[test]
    fn filters_on_the_label_a_container_was_created_with() {
        let fake = sweeper("");
        engine(&fake, &[]).reap_orphans().unwrap();
        let ps = fake.calls().remove(0);
        // A label rather than a name prefix: a label cannot drift from whatever
        // this version happens to name things.
        assert!(ps.contains("label=ghostai.session"), "{ps}");
        assert!(ps.contains(OWNER_LABEL), "{ps}");
    }

    #[test]
    fn spares_this_processs_own_containers() {
        // Shutdown reaps them, and doing it here would kill the container the
        // turn that triggered this sweep is about to use.
        let fake = sweeper("abc me:1\\n");
        engine(&fake, &[]).reap_orphans().unwrap();
        assert!(removed(&fake).is_empty());
    }

    #[test]
    fn spares_a_peers_container_while_that_peer_is_running() {
        let fake = sweeper("abc peer:2\\n");
        engine(&fake, &["peer:2"]).reap_orphans().unwrap();
        assert!(removed(&fake).is_empty());
    }

    #[test]
    fn removes_a_container_whose_owner_is_gone() {
        let fake = sweeper("abc peer:2\\ndef peer:3\\n");
        engine(&fake, &["peer:3"]).reap_orphans().unwrap();
        // A force removal, not a stop: these are already unowned, and a stop on
        // a container whose process is gone waits out the timeout for nothing.
        assert_eq!(removed(&fake), vec!["rm --force abc".to_owned()]);
    }

    #[test]
    fn removes_an_unlabelled_container_from_before_the_label_existed() {
        // Reaped, which is the behaviour it was created under.
        let fake = sweeper("bare\\n");
        engine(&fake, &[]).reap_orphans().unwrap();
        assert_eq!(removed(&fake), vec!["rm --force bare".to_owned()]);
    }

    #[test]
    fn ignores_blank_lines_and_a_listing_that_failed() {
        let fake = sweeper("\\n   \\n");
        engine(&fake, &[]).reap_orphans().unwrap();
        assert!(removed(&fake).is_empty());

        // A `ps` that exits non-zero yields nothing rather than a refusal: an
        // orphan nobody could list is untidy, never fatal.
        let failing = fake_failing();
        engine(&failing, &[]).reap_orphans().unwrap();
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
    assert_eq!(
        ghostai_runtime::toolbox_pool::CONTROL_TIMEOUT,
        Duration::from_secs(5)
    );
    assert_eq!(
        ghostai_runtime::toolbox_pool::START_TIMEOUT,
        Duration::from_mins(1)
    );
    let _ = Path::new("/");
}
