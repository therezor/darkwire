//! One warm container per session, and the [`CommandRunner`] that talks to it.
//!
//! The key is `(agent_id, workspace_id, session_key)`, and each third of that is
//! load-bearing:
//!
//!  - **The agent** decides the *policy* — which toolbox, and how much of its
//!    network ceiling to use. Two agents on one conversation are two sandboxes.
//!  - **The workspace** decides what is *mounted*, so it cannot be shared across
//!    one; this is the same axis [`crate::JailCache`] is keyed on.
//!  - **The session** decides the *instance*. Keying on the agent alone would
//!    put two conversations in one container, which for a security agent means
//!    one engagement's loot sitting in another's `/tmp`. Keying per *call* would
//!    be stricter still and pay a container start on every command.
//!
//! Starting is lazy, on the first command that needs it, because an install with
//! six sandboxed agents should not run six containers to answer one question.
//! Reaping is on idle, on session end, and on reconfigure — the last of those
//! because a toolbox can change underneath a running pool, and a container
//! started under the old manifest must not outlive it.
//!
//! **Failure to start is a refusal, never a downgrade.** A sandbox that cannot
//! be created must not fall back to running the command on the host: that is the
//! one failure mode where the operator believes there is a boundary and there is
//! not.

use std::fs;
use std::process;
use std::sync::Arc;
use std::time::Duration;

use ghostai_core::{Clock, ErrorKind, GhostError, Result, SystemClock};
use ghostai_protocol::Toolbox;
use ghostai_security::{
    ApprovedToolbox, ToolboxStore, assert_network_within_ceiling, effective_network,
};
use ghostai_tools::{
    BoxFuture, CommandRunner, ContainerCreateOptions, ContainerRunner, ContainerRunnerOptions,
    RunOutcome, RunRequest, RunnerResolver, ToolboxMount, ToolboxRequest, container_create_argv,
    container_is_gone,
};
use indexmap::IndexMap;
use nix::unistd::Pid;
use parking_lot::Mutex;

/// Beyond this many live containers the least-recently-used session is reaped.
pub const MAX_LIVE_TOOLBOXES: usize = 4;

/// How long a container may sit unused before it is stopped.
pub const TOOLBOX_IDLE_MS: i64 = 10 * 60_000;

/// Label naming the process that created a sandbox.
///
/// Without it, the orphan sweep cannot tell an orphan from a *peer's live
/// container* — `ghostai.session` is on every sandbox this install ever starts,
/// so a second GhostAI process, a `ghostai chat` beside a running `ghostai
/// serve`, or a restart that overlaps the old process by a second, would force
/// away containers a turn was executing in. The symptom is the daemon's "No such
/// container" landing in the model's tool result, which is the one this label
/// exists to prevent.
pub const OWNER_LABEL: &str = "ghostai.owner";

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
    /// `ghostai.session` label so this can find them without guessing at names.
    fn reap_orphans(&self) -> Result<()>;
}

/// Builds the runner for a container the pool has just started.
///
/// A seam for the pool's *own* behaviour — marking a container busy, rebuilding
/// one that disappeared — none of which is about `docker exec` and all of which
/// otherwise needs a daemon to observe.
pub type RunnerFactory = Arc<dyn Fn(&str, &Toolbox) -> Arc<dyn CommandRunner> + Send + Sync>;

/// Translates GhostAI's view of a path into the *daemon's*.
pub type HostPathFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Supplies container and run names. Injected for deterministic tests.
pub type IdFactory = Arc<dyn Fn() -> String + Send + Sync>;

/// What a pool is built from.
pub struct ToolboxPoolOptions {
    /// The approval ledger, re-read on every turn.
    pub toolboxes: Arc<ToolboxStore>,
    /// How containers are started and stopped.
    pub engine: Arc<dyn ContainerEngine>,
    /// Where command transcripts are written.
    ///
    /// Outside the workspace, because the host writes them while the container
    /// holds the workspace writable.
    pub runs_dir: std::path::PathBuf,
    /// How the *daemon* sees a path, given GhostAI's view of it.
    ///
    /// Identity for a host install. A containerised GhostAI has to translate,
    /// because a bind path is resolved by the daemon and not by this process —
    /// the failure otherwise is a silently empty mount rather than an error.
    /// Applied to every path that reaches a volume, which is the workspace *and*
    /// the toolbox manifest.
    pub host_path: Option<HostPathFn>,
    /// Stamps last use and decides what is idle.
    pub clock: Arc<dyn Clock>,
    /// How long a container may sit unused.
    pub idle_ms: i64,
    /// The live-container cap.
    pub max_live: usize,
    /// Container and run names.
    pub new_id: Option<IdFactory>,
    /// Builds the runner for a started container.
    pub new_runner: Option<RunnerFactory>,
    /// What to label containers with, so a sweep can tell this process's from a
    /// peer's. Defaults to [`owner_tag`].
    pub owner: Option<String>,
}

impl ToolboxPoolOptions {
    /// Options with the shipped bounds, the host clock and no injected seams.
    pub fn new(
        toolboxes: Arc<ToolboxStore>,
        engine: Arc<dyn ContainerEngine>,
        runs_dir: impl Into<std::path::PathBuf>,
    ) -> ToolboxPoolOptions {
        ToolboxPoolOptions {
            toolboxes,
            engine,
            runs_dir: runs_dir.into(),
            host_path: None,
            clock: Arc::new(SystemClock),
            idle_ms: TOOLBOX_IDLE_MS,
            max_live: MAX_LIVE_TOOLBOXES,
            new_id: None,
            new_runner: None,
            owner: None,
        }
    }
}

impl std::fmt::Debug for ToolboxPoolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolboxPoolOptions")
            .field("runs_dir", &self.runs_dir)
            .field("idle_ms", &self.idle_ms)
            .field("max_live", &self.max_live)
            .finish_non_exhaustive()
    }
}

/// One live container.
struct Entry {
    name: String,
    runner: Arc<dyn CommandRunner>,
    /// Kept beside the composite key rather than parsed back out of it.
    ///
    /// Recovering the session from the key by suffix match is wrong twice: a
    /// session key containing a space can match another session's key, and a key
    /// that happens to end with another's text is reaped with it. Storing the
    /// value removes the parsing entirely.
    session_key: String,
    /// What the manifest hashed to when this container was started.
    manifest_sha256: String,
    /// The turn that asked for this container, kept so it can be rebuilt.
    ///
    /// A container can go away underneath a warm session — the daemon
    /// restarting, a prune, an operator tidying up — and rebuilding it needs the
    /// mount, the network and the toolbox name that created it. Without them the
    /// only recovery is to fail the command.
    request: ToolboxRequest,
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
    /// Insertion order is the recency order.
    entries: IndexMap<String, Entry>,
    /// The current policy per key, which a turn's runner reads on every command.
    ///
    /// Held here rather than read from the live [`Entry`] because with a lazily
    /// started container there is no entry to read it from the first time — and
    /// after a container is dropped there is none to read it from again. A later
    /// turn on the same session overwrites it, which keeps the runner a turn is
    /// holding pointed at current policy.
    specs: IndexMap<String, ToolboxRequest>,
    /// The runner handed to a turn, per key.
    ///
    /// Cached so asking twice in one session is the same object, which is what
    /// tells a reader nothing restarted.
    facades: IndexMap<String, Arc<dyn CommandRunner>>,
    counter: u64,
    swept: bool,
}

/// Live sandboxes, and the runners that reach them.
pub struct ToolboxPool {
    options: ToolboxPoolOptions,
    owner: String,
    live: Mutex<Live>,
    /// A handle on itself, so a turn's runner can outlive any one container
    /// without the caller having to hold two objects.
    me: std::sync::Weak<ToolboxPool>,
}

impl std::fmt::Debug for ToolboxPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolboxPool")
            .field("live", &self.live.lock().entries.len())
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// The composite key a container is held under.
fn key_of(request: &ToolboxRequest) -> String {
    format!(
        "{} {} {}",
        request.agent_id, request.workspace_id, request.session_key
    )
}

impl ToolboxPool {
    /// A pool over `options`.
    pub fn new(options: ToolboxPoolOptions) -> Arc<ToolboxPool> {
        let owner = options.owner.clone().unwrap_or_else(owner_tag);
        Arc::new_cyclic(|me| ToolboxPool {
            options,
            owner,
            live: Mutex::new(Live {
                entries: IndexMap::new(),
                specs: IndexMap::new(),
                facades: IndexMap::new(),
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

    /// Stops every container this session owns. Called when a session ends.
    pub fn release_session(&self, session_key: &str) {
        let doomed: Vec<String> = self
            .live
            .lock()
            .entries
            .iter()
            .filter(|(_, entry)| entry.session_key == session_key)
            .map(|(key, _)| key.clone())
            .collect();
        for key in doomed {
            self.drop_entry(&key);
        }
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
    /// pool is built whenever any agent names a toolbox, which happens at boot —
    /// and listing containers against a socket whose daemon has gone away does
    /// not fail fast, it blocks. Measured at 62 seconds. Sweeping at
    /// construction therefore hung `ghostai serve` for a minute before it bound
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

    /// Starts this key's container if it has none.
    ///
    /// The other half of a policy-only [`RunnerResolver::for_turn`]: every path
    /// that runs a command goes through here first, so "there is an entry" is
    /// still true by the time anything needs one — established at first use
    /// rather than at turn open.
    ///
    /// The approval is re-checked rather than reused from `for_turn`, and the
    /// rebuild path does the same. A turn can sit between its opening and its
    /// first tool call for a long time, and a toolbox revoked in that window
    /// must not get a container.
    fn ensure(&self, key: &str, spec: &ToolboxRequest) -> Result<()> {
        if self.live.lock().entries.contains_key(key) {
            return Ok(());
        }
        let approved = self.options.toolboxes.require(&spec.toolbox)?;
        assert_network_within_ceiling(&approved.toolbox, &spec.network, &spec.agent_id)?;
        let entry = self.start(spec, &approved)?;
        self.live.lock().entries.insert(key.to_owned(), entry);
        self.evict_beyond_cap(key);
        Ok(())
    }

    /// Builds and registers one container.
    fn start(&self, request: &ToolboxRequest, approved: &ApprovedToolbox) -> Result<Entry> {
        let toolbox = &approved.toolbox;
        let name = format!("ghost-sbx-{}", self.next_id());

        // **Every** path handed to the daemon goes through the translation, not
        // just the workspace. The manifest lives under `GHOSTAI_HOME`, so a
        // containerised GhostAI that translated only the workspace would ask the
        // daemon to mount its own copy of a path that means something else on
        // the host, and usually nothing. The failure is a container that starts
        // and carries the wrong policy file, which is worse than one that
        // refuses.
        let mut create = ContainerCreateOptions::new(
            toolbox.clone(),
            effective_network(toolbox, &request.network),
            ToolboxMount {
                host_path: self.daemon_path(&request.workspace_root),
                container_path: toolbox.workdir.clone(),
            },
            name.clone(),
        );
        create.manifest_path = Some(self.daemon_path(&approved.manifest_path.to_string_lossy()));
        create.runs_path = Some(self.daemon_path(&self.options.runs_dir.to_string_lossy()));
        create
            .labels
            .insert("ghostai.session".to_owned(), request.session_key.clone());
        create
            .labels
            .insert("ghostai.toolbox".to_owned(), toolbox.name.clone());
        create
            .labels
            .insert(OWNER_LABEL.to_owned(), self.owner.clone());
        let argv = container_create_argv(&create)?;

        // The *root*, not this container's subdirectory. A bind mount refuses a
        // source the daemon cannot see, and a desktop daemon's file sharing does
        // not see a directory created microseconds earlier — so the mounted path
        // has to be one that already existed. The per-container subdirectory is
        // created on the host side, inside a mount the container already has.
        if let Err(error) = fs::create_dir_all(&self.options.runs_dir) {
            return Err(GhostError::new(
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
            GhostError::new(
                ErrorKind::Tool,
                format!(
                    "No container runtime is reachable, so agent \"{}\" could not run its \
                     command.\n  Start Docker (or Podman) and try again. Everything that does \
                     not need a\n  sandbox keeps working meanwhile.",
                    request.agent_id
                ),
            )
            .with_detail("agentId", request.agent_id.clone())
            .with_detail("toolbox", toolbox.name.clone())
            .with_source(error)
        })?;
        self.sweep_once();

        // Refused, never downgraded to the host. See the module header.
        //
        // The daemon's own words are included rather than logged and swallowed:
        // a bare "could not be started" sends the reader to the logs for the one
        // fact that would have told them what to do — a missing image, a bad
        // flag, a mount source the daemon cannot see.
        self.options.engine.start(&argv).map_err(|error| {
            GhostError::new(
                ErrorKind::Tool,
                format!(
                    "The sandbox for agent \"{}\" could not be started, so the command was not \
                     run.\n  {}",
                    request.agent_id, error.message
                ),
            )
            .with_detail("agentId", request.agent_id.clone())
            .with_detail("toolbox", toolbox.name.clone())
            .with_source(error)
        })?;

        tracing::info!(container = %name, toolbox = %toolbox.name, "sandbox started");

        let runner = if let Some(factory) = &self.options.new_runner {
            factory(&name, toolbox)
        } else {
            {
                let runs_root = self.options.runs_dir.clone();
                let pool_ids = self.options.new_id.clone();
                let clock = Arc::clone(&self.options.clock);
                let counter = Arc::new(Mutex::new(0u64));
                let mut runner = ContainerRunnerOptions {
                    toolbox: toolbox.clone(),
                    container_name: name.clone(),
                    runs_root,
                    bin: None,
                    // Keyed on the container name this pool generated, never on
                    // the client-chosen session key — that string has no
                    // business becoming a path component.
                    next_run_id: Arc::new(move || {
                        if let Some(new_id) = &pool_ids {
                            return new_id();
                        }
                        let mut counter = counter.lock();
                        *counter += 1;
                        format!("{}-{counter}", clock.now_ms())
                    }),
                    inner: None,
                };
                runner.bin = None;
                Arc::new(ContainerRunner::new(runner)) as Arc<dyn CommandRunner>
            }
        };

        Ok(Entry {
            name,
            runner,
            session_key: request.session_key.clone(),
            manifest_sha256: approved.manifest_sha256.clone(),
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
                return Err(GhostError::new(
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

    /// The policy a turn's runner should start a container from, right now.
    fn spec_for(&self, key: &str) -> Option<ToolboxRequest> {
        self.live.lock().specs.get(key).cloned()
    }

    /// Drops idle containers.
    fn reap_idle(&self) {
        if self.options.idle_ms <= 0 {
            return;
        }
        let cutoff = self.options.clock.now_ms() - self.options.idle_ms;
        let doomed: Vec<String> = self
            .live
            .lock()
            .entries
            .iter()
            .filter(|(_, entry)| entry.busy == 0 && entry.last_used_ms < cutoff)
            .map(|(key, _)| key.clone())
            .collect();
        for key in doomed {
            self.drop_entry(&key);
        }
    }

    /// Drops least-recently-used entries until the cap is met.
    ///
    /// `keep` is the entry the caller is about to hand back, and excluding it is
    /// not a nicety: with a cap of zero — or one, on a pool that just evicted
    /// down to it — the newest entry is also the only entry, so an unguarded
    /// loop would stop the container it is in the middle of returning a runner
    /// for. Every command would then fail against a container that no longer
    /// exists, and the pool would report having started one.
    ///
    /// A busy container is skipped for the same reason the idle sweep skips it,
    /// with one consequence worth naming: enough concurrent long commands leave
    /// the pool *over* its cap rather than killing work to get under it. The cap
    /// is there to stop containers accumulating unused, and a container with a
    /// command in it is not that.
    fn evict_beyond_cap(&self, keep: &str) {
        let doomed: Vec<String> = {
            let live = self.live.lock();
            let mut over = live.entries.len();
            let mut doomed = Vec::new();
            for (key, entry) in &live.entries {
                if over <= self.options.max_live {
                    break;
                }
                if key != keep && entry.busy == 0 {
                    doomed.push(key.clone());
                    over -= 1;
                }
            }
            doomed
        };
        for key in doomed {
            self.drop_entry(&key);
        }
    }

    /// Stops one container and forgets it.
    fn drop_entry(&self, key: &str) {
        let entry = {
            let mut live = self.live.lock();
            // The facade outlives one container but not the entry: a turn still
            // holding it rebuilds through it, while a later resolve builds a
            // fresh one rather than this map growing by one closure per session
            // forever.
            live.facades.shift_remove(key);
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
    }
}

/// The runner a turn holds, which outlives any one container.
///
/// Two jobs beyond delegating. It marks the container busy for the duration of
/// each command, so an idle sweep cannot stop something mid-scan. And it
/// recognises the daemon reporting that the container is gone — which arrives as
/// an ordinary non-zero exit with the daemon's words on stderr — and rebuilds it
/// rather than passing that off as the command's own failure. The retry is safe
/// for the one reason that matters: an exec that could not find its container
/// never started the command, so nothing has run twice.
struct Facade {
    /// Weak, because the pool owns the facade: a strong handle here would be a
    /// cycle, and a facade a turn is still holding after the runtime dropped its
    /// pool has nothing left to run in anyway.
    pool: std::sync::Weak<ToolboxPool>,
    key: String,
}

impl CommandRunner for Facade {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        Box::pin(async move {
            let Some(pool) = self.pool.upgrade() else {
                return Err(GhostError::new(
                    ErrorKind::Tool,
                    "The sandbox pool this turn belongs to has been shut down.",
                ));
            };
            let Some(spec) = pool.spec_for(&self.key) else {
                return Err(GhostError::new(
                    ErrorKind::Internal,
                    "This turn's sandbox policy is no longer in the pool.",
                )
                .with_detail("key", self.key.clone()));
            };

            // Where the container actually comes from now. Anything this raises
            // — a dead daemon, a revoked toolbox — fails the command, which the
            // tool registry renders as a failed tool card rather than letting it
            // unwind the turn.
            pool.ensure(&self.key, &spec)?;

            let outcome = pool.run_on(&self.key, request.clone()).await?;
            if !container_is_gone(&outcome) {
                return Ok(outcome);
            }

            let stale = pool
                .live
                .lock()
                .entries
                .get(&self.key)
                .map(|entry| (entry.name.clone(), entry.request.toolbox.clone()));
            let Some((name, toolbox)) = stale else {
                return Ok(outcome);
            };
            tracing::warn!(
                container = %name,
                toolbox = %toolbox,
                "sandbox disappeared; rebuilding it and retrying the command"
            );

            pool.drop_entry(&self.key);
            pool.ensure(&self.key, &spec)?;
            // Once. A second disappearance is something other than a stale
            // handle, and a loop that keeps rebuilding would hide it.
            pool.run_on(&self.key, request).await
        })
    }
}

impl RunnerResolver for ToolboxPool {
    /// The runner for a turn — policy only, and deliberately no container.
    ///
    /// Resolving a sandbox is on the turn-open path, and starting one there
    /// meant probing the daemon there too. A daemon that has gone away did not
    /// fail this turn, it blocked for five seconds and *then* failed it — every
    /// session and every route with it. Worse, it failed before the turn had
    /// opened, so the failure had no turn to belong to and the operator was
    /// offered no way to re-run it.
    ///
    /// So this decides *whether* a sandbox is allowed and the first command
    /// starts it. A turn that calls no tool now touches the daemon not at all,
    /// and a daemon that is down surfaces as a failed tool card inside a live
    /// turn — which is a thing the model can read and the operator can act on.
    ///
    /// The trait cannot report a refusal, so a revoked or over-privileged
    /// toolbox arrives here as `None` — which would mean the host. The runtime
    /// therefore calls [`ToolboxPool::resolve_turn`], which keeps the refusal;
    /// this method exists for the loop, which asks after the runtime has already
    /// decided the agent may have a sandbox at all.
    fn for_turn(&self, request: &ToolboxRequest) -> Option<Arc<dyn CommandRunner>> {
        match self.resolve_turn(request) {
            Ok(runner) => runner,
            // Refusal, never a downgrade: a turn whose toolbox went away between
            // its opening and this call gets a runner that fails the command,
            // rather than one that runs it on the host.
            Err(error) => Some(Arc::new(Refusing::new(&error))),
        }
    }
}

impl ToolboxPool {
    /// [`RunnerResolver::for_turn`], with the refusal still visible.
    ///
    /// `Ok(None)` is an agent that names no toolbox, which is the host and is
    /// not a refusal. An error is a toolbox that cannot be honoured — revoked,
    /// edited since approval, asking for more network than its ceiling allows —
    /// and the runtime reports it where the operator can act on it.
    pub fn resolve_turn(&self, request: &ToolboxRequest) -> Result<Option<Arc<dyn CommandRunner>>> {
        if request.toolbox.is_empty() {
            return Ok(None);
        }
        let key = key_of(request);
        self.reap_idle();

        // **Before the cache, not after.** Requiring the toolbox is the only
        // thing that re-reads the manifest and re-checks its hash against the
        // approvals table, and a warm entry that skipped it kept serving a
        // toolbox the operator had revoked — or edited into something they
        // considered unsafe — for as long as the session stayed active. A revoke
        // is a different process writing the shared database, so nothing
        // notifies this pool; asking every turn is what makes revocation mean
        // something. It costs one read, one hash and one row.
        let approved = self.options.toolboxes.require(&request.toolbox)?;
        assert_network_within_ceiling(&approved.toolbox, &request.network, &request.agent_id)?;

        let stale = {
            let mut live = self.live.lock();
            // A second turn on the same session may carry a different workspace
            // root or network, and it is the newest one that any command should
            // start from.
            live.specs.insert(key.clone(), request.clone());
            match live.entries.get_mut(&key) {
                None => false,
                Some(entry) if entry.manifest_sha256 == approved.manifest_sha256 => {
                    entry.last_used_ms = self.options.clock.now_ms();
                    // Re-inserted so iteration order stays least-recently-used
                    // first.
                    if let Some(entry) = live.entries.shift_remove(&key) {
                        live.entries.insert(key.clone(), entry);
                    }
                    false
                }
                // A live container started from a manifest that has since
                // changed is stopped rather than reused: it was built with the
                // old policy's flags. The next command starts a replacement from
                // the manifest as it is now.
                Some(_) => true,
            }
        };
        if stale {
            self.drop_entry(&key);
        }

        Ok(Some(self.facade_for(&key)))
    }

    /// The runner a turn holds, built once per key.
    fn facade_for(&self, key: &str) -> Arc<dyn CommandRunner> {
        let mut live = self.live.lock();
        if let Some(cached) = live.facades.get(key) {
            return Arc::clone(cached);
        }
        let facade: Arc<dyn CommandRunner> = Arc::new(Facade {
            pool: self.me.clone(),
            key: key.to_owned(),
        });
        live.facades.insert(key.to_owned(), Arc::clone(&facade));
        facade
    }
}

/// A runner that fails every command with the reason the sandbox was refused.
///
/// The shape a refusal takes once the caller can no longer be told: the command
/// does not run, and the model is told why, which is the whole of "refusal,
/// never a downgrade" at this altitude.
struct Refusing {
    kind: ErrorKind,
    message: String,
    details: serde_json::Map<String, serde_json::Value>,
}

impl Refusing {
    /// A refusal that can be raised again for every command on this turn.
    ///
    /// The parts rather than the error, because an error carries a source chain
    /// that is not duplicable and a refusal has to be answerable more than once.
    fn new(error: &GhostError) -> Refusing {
        Refusing {
            kind: error.kind,
            message: error.message.clone(),
            details: error.details.clone(),
        }
    }
}

impl CommandRunner for Refusing {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        let _ = request;
        Box::pin(async move {
            Err(GhostError::new(self.kind, self.message.clone()).with_details(self.details.clone()))
        })
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
            return Err(GhostError::new(
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
        Err(GhostError::new(
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
                GhostError::new(
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
                    return Err(GhostError::new(
                        ErrorKind::Tool,
                        format!("Could not wait for {}: {error}", self.bin),
                    )
                    .with_source(error));
                }
            }
        }
        let output = child.wait_with_output().map_err(|error| {
            GhostError::new(
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
            "label=ghostai.session",
            "--format",
            &format,
        ]));

        for row in rows.lines().map(str::trim).filter(|row| !row.is_empty()) {
            let (id, container_owner) = match row.split_once(' ') {
                Some((id, rest)) => (id, rest.trim()),
                None => (row, ""),
            };

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
