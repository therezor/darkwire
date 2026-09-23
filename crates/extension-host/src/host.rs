//! Discover, authorise, start, collect, unload — and never take the boot down.
//!
//! The lifecycle is the MCP manager's, deliberately and in every detail that
//! matters, because the two solve the same problem: a set of external processes
//! an operator configures, changing under a running host.
//!
//!  - **[`reconcile`](ExtensionHost::reconcile) is the only entry point.** It
//!    takes the whole `config.extensions` block and works out the difference,
//!    rather than offering add and remove for a caller to sequence correctly.
//!  - **Nothing here fails.** Not a manifest that will not parse, not an
//!    extension that has never been approved, not an `initialize` that dies on
//!    the first line. Every one of those is a `state` on a row with a sentence
//!    beside it, because a boot that refuses because of one extension out of
//!    five is a worse outcome than a boot that runs with four.
//!  - **Status is a list, not a log.** The panel and `darkwire extension list`
//!    read [`status`](ExtensionHost::status); nothing has to grep anything.
//!
//! What it does *not* do is apply anything. `tools()`, `channels()`,
//! `providers()`, `contributors()` and `commands()` are accessors, and the
//! composition root is what puts them into a registry, a channel manager and an
//! agent loop. That is the same split the MCP tool sink makes and for the same
//! reason: this crate knows what an extension asked for, and the layer above
//! knows where such things go.
//!
//! ## Four consequences of the process boundary
//!
//!  - **A `failed` row is retried.** A process carries no memory of the
//!    activation that failed, so a reconcile simply starts it again and can get
//!    a different answer. The live-lock this invites is stopped by
//!    [`settle`](Inner::settle): a row that lands in the
//!    same state, at the same digest, with the same sentence, announces
//!    nothing. Which is why a failure sentence must never carry a pid or a
//!    timestamp — it would differ on every pass and announce a change that did
//!    not happen.
//!  - **Reloading needs no restart.** `drifted` kills the process and
//!    holds the row; approving the new bytes and reconciling starts it again
//!    from them. There is no module cache left to defeat.
//!  - **A crash is a state.** An extension that dies while `ready` becomes
//!    `failed` with its exit status, its bag is dropped whole, and the tool set
//!    is announced as moved. It is restarted **once**, after a delay, and then
//!    left alone until the digest moves or an operator reconciles — a process
//!    that crashes on startup would otherwise be restarted forever.
//!  - **Rows land when the spawn does.** A reconcile returns before a child has
//!    finished its handshake, so a newly discovered extension has *no row* for
//!    a moment rather than a misleading one, and an existing extension keeps the
//!    row it had. The ten-second handshake cap is what bounds that gap.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use darkwire_agent::ContextContributor;
use darkwire_channels::ChannelFactory;
use darkwire_core::Result;
use darkwire_mcp::{McpCallOptions, McpCallResult, McpCallTarget, McpToolDescriptor};
use darkwire_protocol::json::Object;
use darkwire_protocol::{
    ExtensionCommand, ExtensionContribution, ExtensionManifest, ExtensionState, ExtensionStatus,
    ExtensionsConfig,
};
use darkwire_providers::ProviderSpec;
use darkwire_security::{ExtensionResolution, ExtensionResolutionState, ExtensionStore};
use darkwire_tools::AnyTool;
use parking_lot::Mutex;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::bag::{Registration, RegistrationBag, kind_name};
use crate::manifest::{V1_UNSUPPORTED, discover, refuses_version, settings_for};
use crate::methods::{self, ChannelList, CommandList, ExtensionContributor, HostMethods, SecretFn};
use crate::process::{self, SpawnOptions};
use crate::registration::{add_bridged_tool, add_provider};
use crate::rpc::{DarkwireInit, RpcClient, RpcFailure};

/// How long each stage of a life is given.
///
/// Injected rather than constant because a test that had to wait ten real
/// seconds for a handshake cap would be a test nobody runs. Paused time is not
/// the answer for these: a paused clock advances the moment every task is idle
/// on I/O, which for a real child means the cap fires before it can answer.
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    /// How long `initialize` may take before the extension is `failed`.
    pub init_timeout: Duration,
    /// How long each stop escalation waits.
    pub kill_grace: Duration,
    /// How long after a crash the one automatic restart happens.
    pub respawn_delay: Duration,
    /// How long a message may wait for an extension to read its input before
    /// the extension counts as hung, which is handled as a crash.
    pub write_timeout: Duration,
}

impl Default for Timings {
    fn default() -> Timings {
        Timings {
            init_timeout: Duration::from_secs(10),
            kill_grace: Duration::from_millis(process::KILL_GRACE_MS),
            respawn_delay: Duration::from_secs(5),
            write_timeout: crate::rpc::REQUEST_TIMEOUT,
        }
    }
}

/// Where a secret comes from, by extension id.
pub type SecretLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// What the host needs to run extensions.
pub struct ExtensionHostOptions {
    /// The approval ledger and the install directory it reads.
    pub store: ExtensionStore,
    /// The data root. Each extension is given `<root>/extension-data/<id>`.
    pub root: PathBuf,
    /// A secret from the vault under the `extensions` namespace.
    pub secret_for: Option<SecretLookup>,
    /// What the host calls itself in the handshake.
    pub host_version: String,
    /// Called whenever what is loaded changes. The transport's seam.
    pub on_changed: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The four deadlines.
    pub timings: Timings,
}

impl ExtensionHostOptions {
    /// Options with the shipped deadlines and no secrets or listener.
    pub fn new(store: ExtensionStore, root: impl Into<PathBuf>) -> ExtensionHostOptions {
        ExtensionHostOptions {
            store,
            root: root.into(),
            secret_for: None,
            host_version: env!("CARGO_PKG_VERSION").to_owned(),
            on_changed: None,
            timings: Timings::default(),
        }
    }

    /// Supplies the vault lookup.
    #[must_use]
    pub fn with_secrets(mut self, secret_for: SecretLookup) -> ExtensionHostOptions {
        self.secret_for = Some(secret_for);
        self
    }

    /// Supplies the change listener.
    #[must_use]
    pub fn with_listener(mut self, listener: Arc<dyn Fn() + Send + Sync>) -> ExtensionHostOptions {
        self.on_changed = Some(listener);
        self
    }

    /// Replaces the deadlines.
    #[must_use]
    pub fn with_timings(mut self, timings: Timings) -> ExtensionHostOptions {
        self.timings = timings;
        self
    }
}

/// A live extension: its connection and everything hanging off it.
struct Running {
    client: Arc<RpcClient>,
    process: Arc<process::ExtensionProcess>,
    host: Arc<HostMethods>,
    contributor: Option<Arc<ExtensionContributor>>,
}

/// A start that has not landed yet.
struct Starting {
    generation: u64,
    token: CancellationToken,
    /// What it is starting, so a reconcile asking for the same thing leaves
    /// it to finish rather than superseding it.
    digest: String,
    settings: Object,
}

/// One extension the host currently holds, running or not.
struct Loaded {
    status: ExtensionStatus,
    registration: Option<Registration>,
    running: Option<Running>,
    /// The digest it was started at, so a reconcile can tell "unchanged".
    digest: String,
    /// The settings block it was started with. Editing them restarts it.
    settings: Object,
    /// Fires when this generation is retired. Covers the spawn task *and* the
    /// child, so a reconcile that arrives mid-handshake cannot leave two.
    token: CancellationToken,
    /// Which start this is. A task whose generation is stale writes nothing.
    generation: u64,
    /// Whether the one free restart has been spent.
    respawned: bool,
}

struct Inner {
    options: ExtensionHostOptions,
    loaded: Mutex<BTreeMap<String, Loaded>>,
    /// The start in flight for each id.
    ///
    /// Kept apart from `loaded` because a first start has no row to hang them
    /// on, and a stop has to be able to retire a start that has not landed.
    /// Locked only inside `loaded` or on its own, never the other way round.
    starting: Mutex<HashMap<String, Starting>>,
    revision: AtomicU64,
    generation: AtomicU64,
    in_flight: AtomicUsize,
    closed: AtomicBool,
}

/// Third-party code, authorised and run beside the host.
///
/// Cloneable and cheap to clone: every accessor reads a shared map.
#[derive(Clone)]
pub struct ExtensionHost {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ExtensionHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionHost")
            .field("loaded", &self.inner.loaded.lock().len())
            .field("revision", &self.revision())
            .finish_non_exhaustive()
    }
}

impl ExtensionHost {
    /// A host over one install directory and one approval ledger.
    pub fn new(options: ExtensionHostOptions) -> ExtensionHost {
        ExtensionHost {
            inner: Arc::new(Inner {
                options,
                loaded: Mutex::new(BTreeMap::new()),
                starting: Mutex::new(HashMap::new()),
                revision: AtomicU64::new(0),
                generation: AtomicU64::new(0),
                in_flight: AtomicUsize::new(0),
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// Bumped on every change to what is loaded. See the listener option.
    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::SeqCst)
    }

    /// How many extensions are mid-start.
    ///
    /// A reconcile returns before its children have finished their handshakes,
    /// so this is how a caller that must wait — a test, an operator command —
    /// knows when the picture is complete.
    pub fn in_flight(&self) -> usize {
        self.inner.in_flight.load(Ordering::SeqCst)
    }

    /// Waits until nothing is mid-start, or `timeout` passes.
    ///
    /// Answers whether it settled. Polled rather than signalled because the
    /// thing being waited for is "no task is running", which no single task can
    /// announce.
    pub async fn quiesce(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.in_flight() > 0 {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        true
    }

    /// Brings what is running into line with what is installed and configured.
    ///
    /// Four outcomes per extension, and the third is the one worth naming: an
    /// extension whose digest and settings are unchanged and whose process is
    /// still alive is **left running**. A reconcile happens on every settings
    /// save, and tearing down and rebuilding a channel because an unrelated
    /// panel was edited would drop connections for no reason.
    pub fn reconcile(&self, config: &ExtensionsConfig) {
        if self.inner.closed.load(Ordering::SeqCst) {
            return;
        }
        let disabled: HashSet<&str> = config.disabled.iter().map(String::as_str).collect();
        let wanted = discover(&self.inner.options.store, config);
        let mut changed = false;

        let gone: Vec<String> = self
            .inner
            .loaded
            .lock()
            .keys()
            .filter(|id| !wanted.contains_key(*id))
            .cloned()
            .collect();
        for id in gone {
            self.inner.retire(&id);
            changed = true;
        }

        for (id, resolution) in wanted {
            let settings = settings_for(config, &id);

            if disabled.contains(id.as_str()) {
                self.inner.stop_running(&id);
                changed |= self
                    .inner
                    .settle(row(&resolution, ExtensionState::Disabled, None));
                continue;
            }

            // Before the approval state, because it is the more specific
            // answer: an approved v1 bundle is still one this build cannot run,
            // and "approve it" would be advice that leads nowhere.
            if refuses_version(&resolution) {
                self.inner.stop_running(&id);
                changed |= self.inner.settle(row(
                    &resolution,
                    ExtensionState::Failed,
                    Some(V1_UNSUPPORTED.to_owned()),
                ));
                continue;
            }

            if resolution.state != ExtensionResolutionState::Approved {
                self.inner.stop_running(&id);
                changed |= self.inner.settle(row(
                    &resolution,
                    state_of(resolution.state),
                    resolution.problem.clone(),
                ));
                continue;
            }

            if self.inner.is_current(&id, &resolution.digest, &settings) {
                continue;
            }

            // The row it already has stands until the new start lands, so an
            // extension being restarted does not vanish from the panel while
            // its replacement is still handshaking.
            self.inner.stop_running(&id);
            self.inner.start(resolution, settings, false);
            changed = true;
        }

        if changed {
            self.inner.announce();
        }
    }

    /// The process id of one running extension.
    ///
    /// For diagnostics and for a test that has to reach around the host. It is
    /// deliberately *not* on the status row: the row's fields are compared to
    /// decide whether anything moved, and a pid changes on every restart, so a
    /// row carrying one would announce a change on every respawn.
    pub fn pid(&self, id: &str) -> Option<u32> {
        self.inner
            .loaded
            .lock()
            .get(id)
            .and_then(|entry| entry.running.as_ref())
            .and_then(|running| running.process.pid())
    }

    /// Every row, by id. What the panel and the CLI read.
    pub fn status(&self) -> Vec<ExtensionStatus> {
        self.inner
            .loaded
            .lock()
            .values()
            .map(|entry| entry.status.clone())
            .collect()
    }

    /// How many are actually running. What `GET /api/status` reports.
    pub fn loaded_count(&self) -> usize {
        self.inner
            .loaded
            .lock()
            .values()
            .filter(|entry| entry.status.state == ExtensionState::Ready)
            .count()
    }

    /// Every tool every running extension contributed.
    pub fn tools(&self) -> Vec<AnyTool> {
        self.collect(|registration| registration.tools.clone())
    }

    /// Every channel factory.
    pub fn channels(&self) -> Vec<ChannelFactory> {
        self.collect(|registration| registration.channels.clone())
    }

    /// Every provider type, as data.
    pub fn providers(&self) -> Vec<ProviderSpec> {
        self.collect(|registration| registration.providers.clone())
    }

    /// Every prompt contributor.
    pub fn contributors(&self) -> Vec<Arc<dyn ContextContributor>> {
        self.collect(|registration| registration.contributors.clone())
    }

    /// Every command, as the wire describes them. Running stays on this side.
    pub fn commands(&self) -> Vec<ExtensionCommand> {
        self.collect(|registration| registration.commands.clone())
    }

    fn collect<T>(&self, pick: impl Fn(&Registration) -> Vec<T>) -> Vec<T> {
        self.inner
            .loaded
            .lock()
            .values()
            .filter_map(|entry| entry.registration.as_ref())
            .flat_map(pick)
            .collect()
    }

    /// Runs one command.
    ///
    /// Errors where the rest of this type answers, and the asymmetry is
    /// deliberate: this is a request about one command, so a refusal is its
    /// answer. A command that *fails* is turned into a failed result rather
    /// than propagated — an extension's bug should read as "that did not work"
    /// in the composer, not as a 500.
    pub async fn run_command(
        &self,
        id: &str,
        args: &str,
        session_key: Option<&str>,
        token: &CancellationToken,
    ) -> Result<methods::CommandOutcome> {
        let client = {
            let loaded = self.inner.loaded.lock();
            loaded
                .values()
                .find(|entry| {
                    entry
                        .registration
                        .as_ref()
                        .is_some_and(|reg| reg.commands.iter().any(|c| c.id == id))
                })
                .and_then(|entry| entry.running.as_ref())
                .map(|running| Arc::clone(&running.client))
        };
        let Some(client) = client else {
            return Err(methods::no_such_command(id));
        };
        Ok(methods::run_command(&client, id, args, session_key, token).await)
    }

    /// Stops everything. Idempotent, and safe to call on a failed boot.
    pub async fn stop(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let (entries, starting): (Vec<Loaded>, Vec<Starting>) = {
            let mut loaded = self.inner.loaded.lock();
            let starting = std::mem::take(&mut *self.inner.starting.lock());
            (
                std::mem::take(&mut *loaded).into_values().collect(),
                starting.into_values().collect(),
            )
        };
        for start in starting {
            start.token.cancel();
        }
        for entry in entries {
            entry.token.cancel();
            if let Some(running) = entry.running {
                running.client.close();
                running.host.clear_channels();
                running.process.stop().await;
            }
        }
        self.inner.announce();
    }
}

impl Inner {
    /// Whether this extension is already running, or already starting, at
    /// exactly these bytes and these settings.
    ///
    /// A `failed` row deliberately does **not** count. In-process, retrying an
    /// activation that threw could not produce a different answer; a process
    /// has no such memory, so the retry is worth the attempt and the live-lock
    /// is prevented by [`settle`](Self::settle) instead.
    fn is_current(&self, id: &str, digest: &str, settings: &Object) -> bool {
        let loaded = self.loaded.lock();
        let starting = self.starting.lock().get(id).is_some_and(|start| {
            !start.token.is_cancelled() && start.digest == digest && start.settings == *settings
        });
        if starting {
            return true;
        }
        let Some(entry) = loaded.get(id) else {
            return false;
        };
        entry.status.state == ExtensionState::Ready
            && entry.digest == digest
            && entry.settings == *settings
            && entry
                .running
                .as_ref()
                .is_some_and(|running| !running.process.has_exited())
    }

    /// Records a row that is *not* running, and says whether it moved.
    ///
    /// The "and says whether it moved" is the whole of it, and leaving it out
    /// is a live-lock rather than a missed optimisation: a reconcile that
    /// announced unconditionally for an unapproved extension wakes the
    /// composition root, which rebuilds, which reconciles again — and since the
    /// extension is still unapproved, again, forever.
    ///
    /// Compared on the three fields that can move without the id moving: the
    /// state, the digest of what is on disk, and the sentence explaining it.
    /// Everything else on the row is derived from the manifest, which cannot
    /// change without the digest changing.
    fn settle(&self, status: ExtensionStatus) -> bool {
        let mut loaded = self.loaded.lock();
        let previous = loaded.get(&status.id);
        let moved = previous.is_none_or(|entry| {
            entry.status.state != status.state
                || entry.status.digest != status.digest
                || entry.status.last_error != status.last_error
        });
        let digest = status.digest.clone();
        loaded.insert(
            status.id.clone(),
            Loaded {
                status,
                registration: None,
                running: None,
                digest,
                settings: Object::new(),
                token: CancellationToken::new(),
                generation: 0,
                respawned: false,
            },
        );
        moved
    }

    /// Takes one extension's process and bag away, and **leaves its row**.
    ///
    /// The row is what [`settle`](Self::settle) compares against to decide
    /// whether anything moved, so removing it here would make every reconcile
    /// look like a change — which is the live-lock `settle` exists to prevent,
    /// reintroduced one function along. An install with a single unapproved
    /// extension would announce, wake the composition root, rebuild, reconcile,
    /// and do it again forever.
    ///
    /// Cancels first and tears down after, so a reconcile arriving while a
    /// handshake is in flight retires that generation before the next one
    /// starts — which is what stops two children holding one data directory.
    fn stop_running(&self, id: &str) {
        let (starting, token, running) = {
            let mut loaded = self.loaded.lock();
            let starting = self.starting.lock().remove(id).map(|start| start.token);
            let Some(entry) = loaded.get_mut(id) else {
                if let Some(starting) = starting {
                    starting.cancel();
                }
                return;
            };
            // The bag goes now rather than when the teardown resolves: a turn
            // starting in the meantime must not be offered a tool whose process
            // is being killed.
            entry.registration = None;
            (starting, entry.token.clone(), entry.running.take())
        };
        if let Some(starting) = starting {
            starting.cancel();
        }
        token.cancel();
        if let Some(running) = running {
            tear_down(running);
        }
    }

    /// Stops one extension and forgets it entirely.
    ///
    /// Only for an extension that is no longer *installed*: an uninstalled
    /// extension has no row, because "not installed" and "installed and off"
    /// are different things to see on a panel and only the second has a switch
    /// to flip.
    fn retire(&self, id: &str) {
        self.stop_running(id);
        self.loaded.lock().remove(id);
    }

    fn announce(&self) {
        self.revision.fetch_add(1, Ordering::SeqCst);
        if let Some(listener) = &self.options.on_changed {
            listener();
        }
    }

    /// Starts one approved extension, on a task.
    ///
    /// `respawned` says whether the one free restart has already been spent.
    /// A reconcile passes `false`, which re-arms it: an operator who changed
    /// something has earned another attempt. The crash watcher passes `true`,
    /// which is what stops a process that dies on startup from being restarted
    /// forever.
    fn start(self: &Arc<Self>, resolution: ExtensionResolution, settings: Object, respawned: bool) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let token = CancellationToken::new();
        let superseded = self.starting.lock().insert(
            resolution.id.clone(),
            Starting {
                generation,
                token: token.clone(),
                digest: resolution.digest.clone(),
                settings: settings.clone(),
            },
        );
        if let Some(superseded) = superseded {
            superseded.token.cancel();
        }
        let inner = Arc::clone(self);
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let spawn_token = token.clone();
        tokio::spawn(async move {
            inner
                .activate(resolution, settings, generation, spawn_token, respawned)
                .await;
            inner.in_flight.fetch_sub(1, Ordering::SeqCst);
        });
    }

    /// Spawns the child and completes the handshake, or says why it could not.
    ///
    /// Split out of [`activate`](Self::activate) because the two halves fail
    /// differently: everything here ends as one sentence on a `failed` row,
    /// and everything after it has already produced a working connection and
    /// can only produce *warnings*.
    async fn connect(
        &self,
        manifest: &ExtensionManifest,
        resolution: &ExtensionResolution,
        settings: &Object,
        data_dir: &std::path::Path,
    ) -> std::result::Result<
        (
            Arc<process::ExtensionProcess>,
            Arc<RpcClient>,
            Arc<HostMethods>,
        ),
        String,
    > {
        let id = manifest.id.clone();
        let options = SpawnOptions::new(
            id.clone(),
            resolution.dir.clone(),
            manifest.command.clone(),
            data_dir.to_path_buf(),
        )
        .with_env(manifest.env.clone())
        .with_kill_grace(self.options.timings.kill_grace);

        let process::Spawned {
            stdout,
            stdin,
            process,
        } = process::spawn(options).map_err(|error| error.message)?;

        // Bound to this extension's own id at construction, which is what makes
        // `darkwire/secret` unable to name anyone else's.
        let lookup = self.options.secret_for.clone();
        let own_id = id.clone();
        let secret: Option<SecretFn> =
            lookup.map(|lookup| Arc::new(move || lookup(&own_id)) as SecretFn);
        let host = HostMethods::new(id.clone(), secret);
        let client = RpcClient::start(
            stdout,
            stdin,
            Arc::clone(&host) as Arc<dyn crate::rpc::RpcHandler>,
            &CancellationToken::new(),
            self.options.timings.write_timeout,
        );

        let init = DarkwireInit {
            extension_id: id,
            settings: settings.clone(),
            data_dir: data_dir.to_string_lossy().into_owned(),
            host_version: self.options.host_version.clone(),
        };
        let handshake =
            tokio::time::timeout(self.options.timings.init_timeout, client.initialize(&init)).await;

        let problem = match handshake {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(error.message),
            Err(_) => Some(format!(
                "The extension did not complete its handshake within {} ms.",
                self.options.timings.init_timeout.as_millis()
            )),
        };
        if let Some(problem) = problem {
            client.close();
            process.stop().await;
            return Err(problem);
        }
        Ok((process, client, host))
    }

    /// The whole of a start: spawn, handshake, probe, and write the row.
    ///
    /// Answers, never fails. Everything that can go wrong becomes a `failed`
    /// row with a sentence, because a boot must survive an extension that does
    /// not.
    async fn activate(
        self: &Arc<Self>,
        resolution: ExtensionResolution,
        settings: Object,
        generation: u64,
        token: CancellationToken,
        respawned: bool,
    ) {
        // `approved` is only reachable with a parsed manifest, so this is a
        // narrowing rather than a real branch — stated as one anyway, because
        // the alternative is an unwrap on the load path.
        let Some(manifest) = resolution.manifest.clone() else {
            self.land(
                generation,
                &token,
                row(
                    &resolution,
                    ExtensionState::Failed,
                    Some("The extension has no readable manifest.".to_owned()),
                ),
                None,
                None,
                settings,
                respawned,
            );
            return;
        };

        let id = manifest.id.clone();
        let data_dir = process::data_dir_for(&self.options.root, &id);
        let (child, client, host) = match self
            .connect(&manifest, &resolution, &settings, &data_dir)
            .await
        {
            Ok(connected) => connected,
            Err(problem) => {
                self.land(
                    generation,
                    &token,
                    row(&resolution, ExtensionState::Failed, Some(problem)),
                    None,
                    None,
                    settings,
                    respawned,
                );
                return;
            }
        };

        let warnings = Arc::new(Mutex::new(Vec::new()));
        let (registration, contributor) =
            probe(&manifest, &client, &host, Arc::clone(&warnings)).await;

        let mut status = row(&resolution, ExtensionState::Ready, None);
        status.tools = registration.tool_names();
        status.channels = registration.channel_ids();
        status.providers = registration.provider_ids();
        status.commands = registration.command_ids();
        status.warnings = registration.warnings.clone();
        status.warnings.extend(warnings.lock().iter().cloned());

        let running = Running {
            client: Arc::clone(&client),
            process: Arc::clone(&child),
            host,
            contributor,
        };
        let landed = self.land(
            generation,
            &token,
            status,
            Some(registration),
            Some(running),
            settings,
            respawned,
        );
        if !landed {
            // A reconcile overtook this start. The row belongs to a newer
            // generation, so this child is surplus and goes now.
            client.close();
            child.stop().await;
            return;
        }

        self.watch(id, generation, resolution, child, client, token);
    }

    /// Writes a row, unless this generation has been retired underneath it.
    ///
    /// The generation check is the whole of the respawn race: two reconciles a
    /// second apart each start a child, and without this the slower one's row
    /// would overwrite the faster one's while its process kept running. Only
    /// the start registered in `starting` may land, and the check and the
    /// write share one lock with [`stop_running`](Self::stop_running).
    fn land(
        &self,
        generation: u64,
        token: &CancellationToken,
        status: ExtensionStatus,
        registration: Option<Registration>,
        running: Option<Running>,
        settings: Object,
        respawned: bool,
    ) -> bool {
        let replaced = {
            let mut loaded = self.loaded.lock();
            let mut starting = self.starting.lock();
            let current = starting
                .get(&status.id)
                .is_some_and(|start| start.generation == generation);
            if !current || token.is_cancelled() || self.closed.load(Ordering::SeqCst) {
                return false;
            }
            starting.remove(&status.id);
            drop(starting);
            let digest = status.digest.clone();
            loaded.insert(
                status.id.clone(),
                Loaded {
                    status,
                    registration,
                    running,
                    digest,
                    settings,
                    token: token.clone(),
                    generation,
                    respawned,
                },
            )
        };
        // Every path here stops the old process first, so this finds nothing.
        // If one ever does not, the child it held must not outlive its row.
        if let Some(replaced) = replaced {
            replaced.token.cancel();
            if let Some(running) = replaced.running {
                tear_down(running);
            }
        }
        self.announce();
        true
    }

    /// Watches a running extension for a crash.
    ///
    /// One automatic restart, after a delay, and then nothing until the digest
    /// moves or an operator reconciles. A process that dies on startup would
    /// otherwise be restarted forever, which is a busy loop with a log line
    /// attached.
    ///
    /// A child that stopped reading its input is a crash too. It is alive and
    /// will never answer again, so it is stopped here and reported the same
    /// way.
    fn watch(
        self: &Arc<Self>,
        id: String,
        generation: u64,
        resolution: ExtensionResolution,
        child: Arc<process::ExtensionProcess>,
        client: Arc<RpcClient>,
        token: CancellationToken,
    ) {
        let inner = Arc::clone(self);
        tokio::spawn(async move {
            let status = tokio::select! {
                () = token.cancelled() => return,
                status = child.wait() => status,
                reason = client.stalled() => {
                    child.stop().await;
                    reason
                }
            };
            if token.is_cancelled() {
                return;
            }

            let respawn = {
                let mut loaded = inner.loaded.lock();
                let Some(entry) = loaded.get_mut(&id) else {
                    return;
                };
                if entry.generation != generation {
                    return;
                }
                // The bag goes whole. A tool whose process is gone must not be
                // offered to a turn that starts in the meantime.
                entry.registration = None;
                entry.running = None;
                entry.status.state = ExtensionState::Failed;
                entry.status.last_error = Some(status.clone());
                entry.status.tools.clear();
                entry.status.channels.clear();
                entry.status.providers.clear();
                entry.status.commands.clear();
                let first = !entry.respawned;
                entry.respawned = true;
                first
            };
            tracing::warn!(
                target: "extension",
                extension = %id,
                status,
                respawning = respawn,
                "an extension process ended while it was running"
            );
            inner.announce();

            if !respawn {
                return;
            }
            tokio::time::sleep(inner.options.timings.respawn_delay).await;
            if token.is_cancelled() || inner.closed.load(Ordering::SeqCst) {
                return;
            }
            let settings = {
                let loaded = inner.loaded.lock();
                loaded
                    .get(&id)
                    .map(|entry| entry.settings.clone())
                    .unwrap_or_default()
            };
            inner.start(resolution, settings, true);
        });
    }
}

/// Closes a running extension's connection and stops its process, on a task.
fn tear_down(running: Running) {
    running.client.close();
    running.host.clear_channels();
    if let Some(contributor) = running.contributor {
        contributor.forget();
    }
    tokio::spawn(async move { running.process.stop().await });
}

/// Calls every list method once, and records what came back.
///
/// **Every** kind is asked, not only the declared ones, and that is what makes
/// the two mirror-image warnings fall out of one pass: a kind the manifest
/// declared and the extension answers `-32601` to earns "declares X but does
/// not implement Y", and a kind the extension answers and the manifest never
/// declared is dropped by the bag with the warning it already knows how to
/// write. Four extra round trips at load time is the whole cost.
async fn probe(
    manifest: &ExtensionManifest,
    client: &Arc<RpcClient>,
    host: &Arc<HostMethods>,
    warnings: Arc<Mutex<Vec<String>>>,
) -> (Registration, Option<Arc<ExtensionContributor>>) {
    let mut bag = RegistrationBag::new(manifest);
    let target: Arc<dyn McpCallTarget> = Arc::new(ExtensionTarget {
        client: Arc::clone(client),
    });

    match tools_of(client).await {
        Probe::Answered(descriptors) => {
            for descriptor in &descriptors {
                add_bridged_tool(&mut bag, &manifest.id, descriptor, Arc::clone(&target));
            }
        }
        Probe::Missing => missing(&mut bag, ExtensionContribution::Tools, "tools/list"),
        Probe::Broken(problem) => broken(&mut bag, "tools/list", &problem),
    }

    match methods::list::<CommandList>(client, methods::COMMANDS_LIST).await {
        Ok(Some(list)) => {
            for entry in list.commands {
                bag.add_command(ExtensionCommand {
                    id: entry.id,
                    extension_id: manifest.id.clone(),
                    description: entry.description,
                    args_hint: entry.args_hint,
                });
            }
        }
        Ok(None) => missing(
            &mut bag,
            ExtensionContribution::Commands,
            methods::COMMANDS_LIST,
        ),
        Err(failure) => broken(&mut bag, methods::COMMANDS_LIST, &failure.message()),
    }

    match methods::list::<ChannelList>(client, methods::CHANNELS_LIST).await {
        Ok(Some(list)) => {
            for entry in list.channels {
                bag.add_channel(methods::channel_factory(
                    &entry.id,
                    Arc::clone(client),
                    Arc::clone(host),
                ));
            }
        }
        Ok(None) => missing(
            &mut bag,
            ExtensionContribution::Channels,
            methods::CHANNELS_LIST,
        ),
        Err(failure) => broken(&mut bag, methods::CHANNELS_LIST, &failure.message()),
    }

    // The context probe is a real call for the unnamed default agent, whose
    // answer is discarded: what it establishes is that the method exists. The
    // section that reaches a prompt is fetched per turn, with the agent the
    // turn is actually running as.
    let contributor = match client
        .request(methods::CONTEXT_STATIC, json!({"agentId": ""}))
        .await
    {
        Ok(_) => {
            let made = Arc::new(ExtensionContributor::new(
                manifest.id.clone(),
                Arc::clone(client),
                warnings,
            ));
            bag.add_contributor(Arc::clone(&made) as Arc<dyn ContextContributor>);
            Some(made)
        }
        Err(failure) if failure.is_method_not_found() => {
            missing(
                &mut bag,
                ExtensionContribution::Context,
                methods::CONTEXT_STATIC,
            );
            None
        }
        Err(failure) => {
            broken(&mut bag, methods::CONTEXT_STATIC, &failure.message());
            None
        }
    };

    // Providers need no round trip: they are manifest data, which is the whole
    // point of the change. The bag still drops them when `contributes` does not
    // name `providers`, so the declaration stays honest for these too.
    for spec in &manifest.providers {
        add_provider(&mut bag, spec);
    }

    (bag.finish(), contributor)
}

/// What one probe established.
enum Probe {
    Answered(Vec<McpToolDescriptor>),
    Missing,
    Broken(String),
}

async fn tools_of(client: &Arc<RpcClient>) -> Probe {
    match client.request("tools/list", json!({})).await {
        Ok(value) => {
            let descriptors = value
                .get("tools")
                .cloned()
                .and_then(|tools| serde_json::from_value(tools).ok())
                .unwrap_or_default();
            Probe::Answered(descriptors)
        }
        Err(failure) if failure.is_method_not_found() => Probe::Missing,
        Err(failure) => Probe::Broken(failure.message()),
    }
}

/// The mirror image of the bag's undeclared-kind warning: a kind the manifest
/// declared and the extension does not implement.
fn missing(bag: &mut RegistrationBag, kind: ExtensionContribution, method: &str) {
    if !bag.declares(kind) {
        return;
    }
    bag.warn(format!(
        "The manifest declares \"{}\" but the extension does not implement \"{method}\".",
        kind_name(kind)
    ));
}

fn broken(bag: &mut RegistrationBag, method: &str, problem: &str) {
    bag.warn(format!("\"{method}\" failed: {problem}"));
}

/// The extension's connection, as something the MCP bridge can call.
///
/// One adapter, so the bridge stays the bridge: it knows how to turn a
/// descriptor into a tool and how to call one, and it does not need to learn
/// that some of its servers are extensions.
struct ExtensionTarget {
    client: Arc<RpcClient>,
}

impl McpCallTarget for ExtensionTarget {
    fn call(
        &self,
        upstream_name: &str,
        args: Object,
        options: McpCallOptions,
    ) -> futures::future::BoxFuture<'_, Result<McpCallResult>> {
        let name = upstream_name.to_owned();
        Box::pin(async move {
            let params = json!({"name": name, "arguments": args});
            let call = self
                .client
                .request_cancellable("tools/call", params, &options.token)
                .await;
            match call {
                Ok(value) => Ok(serde_json::from_value(value).unwrap_or_else(|error| {
                    McpCallResult {
                        content: vec![darkwire_mcp::McpContentPart::text(format!(
                            "The extension answered with a result this host could not read: {error}"
                        ))],
                        is_error: Some(true),
                        structured_content: None,
                    }
                })),
                Err(RpcFailure::Peer(error)) => Ok(McpCallResult {
                    content: vec![darkwire_mcp::McpContentPart::text(error.message)],
                    is_error: Some(true),
                    structured_content: None,
                }),
                Err(RpcFailure::Transport(error)) => Err(error),
            }
        })
    }
}

/// The wire state for one resolution state. Never `ready`: running decides that.
fn state_of(state: ExtensionResolutionState) -> ExtensionState {
    match state {
        ExtensionResolutionState::Unapproved => ExtensionState::Unapproved,
        ExtensionResolutionState::Drifted => ExtensionState::Drifted,
        // An approved resolution never reaches here; a failed one is `failed`.
        ExtensionResolutionState::Failed | ExtensionResolutionState::Approved => {
            ExtensionState::Failed
        }
    }
}

/// A row built from what the manifest and the resolution already say.
fn row(
    resolution: &ExtensionResolution,
    state: ExtensionState,
    last_error: Option<String>,
) -> ExtensionStatus {
    let manifest = resolution.manifest.as_ref();
    ExtensionStatus {
        id: resolution.id.clone(),
        state,
        version: manifest.map(|m| m.version.clone()).unwrap_or_default(),
        label: manifest.map(|m| m.label.clone()).unwrap_or_default(),
        description: manifest.map(|m| m.description.clone()).unwrap_or_default(),
        contributes: manifest.map(|m| m.contributes.clone()).unwrap_or_default(),
        tools: Vec::new(),
        channels: Vec::new(),
        providers: Vec::new(),
        commands: Vec::new(),
        digest: resolution.digest.clone(),
        approved_at_ms: resolution
            .approved_at_ms
            .and_then(|ms| u64::try_from(ms).ok()),
        last_error,
        warnings: Vec::new(),
    }
}
