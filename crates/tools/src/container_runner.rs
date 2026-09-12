//! A [`CommandRunner`] that runs the guarded command inside a container.
//!
//! The exec guard has already decided **whether** the command may run; this
//! decides **where**, which is the split `runner` describes. What it owns is
//! the translation that module warns about — [`ExecPlan`] is host-shaped, and
//! a container sees the workspace somewhere else — plus three things that are
//! not obvious until they break:
//!
//!  - **No shell string is ever built.** Tee-ing output and recording a pid
//!    want a shell, and interpolating a command into one would re-create
//!    exactly the injection surface the argv contract exists to remove.
//!    Instead the script is a **fixed literal** and the command arrives as
//!    positional parameters: `sh -c '<constant>' ghost-exec <dir> <file>
//!    <args…>` leaves `"$@"` holding the argv, unquoted and uninterpreted.
//!    Nothing the model wrote is ever parsed by a shell.
//!
//!  - **The host writes the transcript, the container only reads it.** The
//!    full stream is already arriving on this side, so `tee` inside the
//!    container would be a second copy and a race against the mount. The files
//!    land *outside* the workspace and are mounted back read-only — see
//!    [`RUNS_MOUNT_DIR`] for the escape that made that necessary. This is what
//!    lets the model be handed ~80 tokens and a path instead of a 12,000-token
//!    scan.
//!
//!  - **`PATH` does not cross the boundary.** `plan.env` is built from a *host*
//!    allow-list, and a host `PATH` inside a Kali container points at binaries
//!    that are not there. Only names the profile names are passed through, and
//!    their values come from the plan.
//!
//! Killing the `docker exec` client on this side leaves the process running on
//! the other, which would quietly break the "one cancellation reaches the
//! child" invariant. So the script records its own pid — `exec` replaces the
//! shell and keeps it — and cancellation sends a signal *inside* the container.
//! The known limit: a process that forks children of its own leaves them
//! behind, which `--init` reaps rather than orphans.
//!
//! The daemon is driven through its CLI, and `podman` is wire-compatible for
//! everything used here. The argv builders are a security-reviewed artefact —
//! every flag that can never appear is asserted by test — and a typed daemon
//! API would rewrite them.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::LazyLock;

use ghostai_core::{Clock, ErrorKind, GhostError, Result};
use ghostai_protocol::{SeccompProfile, Toolbox, ToolboxNetworkMode, ToolboxRuntime};
use ghostai_security::{EffectiveNetwork, ExecPlan};
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::runner::{CommandRunner, LocalRunner, OutputStream, OutputTee, RunOutcome, RunRequest};
use crate::tool::BoxFuture;

/// Where a container sees its own transcripts, read-only.
///
/// The files are written by the host, from a directory *outside* the workspace
/// — the install's runs directory. Under `<workspace>/.ghost/runs` they would be
/// a host process writing into a directory the agent can write too: planting
/// `ln -s ~/.ssh/authorized_keys …/stdout.log` makes the host overwrite an
/// arbitrary host file as the GhostAI user. Mounting them back read-only keeps
/// the recovery path — `grep` your own truncated output — with no way to plant
/// anything.
///
/// A *sibling* of [`TOOLBOX_MOUNT_DIR`], never nested inside it.
/// `/run/ghost/runs` would ask runc to create a mountpoint inside a mount that
/// is itself read-only, which fails outright: "make mountpoint … read-only file
/// system". The same trap [`TOOLBOX_MOUNT_DIR`] documents, one level along.
pub const RUNS_MOUNT_DIR: &str = "/run/ghost-runs";

/// Where the approved manifest is mounted, read-only.
///
/// Outside the workspace, and that is not cosmetic. Nesting it under the
/// workdir — `/workspace/.ghost/profile.json` — asks the runtime to create a
/// mountpoint *inside* a bind mount it is in the middle of establishing, which
/// `runc` refuses outright: "mountpoint is outside of rootfs". Mounting the
/// profile's own directory somewhere of its own has no such problem, and
/// immutability from the mount table is satisfied wherever the mount lands.
pub const TOOLBOX_MOUNT_DIR: &str = "/run/ghost";

/// Records the pid, then becomes the command.
///
/// A constant. `$1` is the pid file and `"$@"` is the argv after the shift, so
/// no part of it is ever built from a string the model produced. `exec` means
/// the pid written is the command's own, not a shell that would exit first.
const EXEC_SCRIPT: &str = r#"echo $$ > "$1"; shift; exec "$@""#;

/// Signals the recorded pid.
///
/// The digit check is not defensive padding. The pid file lives on a tmpfs the
/// agent can write, so `-1` in it would turn a timeout into `kill -TERM -1` —
/// every process in the namespace, including PID 1, which kills the container
/// and leaves the pool serving an entry for something that no longer exists.
/// Only a bare positive integer is ever signalled.
const KILL_SCRIPT: &str = r#"p=$(cat "$1" 2>/dev/null); case "$p" in "" | *[!0-9]* ) exit 0 ;; esac; kill -"$2" "$p" 2>/dev/null || true"#;

/// How a container sees the workspace, and where the daemon finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolboxMount {
    /// The workspace as **the daemon** resolves it, which is not always as
    /// GhostAI sees it: a containerised GhostAI asking for its own
    /// `/data/workspace` gets the host's.
    pub host_path: String,
    /// `toolbox.workdir`.
    pub container_path: String,
}

/// What `docker run` needs for a session's sandbox.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerCreateOptions {
    /// The approved manifest.
    pub toolbox: Toolbox,
    /// The network the agent is actually getting.
    pub network: EffectiveNetwork,
    /// The workspace mount.
    pub mount: ToolboxMount,
    /// The container's name.
    pub container_name: String,
    /// Set when an egress gateway owns the network namespace.
    pub gateway_container: Option<String>,
    /// Absolute host path of the approved manifest.
    ///
    /// Its *directory* is what gets mounted: binding a single file needs the
    /// mountpoint to exist in the image, and a directory mount does not.
    pub manifest_path: Option<String>,
    /// Host transcript *root*, long-lived. Mounted read-only. See
    /// [`container_run_dir`].
    pub runs_path: Option<String>,
    /// Labels for the daemon, in order.
    pub labels: IndexMap<String, String>,
}

impl ContainerCreateOptions {
    /// Options with no gateway, manifest, transcripts or labels.
    pub fn new(
        toolbox: Toolbox,
        network: EffectiveNetwork,
        mount: ToolboxMount,
        container_name: impl Into<String>,
    ) -> ContainerCreateOptions {
        ContainerCreateOptions {
            toolbox,
            network,
            mount,
            container_name: container_name.into(),
            gateway_container: None,
            manifest_path: None,
            runs_path: None,
            labels: IndexMap::new(),
        }
    }
}

/// `ALL` is dropped whatever the profile says, then named capabilities are
/// added back.
///
/// Not `toolbox.caps.drop` alone: a manifest that left it empty — through an
/// edit, a template someone trimmed, or a future schema default nobody thought
/// about — would inherit Docker's default capability set rather than none, and
/// the resulting container would look correctly configured in every other
/// respect. The floor belongs in code, where no data can lower it. The
/// profile's own `drop` list is still emitted, so an operator can be explicit
/// without that meaning anything different.
fn capability_flags(toolbox: &Toolbox) -> Vec<String> {
    let mut flags = vec!["--cap-drop=ALL".to_owned()];
    for capability in &toolbox.caps.drop {
        if !capability.eq_ignore_ascii_case("ALL") {
            flags.push(format!("--cap-drop={capability}"));
        }
    }
    for capability in &toolbox.caps.add {
        flags.push(format!("--cap-add={capability}"));
    }
    flags
}

/// How the container reaches the network.
///
/// `container:<gw>` makes it join the gateway's namespace, where the gateway's
/// nftables rules already apply and the sandbox — holding no `NET_ADMIN`,
/// which the toolbox policy refuses — cannot flush them.
fn network_flags(options: &ContainerCreateOptions) -> Result<Vec<String>> {
    if options.network.mode == ToolboxNetworkMode::None {
        return Ok(vec!["--network=none".to_owned()]);
    }
    if let Some(gateway) = &options.gateway_container {
        return Ok(vec![format!("--network=container:{gateway}")]);
    }
    if options.network.mode == ToolboxNetworkMode::Allowlist {
        // Refused rather than silently run wide open. An allow-list with no
        // gateway to enforce it is indistinguishable from no allow-list at all,
        // and this is the failure that would look like it worked.
        return Err(GhostError::new(
            ErrorKind::Internal,
            "A scoped-egress sandbox needs a gateway container; refusing to start it with open egress.",
        )
        .with_detail("container", options.container_name.as_str()));
    }
    Ok(vec!["--network=bridge".to_owned()])
}

/// A bind mount as `--mount`, not `--volume`.
///
/// `--volume src:dst[:opts]` is colon-delimited and `:` is a legal character in
/// a path: a workspace at `/Users/me/Notes:2024/ws` parses as source
/// `/Users/me/Notes`, target `2024/ws`, options `/workspace`, and every
/// sandboxed turn fails with an opaque "invalid mode" naming nothing. `--mount`
/// takes `key=value` pairs and has no such ambiguity.
fn bind_mount(source: &str, target: &str, read_only: bool) -> String {
    let mut parts = vec![
        "type=bind".to_owned(),
        format!("src={source}"),
        format!("dst={target}"),
    ];
    if read_only {
        parts.push("ro".to_owned());
    }
    parts.join(",")
}

fn runtime_flag(runtime: ToolboxRuntime) -> Option<String> {
    match runtime {
        ToolboxRuntime::Runc => None,
        ToolboxRuntime::Runsc => Some("--runtime=runsc".to_owned()),
        ToolboxRuntime::Kata => Some("--runtime=kata".to_owned()),
    }
}

/// `cpus` as docker spells it: an integral count without a fraction.
fn cpus_text(cpus: f64) -> String {
    if cpus.fract() == 0.0 {
        format!("{cpus:.0}")
    } else {
        cpus.to_string()
    }
}

fn parent_dir(path: &str) -> String {
    Path::new(path).parent().map_or_else(
        || ".".to_owned(),
        |parent| parent.to_string_lossy().into_owned(),
    )
}

/// The `docker run` argv for a session's sandbox.
///
/// Pure, so the whole flag set is testable without a daemon — including the
/// assertions that matter most, which are about what can *never* appear.
pub fn container_create_argv(options: &ContainerCreateOptions) -> Result<Vec<String>> {
    let toolbox = &options.toolbox;
    let mount = &options.mount;
    let mut argv: Vec<String> = ["run", "--detach", "--rm", "--name"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    argv.push(options.container_name.clone());

    for (key, value) in &options.labels {
        argv.push("--label".to_owned());
        argv.push(format!("{key}={value}"));
    }

    // Without it the entrypoint override below leaves the shell as PID 1 with
    // no signal handler, so signals never reach children and exited processes
    // are never reaped.
    argv.push("--init".to_owned());

    argv.extend(runtime_flag(toolbox.runtime));

    if toolbox.limits.memory_mb > 0 {
        argv.push(format!("--memory={}m", toolbox.limits.memory_mb));
    }
    if toolbox.limits.cpus > 0.0 {
        argv.push(format!("--cpus={}", cpus_text(toolbox.limits.cpus)));
    }
    if toolbox.limits.pids_max > 0 {
        argv.push(format!("--pids-limit={}", toolbox.limits.pids_max));
    }
    if toolbox.limits.shm_size_mb > 0 {
        argv.push(format!("--shm-size={}m", toolbox.limits.shm_size_mb));
    }

    argv.extend(capability_flags(toolbox));
    if toolbox.security.no_new_privileges {
        argv.push("--security-opt=no-new-privileges".to_owned());
    }
    if toolbox.security.seccomp == SeccompProfile::Unconfined {
        argv.push("--security-opt=seccomp=unconfined".to_owned());
    }
    if toolbox.security.read_only_root {
        argv.push("--read-only".to_owned());
    }
    for spec in &toolbox.security.tmpfs {
        argv.push(format!("--tmpfs={spec}"));
    }
    for spec in &toolbox.security.devices {
        argv.push(format!("--device={spec}"));
    }
    if !toolbox.user.is_empty() {
        argv.push(format!("--user={}", toolbox.user));
    }

    argv.extend(network_flags(options)?);

    argv.push("--mount".to_owned());
    argv.push(bind_mount(&mount.host_path, &mount.container_path, false));
    // Read-only, so the policy the container runs under is immutable from the
    // mount table rather than by anyone remembering to check it. The profile's
    // *directory*, and outside the workdir — see `TOOLBOX_MOUNT_DIR`.
    if let Some(manifest) = &options.manifest_path {
        argv.push("--mount".to_owned());
        argv.push(bind_mount(&parent_dir(manifest), TOOLBOX_MOUNT_DIR, true));
    }
    // Read-only, so the agent can read its own truncated output and cannot
    // plant a symlink where the host is about to write the next one.
    if let Some(runs) = &options.runs_path {
        argv.push("--mount".to_owned());
        argv.push(bind_mount(runs, RUNS_MOUNT_DIR, true));
    }
    argv.push("--workdir".to_owned());
    argv.push(mount.container_path.clone());

    // Idle forever as PID 1's child. `tail -f /dev/null` rather than `sleep
    // infinity`, which busybox does not always accept.
    argv.push("--entrypoint".to_owned());
    argv.push("/bin/sh".to_owned());
    argv.push(toolbox.image.clone());
    argv.push("-c".to_owned());
    argv.push("exec tail -f /dev/null".to_owned());
    Ok(argv)
}

/// What `docker exec` needs for one command.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerExecOptions<'a> {
    /// The guarded command.
    pub plan: &'a ExecPlan,
    /// The approved manifest.
    pub toolbox: &'a Toolbox,
    /// The container to run in.
    pub container_name: &'a str,
    /// Identifies this run's transcript directory and pid file.
    pub run_id: &'a str,
}

/// The container-side path of a run's transcript directory, read-only.
///
/// Namespaced by container name because the mount is the *long-lived*
/// transcript root rather than a per-container directory. Mounting a directory
/// created microseconds earlier is unreliable on Docker Desktop, whose file
/// sharing does not see it yet and answers "bind source path does not exist"
/// for a path that demonstrably does — measured, and not fixed by retrying.
/// The parent exists from the first container onward, so there is nothing to
/// race.
///
/// The cost, stated plainly: a container can read *other* containers'
/// transcripts. That is this install's own command output under one operator,
/// and the property that matters — the agent cannot **write** where the host
/// writes — is unchanged.
pub fn container_run_dir(container_name: &str, run_id: &str) -> String {
    format!("{RUNS_MOUNT_DIR}/{container_name}/{run_id}")
}

/// Where the pid goes: a tmpfs inside the container, not the transcript
/// directory.
///
/// The transcript mount is read-only, so the command could not write there —
/// and a pid file the *host* had to create would have to match the container
/// user's uid, which the profile is explicitly encouraged to set to something
/// else.
fn container_pid_file(run_id: &str) -> String {
    format!("/tmp/.ghost-{run_id}.pid")
}

/// The `docker exec` argv for one command.
///
/// The environment is rebuilt rather than forwarded: see the module docs on
/// `PATH`. A name the profile lists but the plan does not carry is simply
/// absent, which is the right outcome — an empty value would shadow the
/// image's own.
pub fn container_exec_argv(options: &ContainerExecOptions<'_>) -> Vec<String> {
    let mut argv = vec![
        "exec".to_owned(),
        "--workdir".to_owned(),
        options.toolbox.workdir.clone(),
    ];
    for name in &options.toolbox.env {
        if let Some(value) = options.plan.env.get(name) {
            argv.push("--env".to_owned());
            argv.push(format!("{name}={value}"));
        }
    }
    argv.push(options.container_name.to_owned());
    argv.push("/bin/sh".to_owned());
    argv.push("-c".to_owned());
    argv.push(EXEC_SCRIPT.to_owned());
    argv.push("ghost-exec".to_owned());
    argv.push(container_pid_file(options.run_id));
    argv.push(options.plan.file.clone());
    argv.extend(options.plan.args.iter().cloned());
    argv
}

/// Which signal to send a run's recorded pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KillSignal {
    /// Ask it to stop.
    Term,
    /// Insist.
    Kill,
}

impl KillSignal {
    /// The name `kill -<name>` takes.
    pub fn as_str(self) -> &'static str {
        match self {
            KillSignal::Term => "TERM",
            KillSignal::Kill => "KILL",
        }
    }
}

/// The `docker exec` argv that signals a run's recorded pid.
pub fn container_kill_argv(container_name: &str, run_id: &str, signal: KillSignal) -> Vec<String> {
    vec![
        "exec".to_owned(),
        container_name.to_owned(),
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        KILL_SCRIPT.to_owned(),
        "ghost-kill".to_owned(),
        container_pid_file(run_id),
        signal.as_str().to_owned(),
    ]
}

/// Where a run's transcript lives on the host, and where the model should look.
///
/// Opened on the host, deliberately: the directory must exist before the
/// container writes into the run, and creating it here rather than in the
/// script removes a race against the mount becoming visible.
///
/// A write failure is swallowed rather than raised. Losing a transcript is a
/// degraded result — the model still gets the command's output inline — while
/// a failing write mid-run would end the turn. A disk filling up must not end
/// the turn.
pub struct Transcript {
    host_dir: PathBuf,
    container_dir: String,
    stdout: Mutex<Option<File>>,
    stderr: Mutex<Option<File>>,
}

impl std::fmt::Debug for Transcript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transcript")
            .field("host_dir", &self.host_dir)
            .field("container_dir", &self.container_dir)
            .finish_non_exhaustive()
    }
}

impl Transcript {
    /// Opens a run's transcript under `runs_root/<container>/<run>`.
    pub fn open(runs_root: &Path, container_name: &str, run_id: &str) -> Result<Transcript> {
        let host_dir = runs_root.join(container_name).join(run_id);
        std::fs::create_dir_all(&host_dir).map_err(|error| {
            GhostError::new(
                ErrorKind::Storage,
                format!(
                    "Could not create the transcript directory {}",
                    host_dir.display()
                ),
            )
            .with_source(error)
        })?;
        // Files that cannot be opened are simply not written; see the type docs.
        let open = |name: &str| File::create(host_dir.join(name)).ok();
        Ok(Transcript {
            stdout: Mutex::new(open("stdout.log")),
            stderr: Mutex::new(open("stderr.log")),
            container_dir: container_run_dir(container_name, run_id),
            host_dir,
        })
    }

    /// Where the files are on the host.
    pub fn host_dir(&self) -> &Path {
        &self.host_dir
    }

    /// Where the container sees them, read-only.
    pub fn container_dir(&self) -> &str {
        &self.container_dir
    }

    /// Flushes and closes both files. After this they are safe to read.
    pub fn close(&self) {
        for file in [&self.stdout, &self.stderr] {
            if let Some(mut handle) = file.lock().take() {
                let _ = handle.flush();
            }
        }
    }
}

impl OutputTee for Transcript {
    fn write(&self, stream: OutputStream, chunk: &[u8]) {
        let file = match stream {
            OutputStream::Stdout => &self.stdout,
            OutputStream::Stderr => &self.stderr,
        };
        if let Some(handle) = file.lock().as_mut() {
            let _ = handle.write_all(chunk);
        }
    }
}

static GONE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Docker: removed, or stopped but still present.
        r"(?i)^Error response from daemon: (No such container|Container \S+ is not running)",
        r"(?i)^Error: No such container",
        // Podman phrases both cases differently again.
        r"(?i)^Error: no container with name or ID .* found",
        r"(?i)^Error: can only create exec sessions on running containers",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).unwrap_or_else(|_| unreachable!("the pattern is a literal")))
    .collect()
});

/// Whether a `docker exec` failed because the *container* is gone.
///
/// The distinction matters because both arrive the same way: a non-zero exit
/// and something on stderr. A container that was removed out from under a warm
/// session — Docker Desktop restarted, `docker system prune`, an operator
/// tidying up — makes every command in that session fail with the daemon's
/// words, and the model is told its `nmap` invocation failed when `nmap` never
/// ran. It then rewrites a command that was already correct.
///
/// Anchored at the start of stderr, not searched for anywhere in it: the daemon
/// writes its refusal *instead of* the command's output, so the message is the
/// whole of stderr. Matching mid-stream would misread a command that merely
/// printed one of these strings. The cost of a false positive is one wasted
/// container start, not a wrong answer.
pub fn container_is_gone(outcome: &RunOutcome) -> bool {
    match outcome.code {
        None | Some(0) => false,
        Some(_) => {
            let stderr = outcome.stderr.trim_start();
            GONE_PATTERNS.iter().any(|pattern| pattern.is_match(stderr))
        }
    }
}

/// Supplies a run id per command. Injected so tests are deterministic.
pub type RunIdSource = Arc<dyn Fn() -> String + Send + Sync>;

/// How a [`ContainerRunner`] is bound to its container.
#[derive(Clone)]
pub struct ContainerRunnerOptions {
    /// The approved manifest.
    pub toolbox: Toolbox,
    /// The container every command runs in.
    pub container_name: String,
    /// Host transcript root, shared by every container and long-lived.
    ///
    /// Outside the workspace — see [`RUNS_MOUNT_DIR`]. Each container's files
    /// sit under `<runs_root>/<container_name>/`, keyed on a name the pool
    /// generates rather than on a client-chosen session key that has no
    /// business becoming a path component.
    pub runs_root: PathBuf,
    /// The daemon CLI. Defaults to `docker`; `podman` is wire-compatible for
    /// everything used here.
    pub bin: Option<String>,
    /// Supplies a run id per command.
    pub next_run_id: RunIdSource,
    /// Where the CLI process itself runs. Defaults to [`LocalRunner`]; the seam
    /// tests assert argv through without a daemon.
    pub inner: Option<Arc<dyn CommandRunner>>,
}

impl std::fmt::Debug for ContainerRunnerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerRunnerOptions")
            .field("toolbox", &self.toolbox.name)
            .field("container_name", &self.container_name)
            .field("runs_root", &self.runs_root)
            .field("bin", &self.bin)
            .finish_non_exhaustive()
    }
}

/// A [`CommandRunner`] bound to one container.
///
/// The spawn itself is delegated, so output caps, EOF-and-exit completion, the
/// `SIGTERM`→`SIGKILL` escalation and cancellation all stay in one
/// implementation rather than being reimplemented slightly differently here.
pub struct ContainerRunner {
    toolbox: Toolbox,
    container_name: String,
    runs_root: PathBuf,
    bin: String,
    next_run_id: RunIdSource,
    inner: Arc<dyn CommandRunner>,
}

impl std::fmt::Debug for ContainerRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerRunner")
            .field("container_name", &self.container_name)
            .field("bin", &self.bin)
            .finish_non_exhaustive()
    }
}

/// The `PATH` the daemon CLI is found on: this process's, and nothing else of
/// its environment. The client's cwd and env are this process's business, not
/// the sandbox's.
fn client_env() -> IndexMap<String, String> {
    let mut env = IndexMap::new();
    env.insert("PATH".to_owned(), std::env::var("PATH").unwrap_or_default());
    env
}

impl ContainerRunner {
    /// Binds a runner to one container.
    pub fn new(options: ContainerRunnerOptions) -> ContainerRunner {
        ContainerRunner {
            toolbox: options.toolbox,
            container_name: options.container_name,
            runs_root: options.runs_root,
            bin: options.bin.unwrap_or_else(|| "docker".to_owned()),
            next_run_id: options.next_run_id,
            inner: options
                .inner
                .unwrap_or_else(|| Arc::new(LocalRunner::default())),
        }
    }

    /// A plan for the daemon client itself. `max_output_bytes` carries through
    /// so the model still sees a bounded result even though the transcript on
    /// disk is complete.
    fn client_plan(&self, plan: &ExecPlan, args: Vec<String>) -> ExecPlan {
        ExecPlan {
            file: self.bin.clone(),
            args,
            cwd: self.runs_root.clone(),
            env: client_env(),
            timeout_ms: plan.timeout_ms,
            max_output_bytes: plan.max_output_bytes,
            paths: plan.paths.clone(),
        }
    }

    /// Signals the recorded pid inside the container, best effort.
    ///
    /// Detached: a container that has already exited is the common case, and
    /// failing the turn over a kill that had nothing to kill would be worse
    /// than the leak this is defending against.
    fn signal_inside(
        &self,
        plan: &ExecPlan,
        run_id: &str,
        signal: KillSignal,
        clock: &Arc<dyn Clock>,
    ) {
        let mut kill_plan = self.client_plan(
            plan,
            container_kill_argv(&self.container_name, run_id, signal),
        );
        kill_plan.timeout_ms = 5_000;
        let request = RunRequest {
            plan: kill_plan,
            timeout_ms: 5_000,
            token: CancellationToken::new(),
            clock: Arc::clone(clock),
            tee: None,
        };
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let _ = inner.run(request).await;
        });
    }
}

impl CommandRunner for ContainerRunner {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        Box::pin(async move {
            let run_id = (self.next_run_id)();
            let transcript = Arc::new(Transcript::open(
                &self.runs_root,
                &self.container_name,
                &run_id,
            )?);
            let exec_argv = container_exec_argv(&ContainerExecOptions {
                plan: &request.plan,
                toolbox: &self.toolbox,
                container_name: &self.container_name,
                run_id: &run_id,
            });
            let client = RunRequest {
                plan: self.client_plan(&request.plan, exec_argv),
                timeout_ms: request.timeout_ms,
                token: request.token.clone(),
                clock: Arc::clone(&request.clock),
                tee: Some(Arc::clone(&transcript) as Arc<dyn OutputTee>),
            };

            let outcome = self.inner.run(client).await;
            // Closed before the caller hands the model this path: a half-written
            // file is worse than none.
            transcript.close();
            match outcome {
                Ok(outcome) => {
                    // The client returning is not the process ending when the
                    // client was killed rather than the command finishing.
                    if outcome.timed_out {
                        self.signal_inside(
                            &request.plan,
                            &run_id,
                            KillSignal::Kill,
                            &request.clock,
                        );
                    }
                    Ok(RunOutcome {
                        transcript_dir: Some(transcript.container_dir().to_owned()),
                        ..outcome
                    })
                }
                Err(error) => {
                    self.signal_inside(&request.plan, &run_id, KillSignal::Term, &request.clock);
                    Err(error)
                }
            }
        })
    }
}
