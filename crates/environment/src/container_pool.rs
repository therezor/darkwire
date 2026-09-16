//! Warm containers and the [`CommandRunner`] that talks to them.
//!
//! **An environment is a place, and one place is one container.** An instance is
//! keyed by workspace, mount, definition and the **effective network**:
//!
//!  - **The workspace** decides what is *mounted*, so it cannot be shared
//!    across one.
//!  - **The network** is deliberately present: two agents reaching different
//!    parts of the network are not interchangeable, and an instance that served
//!    the wider of the two requests would quietly hand the narrower one a reach
//!    nobody granted it.
//!
//! **Neither the agent nor the session is in the key**, and that is the whole
//! point: a parent and every subagent it delegates to work in one container
//! rather than one each. Keying on the session cost four containers for one
//! fan-out, against a cap of four, on hardware where four containers is the
//! whole machine. Operation permissions are per agent and enforced elsewhere, so
//! sharing an instance does not share authority.
//!
//! What it costs is the container's own ephemeral filesystem: two commands may
//! write `/tmp` and `$HOME` at once. The workspace was already shared across
//! containers by bind mount, so that is the new exposure and it is bounded.
//!
//! Starting is lazy, on the first command that needs it, because an install
//! with six containerised agents should not run six containers to answer one
//! question. Reaping is on idle and on reconfigure.
//!
//! **Failure to start is a refusal, never a downgrade.** A container that
//! cannot be created must not fall back to running the command on the host:
//! that is the one failure mode where the operator believes there is a boundary
//! and there is not.

use std::fs;
use std::process;
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::{Clock, ErrorKind, Result, SystemClock, WireError};
use darkwire_protocol::environment::EnvironmentDefinition;
use darkwire_protocol::{EnvironmentNetwork, NetworkMode, SandboxInstanceSummary};
use darkwire_security::{InstalledEnvironment, PolicyStore, assert_environment_network};
use darkwire_tools::{
    BoxFuture, CommandRunner, ContainerCreateOptions, ContainerRunner, ContainerRunnerOptions,
    PlacementRequest, RunOutcome, RunRequest, WorkspaceMount, container_create_argv,
    container_is_gone,
};
use indexmap::IndexMap;
use nix::unistd::Pid;
use parking_lot::Mutex;

/// Beyond this many live containers the least-recently-used session is reaped.
pub const MAX_LIVE_CONTAINERS: usize = 4;

/// How long a container may sit unused before it is stopped.
pub const CONTAINER_IDLE_MS: i64 = 10 * 60_000;

/// Label naming the process that created a sandbox.
///
/// Without it, the orphan sweep cannot tell an orphan from a *peer's live
/// container* — `darkwire.session` is on every sandbox this install ever starts,
/// so a second DarkWire process, a `darkwire chat` beside a running `darkwire
/// serve`, or a restart that overlaps the old process by a second, would force
/// away containers a turn was executing in. The symptom is the daemon's "No such
/// container" landing in the model's tool result, which is the one this label
/// exists to prevent.
pub const OWNER_LABEL: &str = "darkwire.owner";

/// This process, as a label value.
///
/// Host *and* pid, because a pid alone is only unique within one kernel: a
/// remote or shared daemon serves several hosts, and pid 42 on two of them is
/// two different processes. Both halves are needed for the liveness check below
/// to be asking about the right process.
pub fn owner_tag() -> String {
    format!(
        "{}:{}",
        gethostname::gethostname().to_string_lossy(),
        process::id()
    )
}

/// Whether the process named by an owner tag is still running.
///
/// **Conservative by design: it answers `true` whenever it cannot tell.** The
/// two outcomes are not symmetric — sparing a container that is genuinely
/// orphaned leaks a workspace mount until the next sweep that *can* tell, while
/// reaping one that is live kills a command mid-flight and reports the daemon's
/// words to the model as its own failure. So an owner from another host, or one
/// that does not parse, is left alone.
///
/// Signal zero is the standard existence probe: it delivers nothing and only
/// reports whether the pid could be signalled. `EPERM` means it exists and
/// belongs to someone else, which is still "alive".
pub fn owner_process_looks_alive(owner: &str) -> bool {
    let Some(separator) = owner.rfind(':') else {
        return true;
    };
    if owner[..separator] != *gethostname::gethostname().to_string_lossy() {
        return true;
    }
    let Ok(pid) = owner[separator + 1..].parse::<i32>() else {
        return true;
    };
    if pid <= 0 {
        return true;
    }
    match nix::sys::signal::kill(Pid::from_raw(pid), None) {
        Ok(()) => true,
        Err(errno) => errno == nix::errno::Errno::EPERM,
    }
}

/// Spawns and stops containers. Injected so the pool is testable with no daemon.
pub trait ContainerEngine: Send + Sync {
    /// Provision enforced egress before any sandbox joins its namespace.
    fn gateway(
        &self,
        _name: &str,
        _container: &EnvironmentDefinition,
        network: &EnvironmentNetwork,
    ) -> Result<Option<String>> {
        if network.mode == NetworkMode::Allowlist {
            return Err(WireError::new(
                ErrorKind::Tool,
                "This engine cannot enforce restricted egress",
            ));
        }
        Ok(None)
    }
    /// Starts the container `argv` describes.
    fn start(&self, argv: &[String]) -> Result<()>;
    /// Stops one by name.
    fn stop(&self, name: &str) -> Result<()>;
    /// Fails when the daemon is unreachable. Called once per container start.
    fn probe(&self) -> Result<()>;
    /// Removes sandbox containers left behind by a previous process.
    ///
    /// Shutdown reaps what this process started, and reaps nothing at all when
    /// the process did not get to run it — a `SIGKILL`, an OOM, a crashed host.
    /// Without a sweep those containers accumulate silently, each holding a
    /// workspace mount and its share of memory, and the only sign is a machine
    /// that is slowly more loaded than it should be. Every sandbox carries a
    /// `darkwire.session` label so this can find them without guessing at names.
    fn reap_orphans(&self) -> Result<()>;
}

/// Builds the runner for a container the pool has just started.
///
/// A seam for the pool's *own* behaviour — marking a container busy, rebuilding
/// one that disappeared — none of which is about `docker exec` and all of which
/// otherwise needs a daemon to observe.
pub type RunnerFactory =
    Arc<dyn Fn(&str, &EnvironmentDefinition) -> Arc<dyn CommandRunner> + Send + Sync>;

/// Translates DarkWire's view of a path into the *daemon's*.
pub type HostPathFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Supplies container and run names. Injected for deterministic tests.
pub type IdFactory = Arc<dyn Fn() -> String + Send + Sync>;

/// What a pool is built from.
pub struct ContainerPoolOptions {
    /// Container CLI used consistently for both control and execution.
    pub bin: Option<String>,
    /// The approval ledger, re-read on every turn.
    pub policies: Arc<PolicyStore>,
    /// How containers are started and stopped.
    pub engine: Arc<dyn ContainerEngine>,
    /// Where command transcripts are written.
    ///
    /// Outside the workspace, because the host writes them while the container
    /// holds the workspace writable.
    pub runs_dir: std::path::PathBuf,
    /// How the *daemon* sees a path, given DarkWire's view of it.
    ///
    /// Identity for a host install. A containerised DarkWire has to translate,
    /// because a bind path is resolved by the daemon and not by this process —
    /// the failure otherwise is a silently empty mount rather than an error.
    /// Applied to every path that reaches a volume.
    pub host_path: Option<HostPathFn>,
    /// Stamps last use and decides what is idle.
    pub clock: Arc<dyn Clock>,
    /// How long a container may sit unused.
    pub idle_ms: i64,
    /// The live-container cap. Zero is no cap, the same way `idle_ms` of zero
    /// is no sweep — an operator who writes it means "do not bound this", and
    /// reading it as "bound this at nothing" would refuse every container.
    pub max_live: usize,
    /// Container and run names.
    pub new_id: Option<IdFactory>,
    /// Builds the runner for a started container.
    pub new_runner: Option<RunnerFactory>,
    /// What to label containers with, so a sweep can tell this process's from a
    /// peer's. Defaults to [`owner_tag`].
    pub owner: Option<String>,
}

impl ContainerPoolOptions {
    /// Options with the shipped bounds, the host clock and no injected seams.
    pub fn new(
        policies: Arc<PolicyStore>,
        engine: Arc<dyn ContainerEngine>,
        runs_dir: impl Into<std::path::PathBuf>,
    ) -> ContainerPoolOptions {
        ContainerPoolOptions {
            bin: None,
            policies,
            engine,
            runs_dir: runs_dir.into(),
            host_path: None,
            clock: Arc::new(SystemClock),
            idle_ms: CONTAINER_IDLE_MS,
            max_live: MAX_LIVE_CONTAINERS,
            new_id: None,
            new_runner: None,
            owner: None,
        }
    }
}

impl std::fmt::Debug for ContainerPoolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerPoolOptions")
            .field("runs_dir", &self.runs_dir)
            .field("idle_ms", &self.idle_ms)
            .field("max_live", &self.max_live)
            .finish_non_exhaustive()
    }
}

/// One live container.
struct Entry {
    gateway: Option<String>,
    agents: std::collections::BTreeSet<String>,
    name: String,
    runner: Arc<dyn CommandRunner>,
    /// What the definition hashed to when this container was started.
    digest: String,
    /// The turn that asked for this container, kept so it can be rebuilt.
    ///
    /// A container can go away underneath a warm session — the daemon
    /// restarting, a prune, an operator tidying up — and rebuilding it needs the
    /// mount, the network and the container name that created it. Without them
    /// the only recovery is to fail the command.
    request: PlacementRequest,
    last_used_ms: i64,
    /// How many commands are running in this container right now.
    ///
    /// `last_used_ms` is stamped once per *turn*, so a scan that runs for twenty
    /// minutes looks idle for nineteen of them — and a turn in another session
    /// that triggers a sweep would stop the container that scan is running in,
    /// which surfaces as the command failing for no stated reason. A container
    /// with work in it is never reaped, whatever its timestamp says.
    busy: u32,
}

/// The mutable half, behind one lock.
struct Live {
    epochs: IndexMap<String, u64>,
    /// Insertion order is the recency order.
    entries: IndexMap<String, Entry>,
    /// The newest policy seen per key, which [`ContainerPool::warm`] starts
    /// from.
    ///
    /// Held here rather than read from the live [`Entry`] because with a lazily
    /// started container there is no entry to read it from the first time — and
    /// after a container is dropped there is none to read it from again.
    specs: IndexMap<String, PlacementRequest>,
    counter: u64,
    swept: bool,
}

/// Live sandboxes, and the runners that reach them.
pub struct ContainerPool {
    starting: Mutex<()>,
    options: ContainerPoolOptions,
    owner: String,
    live: Mutex<Live>,
    /// A handle on itself, so a turn's runner can outlive any one container
    /// without the caller having to hold two objects.
    me: std::sync::Weak<ContainerPool>,
}

impl std::fmt::Debug for ContainerPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerPool")
            .field("live", &self.live.lock().entries.len())
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// The key a container is held under.
///
/// **An environment is a place, so everything asking for the same place gets the
/// same container.** Neither the agent nor the session is in here, which is what
/// makes a parent and the subagents it delegates to work side by side in one
/// container rather than one each. On a small board that is the difference
/// between four containers and one.
///
/// Serialised rather than joined with a separator, because every component is
/// operator- or client-supplied and a workspace path containing the separator
/// would otherwise collide with a different workspace. The network is part of
/// it: an instance that reaches more than this request asked for is not this
/// request's instance.
///
/// **The digest is deliberately not in here**, even though it is identity. An
/// edited definition has to *replace* the container its old bytes started, and
/// keying on it would leave the old one live under a key nothing asks for again
/// until the idle sweep finds it. `resolve_turn` compares `Entry::digest`
/// instead and drops the entry in place, which is the same decision without the
/// leak.
fn key_of(request: &PlacementRequest) -> Result<String> {
    serde_json::to_string(&(
        &request.workspace_id,
        &request.workspace_root,
        &request.environment,
        &request.network,
    ))
    .map_err(|error| WireError::new(ErrorKind::Internal, error.to_string()))
}

/// The approved definition a request resolves to, with its approval hash.
struct ContainerSpec {
    definition: EnvironmentDefinition,
    digest: String,
}

impl ContainerPool {
    /// Safe lifecycle metadata; contains no host paths or command arguments.
    pub fn status(&self) -> Vec<SandboxInstanceSummary> {
        self.live
            .lock()
            .entries
            .values()
            .map(|entry| SandboxInstanceSummary {
                id: entry.name.clone(),
                workspace: entry.request.workspace_id.clone(),
                environment: entry.request.environment.clone(),
                busy: u64::from(entry.busy),
                last_used_ms: entry.last_used_ms.max(0).cast_unsigned(),
                agents: entry.agents.iter().cloned().collect(),
            })
            .collect()
    }

    /// Stop an exact managed instance, whatever it is in the middle of.
    ///
    /// A busy instance is stopped rather than refused. A command already inside
    /// the container dies with the container; anything queued behind it comes
    /// back aborted through the epoch bump below, and the next command starts a
    /// fresh instance from the same definition. There is nothing an operator
    /// could lose here that a refusal would have saved, which is why there is no
    /// longer a flag to get past one.
    pub fn stop_instance(&self, name: &str) -> Result<()> {
        let _starting = self.starting.lock();
        let key = {
            let live = self.live.lock();
            let (key, _) = live
                .entries
                .iter()
                .find(|(_, entry)| entry.name == name)
                .ok_or_else(|| {
                    WireError::new(ErrorKind::InvalidInput, "Unknown managed container")
                })?;
            key.clone()
        };
        *self.live.lock().epochs.entry(key.clone()).or_default() += 1;
        self.drop_entry(&key);
        Ok(())
    }

    /// Restart an exact managed instance using its registered policy.
    pub fn restart_instance(&self, name: &str) -> Result<()> {
        let (key, spec) = {
            let live = self.live.lock();
            let (key, entry) = live
                .entries
                .iter()
                .find(|(_, entry)| entry.name == name)
                .ok_or_else(|| {
                    WireError::new(ErrorKind::InvalidInput, "Unknown managed container")
                })?;
            (key.clone(), entry.request.clone())
        };
        self.stop_instance(name)?;
        self.ensure(&key, &spec)
    }

    /// Explicitly warm an approved instance without executing a tool.
    pub fn warm(&self, request: &PlacementRequest) -> Result<()> {
        self.resolve_turn(request)?;
        let keys: Vec<String> = self
            .live
            .lock()
            .specs
            .iter()
            .filter(|(_, spec)| *spec == request)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.ensure(&key, request)?;
        }
        Ok(())
    }

    /// Periodic cleanup, plus a sweep for instances whose definition moved out
    /// from under them. An in-flight operation re-reads on its own ticker; this
    /// is what catches an idle warm container nothing is about to call.
    pub fn maintain(&self) {
        self.reap_idle();
        let valid: std::collections::BTreeSet<String> = self
            .options
            .policies
            .list_environments()
            .into_iter()
            .filter_map(|entry| self.options.policies.require_environment(&entry.name).ok())
            .map(|entry| entry.digest)
            .collect();
        let invalid: Vec<String> = self
            .live
            .lock()
            .entries
            .iter()
            .filter(|(_, entry)| !valid.contains(&entry.digest))
            .map(|(key, _)| key.clone())
            .collect();
        for key in invalid {
            self.drop_entry(&key);
        }
    }

    /// A pool over `options`.
    pub fn new(options: ContainerPoolOptions) -> Arc<ContainerPool> {
        let owner = options.owner.clone().unwrap_or_else(owner_tag);
        Arc::new_cyclic(|me| ContainerPool {
            starting: Mutex::new(()),
            options,
            owner,
            live: Mutex::new(Live {
                entries: IndexMap::new(),
                specs: IndexMap::new(),
                epochs: IndexMap::new(),
                counter: 0,
                swept: false,
            }),
            me: me.clone(),
        })
    }

    /// Live container names, for tests and diagnostics.
    pub fn live(&self) -> Vec<String> {
        self.live
            .lock()
            .entries
            .values()
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// Stops everything. Called on reconfigure and on shutdown.
    pub fn close(&self) {
        let doomed: Vec<String> = self.live.lock().entries.keys().cloned().collect();
        for key in doomed {
            self.drop_entry(&key);
        }
    }

    /// Sweeps orphans once, on the first turn that actually needs a container.
    ///
    /// **Not at construction**, and that is a fix rather than a preference. The
    /// pool is built whenever any agent names a container, which happens at boot —
    /// and listing containers against a socket whose daemon has gone away does
    /// not fail fast, it blocks. Measured at 62 seconds. Sweeping at
    /// construction therefore hung `darkwire serve` for a minute before it bound
    /// its port, on an install whose only sin was having a research agent
    /// configured while the daemon was closed.
    ///
    /// Failure is logged rather than raised: an orphan nobody could remove is
    /// untidy, and refusing the turn over it would turn untidy into unusable.
    fn sweep_once(&self) {
        {
            let mut live = self.live.lock();
            if live.swept {
                return;
            }
            live.swept = true;
        }
        if let Err(error) = self.options.engine.reap_orphans() {
            tracing::warn!(error = %error.message, "could not sweep orphaned sandboxes");
        }
    }

    fn next_id(&self) -> String {
        let counter = {
            let mut live = self.live.lock();
            live.counter += 1;
            live.counter
        };
        match &self.options.new_id {
            Some(new_id) => new_id(),
            None => format!("{}-{counter}", self.options.clock.now_ms()),
        }
    }

    /// How the daemon sees `path`.
    fn daemon_path(&self, path: &str) -> String {
        match &self.options.host_path {
            Some(translate) => translate(path),
            None => path.to_owned(),
        }
    }

    /// The definition a request resolves to, or `None` when it names no
    /// container and so runs on the host.
    ///
    /// The agent's tool permissions are not consulted: what an agent may call
    /// and where a call runs are two decisions, and the pool answers only the
    /// second.
    fn container_spec(&self, request: &PlacementRequest) -> Result<Option<ContainerSpec>> {
        if request.environment.is_empty() {
            return Ok(None);
        }
        let InstalledEnvironment { definition, digest } = self
            .options
            .policies
            .require_environment(&request.environment)?;
        Ok(Some(ContainerSpec { definition, digest }))
    }

    /// The key this request's instance lives under.
    fn key_for(request: &PlacementRequest, _spec: &ContainerSpec) -> Result<String> {
        key_of(request)
    }

    /// Starts this key's container if it has none.
    ///
    /// The other half of a policy-only [`ContainerPool::resolve_turn`]: every
    /// path that runs a command goes through here first, so "there is an entry"
    /// is still true by the time anything needs one — established at first use
    /// rather than at turn open.
    ///
    /// The approval is re-checked rather than reused from `resolve_turn`, and
    /// the rebuild path does the same. A turn can sit between its opening and
    /// its first tool call for a long time, and a container revoked in that
    /// window must not be started.
    ///
    /// This is also where the cap is enforced, and it is enforced *before* the
    /// start rather than swept up after one: at the cap, the least-recently-used
    /// idle instance makes room, and when every instance has a command in it the
    /// turn is told to come back. Evicting a busy container would kill work
    /// somebody is waiting on to serve somebody who has not started yet.
    fn ensure(&self, key: &str, spec: &PlacementRequest) -> Result<()> {
        let _starting = self.starting.lock();
        let approved = self
            .container_spec(spec)?
            .ok_or_else(|| WireError::new(ErrorKind::Config, "No container selected"))?;
        assert_environment_network(&spec.network, &spec.agent_id)?;
        if let Some(entry) = self.live.lock().entries.get_mut(key) {
            entry.agents.insert(spec.agent_id.clone());
            return Ok(());
        }
        self.reap_idle();
        let candidate = {
            let live = self.live.lock();
            if self.options.max_live == 0 || live.entries.len() < self.options.max_live {
                None
            } else {
                Some(
                    live.entries
                        .iter()
                        .find(|(_, entry)| entry.busy == 0)
                        .map(|(key, _)| key.clone())
                        .ok_or_else(|| {
                            WireError::new(
                                ErrorKind::Tool,
                                "All sandbox capacity is busy; retry when a command completes",
                            )
                        })?,
                )
            }
        };
        if let Some(candidate) = candidate {
            self.drop_entry(&candidate);
        }
        let entry = self.start(spec, &approved)?;
        self.live.lock().entries.insert(key.to_owned(), entry);
        Ok(())
    }

    /// The shipped runner for a started container: `docker exec` into it.
    ///
    /// Only reached when no [`RunnerFactory`] was injected, which in practice
    /// means everywhere but a test.
    fn docker_exec_runner(
        &self,
        name: &str,
        container: &EnvironmentDefinition,
    ) -> Arc<dyn CommandRunner> {
        let pool_ids = self.options.new_id.clone();
        let clock = Arc::clone(&self.options.clock);
        let counter = Arc::new(Mutex::new(0u64));
        Arc::new(ContainerRunner::new(ContainerRunnerOptions {
            container: container.clone(),
            container_name: name.to_owned(),
            runs_root: self.options.runs_dir.clone(),
            bin: self.options.bin.clone(),
            // Keyed on the container name this pool generated, never on the
            // client-chosen session key — that string has no business becoming
            // a path component.
            next_run_id: Arc::new(move || {
                if let Some(new_id) = &pool_ids {
                    return new_id();
                }
                let mut counter = counter.lock();
                *counter += 1;
                format!("{}-{counter}", clock.now_ms())
            }),
            inner: None,
        }))
    }

    /// Builds and registers one container.
    fn start(&self, request: &PlacementRequest, approved: &ContainerSpec) -> Result<Entry> {
        let container = &approved.definition;
        let name = format!("dw-sbx-{}", self.next_id());
        fs::create_dir_all(self.options.runs_dir.join(&name))
            .map_err(|e| WireError::new(ErrorKind::Tool, e.to_string()))?;

        // **Every** path handed to the daemon goes through the translation, not
        // just the workspace. A containerised DarkWire that translated only the
        // workspace would ask the daemon to mount its own copy of a path that
        // means something else on the host, and usually nothing. The failure is
        // a container that starts with the wrong directory bound into it, which
        // is worse than one that refuses.
        let mut create = ContainerCreateOptions::new(
            container.clone(),
            request.network.clone(),
            WorkspaceMount {
                host_path: self.daemon_path(&request.workspace_root),
                environment_path: container.workdir.clone(),
            },
            name.clone(),
        );
        // Podman copies a sysfs directory up into a tmpfs laid over it, which
        // needs a capability this container does not hold, so the masking is
        // asked for only where it works.
        create.mask_sysfs = self
            .options
            .bin
            .as_deref()
            .unwrap_or("docker")
            .ends_with("docker");
        create.runs_path = Some(self.daemon_path(&self.options.runs_dir.to_string_lossy()));
        create
            .labels
            .insert("darkwire.session".to_owned(), request.session_key.clone());
        create
            .labels
            .insert("darkwire.container".to_owned(), container.name.clone());
        create
            .labels
            .insert(OWNER_LABEL.to_owned(), self.owner.clone());
        // Validate mounts and hardening before allocating a gateway. Restricted
        // networking needs its namespace name, so use a non-started placeholder
        // for this validation pass.
        create.gateway_container = Some(format!("{name}-gateway"));
        container_create_argv(&create)?;
        create.gateway_container = None;

        // The *root*, not this container's subdirectory. A bind mount refuses a
        // source the daemon cannot see, and a desktop daemon's file sharing does
        // not see a directory created microseconds earlier — so the mounted path
        // has to be one that already existed. The per-container subdirectory is
        // created on the host side, inside a mount the container already has.
        if let Err(error) = fs::create_dir_all(&self.options.runs_dir) {
            return Err(WireError::new(
                ErrorKind::Tool,
                format!(
                    "The transcript directory {} could not be created, so the sandbox was not \
                     started.",
                    self.options.runs_dir.display()
                ),
            )
            .with_source(error));
        }

        // Probed here rather than at build: a container runtime that is not
        // running is a condition that changes while the server is up, and
        // distinguishing it from "the container failed to start" is the
        // difference between an operator starting the daemon and an operator
        // debugging a manifest.
        self.options.engine.probe().map_err(|error| {
            WireError::new(
                ErrorKind::Tool,
                format!(
                    "No container runtime is reachable, so agent \"{}\" could not run its \
                     command.\n  Start Docker (or Podman) and try again. Everything that does \
                     not need a\n  sandbox keeps working meanwhile.",
                    request.agent_id
                ),
            )
            .with_detail("agentId", request.agent_id.clone())
            .with_detail("environment", container.name.clone())
            .with_source(error)
        })?;
        self.sweep_once();

        create.gateway_container =
            self.options
                .engine
                .gateway(&name, container, &create.network)?;
        let argv = container_create_argv(&create).inspect_err(|_| {
            if let Some(gateway) = &create.gateway_container {
                let _ = self.options.engine.stop(gateway);
            }
        })?;

        // Refused, never downgraded to the host. See the module header.
        //
        // The daemon's own words are included rather than logged and swallowed:
        // a bare "could not be started" sends the reader to the logs for the one
        // fact that would have told them what to do — a missing image, a bad
        // flag, a mount source the daemon cannot see.
        self.options.engine.start(&argv).map_err(|error| {
            if let Some(gateway) = &create.gateway_container {
                let _ = self.options.engine.stop(gateway);
            }
            WireError::new(
                ErrorKind::Tool,
                format!(
                    "The sandbox for agent \"{}\" could not be started, so the command was not \
                     run.\n  {}",
                    request.agent_id, error.message
                ),
            )
            .with_detail("agentId", request.agent_id.clone())
            .with_detail("environment", container.name.clone())
            .with_source(error)
        })?;

        tracing::info!(instance = %name, container = %container.name, "container started");

        let runner = match &self.options.new_runner {
            Some(factory) => factory(&name, container),
            None => self.docker_exec_runner(&name, container),
        };

        Ok(Entry {
            gateway: create.gateway_container.clone(),
            agents: std::collections::BTreeSet::from([request.agent_id.clone()]),
            name,
            runner,
            digest: approved.digest.clone(),
            request: request.clone(),
            last_used_ms: self.options.clock.now_ms(),
            busy: 0,
        })
    }

    /// Runs one command in this key's container, marking it busy meanwhile.
    async fn run_on(&self, key: &str, request: RunRequest) -> Result<RunOutcome> {
        let runner = {
            let mut live = self.live.lock();
            let Some(entry) = live.entries.get_mut(key) else {
                // Every caller runs `ensure` immediately before this, and
                // `ensure` either puts an entry in place or fails — so this is a
                // bug rather than a state to recover from, and it says which
                // one.
                return Err(WireError::new(
                    ErrorKind::Internal,
                    "The sandbox for this turn is no longer in the pool.",
                )
                .with_detail("key", key));
            };
            entry.busy += 1;
            Arc::clone(&entry.runner)
        };

        let outcome = runner.run(request).await;

        let mut live = self.live.lock();
        if let Some(entry) = live.entries.get_mut(key) {
            entry.busy = entry.busy.saturating_sub(1);
            // Stamped on the way out as well as per turn: a command that ran for
            // twenty minutes leaves a container that was used twenty minutes
            // ago, not one that has been idle since the turn began.
            entry.last_used_ms = self.options.clock.now_ms();
        }
        outcome
    }

    /// How many times this key has been explicitly stopped or restarted.
    fn epoch_of(&self, key: &str) -> u64 {
        self.live
            .lock()
            .epochs
            .get(key)
            .copied()
            .unwrap_or_default()
    }

    /// Drops idle containers.
    fn reap_idle(&self) {
        if self.options.idle_ms <= 0 {
            return;
        }
        let cutoff = self.options.clock.now_ms() - self.options.idle_ms;
        let doomed: Vec<String> = {
            let live = self.live.lock();
            live.entries
                .iter()
                .filter(|(_, entry)| entry.busy == 0 && entry.last_used_ms < cutoff)
                .map(|(key, _)| key.clone())
                .collect()
        };
        for key in doomed {
            self.drop_entry(&key);
        }
    }

    /// Stops one container and forgets it.
    fn drop_entry(&self, key: &str) {
        let entry = {
            let mut live = self.live.lock();
            live.entries.shift_remove(key)
        };
        let Some(entry) = entry else {
            return;
        };
        // The transcripts go with the container. Nothing prunes them otherwise,
        // and a scanning agent writes a lot of them. A directory that will not
        // delete is untidy, never fatal.
        let _ = fs::remove_dir_all(self.options.runs_dir.join(&entry.name));
        if let Err(error) = self.options.engine.stop(&entry.name) {
            // A container that is already gone is the common case, and a failure
            // to stop one must not take down the turn that triggered the sweep.
            tracing::warn!(container = %entry.name, error = %error.message, "sandbox stop failed");
        }
        if let Some(gateway) = entry.gateway {
            let _ = self.options.engine.stop(&gateway);
        }
    }
}

/// The runner a turn holds, which outlives any one container.
///
/// Three jobs beyond delegating.
///
/// It **refuses after an explicit stop or restart**, by comparing the epoch it
/// was minted at against the instance's. An operator who stops an instance means
/// it; handing the next command a fresh container under the same key would make
/// the stop look like it did nothing.
///
/// And it **rebuilds a container that disappeared** — which arrives as an
/// ordinary non-zero exit with the daemon's words on stderr — rather than
/// passing that off as the command's own failure. The two are told apart by the
/// epoch: a stop bumps it and a daemon restart does not, so this retries only
/// what nobody asked to end. The retry is safe for the one reason that matters:
/// an exec that could not find its container never started the command, so
/// nothing has run twice.
struct Facade {
    /// Weak, because the pool owns the facade: a strong handle here would be a
    /// cycle, and a facade a turn is still holding after the runtime dropped its
    /// pool has nothing left to run in anyway.
    pool: std::sync::Weak<ContainerPool>,
    key: String,
    spec: PlacementRequest,
    epoch: u64,
}

impl CommandRunner for Facade {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        Box::pin(async move {
            // No lease. Commands on one instance run at once, because an
            // environment is a place several agents work in and queueing them
            // behind each other made a fan-out of subagents finish one at a
            // time. What that costs is the container's own ephemeral
            // filesystem: the workspace was already shared across containers by
            // bind mount, and a pid file is keyed per run.
            let Some(pool) = self.pool.upgrade() else {
                return Err(WireError::new(
                    ErrorKind::Tool,
                    "The sandbox pool this turn belongs to has been shut down.",
                ));
            };
            if pool.epoch_of(&self.key) != self.epoch {
                return Err(WireError::aborted("Sandbox was stopped or restarted"));
            }

            // Where the container actually comes from now. Anything this raises
            // — a dead daemon, a revoked container — fails the command, which the
            // tool registry renders as a failed tool card rather than letting it
            // unwind the turn.
            pool.ensure(&self.key, &self.spec)?;

            let outcome = pool.run_on(&self.key, request.clone()).await?;
            if !container_is_gone(&outcome) {
                return Ok(outcome);
            }

            let stale = pool
                .live
                .lock()
                .entries
                .get(&self.key)
                .map(|entry| (entry.name.clone(), entry.request.environment.clone()));
            let Some((name, container)) = stale else {
                return Ok(outcome);
            };
            tracing::warn!(
                instance = %name,
                container = %container,
                "sandbox disappeared; rebuilding it and retrying the command"
            );

            pool.drop_entry(&self.key);
            pool.ensure(&self.key, &self.spec)?;
            // Once. A second disappearance is something other than a stale
            // handle, and a loop that keeps rebuilding would hide it.
            pool.run_on(&self.key, request).await
        })
    }
}

impl ContainerPool {
    /// Check whether the configured container daemon is reachable without
    /// starting or changing an instance.
    pub fn probe_engine(&self) -> Result<()> {
        self.options.engine.probe()
    }

    /// The runner for a turn, decided without touching the daemon.
    ///
    /// Resolving is on the turn-open path, and starting a container there meant
    /// probing the daemon there too. A daemon that has gone away did not fail
    /// that turn, it blocked for five seconds and *then* failed it. So this
    /// decides *whether* a container is allowed and the first command starts
    /// one, which puts a daemon outage on a tool card inside a live turn.
    ///
    /// `Ok(None)` is a request that selects no container, which is the host and
    /// is not a refusal. An error is a container that cannot be honoured —
    /// revoked, edited since approval, asked for egress nothing could enforce —
    /// and the service reports it where the operator can act on it.
    pub fn resolve_turn(
        &self,
        request: &PlacementRequest,
    ) -> Result<Option<Arc<dyn CommandRunner>>> {
        let Some(approved) = self.container_spec(request)? else {
            return Ok(None);
        };
        self.reap_idle();

        // **Before the cache, not after.** Requiring the container is the only
        // thing that re-reads the definition and re-checks its hash against the
        // approval, and a warm entry that skipped it kept serving a container
        // the operator had revoked — or edited into something they considered
        // unsafe — for as long as the session stayed active. A revoke is another
        // process writing a file, so nothing notifies this pool; asking every
        // turn is what makes revocation mean something. It costs one read and
        // one hash.
        assert_environment_network(&request.network, &request.agent_id)?;
        let key = Self::key_for(request, &approved)?;

        let stale = {
            let mut live = self.live.lock();
            // A second turn on the same session may carry a different workspace
            // root or network, and it is the newest one that any command should
            // start from.
            live.specs.insert(key.clone(), request.clone());
            match live.entries.get_mut(&key) {
                None => false,
                Some(entry) if entry.digest == approved.digest => {
                    entry.agents.insert(request.agent_id.clone());
                    entry.last_used_ms = self.options.clock.now_ms();
                    // Re-inserted so iteration order stays least-recently-used
                    // first.
                    if let Some(entry) = live.entries.shift_remove(&key) {
                        live.entries.insert(key.clone(), entry);
                    }
                    false
                }
                // A live container started from a definition that has since
                // changed is stopped rather than reused: it was built with the
                // old definition's flags. The next command starts a replacement
                // from the definition as it is now.
                Some(_) => true,
            }
        };
        if stale {
            self.drop_entry(&key);
        }

        let epoch = self.epoch_of(&key);
        Ok(Some(Arc::new(Facade {
            pool: self.me.clone(),
            key,
            spec: request.clone(),
            epoch,
        })))
    }
}

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
/// Synchronous because every call here is a control-plane operation on the order
/// of tens of milliseconds, and the alternative is an async `for_turn` that every
/// caller above would have to await for the sake of one detached run.
pub struct DockerEngine {
    gateway_image: Option<String>,
    bin: String,
    owner: String,
    is_owner_alive: LivenessFn,
    control_timeout: Duration,
    start_timeout: Duration,
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
    })
}

impl DockerEngine {
    /// One control-plane call, bounded.
    ///
    /// A timeout arrives as a killing signal rather than as a non-zero status,
    /// so checking the status alone would read "killed after 5s" as a clean
    /// failure of the command itself.
    fn run(&self, argv: &[String], what: &str, timeout: Duration) -> Result<()> {
        let output = self.capture_raw(argv, timeout)?;
        if output.code == Some(0) {
            return Ok(());
        }
        if output.code.is_none() {
            return Err(WireError::new(
                ErrorKind::Tool,
                format!(
                    "{} {what} did not respond within {}ms — is the daemon running?",
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
    /// A deadline that fires does **not** then read the pipes to EOF. A CLI that
    /// forks — and `docker` does — leaves the grandchild holding the write ends,
    /// so draining them would wait out the very command the deadline exists to
    /// abandon: the bound would be no bound at all. The killed child is reported
    /// with no exit code and no output, which is exactly what `run` needs to
    /// tell a deadline from a refusal.
    fn capture_raw(&self, argv: &[String], timeout: Duration) -> Result<CliOutput> {
        let mut child = process::Command::new(&self.bin)
            .args(argv)
            .stdin(process::Stdio::null())
            .stdout(process::Stdio::piped())
            .stderr(process::Stdio::piped())
            .spawn()
            .map_err(|error| {
                WireError::new(
                    ErrorKind::Tool,
                    format!("Could not run {}: {error}", self.bin),
                )
                .with_detail("bin", self.bin.clone())
                .with_source(error)
            })?;

        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(CliOutput {
                        code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                    });
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    return Err(WireError::new(
                        ErrorKind::Tool,
                        format!("Could not wait for {}: {error}", self.bin),
                    )
                    .with_source(error));
                }
            }
        }
        let output = child.wait_with_output().map_err(|error| {
            WireError::new(
                ErrorKind::Tool,
                format!("Could not read {} output: {error}", self.bin),
            )
            .with_source(error)
        })?;
        Ok(CliOutput {
            code: Some(output.status.code().unwrap_or(-1)),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Stdout of one call, or empty when it failed.
    fn capture(&self, argv: &[String]) -> String {
        match self.capture_raw(argv, self.control_timeout) {
            Ok(output) if output.code == Some(0) => output.stdout,
            _ => String::new(),
        }
    }
}

impl ContainerEngine for DockerEngine {
    fn gateway(
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
        args.extend(network.hosts.clone());
        self.run(&args, "gateway start", self.start_timeout)?;
        let ready = self.run(&argv(&["exec", &gateway, "sh", "-c", "for n in 1 2 3 4 5 6 7 8 9 10; do test -f /tmp/ready && exit 0; sleep 0.2; done; exit 1"]), "gateway readiness", self.control_timeout);
        if let Err(error) = ready {
            let _ = self.stop(&gateway);
            return Err(error);
        }
        Ok(Some(gateway))
    }
    fn probe(&self) -> Result<()> {
        self.run(
            &argv(&["version", "--format", "{{.Server.Version}}"]),
            "version",
            self.control_timeout,
        )
    }

    fn reap_orphans(&self) -> Result<()> {
        // A label filter rather than a name prefix: a label is what the
        // container was *created* with, so it cannot drift from whatever this
        // version happens to name things. The owner comes back in the same call
        // because the daemon cannot filter on *not* matching a label, so the
        // decision has to be made here.
        let format = format!("{{{{.ID}}}} {{{{.Label \"{OWNER_LABEL}\"}}}}");
        let rows = self.capture(&argv(&[
            "ps",
            "--all",
            "--filter",
            "label=darkwire.session",
            "--format",
            &format,
        ]));

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
            // A peer's, and it is still running. An unlabelled container — from
            // a version before this label existed — is reaped, which is the
            // behaviour it was created under.
            if !container_owner.is_empty() && (self.is_owner_alive)(container_owner) {
                continue;
            }

            // A force removal, not a stop: these are already unowned, and a stop
            // on a container whose process is gone waits out the timeout for
            // nothing.
            self.capture(&argv(&["rm", "--force", id]));
        }
        Ok(())
    }

    fn start(&self, args: &[String]) -> Result<()> {
        match self.run(args, "run", self.start_timeout) {
            Ok(()) => Ok(()),
            Err(error) => {
                // A desktop daemon's file sharing does not see a directory the
                // instant it is created: the transcript directory is made
                // microseconds before this call, and the daemon can answer "bind
                // source path does not exist" for a path that demonstrably does.
                // One retry after a beat is the whole fix, and it is scoped to
                // that exact message so a genuinely absent path still fails fast.
                if !error.message.contains("bind source path does not exist") {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(250));
                self.run(args, "run", self.start_timeout)
            }
        }
    }

    fn stop(&self, name: &str) -> Result<()> {
        // Two seconds: a sandbox holds no state worth a graceful shutdown, and a
        // reap that blocks ten seconds per container is a reap nobody runs.
        self.run(
            &argv(&["stop", "--time", "2", name]),
            "stop",
            self.control_timeout,
        )
    }
}

/// Borrowed argv as owned, which is what the engine's calls take.
fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}
