//! The child process an extension is, and the four things the host controls
//! about it: what it runs, what it can see, where its noise goes, and how it
//! is made to stop.
//!
//! ## What it runs
//!
//! `Command::new(argv[0]).args(&argv[1..])`, with the install directory as the
//! working directory and no shell anywhere. The security layer has already
//! refused a shell binary and a program path that escapes the install, so by
//! the time an argv reaches here it is arithmetic.
//!
//! ## What it can see
//!
//! The environment is **built, not inherited**. A host process that is serving
//! models has provider API keys in its own environment, and passing that
//! wholesale to third-party code would hand over every credential on the box to
//! anything an operator approved once. So the child gets the four variables a
//! program needs to run at all, plus whatever names the manifest asked for —
//! *names*, so a manifest can request `NODE_EXTRA_CA_CERTS` but cannot invent a
//! value — plus two the host sets itself, which are the whole of what an
//! extension knows about its own installation before `initialize` arrives.
//!
//! ## Where its noise goes
//!
//! stdout is the wire and nothing else may touch it. stderr is drained into
//! `tracing` under a byte budget: past the budget the logging stops but the
//! *draining does not*, because a child whose stderr pipe fills up blocks on
//! its next write and looks, from out here, exactly like one that hung.
//!
//! ## How it is made to stop
//!
//! Closing stdin, then `SIGTERM`, then `SIGKILL`, each with the same grace
//! between — the order the local command runner uses, for the same reason.
//! Signals go to the process **group**, not the process: `node index.mjs` that
//! forked a worker would otherwise leave the worker behind holding the pipe,
//! and `kill_on_drop` reaches only the child it spawned. The group is created
//! at spawn time with `process_group(0)` so the host's own group is never in
//! range of a signal meant for an extension.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::DEFAULT_EXTENSION_ENV;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// How long a child gets to leave on its own before the next escalation.
pub const KILL_GRACE_MS: u64 = 2000;

/// Beyond this many stderr bytes the logging stops. The stream is still
/// drained, so the child never blocks on a full pipe.
pub const STDERR_BUDGET_BYTES: usize = darkwire_mcp::STDERR_BUDGET_BYTES;

/// The extension's own id, for a child that wants to know what it was
/// installed as without waiting for `initialize`.
pub const ENV_EXTENSION_ID: &str = "DARKWIRE_EXTENSION_ID";

/// The directory an extension may write to.
pub const ENV_EXTENSION_DATA_DIR: &str = "DARKWIRE_EXTENSION_DATA_DIR";

/// What to spawn, and the environment to spawn it in.
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    /// The extension id. Names the log target and the id variable.
    pub id: String,
    /// The install directory. Becomes the child's working directory.
    pub dir: PathBuf,
    /// argv. Already checked by the security layer.
    pub command: Vec<String>,
    /// Host variable *names* the manifest asked for, beyond the default four.
    pub env: Vec<String>,
    /// The directory the child is told it may write to.
    pub data_dir: PathBuf,
    /// How long each stop escalation waits.
    pub kill_grace: Duration,
}

impl SpawnOptions {
    /// Options with the shipped grace period.
    pub fn new(
        id: impl Into<String>,
        dir: impl Into<PathBuf>,
        command: Vec<String>,
        data_dir: impl Into<PathBuf>,
    ) -> SpawnOptions {
        SpawnOptions {
            id: id.into(),
            dir: dir.into(),
            command,
            env: Vec::new(),
            data_dir: data_dir.into(),
            kill_grace: Duration::from_millis(KILL_GRACE_MS),
        }
    }

    /// Adds the manifest's additional variable names.
    #[must_use]
    pub fn with_env(mut self, env: Vec<String>) -> SpawnOptions {
        self.env = env;
        self
    }

    /// Shortens every escalation, for a test that must not wait seconds.
    #[must_use]
    pub fn with_kill_grace(mut self, kill_grace: Duration) -> SpawnOptions {
        self.kill_grace = kill_grace;
        self
    }
}

/// The environment a child is given: names from the allow-list that this host
/// actually has, plus the two the host sets.
///
/// A name that is not set here is simply absent there. That is better than an
/// empty string, which a program reading `TMPDIR` would treat as a directory.
fn environment(options: &SpawnOptions) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let wanted = DEFAULT_EXTENSION_ENV
        .iter()
        .map(|name| (*name).to_owned())
        .chain(options.env.iter().cloned());
    for name in wanted {
        if out
            .iter()
            .any(|(existing, _): &(String, String)| *existing == name)
        {
            continue;
        }
        if let Ok(value) = std::env::var(&name) {
            out.push((name, value));
        }
    }
    out.push((ENV_EXTENSION_ID.to_owned(), options.id.clone()));
    out.push((
        ENV_EXTENSION_DATA_DIR.to_owned(),
        options.data_dir.to_string_lossy().into_owned(),
    ));
    out
}

/// How a child ended, as a sentence for the row.
///
/// Deliberately free of a pid and a timestamp: the reconcile that decides
/// whether a row *moved* compares this string, and a sentence carrying either
/// would differ on every pass and announce a change that did not happen.
fn describe_exit(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("The extension process was killed by signal {signal}.");
        }
    }
    match status.code() {
        Some(code) => format!("The extension process exited with status {code}."),
        None => "The extension process exited.".to_owned(),
    }
}

/// Signals a whole process group, best effort.
///
/// A child that has already gone is the common case and there is nothing to do
/// about a signal that cannot be delivered.
#[cfg(unix)]
fn signal_group(pid: Option<u32>, signal: nix::sys::signal::Signal) {
    if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
        let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), signal);
    }
}

#[cfg(not(unix))]
fn signal_group(_pid: Option<u32>, _signal: ()) {}

/// A running extension, and the streams the wire is carried on.
#[derive(Debug)]
pub struct Spawned {
    /// The child's stdout: every frame the extension sends.
    pub stdout: ChildStdout,
    /// The child's stdin: every frame the host sends. Closing it asks it to
    /// stop.
    pub stdin: ChildStdin,
    /// The handle that watches it and takes it down.
    pub process: Arc<ExtensionProcess>,
}

/// One supervised child process.
///
/// Holds no `Child`: the supervisor task owns it, because waiting on a child
/// needs it exclusively and two owners would mean the host could not both watch
/// for a crash and ask for a stop.
pub struct ExtensionProcess {
    id: String,
    pid: Option<u32>,
    token: CancellationToken,
    exit: watch::Receiver<Option<String>>,
}

impl std::fmt::Debug for ExtensionProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionProcess")
            .field("id", &self.id)
            .field("pid", &self.pid)
            .field("running", &self.exit.borrow().is_none())
            .finish_non_exhaustive()
    }
}

impl ExtensionProcess {
    /// The extension this process belongs to.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Its process id, while it is running.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Whether it has ended.
    pub fn has_exited(&self) -> bool {
        self.exit.borrow().is_some()
    }

    /// Waits for it to end, and answers with the sentence describing how.
    ///
    /// Returns immediately when it already has, which is what lets the host
    /// watch a process it may have just asked to stop without racing.
    pub async fn wait(&self) -> String {
        let mut exit = self.exit.clone();
        loop {
            if let Some(status) = exit.borrow().clone() {
                return status;
            }
            if exit.changed().await.is_err() {
                return "The extension process ended.".to_owned();
            }
        }
    }

    /// Asks it to stop, and escalates until it has.
    ///
    /// Returns once the child is gone, so a caller that respawns cannot end up
    /// with two children holding the same data directory.
    pub async fn stop(&self) {
        self.token.cancel();
        self.wait().await;
    }
}

/// Spawns one extension.
///
/// Fails only where spawning itself fails — a program that is not on `PATH`, a
/// directory that is gone. Everything after that is the host's problem to
/// report on a row, which is why this is the only fallible step.
pub fn spawn(options: SpawnOptions) -> Result<Spawned> {
    let Some((program, args)) = options.command.split_first() else {
        return Err(
            WireError::new(ErrorKind::Config, "The extension names no command to run.")
                .with_detail("extension", options.id.as_str()),
        );
    };

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&options.dir)
        .env_clear()
        .envs(environment(&options))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The backstop: a host that panics or is killed does not leave a child
        // holding a pipe. It reaches the child alone, which is why the group
        // below exists as well.
        .kill_on_drop(true);

    #[cfg(unix)]
    {
        // A new process group, so a signal meant for this extension cannot
        // reach the host, and so a worker the extension forked is in range.
        command.process_group(0);
    }

    let mut child = command.spawn().map_err(|error| {
        WireError::new(
            ErrorKind::Extension,
            format!(
                "The extension \"{}\" could not be started: {program} ({error})",
                options.id
            ),
        )
        .with_detail("extension", options.id.as_str())
        .with_detail("program", program.as_str())
        .with_source(error)
    })?;

    let pid = child.id();
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(WireError::new(
            ErrorKind::Internal,
            "The extension process was spawned without its pipes.",
        )
        .with_detail("extension", options.id.as_str()));
    };

    drain_stderr(options.id.clone(), stderr);

    let token = CancellationToken::new();
    let (exit_tx, exit_rx) = watch::channel(None);
    let supervisor_token = token.clone();
    let grace = options.kill_grace;
    let id = options.id.clone();
    tokio::spawn(async move {
        let status = supervise(&mut child, pid, &supervisor_token, grace).await;
        tracing::debug!(target: "extension", extension = %id, status, "the extension process ended");
        let _ = exit_tx.send(Some(status));
    });

    Ok(Spawned {
        stdout,
        stdin,
        process: Arc::new(ExtensionProcess {
            id: options.id,
            pid,
            token,
            exit: exit_rx,
        }),
    })
}

/// Waits for the child, escalating once the host has asked it to stop.
async fn supervise(
    child: &mut tokio::process::Child,
    pid: Option<u32>,
    token: &CancellationToken,
    grace: Duration,
) -> String {
    tokio::select! {
        status = child.wait() => return exited(status),
        () = token.cancelled() => {}
    }

    // Stdin is already closed: the writer task shuts it down when the same
    // token fires. A well-behaved child is gone before the first escalation.
    #[cfg(unix)]
    {
        if let Ok(Some(status)) = wait_for(child, grace).await {
            return exited(Ok(status));
        }
        signal_group(pid, nix::sys::signal::Signal::SIGTERM);
        if let Ok(Some(status)) = wait_for(child, grace).await {
            return exited(Ok(status));
        }
        signal_group(pid, nix::sys::signal::Signal::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        if let Ok(Some(status)) = wait_for(child, grace).await {
            return exited(Ok(status));
        }
        let _ = child.start_kill();
    }
    exited(child.wait().await)
}

/// The child's exit, or `None` when `grace` ran out first.
async fn wait_for(
    child: &mut tokio::process::Child,
    grace: Duration,
) -> Result<Option<std::process::ExitStatus>> {
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(status) => Ok(Some(status.map_err(|error| {
            WireError::new(ErrorKind::Extension, "Could not wait for the extension.")
                .with_source(error)
        })?)),
        Err(_) => Ok(None),
    }
}

fn exited(status: std::io::Result<std::process::ExitStatus>) -> String {
    match status {
        Ok(status) => describe_exit(status),
        Err(error) => format!("The extension process could not be waited for: {error}"),
    }
}

/// Reads stderr to the end, logging what fits in the budget.
fn drain_stderr(id: String, stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut spent = 0usize;
        let mut warned = false;
        while let Ok(Some(line)) = lines.next_line().await {
            spent = spent.saturating_add(line.len());
            if spent <= STDERR_BUDGET_BYTES {
                // One target for every extension, with the id as a field: a
                // filter can select one extension, and `tracing`'s target must
                // be a constant.
                tracing::info!(target: "extension", extension = %id, "{line}");
            } else if !warned {
                warned = true;
                tracing::warn!(
                    target: "extension",
                    extension = %id,
                    budget = STDERR_BUDGET_BYTES,
                    "the extension has written more to stderr than is logged; the rest is dropped"
                );
            }
        }
    });
}

/// The data directory an extension is given, beside the install root.
///
/// A sibling, never a child, and that is the whole of the rule: the approval
/// digest covers every byte under the install directory, so an extension
/// writing state in there would revoke its own approval on its first write.
pub fn data_dir_for(root: &Path, id: &str) -> PathBuf {
    root.join("extension-data").join(id)
}
