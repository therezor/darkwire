//! The composition root.
//!
//! Every crate below this one takes its collaborators as options and constructs
//! none of them; this is the single place where a config file becomes a
//! provider, a jail, a store, a registry and a loop. That is what keeps the rest
//! of the repo testable without a filesystem — and it is why this module is the
//! only one that touches the vault, the database and the keychain.
//!
//! It lives in its own crate rather than in the CLI because there is more than
//! one consumer: `ghostai chat`, the HTTP server, the scheduler and every
//! channel all need the same wiring, and wiring implemented twice is wiring that
//! differs in exactly the case nobody tested.
//!
//! The decisions here that are not obvious:
//!
//!  - **Provider resolution is `ghostai-providers`' order, not a second one.**
//!    Resolution runs explicit instance → provider type → the `auto` order, and
//!    answers `None` rather than guessing. Exactly one step follows that: a
//!    provider whose `env_key` is set in the environment. An exported credential
//!    is an operator saying which provider they mean, and
//!    `OPENAI_API_KEY=… ghostai chat` should not need a config file to work.
//!    What it will not do is fall back to *some* provider, because a request
//!    landing at an endpoint nobody chose fails as a 401 from somewhere
//!    unexpected.
//!
//!  - **An unconfigured install is a state, not an error.** A runtime with no
//!    resolvable provider, or none with a model, builds anyway: the loop is
//!    absent, `configured` is false, and everything that does not need a model —
//!    the store, the workspaces, the tool registry, every route but the turn —
//!    works. This is what lets `ghostai serve` come up on a bare machine and
//!    serve the settings UI that fixes it; refusing to construct meant the only
//!    cure for a missing config was to hand-write one.
//!    [`GhostRuntime::require_loop`] is where the refusal moved to, so a
//!    terminal turn still fails with the same message it always did.
//!
//!  - **A construction-time provider/model override outlives a reconfigure.**
//!    `ghostai chat --model x` is a statement about this process, and a settings
//!    save from a browser must not silently move the terminal session onto
//!    another model. A caller that wants config to drive the model — the server
//!    does — simply passes neither.
//!
//!  - **A reconfigure rebuilds everything derived and keeps everything owned.**
//!    The store, the tool registry and the steering queue survive; the provider,
//!    the jail and the loop are rebuilt. A turn already running keeps the loop it
//!    started on, which is the only coherent answer: its provider request is in
//!    flight and its tool definitions are already in the model's context.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use ghostai_agent::approval::ApprovalGate;
use ghostai_agent::{
    AgentLoop, AgentLoopOptions, ContextContributor, Host, LoopAgent, LoopResolver,
    MemoryContributor, PromptAgent, PromptToolbox, PromptToolboxTool, SkillsContributor,
    SteeringQueue, subagent_map,
};
use ghostai_core::paths::ResolveGhostPaths;
use ghostai_core::{
    Clock, Database, ErrorKind, GhostError, GhostPaths, LoadConfigOptions, Result, SessionStore,
    SystemClock, WorkspaceStore, load_config,
};
use ghostai_extension_host::{ExtensionHost, ExtensionHostOptions};
use ghostai_mcp::{
    BackoffOptions, McpConnector, McpManager, McpManagerOptions, SdkConnector, SdkConnectorOptions,
};
use ghostai_protocol::{
    Config, ConfigPatch, DEFAULT_AGENT_ID, McpServerStatus, NetworkMode, ProviderConfig,
    SandboxRequest, TOOLBOX_DEFAULT_KEY, ToolPermission, ToolPermissions, ToolSource, new_uuid,
};
use ghostai_providers::{
    ChatProvider, PROVIDERS, ProviderInstance, ProviderSpec, ResolveInstanceOptions,
    resolve_connection, resolve_instance,
};
use ghostai_security::{
    CredentialVault, ExtensionStore, InstalledToolbox, JailResolver, OsRandom, PolicyStore,
    RandomSource, WorkspaceJail, assert_gateway_compatible, narrow_permission,
};
use ghostai_tools::{
    AnyTool, AutomationResolver, BuiltinOptions, ToolRegistry, ToolRegistryOptions, ToolSink,
    register_builtins,
};
use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};

use crate::agents::{
    AgentConfigWarning, EffectiveAgent, assert_writable_agent_ids, granted,
    prune_dangling_subagents, resolve_agent, resolve_agents, tool_prompt_warnings,
};
use crate::credentials::{PROVIDER_CREDENTIAL_NAMESPACE, VaultChoice, find_credential, open_vault};
use crate::jail_cache::JailCache;
use crate::loop_cache::LoopCache;
use crate::merge::merge_config_patch;
use crate::provider_cache::{ProviderCache, ProviderRequest};
use crate::tool_sink::registry_tool_sink;

/// How the MCP client is wired, or that it is switched off.
///
/// `Off` switches it off the way `tools: false` does the built-ins — an install
/// that has configured no server pays nothing either way, but a test that wants
/// to prove the registry is untouched can say so.
#[derive(Default)]
pub enum McpChoice {
    /// The real SDK connector.
    #[default]
    Default,
    /// No MCP client at all.
    Off,
    /// A connector a test supplies, so nothing spawns a subprocess or opens a
    /// socket.
    Connector {
        /// What to connect with.
        connect: Arc<dyn McpConnector>,
        /// The backoff shape.
        backoff: Option<BackoffOptions>,
        /// Overrides the loopback OAuth callback port.
        callback_port: Option<u16>,
    },
}

impl std::fmt::Debug for McpChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            McpChoice::Default => "McpChoice::Default",
            McpChoice::Off => "McpChoice::Off",
            McpChoice::Connector { .. } => "McpChoice::Connector",
        })
    }
}

/// How the extension host is wired, or that it is switched off.
///
/// `Off` switches it off exactly as [`McpChoice::Off`] does, and for the same
/// two reasons: an install with nothing in `~/.ghostai/extensions` pays nothing
/// either way, and a test that wants to prove the registry holds only built-ins
/// can say so.
#[derive(Debug, Default, Clone)]
pub enum ExtensionChoice {
    /// The real host over `<root>/extensions`.
    #[default]
    Default,
    /// No extension host at all.
    Off,
    /// The real host over a fixture tree.
    Dir(PathBuf),
}

/// Everything a runtime is injected with.
///
/// The injection surface, and the reason nothing below here constructs its own
/// collaborators: a test builds a whole runtime without a keychain, a daemon or
/// a socket by naming the seams it wants.
pub struct RuntimeOptions {
    /// `GHOSTAI_HOME` override.
    pub home: Option<String>,
    /// Wins over the config's `workspace`, and keeps winning after a patch.
    pub workspace: Option<String>,
    /// Pins the model for this process; config cannot move it.
    pub model: Option<String>,
    /// Pins the provider for this process; config cannot move it.
    pub provider: Option<String>,
    /// `false` starts the loop with no tools at all.
    pub tools: bool,
    /// Who to ask before a tool whose risk band is set to `ask` runs.
    ///
    /// Survives a reconfigure: the gate belongs to the process that built the
    /// runtime — a WebSocket hub, a channel — not to the settings, which only
    /// say which risk bands need asking about. Absent means nothing is asked,
    /// which is what a terminal session wants and what a browser-facing server
    /// must not do.
    pub approvals: Option<Arc<dyn ApprovalGate>>,
    /// The environment to read. Defaults to the process environment.
    pub env: Option<HashMap<String, String>>,
    /// Supplies the scheduler a turn's automation tool writes through.
    ///
    /// Injected rather than built here, because the store it needs is created by
    /// the server — which happens *after* this runtime exists.
    pub automation: Option<Arc<dyn AutomationResolver>>,
    /// Translates GhostAI's view of the workspace into the *daemon's*.
    ///
    /// Identity when GhostAI runs on the host. A containerised GhostAI must
    /// supply this: a bind path is resolved by the daemon, so asking for its own
    /// `/data/workspace` would mount the host's path of that name — silently,
    /// and usually as an empty directory.
    pub host_workspace_path: Option<ghostai_environment::container_pool::HostPathFn>,
    /// The credential vault. See [`VaultChoice`].
    pub vault: VaultChoice,
    /// The MCP client.
    pub mcp: McpChoice,
    /// The extension host.
    pub extensions: ExtensionChoice,
    /// A connection to share.
    ///
    /// The auth store and the scheduler live in the same file; handing them one
    /// [`Database`] keeps every write in a single WAL and makes cross-table
    /// transactions possible.
    pub database: Option<Database>,
    /// Wall-clock and monotonic time.
    pub clock: Option<Arc<dyn Clock>>,
    /// Shared between runtimes in a process that builds more than one.
    pub providers: Option<Arc<ProviderCache>>,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        RuntimeOptions {
            home: None,
            workspace: None,
            model: None,
            provider: None,
            tools: true,
            approvals: None,
            env: None,
            automation: None,
            host_workspace_path: None,
            vault: VaultChoice::default(),
            mcp: McpChoice::default(),
            extensions: ExtensionChoice::default(),
            database: None,
            clock: None,
            providers: None,
        }
    }
}

impl std::fmt::Debug for RuntimeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeOptions")
            .field("home", &self.home)
            .field("workspace", &self.workspace)
            .field("model", &self.model)
            .field("provider", &self.provider)
            .field("tools", &self.tools)
            .field("vault", &self.vault)
            .field("mcp", &self.mcp)
            .field("extensions", &self.extensions)
            .finish_non_exhaustive()
    }
}

/// Everything a config produces.
///
/// Replaced as a unit, so a failure changes none of it — and the loop and the
/// instance are absent together, never one without the other.
struct Resolved {
    config: Config,
    /// Every enabled agent, resolved. Rebuilt with the config it came from.
    agents: Vec<EffectiveAgent>,
    /// What the settings asked for and this build had to ignore.
    ///
    /// Held beside the agents rather than raised, because every one of these is
    /// survivable and at least one of them — a delegation to an agent someone
    /// deleted — is not the fault of whoever is starting the server now.
    warnings: Vec<AgentConfigWarning>,
    paths: GhostPaths,
    jails: Arc<JailCache>,
    /// One loop per agent, built on first use. Dropped whole on a reconfigure.
    loops: Arc<LoopCache>,
    agent_loop: Option<AgentLoop>,
    instance: Option<ProviderInstance>,
    model: String,
    has_credential: bool,
    /// The error `require_loop` raises, prepared where the reason is still
    /// known.
    unconfigured: Option<(ErrorKind, String)>,
}

/// What one build resolved about the policy every agent named.
struct BuiltPolicies {
    /// The prompt section describing each toolboxed agent's operations.
    prompts: IndexMap<String, PromptToolbox>,
    /// The permission each agent's grants resolved to, for the advertised-name
    /// warnings.
    permissions: IndexMap<String, ToolPermissions>,
}

/// A provider whose `env_key` is exported, used only after resolution answered
/// nothing.
///
/// Table order decides ties, which puts gateways first — the same precedence the
/// gateway lookup applies.
///
/// Returned as a synthetic instance rather than a bare spec: everything past
/// resolution speaks in instances, and this one's id matches its provider id,
/// which is where a pre-instance install's vault entry already lives.
fn instance_from_env<S: std::hash::BuildHasher>(
    env: &HashMap<String, String, S>,
) -> Option<ProviderInstance> {
    PROVIDERS.iter().find_map(|spec| {
        let key = spec.env_key.as_deref()?;
        if env.get(key).is_none_or(String::is_empty) {
            return None;
        }
        Some(ProviderInstance {
            id: spec.id.clone(),
            spec: spec.clone(),
            config: ProviderConfig {
                kind: spec.id.clone(),
                label: String::new(),
                api_base: None,
                extra_headers: IndexMap::new(),
                models: Vec::new(),
                enabled: true,
            },
        })
    })
}

/// The refusal an install with no resolvable provider gets.
fn no_provider_error(config_file: &std::path::Path) -> (ErrorKind, String) {
    let ids: Vec<&str> = PROVIDERS.iter().map(|spec| spec.id.as_str()).collect();
    (
        ErrorKind::Config,
        format!(
            "No provider could be resolved.\n  Run `ghostai init` to configure one \
             interactively, pass --provider <id> --model <model>,\n  export the provider's API \
             key variable, or add a provider in {}.\n  Known providers: {}",
            config_file.display(),
            ids.join(", ")
        ),
    )
}

/// The refusal an instance with no model gets.
fn no_model_error(
    instance: &ProviderInstance,
    config_file: &std::path::Path,
) -> (ErrorKind, String) {
    (
        ErrorKind::Config,
        format!(
            "No model configured for {}.\n  Run `ghostai init`, pass --model <model>, or set \
             this agent's model in {}.",
            instance.spec.display_name,
            config_file.display()
        ),
    )
}

/// Ids for the sessions and messages this runtime's store mints.
///
/// UUIDv7 off the injected clock and the OS CSPRNG, so a test that pauses time
/// still gets distinct ids and nothing here reaches for ambient randomness.
fn new_id(clock: Arc<dyn Clock>) -> ghostai_core::session_store::IdSource {
    Box::new(move || {
        let mut random = [0u8; 10];
        OsRandom.fill(&mut random);
        new_uuid(u64::try_from(clock.now_ms()).unwrap_or(0), &random)
    })
}

/// Where a config's paths land.
///
/// The same precedence the loader applies — an explicit workspace, then the
/// config file, then `<root>/workspace` — restated here because a reconfigure
/// has a new config and no file read to hang it off.
fn paths_for(config: &Config, options: &RuntimeOptions) -> Result<GhostPaths> {
    let configured = config.workspace.clone();
    let workspace = options.workspace.clone().or(if configured.is_empty() {
        None
    } else {
        Some(configured)
    });
    GhostPaths::resolve(ResolveGhostPaths {
        root: options.home.clone(),
        workspace,
        env: options.env.clone(),
        home: None,
    })
}

/// The composition root: config in, a running agent out.
pub struct GhostRuntime {
    options: RuntimeOptions,
    env: HashMap<String, String>,
    clock: Arc<dyn Clock>,
    file: PathBuf,
    store: Arc<SessionStore>,
    workspaces: Arc<WorkspaceStore>,
    /// Survives a reconfigure, so MCP and extension registrations are not lost.
    tools: Arc<ToolRegistry>,
    /// Survives a reconfigure, so a steer queued mid-turn is not dropped.
    steering: Arc<SteeringQueue>,
    providers: Arc<ProviderCache>,
    /// An injected cache outlives this runtime; closing its adapters is not ours
    /// to do.
    owns_providers: bool,
    /// Survives a reconfigure, for the reason the registry does.
    mcp: Option<McpManager>,
    /// Survives a reconfigure, for the reason the registry does.
    extensions: Option<Arc<ExtensionHost>>,
    /// Where an extension's tools land, remembered by extension id.
    ///
    /// Owned rather than built per call, and that is the whole of its
    /// correctness: the sink is what remembers which names each extension
    /// currently holds, so a fresh one would have nothing to take away and a
    /// disabled extension's tools would stay in the registry forever.
    extension_sink: Arc<dyn ToolSink>,
    /// Re-entry guard for [`Self::on_extensions_changed`]; see its doc.
    rebuilding: AtomicBool,
    current: RwLock<Arc<Resolved>>,
}

/// The runtime an extension announcement wakes, once it exists.
///
/// The host is built before the runtime — it has to be, the runtime holds it —
/// so the listener is handed this slot and the slot is filled the moment the
/// `Arc` is in hand. Weak, because the runtime owns the host which owns the
/// listener, and it is filled *after the first build*, which is what reproduces
/// "subscribed after the first build": until then an announcement finds nothing
/// and returns, and the first build does its own applying.
type RuntimeHook = Arc<OnceLock<Weak<GhostRuntime>>>;

impl std::fmt::Debug for GhostRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let current = self.current.read();
        f.debug_struct("GhostRuntime")
            .field("file", &self.file)
            .field("configured", &current.agent_loop.is_some())
            .field("model", &current.model)
            .finish_non_exhaustive()
    }
}

/// Builds a runtime from `options`.
///
/// Fails only on settings that cannot be built at all — an unusable workspace,
/// an agent naming a toolbox that is not installed. A missing provider or model is a
/// *state*: the runtime comes up unconfigured.
pub fn create_runtime(options: RuntimeOptions) -> Result<Arc<GhostRuntime>> {
    GhostRuntime::new(options)
}

impl GhostRuntime {
    /// See [`create_runtime`].
    pub fn new(options: RuntimeOptions) -> Result<Arc<GhostRuntime>> {
        let env = options
            .env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        let clock: Arc<dyn Clock> = options
            .clock
            .clone()
            .unwrap_or_else(|| Arc::new(SystemClock));

        let loaded = load_config(LoadConfigOptions {
            paths: ResolveGhostPaths {
                root: options.home.clone(),
                workspace: options.workspace.clone(),
                env: Some(env.clone()),
                home: None,
            },
            file: None,
        })?;

        // The database is opened before anything can fail on a bad provider, so
        // a config error does not leave a half-built runtime holding a
        // connection nobody has a reference to close.
        let database = match &options.database {
            Some(shared) => shared.clone(),
            None => Database::open(&loaded.paths.db_file)?,
        };
        let store = Arc::new(SessionStore::new(
            database.clone(),
            Arc::clone(&clock),
            new_id(Arc::clone(&clock)),
        )?);
        // Over the store's own connection, so the registry and the sessions that
        // reference it are in one WAL — a workspace cannot be detached while
        // sessions still name it, and that invariant is unenforceable across two
        // connections.
        let workspaces = Arc::new(WorkspaceStore::new(
            database.clone(),
            loaded.paths.clone(),
            Arc::clone(&clock),
        )?);
        let tools = Arc::new(ToolRegistry::with_options(ToolRegistryOptions {
            // The default agent's, because the registry is built once for the
            // process while a timeout is per agent. A turn on another agent
            // re-reads its own — this is the floor for anything built before an
            // agent is chosen.
            timeout_ms: loaded
                .config
                .agents
                .list
                .get(DEFAULT_AGENT_ID)
                .map_or(0, |entry| entry.settings.tool_timeout_ms),
            clock: Some(Arc::clone(&clock)),
        }));

        let providers = options
            .providers
            .clone()
            .unwrap_or_else(|| Arc::new(ProviderCache::new()));
        let owns_providers = options.providers.is_none();

        // Beside the registry rather than inside the build, and for the same
        // reason the registry is: a settings save must not tear down every
        // connection and open it again. The build hands it the servers and it
        // diffs them.
        let mcp = build_mcp(&options, &tools, &loaded.paths, &clock);
        // The same placement, the same argument. A reconcile leaves an unchanged
        // extension running, so a save on an unrelated panel must not be able to
        // reach a fresh host that has never loaded anything.
        let hook: RuntimeHook = Arc::new(OnceLock::new());
        let extensions = build_extension_host(&options, &database, &loaded.paths, &clock, &hook)?;

        let extension_sink = registry_tool_sink(Arc::clone(&tools), ToolSource::Extension);
        let runtime = Arc::new(GhostRuntime {
            options,
            env,
            clock,
            file: loaded.file,
            store,
            workspaces,
            tools,
            steering: Arc::new(SteeringQueue::new()),
            providers,
            owns_providers,
            mcp,
            extensions,
            extension_sink,
            rebuilding: AtomicBool::new(false),
            // Replaced by the build below before anything can observe it.
            current: RwLock::new(Arc::new(Resolved {
                config: loaded.config.clone(),
                agents: Vec::new(),
                warnings: Vec::new(),
                paths: loaded.paths.clone(),
                jails: Arc::new(JailCache::new(loaded.paths.clone())?),
                loops: Arc::new(LoopCache::new(Arc::new(|_| Ok(None)))),
                agent_loop: None,
                instance: None,
                model: String::new(),
                has_credential: false,
                unconfigured: None,
            })),
        });

        let built = runtime.build(loaded.config, None)?;
        *runtime.current.write() = Arc::new(built);
        // Last, and that ordering is the whole mechanism: the first build starts
        // the first reconcile without waiting for it — an extension is a child
        // process and a build is synchronous — so the extensions an install has
        // are not loaded yet when the first resolve happens. When they land, the
        // host announces, and this puts their tools in the registry and rebuilds
        // so their provider types and prompt sections are resolved too.
        let _ = hook.set(Arc::downgrade(&runtime));
        Ok(runtime)
    }

    /// The current settings tree. Replaced wholesale by a reconfigure.
    pub fn config(&self) -> Config {
        self.current.read().config.clone()
    }

    /// Resolved against the config, so a patched workspace moves it.
    pub fn paths(&self) -> GhostPaths {
        self.current.read().paths.clone()
    }

    /// Where the sandbox service listens for this install.
    fn sandbox_socket(&self) -> std::path::PathBuf {
        ghostai_environment::service::socket_path(
            self.env.get("GHOSTAI_SANDBOX_SOCKET").map(String::as_str),
            &self.paths(),
        )
    }

    /// Management calls use the same service transport as container tools.
    ///
    /// `execute` is refused here rather than filtered at the route: a model's
    /// tool call reaches the service through the agent loop, which supplies the
    /// digest it resolved the toolbox at. An `execute` arriving as a
    /// management call has no such provenance, whatever it claims.
    pub async fn sandbox_request(&self, value: serde_json::Value) -> Result<serde_json::Value> {
        let request: SandboxRequest = serde_json::from_value(value)
            .map_err(|e| GhostError::new(ErrorKind::InvalidInput, e.to_string()))?;
        if matches!(request, SandboxRequest::Execute { .. }) {
            return Err(GhostError::new(
                ErrorKind::PermissionDenied,
                "Tool execution is not a management operation",
            ));
        }
        ghostai_environment::service::SandboxClient::new(self.sandbox_socket())
            .request(request, &tokio_util::sync::CancellationToken::new())
            .await
    }

    /// The config file that was read, or would have been.
    pub fn file(&self) -> &std::path::Path {
        &self.file
    }

    /// Where the conversation lives.
    pub fn store(&self) -> &Arc<SessionStore> {
        &self.store
    }

    /// The registry: listing, naming, moving and detaching. Never a path.
    pub fn workspaces(&self) -> &Arc<WorkspaceStore> {
        &self.workspaces
    }

    /// The shared tool registry.
    pub fn tools(&self) -> &Arc<ToolRegistry> {
        &self.tools
    }

    /// The queue every loop drains.
    pub fn steering(&self) -> &Arc<SteeringQueue> {
        &self.steering
    }

    /// The default workspace's jail — the banner, and the file routes' fallback.
    pub fn jail(&self) -> Arc<WorkspaceJail> {
        self.current.read().jails.default_jail()
    }

    /// Every workspace's jail, keyed by id. Rebuilt only when the root moves.
    pub fn jails(&self) -> Arc<dyn JailResolver> {
        Arc::clone(&self.current.read().jails) as Arc<dyn JailResolver>
    }

    /// Drops the cached jail for one workspace, after its folder has moved.
    ///
    /// A method here rather than on [`JailResolver`]: that trait is the security
    /// crate's, it is what decides whether a path may be touched, and widening
    /// it with a cache operation would put "forget this" in front of every
    /// implementation of a containment boundary.
    pub fn evict_workspace(&self, workspace_id: &str) {
        self.current.read().jails.evict(workspace_id);
    }

    /// Rebuilt by a reconfigure; a running turn keeps the one it started on.
    ///
    /// Absent when nothing is configured. Read it through [`Self::require_loop`]
    /// unless the caller genuinely has something to do with its absence — the
    /// server does, and reports it on the one frame that needs a model rather
    /// than failing every route.
    pub fn agent_loop(&self) -> Option<AgentLoop> {
        self.current.read().agent_loop.clone()
    }

    /// Every agent that can run a turn, the default one first.
    pub fn agents(&self) -> Vec<EffectiveAgent> {
        self.current.read().agents.clone()
    }

    /// What the settings asked for and this build could not honour.
    ///
    /// Served rather than logged because the operator who can fix it is looking
    /// at the settings page, not at the process output — and because these
    /// survive a restart, so a warning nobody surfaced is one nobody ever sees.
    pub fn config_warnings(&self) -> Vec<AgentConfigWarning> {
        self.current.read().warnings.clone()
    }

    /// The endpoint a turn would use, or absent on an unconfigured install.
    pub fn instance(&self) -> Option<ProviderInstance> {
        self.current.read().instance.clone()
    }

    /// The provider type behind the instance. Derived, absent for the same
    /// reason.
    pub fn spec(&self) -> Option<ProviderSpec> {
        self.current
            .read()
            .instance
            .as_ref()
            .map(|instance| instance.spec.clone())
    }

    /// Empty when no model is configured.
    pub fn model(&self) -> String {
        self.current.read().model.clone()
    }

    /// Whether a turn can run at all: a provider and a model both resolved.
    pub fn configured(&self) -> bool {
        self.current.read().agent_loop.is_some()
    }

    /// Whether a credential was found, without saying what it was.
    pub fn has_credential(&self) -> bool {
        self.current.read().has_credential
    }

    /// The loop, or the `config` error explaining what is missing.
    ///
    /// The refusal belongs here rather than at construction — this is the one
    /// call that cannot proceed without an answer. A terminal turn gets the
    /// message; a server gets to start.
    pub fn require_loop(&self) -> Result<AgentLoop> {
        self.require_loop_for(None)
    }

    /// The loop for one agent, built on first use and cached.
    ///
    /// `None` is the default agent, which is what a turn from a session nobody
    /// has bound carries. Fails for an id that names nothing runnable — ask
    /// [`crate::has_agent`] first if the id came off the wire.
    pub fn loop_for(&self, agent_id: Option<&str>) -> Result<Option<AgentLoop>> {
        let current = Arc::clone(&self.current.read());
        match agent_id {
            None | Some("" | DEFAULT_AGENT_ID) => Ok(current.agent_loop.clone()),
            // Fails for an unknown or disabled id, which is the caller's to
            // catch — the hub turns it into an error on one frame rather than a
            // dead socket.
            Some(id) => current.loops.get(id),
        }
    }

    /// [`Self::loop_for`], with the refusal [`Self::require_loop`] makes.
    pub fn require_loop_for(&self, agent_id: Option<&str>) -> Result<AgentLoop> {
        if let Some(agent_loop) = self.loop_for(agent_id)? {
            return Ok(agent_loop);
        }
        let current = Arc::clone(&self.current.read());
        let (kind, message) = current
            .unconfigured
            .clone()
            .unwrap_or_else(|| no_provider_error(&self.file));
        Err(GhostError::new(kind, message))
    }

    /// One agent's resolved provider and model, for a request that is **not** a
    /// turn.
    ///
    /// Deliberately narrow, and worth naming its one caller: the heartbeat's
    /// forced skip-or-run decision, which is a single request carrying one tool
    /// and no history. Everything a turn gets — the tool registry, the approval
    /// gate, history windowing, the turn-stats row — is bypassed here, which is
    /// correct for a classification and wrong for anything that looks like work.
    /// A second caller wanting "just one completion" is a sign it wants a turn.
    ///
    /// `model` overrides what the agent would otherwise use, which is how a
    /// heartbeat gets to run on a cheaper model than the agent's.
    pub fn provider_for(
        &self,
        agent_id: Option<&str>,
        model: Option<&str>,
    ) -> Result<Option<(Arc<dyn ChatProvider>, String)>> {
        let current = Arc::clone(&self.current.read());
        let mut agent = resolve_agent(&current.config, agent_id.or(Some(DEFAULT_AGENT_ID)))?;
        if let Some(model) = model.filter(|model| !model.is_empty()) {
            model.clone_into(&mut agent.settings.model);
        }
        let resolved = self.resolve_provider(&current.config, &agent, &current.paths)?;
        Ok(resolved.provider.map(|provider| (provider, resolved.model)))
    }

    /// Every configured MCP server and the state it is actually in.
    ///
    /// Live rather than configured, which is why it is a method here and not a
    /// corner of the config: "unreachable since 12:04" is not something to write
    /// into `config.yaml`. Empty when the client is switched off.
    pub fn mcp_servers(&self) -> Vec<McpServerStatus> {
        self.mcp
            .as_ref()
            .map_or_else(Vec::new, McpManager::statuses)
    }

    /// The extension host, or absent when this build has none.
    ///
    /// Exposed rather than kept private because two callers above this layer
    /// need it and neither belongs here: `ghostai serve` collects the channel
    /// factories extensions contributed, and the extensions route reports their
    /// status. Both are read-only uses of it — loading is this type's job.
    pub fn extensions(&self) -> Option<&Arc<ExtensionHost>> {
        self.extensions.as_ref()
    }

    /// Re-reads the extensions directory and applies what changed.
    ///
    /// The path an *approval* takes, which a settings save does not: approving
    /// is a row in a table rather than an edit to `config.yaml`, so nothing else
    /// here would notice it.
    pub fn reload_extensions(&self) {
        let current = Arc::clone(&self.current.read());
        if let Some(host) = &self.extensions {
            host.reconcile(&current.config.extensions);
        }
    }

    /// Merge a patch, heal what it orphaned, and rebuild — or change nothing.
    ///
    /// The order matters and each step earns its place:
    ///
    ///  1. **merge** — the generic tree merge, which knows nothing about agents.
    ///  2. **[`assert_writable_agent_ids`]** — refuses an id nothing downstream
    ///     could use, comparing against the current config so an odd key already
    ///     on disk stays deletable.
    ///  3. **[`prune_dangling_subagents`]** — strips delegations to agents this
    ///     patch just deleted. The *pruned* config is what this returns, and
    ///     callers save the return value, so the file is written already healed.
    ///  4. **build** — where an unbuildable agent is still refused outright.
    ///
    /// All-or-nothing throughout: any failure leaves the runtime exactly as it
    /// was.
    ///
    /// Returns the merged config, which is what a caller persists to
    /// `config.yaml` — the runtime deliberately does not write it, because
    /// previewing a patch and saving one are different operations.
    pub fn reconfigure(self: &Arc<Self>, patch: &serde_json::Value) -> Result<Config> {
        let previous = Arc::clone(&self.current.read());
        let merged = merge_config_patch(&previous.config, patch)?;
        assert_writable_agent_ids(&previous.config, &merged)?;
        let (next, _) = prune_dangling_subagents(&merged);
        let built = self.build(next.clone(), Some(&previous))?;
        *self.current.write() = Arc::new(built);
        Ok(next)
    }

    /// [`Self::reconfigure`] from the typed patch a route validated.
    ///
    /// The merge reads raw JSON because `null` and absent mean opposite things
    /// at a handful of paths and an `Option<T>` field collapses them. A typed
    /// patch models exactly the paths where a `null` *deletes* — a provider
    /// instance, an MCP server, an agent — as `Option<Option<_>>`, so those
    /// survive this round trip; a `null` anywhere else was never a deletion and
    /// is a value the re-parse would have refused.
    pub fn apply_patch(self: &Arc<Self>, patch: &ConfigPatch) -> Result<Config> {
        let value = serde_json::to_value(patch).map_err(|error| {
            GhostError::new(
                ErrorKind::InvalidInput,
                "The settings patch could not be represented as JSON.",
            )
            .with_source(error)
        })?;
        self.reconfigure(&value)
    }

    /// Re-reads `config.yaml` and rebuilds everything derived from it.
    ///
    /// The counterpart to [`Self::reconfigure`], and the difference is where the
    /// settings come from: a patch is what a client just sent, and this is what
    /// the file says now. It is for the edits a running server cannot see — a
    /// config hand-edited in an editor, an extension dropped into the directory,
    /// an MCP server whose command changed — which otherwise wait for a restart.
    ///
    /// The whole file, not a merge over what is in memory. A settings save that
    /// was rolled back by hand has to actually come back, and a merge would keep
    /// the value that is no longer written anywhere.
    ///
    /// Same failure contract: a file that cannot be built changes nothing.
    pub fn reload(self: &Arc<Self>) -> Result<Config> {
        // The constructor's own arguments, so a runtime built against a home or
        // a workspace override re-reads the same file it was built from rather
        // than whichever one the environment happens to name now.
        let loaded = load_config(LoadConfigOptions {
            paths: ResolveGhostPaths {
                root: self.options.home.clone(),
                workspace: self.options.workspace.clone(),
                env: Some(self.env.clone()),
                home: None,
            },
            file: None,
        })?;
        let previous = Arc::clone(&self.current.read());
        let built = self.build(loaded.config.clone(), Some(&previous))?;
        *self.current.write() = Arc::new(built);
        Ok(loaded.config)
    }

    /// Stops the connections this runtime owns.
    ///
    /// No containers: this process holds no engine handle. The sandbox service
    /// owns every container's lifetime and reaps its own on shutdown, which is
    /// what lets a warm shared instance outlive one app restart.
    ///
    /// The database is not closed here: it is a shared handle, and whoever
    /// opened it — possibly the server, sharing one WAL with the auth store and
    /// the scheduler — decides when the last reference goes. That is the same
    /// contract a borrowed connection had, arrived at by ownership rather than
    /// by a flag.
    pub async fn close(&self) {
        if let Some(host) = &self.extensions {
            host.stop().await;
        }
        if let Some(mcp) = &self.mcp {
            mcp.close().await;
        }
        if self.owns_providers {
            self.providers.clear();
        }
    }

    /// Config in, everything derived from it out.
    ///
    /// Ordered so that everything able to fail happens before anything mutates:
    /// an unusable workspace fails while the tool registry still holds the
    /// built-ins that were working a moment ago.
    ///
    /// A missing provider or model is *not* one of those failures. It produces a
    /// runtime with no loop, because everything else this builds — the jails,
    /// the tool registry, the paths — is useful without a model, and refusing to
    /// build them would make an unconfigured install unserveable.
    fn build(self: &Arc<Self>, config: Config, previous: Option<&Resolved>) -> Result<Resolved> {
        // Every enabled agent, resolved. This is where an entry naming a sandbox
        // with no backend, or settings that will not merge, fails — before
        // anything below mutates, so a bad save leaves the runtime exactly as it
        // was. Only the *default* agent's loop is constructed here; the rest are
        // built on first use, because an install with six agents and one in use
        // should not open six provider connections at boot.
        let (agents, mut warnings) = resolve_agents(&config)?;
        let paths = paths_for(&config, &self.options)?;

        let default_agent = resolve_agent(&config, Some(DEFAULT_AGENT_ID))?;
        let resolved = self.resolve_provider(&config, &default_agent, &paths)?;

        // A jail canonicalises its root and creates it, so keeping the cache when
        // nothing moved saves that work on every workspace already in use. A
        // moved root invalidates all of them at once: every cached jail was
        // derived from the old one.
        //
        // The cache builds the default in its constructor, so an unusable
        // workspace fails *here* — before any of the mutations below — which is
        // what keeps a reconfigure all-or-nothing.
        let jails = match previous {
            Some(previous) if previous.paths.workspace == paths.workspace => {
                Arc::clone(&previous.jails)
            }
            _ => Arc::new(JailCache::new(paths.clone())?),
        };

        // Before the mutations below, because every failure it can produce — an
        // toolbox that is not installed, a manifest that does not parse, a network
        // request above its ceiling — must leave the runtime serving on the
        // settings that worked a moment ago.
        let built = Self::resolve_policies(&agents, &paths)?;

        // Here rather than in `resolve_agents`, because only now is the full set
        // of names an agent can advertise known: the toolbox's own programs are
        // merged over its map when the loop is built, and warning without them
        // would fire on every override a toolboxed agent has.
        for agent in &agents {
            let mut advertised: Vec<String> = built
                .permissions
                .get(&agent.id)
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default();
            advertised.extend(agent.tools.keys().cloned());
            advertised.extend(
                agent
                    .subagents
                    .iter()
                    .map(|binding| binding.tool_name.clone()),
            );
            warnings.extend(tool_prompt_warnings(agent, &advertised));
        }

        // Past here nothing fails, so the mutations below cannot leave the
        // registry describing a runtime that failed to build.
        self.tools
            .set_timeout_ms(default_agent.settings.tool_timeout_ms);
        // Exact by source: an `exec` switched off in the settings panel has to
        // disappear from the definitions the model sees, and MCP and extension
        // tools registered on this same registry must survive that.
        self.tools.unregister_by_source(ToolSource::Builtin);
        // A disabled scheduler drops `automation` for the same reason a disabled
        // `exec` drops `exec`: an install with no scheduler should not advertise
        // a way to schedule, and a tool that can only answer "this installation
        // has no scheduler" costs a turn to learn what its absence would have
        // said for free.
        if self.options.tools {
            register_builtins(
                &self.tools,
                Some(&config.tools),
                BuiltinOptions {
                    scheduler: config.scheduler.enabled,
                },
            )?;
        }

        // Here, in the region that cannot fail, because that is the whole
        // contract: reconcile is synchronous and infallible, every dial happens
        // on a background task, and an unreachable server becomes a status row
        // rather than a save the operator loses. The same stance an unconfigured
        // provider already has — see the header.
        if let Some(mcp) = &self.mcp {
            mcp.reconcile(&config.tools.mcp_servers);
        }
        if let Some(host) = &self.extensions {
            // Skipped when this build *is* the answer to a reconcile: the one
            // that woke `on_extensions_changed` finished a moment ago, so running
            // another would at best repeat it. Reconcile is idempotent, so this
            // is not what stops the loop — it is the second lock on a door.
            if !self.rebuilding.load(Ordering::SeqCst) {
                host.reconcile(&config.extensions);
            }
            // Whatever is already loaded reaches the shared registry now; an
            // extension that finishes starting later announces through the
            // host's own listener.
            self.apply_extension_tools(host);
        }

        // A fresh cache per build: every loop in the old one was derived from the
        // settings that just changed. A turn already running keeps the loop it
        // started on, because it holds the object rather than looking it up
        // again.
        let (factory, cache_resolver) = self.loop_factory(
            config.clone(),
            paths.clone(),
            Arc::clone(&jails),
            Arc::new(built.prompts.clone()),
        );
        let loops = Arc::new(LoopCache::new(factory));
        cache_resolver.bind(&loops);
        let agent_loop = loops.get(DEFAULT_AGENT_ID)?;

        Ok(Resolved {
            config,
            agents,
            warnings,
            paths,
            jails,
            loops,
            agent_loop,
            instance: resolved.instance,
            model: resolved.model,
            has_credential: resolved.has_credential,
            unconfigured: resolved.unconfigured,
        })
    }

    /// What to do when the set of loaded extensions moves.
    ///
    /// Two halves, and they answer different questions. The sink is the one this
    /// layer owes the registry — per extension id rather than by source, because
    /// clearing the source would take every *other* extension's tools with it.
    ///
    /// The rebuild is the one nothing else would do. A provider spec and a
    /// prompt section are read *during* a build, not looked up per turn the way
    /// a tool is, so an extension that loaded after the last build would
    /// contribute neither until something else happened to trigger one. A failed
    /// rebuild is logged and dropped: the runtime keeps serving what it was
    /// serving, which is the same contract a reconfigure has.
    fn on_extensions_changed(self: &Arc<Self>) {
        let Some(host) = self.extensions.clone() else {
            return;
        };
        self.apply_extension_tools(&host);

        // Guarded, because a build starts a reconcile of its own: an
        // announcement from inside a rebuild would recurse. Reconcile is a no-op
        // when nothing changed, so the recursion is shallow rather than
        // infinite — the guard makes it zero.
        if self.rebuilding.swap(true, Ordering::SeqCst) {
            return;
        }
        let previous = Arc::clone(&self.current.read());
        match self.build(previous.config.clone(), Some(&previous)) {
            Ok(built) => *self.current.write() = Arc::new(built),
            Err(error) => tracing::warn!(
                error = %error.message,
                "settings could not be rebuilt after an extension changed"
            ),
        }
        self.rebuilding.store(false, Ordering::SeqCst);
    }

    /// Puts every loaded extension's tools in the shared registry.
    ///
    /// Per extension id rather than by source, because clearing the `extension`
    /// source would take every *other* extension's tools with it — the same
    /// grain problem one MCP server reconnecting has.
    fn apply_extension_tools(&self, host: &ExtensionHost) {
        let tools = host.tools();
        for status in host.status() {
            let mine: Vec<AnyTool> = tools
                .iter()
                .filter(|tool| status.tools.contains(&tool.definition().name))
                .cloned()
                .collect();
            let rejected = self.extension_sink.replace(&status.id, mine);
            if !rejected.is_empty() {
                tracing::warn!(
                    extension = %status.id,
                    rejected = ?rejected,
                    "extension tools could not be registered"
                );
            }
        }
    }
}

/// What one provider resolution answered.
struct ResolvedProvider {
    provider: Option<Arc<dyn ChatProvider>>,
    instance: Option<ProviderInstance>,
    model: String,
    has_credential: bool,
    unconfigured: Option<(ErrorKind, String)>,
}

impl GhostRuntime {
    /// Every provider type resolution may see: the table, plus extensions'.
    ///
    /// The built-ins come first, so an extension cannot shadow `ollama` by
    /// declaring a spec with that id — the first match wins. An extension's
    /// provider id is namespaced to it anyway, which makes the collision
    /// unreachable rather than merely losable; the ordering is the belt.
    fn provider_specs(&self) -> Vec<ProviderSpec> {
        let mut specs = PROVIDERS.to_vec();
        if let Some(host) = &self.extensions {
            specs.extend(host.providers());
        }
        specs
    }

    /// One agent's endpoint, model and credential.
    ///
    /// Split out of the build because two callers need it and they must not
    /// answer differently: the default agent, whose answer *is* the runtime's
    /// `configured`/`model`/`instance`, and every other agent on first use.
    ///
    /// A construction-time provider or model pin wins for every agent, not just
    /// the default. `ghostai chat --model x` is a statement about this process,
    /// and an agent that quietly ignored it would be the more surprising rule.
    fn resolve_provider(
        &self,
        config: &Config,
        agent: &EffectiveAgent,
        paths: &GhostPaths,
    ) -> Result<ResolvedProvider> {
        let model = self
            .options
            .model
            .clone()
            .unwrap_or_else(|| agent.settings.model.clone());
        let provider_id = self
            .options
            .provider
            .clone()
            .or_else(|| Some(agent.settings.provider.clone()));

        let specs = self.provider_specs();
        let has_credential = |id: &str| self.has_stored_credential(config, id);
        // The full spec list rather than the built-in table alone, so a
        // provider's `type` can name one this build did not ship. An extension
        // that failed to load contributes none, which is what makes a config
        // referring to its provider read as "unconfigured" rather than crashing
        // the resolve.
        let instance = resolve_instance(&ResolveInstanceOptions {
            providers: &config.providers,
            provider: provider_id.as_deref(),
            model: Some(&model),
            has_credential: Some(&has_credential),
            specs: Some(&specs),
        })
        .or_else(|| instance_from_env(&self.env));

        let unconfigured = match &instance {
            None => Some(no_provider_error(&self.file)),
            Some(instance) if model.is_empty() => Some(no_model_error(instance, &self.file)),
            Some(_) => None,
        };

        // Re-read on every build rather than cached: a key saved in the settings
        // UI has to be usable on the next turn, and the vault is the store it
        // landed in.
        let api_key = match &instance {
            None => None,
            Some(instance) => find_credential(instance, paths, &self.env, &self.options.vault)?,
        };

        let provider = match (&instance, &unconfigured) {
            (Some(instance), None) => {
                let connection = resolve_connection(&instance.spec, Some(&instance.config));
                Some(self.providers.get(&ProviderRequest {
                    instance_id: instance.id.clone(),
                    spec: instance.spec.clone(),
                    model: model.clone(),
                    api_base: connection.api_base,
                    extra_headers: connection.extra_headers,
                    api_key: api_key.clone(),
                    wires: None,
                })?)
            }
            _ => None,
        };

        Ok(ResolvedProvider {
            provider,
            instance,
            model,
            has_credential: api_key.is_some(),
            unconfigured,
        })
    }

    /// Whether an instance holds a credential, for the `auto` tie-break only.
    ///
    /// Reads the vault at most once per build and never fails: resolution is
    /// choosing between endpoints, and a vault that will not open is a problem
    /// for the chosen one to report — with the message that names it — rather
    /// than a reason to fail before anything has been chosen.
    fn has_stored_credential(&self, config: &Config, instance_id: &str) -> bool {
        let Some(entry) = config.providers.get(instance_id) else {
            return false;
        };
        let env_key = PROVIDERS
            .iter()
            .find(|spec| spec.id == entry.kind)
            .and_then(|spec| spec.env_key.clone());
        if let Some(key) = env_key
            && self.env.get(&key).is_some_and(|value| !value.is_empty())
        {
            return true;
        }

        match &self.options.vault {
            VaultChoice::None => false,
            VaultChoice::Given(vault) => {
                vault.lock().has(PROVIDER_CREDENTIAL_NAMESPACE, instance_id)
            }
            VaultChoice::Default => {
                let Ok(paths) = paths_for(config, &self.options) else {
                    return false;
                };
                if !paths.vault_file.exists() {
                    return false;
                }
                open_vault(&paths)
                    .is_ok_and(|vault| vault.has(PROVIDER_CREDENTIAL_NAMESPACE, instance_id))
            }
        }
    }

    /// Resolve every toolbox and container an enabled agent names.
    ///
    /// Nothing here reaches a container engine, and that is the point of the
    /// split rather than an optimisation: whether a definition is installed,
    /// installed and internally coherent is static config and belongs in an
    /// all-or-nothing rebuild, while whether an engine is *running* changes
    /// while the server is up. The sandbox service answers the second, on the
    /// first command that needs one.
    fn resolve_policies(agents: &[EffectiveAgent], paths: &GhostPaths) -> Result<BuiltPolicies> {
        let named: Vec<&EffectiveAgent> = agents
            .iter()
            .filter(|agent| !agent.toolbox.name.is_empty() || !agent.container.name.is_empty())
            .collect();
        if named.is_empty() {
            return Ok(BuiltPolicies {
                prompts: IndexMap::new(),
                permissions: IndexMap::new(),
            });
        }

        let policies = PolicyStore::new(paths.policy_dir.clone());

        // Every agent resolved *here*, so a toolbox that is not installed, a
        // definition that does not parse, or an egress request nothing could enforce is a
        // refusal on the save rather than a turn that dies on its first command.
        // The prompt sections fall out of the same pass, which is why this is not
        // two walks.
        let mut prompts = IndexMap::new();
        let mut permissions = IndexMap::new();
        for agent in named {
            let mut workdir = String::new();
            if !agent.container.name.is_empty() {
                let container = policies.require_container(&agent.container.name)?;
                // Whether a *restricted* allow-list can be enforced in this
                // container depends on its uid, its privileges and its
                // capabilities. Checked on the save, where the operator can
                // change either half, rather than at the first command.
                if agent.container.network.mode == NetworkMode::Allowlist {
                    assert_gateway_compatible(&container.definition)?;
                }
                workdir.clone_from(&container.definition.workdir);
            }
            if agent.toolbox.name.is_empty() {
                continue;
            }
            let installed = policies.require_toolbox(&agent.toolbox.name)?;
            let resolved = resolved_permissions(&installed, &agent.toolbox.tools);
            prompts.insert(
                agent.id.clone(),
                PromptToolbox {
                    name: installed.resolved.toolbox.name.clone(),
                    workdir,
                    // Resolved against the same overrides the permission map is,
                    // so the prose and the tool schemas cannot list different
                    // operations. An agent given four of a toolbox's twenty-four
                    // must not be told it has the other twenty.
                    tools: installed
                        .resolved
                        .toolbox
                        .tools
                        .iter()
                        .filter(|grant| resolved.get(&grant.name) != Some(&ToolPermission::Deny))
                        .map(|grant| PromptToolboxTool {
                            name: grant.name.clone(),
                            use_for: installed
                                .resolved
                                .operations
                                .get(&grant.name)
                                .map(|operation| operation.description.clone())
                                .unwrap_or_default(),
                        })
                        .collect(),
                    notes: installed.resolved.toolbox.notes.clone(),
                },
            );
            permissions.insert(agent.id.clone(), resolved);
        }

        Ok(BuiltPolicies {
            prompts,
            permissions,
        })
    }
}

/// Each grant's permission after the agent's own map has tightened it.
fn resolved_permissions(
    installed: &InstalledToolbox,
    overrides: &ToolPermissions,
) -> ToolPermissions {
    installed
        .resolved
        .toolbox
        .tools
        .iter()
        .map(|grant| {
            let requested = overrides
                .get(&grant.name)
                .or_else(|| overrides.get(TOOLBOX_DEFAULT_KEY));
            let permission = requested.map_or(grant.permission, |requested| {
                narrow_permission(grant.permission, *requested)
            });
            (grant.name.clone(), permission)
        })
        .collect()
}

/// Resolves a subagent's loop through the cache that built its parent.
///
/// A resolver rather than a map, because loops are built lazily: handing one a
/// set of them would build every subagent's provider whether or not it was ever
/// used.
struct CacheResolver {
    /// Weak, because the cache owns the factory that owns this resolver: a
    /// strong handle here would be a cycle that never frees.
    loops: Mutex<Option<std::sync::Weak<LoopCache>>>,
}

impl CacheResolver {
    /// Points the resolver at the cache built from its own factory.
    fn bind(&self, loops: &Arc<LoopCache>) {
        *self.loops.lock() = Some(Arc::downgrade(loops));
    }
}

impl LoopResolver for CacheResolver {
    fn loop_for(&self, agent_id: &str) -> Option<AgentLoop> {
        let cache = self.loops.lock().as_ref()?.upgrade()?;
        // A subagent that cannot run is a refusal the model is told about, so a
        // failure here reads the same as an absence: the delegation tool says so
        // rather than unwinding the parent's turn.
        cache.get(agent_id).ok().flatten()
    }
}

impl GhostRuntime {
    /// The factory a [`LoopCache`] is built from.
    ///
    /// Split out because the cache and the resolver refer to each other: a
    /// loop's subagents resolve through the same cache that built it, and the
    /// resolver is filled in once the cache exists.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one call site; every argument is a distinct product of the same build"
    )]
    fn loop_factory(
        self: &Arc<Self>,
        config: Config,
        paths: GhostPaths,
        jails: Arc<JailCache>,
        prompts: Arc<IndexMap<String, PromptToolbox>>,
    ) -> (crate::loop_cache::LoopFactory, Arc<CacheResolver>) {
        let resolver = Arc::new(CacheResolver {
            loops: Mutex::new(None),
        });
        // Weak, because the cache this factory is handed to is held by the
        // runtime: a strong handle would be a cycle the process never frees.
        let runtime = Arc::downgrade(self);
        let bound = Arc::clone(&resolver);
        let factory: crate::loop_cache::LoopFactory = Arc::new(move |agent_id: &str| {
            let Some(runtime) = runtime.upgrade() else {
                return Ok(None);
            };
            runtime.create_loop(
                &config,
                agent_id,
                &paths,
                &jails,
                &prompts,
                Arc::clone(&bound) as Arc<dyn LoopResolver>,
            )
        });
        (factory, resolver)
    }
}

impl GhostRuntime {
    /// The loop for one agent, or `None` when nothing can run a turn.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one call site; every argument is a distinct product of the same build"
    )]
    fn create_loop(
        self: &Arc<Self>,
        config: &Config,
        agent_id: &str,
        paths: &GhostPaths,
        jails: &Arc<JailCache>,
        prompts: &IndexMap<String, PromptToolbox>,
        resolver: Arc<dyn LoopResolver>,
    ) -> Result<Option<AgentLoop>> {
        let agent = resolve_agent(config, Some(agent_id))?;
        let endpoint = self.resolve_provider(config, &agent, paths)?;
        let Some(provider) = endpoint.provider else {
            return Ok(None);
        };

        // Two shapes, and which one an agent gets is decided by whether it
        // names a toolbox:
        //
        //  - **No toolbox** is the built-in scope narrowed by `agent.tools`:
        //    `read_file`, `exec` and whatever MCP servers and extensions
        //    registered, gated by the agent's own map.
        //  - **A toolbox** is a *complete* scope of its granted operations and
        //    nothing else. No `exec` to reach a program the toolbox did not
        //    grant, no ambient MCP tool the operator did not name. A toolbox
        //    that wants a built-in back grants it explicitly as a `registered`
        //    operation, pinned to that tool's definition digest.
        //
        // A view of the one shared registry rather than a registry of its own in
        // both cases: an MCP server is one connection however many agents are
        // configured.
        let mut containerized = None;
        let scope = if agent.toolbox.name.is_empty() {
            self.tools.select(agent.tools.clone())
        } else {
            let store = Arc::new(PolicyStore::new(paths.policy_dir.clone()));
            let installed = store.require_toolbox(&agent.toolbox.name)?;
            containerized = Some(!agent.container.name.is_empty());
            // A command operation runs in a container by being *sent* to the
            // service that owns one. Without a container the same operation runs
            // here, as a guarded child process in the workspace jail — which is
            // why the executor is `None` rather than a local implementation of
            // the same trait.
            let remote = if agent.container.name.is_empty() {
                None
            } else {
                store.require_container(&agent.container.name)?;
                Some(Arc::new(ghostai_environment::service::SandboxClient::new(
                    self.sandbox_socket(),
                ))
                    as Arc<dyn ghostai_tools::operations::OperationExecutor>)
            };
            ghostai_tools::operations::toolbox_operation_scope(
                &installed,
                &store,
                &self.tools,
                &agent.toolbox.tools,
                remote.as_ref(),
            )?
        };

        let mut options = AgentLoopOptions::new(
            provider,
            scope,
            Arc::clone(&self.store),
            Arc::clone(jails) as Arc<dyn JailResolver>,
        );
        options.model = Some(endpoint.model);
        options.config = agent.settings.clone();
        options.tools_config = Arc::new(agent.tools_config.clone());
        options.toolbox = agent.toolbox.clone();
        options.container = agent.container.clone();
        options.toolbox_prompt = prompts.get(&agent.id).cloned();
        // The delegation half of what this agent may do. Beside the tools and
        // for the same reason: both are resolved once, here, so a turn never asks
        // the config who it is allowed to call.
        options.subagents = subagent_map(agent.subagents.clone())?;
        options.resolve_loop = Some(resolver);
        options.steering = Arc::clone(&self.steering);
        options.clock = Arc::clone(&self.clock);
        options.random = Arc::new(OsRandom);
        options.env = Arc::new(self.env.clone());
        options.host = Host::default();
        options.approvals.clone_from(&self.options.approvals);
        options.automation.clone_from(&self.options.automation);
        // Read per turn, so the clock the model is given is the same one the
        // scheduler reads cron expressions against and the UI renders timestamps
        // in — one zone, and no conversion asked of anybody. Read off the *live*
        // settings rather than a snapshot, so an operator who changes the zone
        // does not have to restart to be believed.
        let zone_source = Arc::downgrade(self);
        let fallback = config.ui.timezone.clone();
        options.time_zone = Some(Arc::new(move || {
            zone_source
                .upgrade()
                .map_or_else(|| fallback.clone(), |runtime| runtime.config().ui.timezone)
        }));
        options.agent = Some(LoopAgent {
            prompt: PromptAgent {
                label: agent.label.clone(),
                live_prompt: Some(agent.live_prompt.clone()),
                wrap_up_prompt: Some(agent.wrap_up_prompt.clone()),
                prompt_mode: Some(agent.prompt_mode),
                system_prompt: agent.system_prompt.clone(),
            },
            id: agent.id.clone(),
            tool_prompts: Some(agent.tool_prompts.clone()),
            platform_prompt: Some(match containerized {
                Some(true) if agent.platform_prompt.is_empty() => "## Tool execution\n\nOnly the advertised toolbox operations are callable. Command operations run in the tool container; registered tools run in the app or their configured provider. File tools, when granted, use the workspace jail. Do not assume a shell or generic exec operation is available.".into(),
                Some(false) if agent.platform_prompt.is_empty() => "## Tool execution\n\nOnly the advertised toolbox operations are callable. They run in the app environment or their configured provider, without toolbox container isolation. File tools, when granted, use the workspace jail. Do not assume a shell or generic exec operation is available.".into(),
                _ => agent.platform_prompt.clone(),
            }),
            toolbox_prompt: Some(if containerized.is_some() && agent.toolbox_prompt.is_empty() {
                "## Toolbox: {{name}}\n\nUse only the operations listed in your tools. Their schemas and permission ceilings are fixed by the operator.{{tools}}{{notes}}".into()
            } else { agent.toolbox_prompt.clone() }),
            tool_policy_prompt: Some(agent.tool_policy_prompt.clone()),
        });

        // Which sources may write into the prompt is a composition decision, not
        // the loop's — the loop composes and caches sections and deliberately
        // knows nothing about where one came from. Each contributor is stateless
        // and reads the workspace named by each turn's context, so one instance
        // per loop serves every session on this agent.
        //
        // Each is gated on its tool's permission, which is the feature's only
        // switch: denying `memory` has to remove the section as well as the tool,
        // or an agent that cannot write its memory would still be paying for it
        // in every prompt. A second flag beside the permission map would be a way
        // for the two to disagree.
        //
        // `tools_enabled` gates both on top of that, and it is the broader of the
        // two conditions: off, the request advertises no tools at all, so there
        // is nothing to open a memory or a skill *with*. Both sections are an
        // index of paths plus prose telling the model to read one — handed to a
        // model that cannot call `read_file`, the index is unusable and the prose
        // is false.
        let mut contributors: Vec<Arc<dyn ContextContributor>> = Vec::new();
        if agent.settings.tools_enabled && granted(&agent.tools, "skill") {
            contributors.push(Arc::new(
                SkillsContributor::new(agent.id.clone()).with_template(agent.skills_prompt.clone()),
            ));
        }
        // After skills: sections are appended in order, so the cached prefix
        // grows at the end, and memory is the one a turn can rewrite.
        if agent.settings.tools_enabled && granted(&agent.tools, "memory") {
            contributors.push(Arc::new(
                MemoryContributor::new().with_template(agent.memory_prompt.clone()),
            ));
        }
        // Last, and ungated. Last for the reason memory follows skills: sections
        // append in order, so an extension loading or unloading moves only the
        // tail of the cached prefix. Ungated because there is no tool permission
        // to gate on — an extension's section is not paired with a tool the way
        // `skill` and `memory` are, and the switch an operator has for it is the
        // extension itself.
        if let Some(host) = &self.extensions {
            contributors.extend(host.contributors());
        }
        options.contributors = contributors;

        Ok(Some(AgentLoop::new(options)?))
    }
}

/// The MCP client, or nothing.
///
/// The vault is opened lazily and never fatally: a keychain that will not answer
/// is a reason to keep OAuth tokens in memory for this process, not a reason for
/// an install with no MCP server configured to fail to start.
fn build_mcp(
    options: &RuntimeOptions,
    tools: &Arc<ToolRegistry>,
    paths: &GhostPaths,
    clock: &Arc<dyn Clock>,
) -> Option<McpManager> {
    if matches!(options.mcp, McpChoice::Off) {
        return None;
    }
    let vault = optional_vault(options, paths, "mcp oauth tokens will not be persisted");
    let (connect, backoff, callback_port) = match &options.mcp {
        McpChoice::Connector {
            connect,
            backoff,
            callback_port,
        } => (
            Arc::clone(connect),
            backoff.clone().unwrap_or_default(),
            *callback_port,
        ),
        _ => (
            Arc::new(SdkConnector::new(SdkConnectorOptions::default())) as Arc<dyn McpConnector>,
            BackoffOptions::default(),
            None,
        ),
    };
    Some(McpManager::new(McpManagerOptions {
        sink: registry_tool_sink(Arc::clone(tools), ToolSource::Mcp),
        connect,
        clock: Arc::clone(clock),
        random: Arc::new(OsRandom),
        vault,
        backoff,
        on_status_changed: None,
        callback_port,
        http: reqwest::Client::new(),
        endpoint_guard: None,
    }))
}

/// The extension host, or nothing.
///
/// Shares the runtime's database, so the approval rows land in the same WAL as
/// everything else and a future "delete this extension and its settings" can be
/// one transaction.
fn build_extension_host(
    options: &RuntimeOptions,
    database: &Database,
    paths: &GhostPaths,
    clock: &Arc<dyn Clock>,
    hook: &RuntimeHook,
) -> Result<Option<Arc<ExtensionHost>>> {
    let dir = match &options.extensions {
        ExtensionChoice::Off => return Ok(None),
        ExtensionChoice::Dir(dir) => dir.clone(),
        ExtensionChoice::Default => paths.extensions_dir.clone(),
    };
    let store = ExtensionStore::new(database.clone(), dir, Arc::clone(clock))?;
    let woken = Arc::clone(hook);
    let mut host_options =
        ExtensionHostOptions::new(store, paths.root.clone()).with_listener(Arc::new(move || {
            // Empty until the first build has finished, which is what keeps an
            // announcement during construction from reaching a half-built
            // runtime.
            if let Some(runtime) = woken.get().and_then(Weak::upgrade) {
                runtime.on_extensions_changed();
            }
        }));
    // The vault is opened lazily and never fatally, exactly as it is for MCP: a
    // keychain that will not answer means an extension's secret lookup answers
    // nothing, which is a reason for that extension to refuse to start and not a
    // reason for the install to.
    if let Some(vault) = optional_vault(options, paths, "extension secrets are unavailable") {
        host_options = host_options.with_secrets(Arc::new(move |id: &str| {
            vault.lock().get("extensions", id).map(str::to_owned)
        }));
    }
    Ok(Some(Arc::new(ExtensionHost::new(host_options))))
}

/// The vault, when there is one and it opens.
///
/// Never fatal: both callers treat an unopenable vault as a capability they do
/// not have rather than a reason for the install to refuse to start.
fn optional_vault(
    options: &RuntimeOptions,
    paths: &GhostPaths,
    what: &str,
) -> Option<Arc<Mutex<CredentialVault>>> {
    match &options.vault {
        VaultChoice::None => None,
        VaultChoice::Given(vault) => Some(Arc::clone(vault)),
        VaultChoice::Default => match open_vault(paths) {
            Ok(vault) => Some(Arc::new(Mutex::new(vault))),
            Err(error) => {
                tracing::debug!(error = %error.message, "{what}: the vault could not be opened");
                None
            }
        },
    }
}
