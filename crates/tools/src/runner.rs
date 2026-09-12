//! Where a guarded command actually runs.
//!
//! `exec` has always spawned a child process on the host, inside the workspace
//! jail. That is one answer to "where", not the only one, and the jail's own
//! docs are blunt about the limit: *a workspace is an organisational boundary,
//! not a security boundary, wherever `exec` is enabled — a child process can
//! walk out of it.* Closing that gap means running the command somewhere it
//! cannot walk out of, which is a different backend rather than a stricter
//! guard.
//!
//! So the spawn is behind a seam. [`guard_exec`](ghostai_security::guard_exec)
//! still decides **whether** a command may run and with what arguments and
//! environment; a [`CommandRunner`] decides **where**, and
//! [`ExecPlan`] is already the complete description of the job — file, args,
//! cwd, env, timeout and output budget.
//!
//! The constraint a container backend has to honour, recorded here because it
//! is the part that will bite: the guard computes `cwd` from the jail root and
//! the environment from a host allow-list. Both are host-shaped. A runner that
//! mounts the workspace elsewhere has to translate the working directory and
//! every path in `plan.paths` to its own view — and once only the workspace is
//! mounted, the guard's refuse-outside-the-workspace rule becomes redundant
//! rather than wrong, so it stays.

use std::pin::pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures::future::join;
use ghostai_core::{Clock, ErrorKind, GhostError, Result};
use ghostai_protocol::AgentToolboxNetwork;
use ghostai_security::{ExecPlan, OutputCap};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

use crate::tool::BoxFuture;

/// Grace between asking a child to stop and insisting.
pub const KILL_GRACE_MS: u64 = 2_000;

/// Which pipe a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputStream {
    /// The child's standard output.
    Stdout,
    /// The child's standard error.
    Stderr,
}

/// Receives every byte a command writes, before the output budget is applied.
pub trait OutputTee: Send + Sync {
    /// One chunk from one pipe, in the order it arrived on that pipe.
    fn write(&self, stream: OutputStream, chunk: &[u8]);
}

/// One command to run.
#[derive(Clone)]
pub struct RunRequest {
    /// What to run. Already guarded — a runner does not re-decide policy.
    pub plan: ExecPlan,
    /// `0` means no limit. Already reconciled between the model and the
    /// operator.
    pub timeout_ms: u64,
    /// The turn's cancellation, threaded all the way from the transport.
    pub token: CancellationToken,
    /// Measures the run.
    pub clock: Arc<dyn Clock>,
    /// Where the *complete* output goes, when somebody wants it kept.
    ///
    /// Its presence also changes the overflow behaviour: without a tee, a
    /// command that exceeds its budget has its pipe closed, because reading
    /// bytes nobody will see is waste. With one, reading continues to the end —
    /// the budget then bounds only what the *model* is shown, while the
    /// transcript stays whole. That difference is what lets a 12,000-token scan
    /// come back as a summary and a path with nothing lost.
    pub tee: Option<Arc<dyn OutputTee>>,
}

impl std::fmt::Debug for RunRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunRequest")
            .field("plan", &self.plan)
            .field("timeout_ms", &self.timeout_ms)
            .field("tee", &self.tee.is_some())
            .finish_non_exhaustive()
    }
}

/// What a command did. Identical whether it ran on the host or elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunOutcome {
    /// Standard output, within the budget.
    pub stdout: String,
    /// Standard error, within the budget.
    pub stderr: String,
    /// Whether either stream exceeded its budget.
    pub truncated: bool,
    /// The exit code, or `None` when a signal ended the process.
    pub code: Option<i32>,
    /// The signal that ended it, as `SIGTERM`, or `None`.
    pub signal: Option<String>,
    /// Whether the runner stopped it for outliving its timeout.
    pub timed_out: bool,
    /// Where the full transcript was kept, in the caller's own path vocabulary.
    pub transcript_dir: Option<String>,
}

/// Runs a guarded command somewhere.
pub trait CommandRunner: Send + Sync {
    /// Runs one command to completion, or until cancelled.
    ///
    /// A cancelled run is an `aborted` error; a timed-out one is an outcome with
    /// `timed_out` set. Failing to start the program at all is a `tool` error.
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>>;
}

/// What a turn needs in order to be given the right container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolboxRequest {
    /// The agent running the turn.
    pub agent_id: String,
    /// The workspace the turn is in.
    pub workspace_id: String,
    /// The conversation.
    pub session_key: String,
    /// The agent's toolbox name; empty means the host.
    pub toolbox: String,
    /// How much network the agent asked for.
    pub network: AgentToolboxNetwork,
    /// GhostAI's view of the workspace root, where transcripts are written.
    pub workspace_root: String,
}

/// Supplies the runner a turn's `exec` uses.
///
/// Declared here rather than beside its implementation for the same reason
/// `JailResolver` is declared in `ghostai-security`: the agent has to name the
/// type and sits *below* the composition root that builds one. The
/// implementation — a pool of live containers — lives in the runtime.
///
/// `None` means the host, so `exec` keeps [`LocalRunner`] as its own default and
/// nothing here has to know what running on the host means.
pub trait RunnerResolver: Send + Sync {
    /// The runner for one turn, or `None` for the host.
    fn for_turn(&self, request: &ToolboxRequest) -> Option<Arc<dyn CommandRunner>>;
}

/// A child process on this machine, in the workspace root.
///
/// The behaviour `exec` has always had, and the default whenever a context does
/// not name a runner.
///
///  - **Never a shell.** The program is `plan.file` and the arguments are
///    `plan.args`, handed to the kernel as a vector; there is no string for a
///    metacharacter to be interpreted in.
///  - **The environment is exactly the plan's.** The child inherits nothing
///    from this process: `env_clear` first, then the allow-listed map.
///  - **Output is capped while the child writes**, so a build that logs two
///    megabytes returns its first megabyte rather than nothing, and finishes
///    rather than being killed for talking too much.
///  - **Completion is both pipes at EOF and the exit status**, not the exit
///    alone: a process can exit before its pipes have flushed, and the last
///    lines of a compiler's output are exactly the ones worth having.
///  - **Cancellation reaches the child.** One token runs from the transport
///    through the loop and the registry to a `SIGTERM` here, which is the end
///    of the chain and the only place cancellation becomes a real process going
///    away. A child that ignores `SIGTERM` — an editor, a REPL, anything holding
///    the terminal — gets `SIGKILL` after the grace period, or the turn would
///    wait forever.
#[derive(Debug, Clone)]
pub struct LocalRunner {
    kill_grace: Duration,
}

impl Default for LocalRunner {
    fn default() -> LocalRunner {
        LocalRunner {
            kill_grace: Duration::from_millis(KILL_GRACE_MS),
        }
    }
}

impl LocalRunner {
    /// A runner with the standard grace period.
    pub fn new() -> LocalRunner {
        LocalRunner::default()
    }

    /// A runner that escalates to `SIGKILL` after `grace`. For tests of the
    /// escalation, which should not wait two real seconds.
    pub fn with_kill_grace(grace: Duration) -> LocalRunner {
        LocalRunner { kill_grace: grace }
    }
}

/// Why a run was stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Cancelled,
    TimedOut,
}

/// Reads one pipe into its cap until EOF, or until the budget is spent and
/// nobody is keeping the rest.
///
/// Closing the pipe once the budget is spent is the point of the cap's boolean
/// return: a command that has already written more than the model can be shown
/// gets the same treatment `head` would give it, rather than being read to the
/// end so its output can be thrown away.
async fn drain(
    pipe: Option<impl AsyncRead + Unpin>,
    stream: OutputStream,
    max_bytes: u64,
    tee: Option<&Arc<dyn OutputTee>>,
) -> OutputCap {
    let mut cap = OutputCap::new(max_bytes);
    let Some(mut pipe) = pipe else {
        return cap;
    };
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let read = match pipe.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let chunk = &buffer[..read];
        if let Some(tee) = tee {
            tee.write(stream, chunk);
        }
        if !cap.push(chunk) && tee.is_none() {
            break;
        }
    }
    cap
}

/// The signal that ended a process, by name.
fn signal_name(status: std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .signal()
        .and_then(|number| Signal::try_from(number).ok())
        .map(|signal| signal.as_str().to_owned())
}

fn send(pid: Option<u32>, signal: Signal) {
    if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
        // Best effort: a child that has already exited is the common case, and
        // there is nothing to do about a signal that cannot be delivered.
        let _ = kill(Pid::from_raw(pid), signal);
    }
}

impl CommandRunner for LocalRunner {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        Box::pin(async move {
            let RunRequest {
                plan,
                timeout_ms,
                token,
                tee,
                ..
            } = request;

            let mut child = tokio::process::Command::new(&plan.file)
                .args(&plan.args)
                .current_dir(&plan.cwd)
                .env_clear()
                .envs(&plan.env)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|error| {
                    GhostError::new(
                        ErrorKind::Tool,
                        format!("Could not run {}: {error}", plan.file),
                    )
                    .with_detail("file", plan.file.as_str())
                    .with_source(error)
                })?;

            let pid = child.id();
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            let outputs = join(
                drain(
                    stdout,
                    OutputStream::Stdout,
                    plan.max_output_bytes,
                    tee.as_ref(),
                ),
                drain(
                    stderr,
                    OutputStream::Stderr,
                    plan.max_output_bytes,
                    tee.as_ref(),
                ),
            );
            let mut work = pin!(join(outputs, child.wait()));

            let stop = async {
                if timeout_ms == 0 {
                    token.cancelled().await;
                    return Stop::Cancelled;
                }
                tokio::select! {
                    () = token.cancelled() => Stop::Cancelled,
                    () = tokio::time::sleep(Duration::from_millis(timeout_ms)) => Stop::TimedOut,
                }
            };

            let (((out, err), status), stopped) = tokio::select! {
                finished = &mut work => (finished, None),
                reason = stop => {
                    send(pid, Signal::SIGTERM);
                    let finished = tokio::select! {
                        finished = &mut work => finished,
                        () = tokio::time::sleep(self.kill_grace) => {
                            send(pid, Signal::SIGKILL);
                            work.await
                        }
                    };
                    (finished, Some(reason))
                }
            };

            if stopped == Some(Stop::Cancelled) {
                return Err(GhostError::aborted("exec"));
            }
            let status = status.map_err(|error| {
                GhostError::new(
                    ErrorKind::Tool,
                    format!("Could not wait for {}: {error}", plan.file),
                )
                .with_detail("file", plan.file.as_str())
                .with_source(error)
            })?;
            let out = out.done();
            let err = err.done();
            Ok(RunOutcome {
                stdout: out.text,
                stderr: err.text,
                truncated: out.truncated || err.truncated,
                code: status.code(),
                signal: signal_name(status),
                timed_out: stopped == Some(Stop::TimedOut),
                transcript_dir: None,
            })
        })
    }
}
