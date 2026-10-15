//! The daemon CLI as a [`ContainerEngine`]: `docker`, or `podman`, run with an
//! argv and a deadline, never through a shell.

use std::process;
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::environment::EnvironmentDefinition;
use darkwire_protocol::{EnvironmentNetwork, NetworkMode};
use darkwire_tools::BoxFuture;

use crate::container_pool::{
    ContainerEngine, OWNER_LABEL, ResolvedImage, owner_process_looks_alive, owner_tag,
};

/// How long a control-plane call may take before it is treated as unreachable.
///
/// Not decoration. A daemon CLI talking to a socket whose daemon has gone away —
/// the desktop app quit, the socket file left behind — does not fail fast: it
/// blocks. Measured at 62 seconds for one list. Without a bound, one closed
/// daemon turns every sandboxed turn into a minute of nothing, and anything that
/// called this at boot into a server that never binds its port.
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

/// A container start may have to load a large image, so it gets longer.
pub const START_TIMEOUT: Duration = Duration::from_mins(1);

/// A pull may fetch gigabytes over whatever link the machine has.
///
/// Generous on purpose, and the only deadline here measured in minutes. It is a
/// bound on a runaway rather than an expectation: an operator pressed a button
/// and is watching, and a resolve that failed at thirty seconds on a slow link
/// would send them back to a terminal, which is the thing this removes.
pub const PULL_TIMEOUT: Duration = Duration::from_mins(10);

/// The first non-empty line of some output, trimmed.
///
/// Engine output arrives with a trailing newline, and a failure's stderr is
/// often several lines of which the first is the reason.
fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

/// Whether the process behind an owner tag is still running. Injected so the
/// sweep's ownership rules are testable without a live peer.
pub type LivenessFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// What a real engine is built from.
#[derive(Clone)]
pub struct DockerEngineOptions {
    /// Pinned image containing the firewall bootstrap and domain proxy.
    pub gateway_image: Option<String>,
    /// `podman` is wire-compatible for everything used here.
    pub bin: String,
    /// Defaults to [`owner_tag`], matching the pool's default.
    pub owner: Option<String>,
    /// Injected so the sweep's ownership rules are testable without a peer
    /// process to point at. Defaults to [`owner_process_looks_alive`].
    pub is_owner_alive: Option<LivenessFn>,
    /// How long a control-plane call may take. Defaults to
    /// [`CONTROL_TIMEOUT`].
    ///
    /// A seam rather than a constant because the deadline is the whole of what
    /// "the daemon has gone away" means here, and a test that had to wait out
    /// five real seconds to see it is a test nobody runs.
    pub control_timeout: Option<Duration>,
    /// How long a container start may take. Defaults to [`START_TIMEOUT`].
    pub start_timeout: Option<Duration>,
    /// How long resolving an image may take. Defaults to [`PULL_TIMEOUT`].
    pub pull_timeout: Option<Duration>,
}

impl Default for DockerEngineOptions {
    fn default() -> Self {
        DockerEngineOptions {
            gateway_image: None,
            bin: "docker".to_owned(),
            owner: None,
            is_owner_alive: None,
            control_timeout: None,
            start_timeout: None,
            pull_timeout: None,
        }
    }
}

impl std::fmt::Debug for DockerEngineOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DockerEngineOptions")
            .field("bin", &self.bin)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// The real engine: the `docker` (or `podman`) CLI.
///
/// Each call is a child process awaited under its own deadline, so a daemon
/// that has gone away costs the waiting task that deadline and costs the
/// runtime nothing.
pub struct DockerEngine {
    gateway_image: Option<String>,
    bin: String,
    owner: String,
    is_owner_alive: LivenessFn,
    control_timeout: Duration,
    start_timeout: Duration,
    pull_timeout: Duration,
}

impl std::fmt::Debug for DockerEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DockerEngine")
            .field("bin", &self.bin)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// One CLI call's result, with a killed child distinguishable from a refusal.
struct CliOutput {
    /// `None` when the child was killed for outliving its deadline.
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// The daemon CLI as a [`ContainerEngine`].
pub fn docker_engine(options: DockerEngineOptions) -> Arc<dyn ContainerEngine> {
    Arc::new(DockerEngine {
        gateway_image: options.gateway_image,
        bin: options.bin,
        owner: options.owner.unwrap_or_else(owner_tag),
        is_owner_alive: options
            .is_owner_alive
            .unwrap_or_else(|| Arc::new(owner_process_looks_alive)),
        control_timeout: options.control_timeout.unwrap_or(CONTROL_TIMEOUT),
        start_timeout: options.start_timeout.unwrap_or(START_TIMEOUT),
        pull_timeout: options.pull_timeout.unwrap_or(PULL_TIMEOUT),
    })
}

impl DockerEngine {
    /// One control-plane call, bounded.
    ///
    /// A timeout arrives as a killing signal rather than as a non-zero status,
    /// so checking the status alone would read "killed after 5s" as a clean
    /// failure of the command itself.
    async fn run(&self, argv: &[String], what: &str, timeout: Duration) -> Result<()> {
        let output = self.capture_raw(argv, timeout).await?;
        self.status(&output, what, timeout)
    }

    /// [`DockerEngine::run`]'s answer for an output already captured.
    fn status(&self, output: &CliOutput, what: &str, timeout: Duration) -> Result<()> {
        if output.code == Some(0) {
            return Ok(());
        }
        if output.code.is_none() {
            return Err(WireError::new(
                ErrorKind::Tool,
                format!(
                    "{} {what} did not respond within {}ms. Is the daemon running?",
                    self.bin,
                    timeout.as_millis()
                ),
            )
            .with_detail("bin", self.bin.clone())
            .with_detail("what", what));
        }
        Err(WireError::new(
            ErrorKind::Tool,
            format!("{} {what} failed: {}", self.bin, output.stderr.trim()),
        )
        .with_detail("bin", self.bin.clone())
        .with_detail("what", what))
    }

    /// The raw result of one call, with a hard deadline.
    ///
    /// The deadline covers the exit and both pipes together, and when it fires
    /// the child is killed and the pipes are abandoned rather than read to EOF.
    /// A CLI that forks, and `docker` does, leaves the grandchild holding the
    /// write ends, so draining them would wait out the very command the
    /// deadline exists to abandon. The killed child is reported with no exit
    /// code and no output, which is what `run` needs to tell a deadline from a
    /// refusal.
    async fn capture_raw(&self, argv: &[String], timeout: Duration) -> Result<CliOutput> {
        let mut child = tokio::process::Command::new(&self.bin)
            .args(argv)
            .stdin(process::Stdio::null())
            .stdout(process::Stdio::piped())
            .stderr(process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                WireError::new(
                    ErrorKind::Tool,
                    format!("Could not run {}: {error}", self.bin),
                )
                .with_detail("bin", self.bin.clone())
                .with_source(error)
            })?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let finished = tokio::time::timeout(timeout, async {
            tokio::join!(child.wait(), drain(stdout), drain(stderr))
        })
        .await;
        let Ok((status, stdout, stderr)) = finished else {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Ok(CliOutput {
                code: None,
                stdout: String::new(),
                stderr: String::new(),
            });
        };
        let status = status.map_err(|error| {
            WireError::new(
                ErrorKind::Tool,
                format!("Could not wait for {}: {error}", self.bin),
            )
            .with_source(error)
        })?;
        let unreadable = |error: std::io::Error| {
            WireError::new(
                ErrorKind::Tool,
                format!("Could not read {} output: {error}", self.bin),
            )
            .with_source(error)
        };
        Ok(CliOutput {
            code: Some(status.code().unwrap_or(-1)),
            stdout: String::from_utf8_lossy(&stdout.map_err(unreadable)?).into_owned(),
            stderr: String::from_utf8_lossy(&stderr.map_err(unreadable)?).into_owned(),
        })
    }

    /// The digest-pinned form of a reference the engine already holds.
    ///
    /// `RepoDigests` first, because a registry digest names the image wherever
    /// it is pulled from and is what a definition on another machine can pin
    /// too. An image built locally and never pushed has none, so its own `Id`
    /// is the fallback: still a content address, still immutable, just only
    /// meaningful on this engine. `None` means the engine does not hold it.
    async fn inspect_digest(&self, reference: &str) -> Option<String> {
        let repo = self
            .capture(&argv(&[
                "image",
                "inspect",
                reference,
                "--format",
                "{{index .RepoDigests 0}}",
            ]))
            .await;
        if let Some(digest) = first_line(&repo).filter(|line| line.contains("@sha256:")) {
            return Some(digest.to_owned());
        }
        let id = self
            .capture(&argv(&[
                "image", "inspect", reference, "--format", "{{.Id}}",
            ]))
            .await;
        first_line(&id)
            .filter(|line| line.starts_with("sha256:"))
            .map(str::to_owned)
    }

    /// Stdout of one call, or empty when it failed.
    async fn capture(&self, argv: &[String]) -> String {
        match self.capture_raw(argv, self.control_timeout).await {
            Ok(output) if output.code == Some(0) => output.stdout,
            _ => String::new(),
        }
    }
}

impl DockerEngine {
    async fn start_gateway(
        &self,
        name: &str,
        container: &EnvironmentDefinition,
        network: &EnvironmentNetwork,
    ) -> Result<Option<String>> {
        if network.mode != NetworkMode::Allowlist {
            return Ok(None);
        }
        let image = self.gateway_image.as_ref().ok_or_else(|| {
            WireError::new(
                ErrorKind::Config,
                "Restricted egress requires a pinned gatewayImage in service configuration",
            )
        })?;
        // The gateway's own image gets the digest-pin check every tool image
        // gets. It holds NET_ADMIN, so a repointable tag here would be the one
        // place in the system where an unreviewed image rewrites the filter.
        let mut gateway_definition = container.clone();
        gateway_definition.image.clone_from(image);
        darkwire_security::assert_environment_policy(&gateway_definition)?;
        let rules = darkwire_security::egress::gateway_rules(container, network)?;
        let gateway = format!("{name}-gateway");
        let mut args = argv(&[
            "run",
            "--detach",
            "--rm",
            "--name",
            &gateway,
            "--cap-drop=ALL",
            "--cap-add=NET_ADMIN",
            "--cap-add=SETUID",
            "--cap-add=SETGID",
            "--security-opt=no-new-privileges",
            "--read-only",
            "--tmpfs=/tmp:rw,nosuid,size=16m",
            "--memory=128m",
            "--pids-limit=64",
            "--cpus=0.5",
            "--label",
            &format!("{OWNER_LABEL}={}", self.owner),
            "--label",
            "darkwire.session=gateway",
            "--env",
        ]);
        args.push(format!("DARKWIRE_NFT_RULES={rules}"));
        args.push(image.clone());
        // The whole list, addresses included: the proxy matches an address
        // destination against the blocks, so the two enforcement points read the
        // same entries rather than two halves of them.
        args.extend(network.allow.clone());
        self.run(&args, "gateway start", self.start_timeout).await?;
        let ready = self.run(&argv(&["exec", &gateway, "sh", "-c", "for n in 1 2 3 4 5 6 7 8 9 10; do test -f /tmp/ready && exit 0; sleep 0.2; done; exit 1"]), "gateway readiness", self.control_timeout).await;
        if let Err(error) = ready {
            let _ = self.stop_container(&gateway).await;
            return Err(error);
        }
        Ok(Some(gateway))
    }

    async fn reap(&self) -> Result<()> {
        // A label filter rather than a name prefix: a label is what the
        // container was *created* with, so it cannot drift from whatever this
        // version happens to name things. The owner comes back in the same call
        // because the daemon cannot filter on *not* matching a label, so the
        // decision has to be made here.
        let format = format!("{{{{.ID}}}} {{{{.Label \"{OWNER_LABEL}\"}}}}");
        let rows = self
            .capture(&argv(&[
                "ps",
                "--all",
                "--filter",
                "label=darkwire.session",
                "--format",
                &format,
            ]))
            .await;

        for row in rows.lines().map(str::trim).filter(|row| !row.is_empty()) {
            let (id, container_owner) = match row.split_once(' ') {
                Some((id, rest)) => (id, rest.trim()),
                None => (row, ""),
            };
            if let Some((installation, _)) = self.owner.rsplit_once('/')
                && self.owner.starts_with("service:")
                && !container_owner.starts_with(&format!("{installation}/"))
            {
                continue;
            }

            // This process's own. Shutdown reaps them, and doing it here would
            // kill the container the turn that triggered this sweep is about to
            // use.
            if container_owner == self.owner {
                continue;
            }
            // A peer's, and it is still running. An unlabelled container, from
            // a version before this label existed, is reaped, which is the
            // behaviour it was created under.
            if !container_owner.is_empty() && (self.is_owner_alive)(container_owner) {
                continue;
            }

            // A force removal, not a stop: these are already unowned, and a stop
            // on a container whose process is gone waits out the timeout for
            // nothing.
            self.capture(&argv(&["rm", "--force", id])).await;
        }
        Ok(())
    }

    async fn resolve(&self, reference: &str) -> Result<ResolvedImage> {
        // Locally first, so a reference the engine already holds answers
        // instantly and an operator fixing a typo is not made to wait on a
        // network round trip.
        if let Some(image) = self.inspect_digest(reference).await {
            return Ok(ResolvedImage {
                image,
                pulled: false,
            });
        }
        let pull = self
            .capture_raw(&argv(&["pull", reference]), self.pull_timeout)
            .await?;
        if pull.code != Some(0) {
            // The engine's own words. It knows whether this was a typo, a
            // private registry or no network, and a sentence invented here
            // would be a worse guess than any of them.
            let said = first_line(&pull.stderr).unwrap_or("the engine gave no reason");
            return Err(WireError::new(
                ErrorKind::Tool,
                format!("Could not pull \"{reference}\": {said}"),
            )
            .with_detail("reference", reference.to_owned()));
        }
        let image = self.inspect_digest(reference).await.ok_or_else(|| {
            WireError::new(
                ErrorKind::Tool,
                format!("Pulled \"{reference}\" but the engine reported no digest for it"),
            )
            .with_detail("reference", reference.to_owned())
        })?;
        Ok(ResolvedImage {
            image,
            pulled: true,
        })
    }

    async fn start_container(&self, args: &[String]) -> Result<()> {
        let output = self.capture_raw(args, self.start_timeout).await?;
        // A desktop daemon's file sharing does not see a directory the instant
        // it is created: the transcript directory is made microseconds before
        // this call, and the daemon can answer "bind source path does not
        // exist" for a path that demonstrably does. One retry after a beat is
        // the whole fix, and it is scoped to that exact message so a genuinely
        // absent path still fails fast. Read from the daemon's own stderr, like
        // `container_is_gone`, rather than from an error built out of it.
        if output.code.is_some_and(|code| code != 0)
            && output.stderr.contains("bind source path does not exist")
        {
            tokio::time::sleep(Duration::from_millis(250)).await;
            return self.run(args, "run", self.start_timeout).await;
        }
        self.status(&output, "run", self.start_timeout)
    }

    async fn stop_container(&self, name: &str) -> Result<()> {
        // Two seconds: a sandbox holds no state worth a graceful shutdown, and a
        // reap that blocks ten seconds per container is a reap nobody runs.
        self.run(
            &argv(&["stop", "--time", "2", name]),
            "stop",
            self.control_timeout,
        )
        .await
    }
}

impl ContainerEngine for DockerEngine {
    fn gateway<'a>(
        &'a self,
        name: &'a str,
        container: &'a EnvironmentDefinition,
        network: &'a EnvironmentNetwork,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(self.start_gateway(name, container, network))
    }

    fn probe(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            self.run(
                &argv(&["version", "--format", "{{.Server.Version}}"]),
                "version",
                self.control_timeout,
            )
            .await
        })
    }

    fn reap_orphans(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.reap())
    }

    fn resolve_image<'a>(&'a self, reference: &'a str) -> BoxFuture<'a, Result<ResolvedImage>> {
        Box::pin(self.resolve(reference))
    }

    fn start<'a>(&'a self, argv: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.start_container(argv))
    }

    fn stop<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.stop_container(name))
    }
}

/// Everything a pipe holds until it closes, or nothing for a pipe not taken.
async fn drain(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut bytes).await?;
    }
    Ok(bytes)
}

/// Borrowed argv as owned, which is what the engine's calls take.
fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}
