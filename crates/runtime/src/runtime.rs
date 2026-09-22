//! The composition root.
//!
//! Every crate below this one takes its collaborators as options and constructs
//! none of them; this is the single place where a config file becomes a
//! provider, a jail, a store, a registry and a loop. That is what keeps the rest
//! of the repo testable without a filesystem — and it is why this module is the
//! only one that touches the vault, the database and the keychain.
//!
//! It lives in its own crate rather than in the CLI because there is more than
//! one consumer: `darkwire chat`, the HTTP server, the scheduler and every
//! channel all need the same wiring, and wiring implemented twice is wiring that
//! differs in exactly the case nobody tested.
//!
//! The decisions here that are not obvious:
//!
//!  - **Provider resolution is `darkwire-providers`' order, not a second one.**
//!    Resolution runs explicit instance → provider type → the `auto` order, and
//!    answers `None` rather than guessing. Exactly one step follows that: a
//!    provider whose `env_key` is set in the environment. An exported credential
//!    is an operator saying which provider they mean, and
//!    `OPENAI_API_KEY=… darkwire chat` should not need a config file to work.
//!    What it will not do is fall back to *some* provider, because a request
//!    landing at an endpoint nobody chose fails as a 401 from somewhere
//!    unexpected.
//!
//!  - **An unconfigured install is a state, not an error.** A runtime with no
//!    resolvable provider, or none with a model, builds anyway: the loop is
//!    absent, `configured` is false, and everything that does not need a model —
//!    the store, the workspaces, the tool registry, every route but the turn —
//!    works. This is what lets `darkwire serve` come up on a bare machine and
//!    serve the settings UI that fixes it; refusing to construct meant the only
//!    cure for a missing config was to hand-write one.
//!    [`WireRuntime::require_loop`] is where the refusal moved to, so a
//!    terminal turn still fails with the same message it always did.
//!
//!  - **A construction-time provider/model override outlives a reconfigure.**
//!    `darkwire chat --model x` is a statement about this process, and a settings
//!    save from a browser must not silently move the terminal session onto
//!    another model. A caller that wants config to drive the model — the server
//!    does — simply passes neither.
//!
//!  - **A reconfigure rebuilds everything derived and keeps everything owned.**
//!    The store, the tool registry and the steering queue survive; the provider,
//!    the jail and the loop are rebuilt. A turn already running keeps the loop it
//!    started on, which is the only coherent answer: its provider request is in
//!    flight and its tool registry entries are already in the model's context.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use darkwire_agent::approval::ApprovalGate;
use darkwire_agent::{
    AgentLoop, AgentLoopOptions, ContextContributor, Host, LoopAgent, LoopResolver,
    MemoryContributor, PromptAgent, SkillsContributor, SteeringQueue, TasksContributor,
    subagent_map,
};
use darkwire_core::config::env_names_workspaces;
use darkwire_core::paths::ResolveWirePaths;
use darkwire_core::{
    Clock, Database, ErrorKind, LoadConfigOptions, Result, SessionStore, SystemClock, WireError,
    WirePaths, WorkspaceStore, load_config,
};
use darkwire_extension_host::{ExtensionHost, ExtensionHostOptions};
use darkwire_mcp::{
    BackoffOptions, McpConnector, McpManager, McpManagerOptions, SdkConnector, SdkConnectorOptions,
};
use darkwire_protocol::{
    Config, ConfigPatch, DEFAULT_AGENT_ID, McpServerStatus, NetworkMode, ProviderConfig,
    SandboxRequest, ToolSource, new_uuid,
};
use darkwire_providers::{
    ChatProvider, PROVIDERS, ProviderInstance, ProviderSpec, ResolveInstanceOptions,
    resolve_connection, resolve_instance,
};
use darkwire_security::{
    CredentialVault, DnsResolver, ExtensionStore, HickoryResolver, JailResolver, OsRandom,
    PolicyStore, RandomSource, WorkspaceJail, assert_gateway_compatible,
};
use darkwire_tools::{
    AnyTool, AutomationResolver, BuiltinOptions, LiveWebResolver, Placed, TOOL_SEARCH_NAME,
    ToolRegistry, ToolRegistryOptions, ToolSink, WebResolver, WebSettings, register_builtins,
};
use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};

use crate::agents::{
    AgentConfigWarning, EffectiveAgent, assert_writable_agent_ids, granted,
    prune_dangling_subagents, resolve_agent, resolve_agents, retired_prompt_warnings,
    tool_prompt_warnings,
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
/// two reasons: an install with nothing in `~/.darkwire/extensions` pays nothing
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
    /// `DARKWIRE_HOME` override.
    pub home: Option<String>,
    /// The folder the workspaces live in. Wins over `DARKWIRE_WORKSPACES` and
    /// the config's `workspaces`, and keeps winning after a patch.
    pub workspaces: Option<String>,
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
    /// Translates DarkWire's view of the workspace into the *daemon's*.
    ///
    /// Identity when DarkWire runs on the host. A containerised DarkWire must
    /// supply this: a bind path is resolved by the daemon, so asking for its own
    /// `/data/workspace` would mount the host's path of that name — silently,
    /// and usually as an empty directory.
    pub host_workspace_path: Option<darkwire_environment::container_pool::HostPathFn>,
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
            workspaces: None,
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
            .field("workspaces", &self.workspaces)
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
    paths: WirePaths,
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
            "No provider could be resolved.\n  Run `darkwire init` to configure one \
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
            "No model configured for {}.\n  Run `darkwire init`, pass --model <model>, or set \
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
fn new_id(clock: Arc<dyn Clock>) -> darkwire_core::session_store::IdSource {
    Box::new(move || {
        let mut random = [0u8; 10];
        OsRandom.fill(&mut random);
        new_uuid(u64::try_from(clock.now_ms()).unwrap_or(0), &random)
    })
}

/// Where a config's paths land.
///
/// The same precedence the loader applies, restated here because a reconfigure
/// has a new config and no file read to hang it off: an explicit folder, then
/// `DARKWIRE_WORKSPACES`, then the config file, then `~/DarkWire/workspaces`.
/// The environment is checked before the config value is folded in, or
/// `WirePaths::resolve` would be handed a `Some` and never look at it.
fn paths_for(config: &Config, options: &RuntimeOptions) -> Result<WirePaths> {
    let configured = config.workspaces.clone();
    let workspaces = options.workspaces.clone().or(
        if configured.is_empty() || env_names_workspaces(options.env.as_ref()) {
            None
        } else {
            Some(configured)
        },
    );
    WirePaths::resolve(ResolveWirePaths {
        root: options.home.clone(),
        workspaces,
        env: options.env.clone(),
        home: None,
    })
}

/// A system resolver built on first use, not at startup.
///
/// Every install pays for construction otherwise, including the ones that never
/// grant a web tool, and it is built inside whatever tokio runtime happens to be
/// current when the process starts rather than the one that will use it. A host
/// with no resolver configuration reports it as a network error on the fetch
/// that needed it, which is recoverable, instead of refusing to start.
#[derive(Default)]
struct LazyDns {
    inner: OnceLock<Option<HickoryResolver>>,
}

impl std::fmt::Debug for LazyDns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyDns")
            .field("built", &self.inner.get().is_some())
            .finish()
    }
}

impl DnsResolver for LazyDns {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<std::net::IpAddr>>> + Send + 'a>,
    > {
        Box::pin(async move {
            let resolver = self.inner.get_or_init(|| HickoryResolver::new().ok());
            match resolver {
                Some(resolver) => resolver.resolve(host).await,
                None => Err(WireError::new(
                    ErrorKind::Network,
                    "This host has no DNS resolver configuration, so no name can be resolved.",
                )),
            }
        })
    }
}

/// Gives the runtime its web layer, once it exists to read config back through.
///
/// Separate from `new` because it is a step with its own reasoning, not because
/// of its length: the per-agent settings it reads are live, while the caps and
/// the cache bound are read once, since a cache cannot change its bound without
/// being thrown away.
fn attach_web(runtime: &Arc<WireRuntime>, config: &Config) {
    let source = Arc::downgrade(runtime);
    let at_start = Arc::new(config.clone());
    let caps = &config.tools.web;
    let _ = runtime.web.set(Arc::new(LiveWebResolver::new(
        Arc::new(move || {
            source.upgrade().map_or_else(
                || Arc::clone(&at_start),
                |runtime| Arc::new(runtime.config()),
            )
        }),
        WebSettings {
            search_provider: caps.search_provider,
            search_url: caps.search_url.clone(),
            user_agent: caps.user_agent.clone(),
            // Seconds in the file, because nobody reasons about a fetch in
            // milliseconds. Milliseconds below, because that is what the guard
            // and the cache take, and one conversion here beats five.
            timeout_ms: caps.timeout_seconds.saturating_mul(1_000),
            read_timeout_ms: caps.read_timeout_seconds.saturating_mul(1_000),
            max_bytes: caps.max_bytes,
            cache_entries: usize::try_from(caps.cache_entries).unwrap_or(0),
            cache_ttl_ms: caps.cache_ttl_seconds.saturating_mul(1_000),
        },
        Arc::new(LazyDns::default()),
        Arc::clone(&runtime.clock),
        Arc::new(OsRandom),
    )) as Arc<dyn WebResolver>);
}

/// The composition root: config in, a running agent out.
pub struct WireRuntime {
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
    /// Survives a reconfigure for the same reason: it owns the page and search
    /// caches, and emptying those on every settings save would be felt exactly
    /// when an operator is retrying something.
    ///
    /// Set once, immediately after the runtime exists, because it reads the
    /// live config back through it. The per-agent settings it reads are live;
    /// the caps and the cache size in `tools.web` are read once here, because a
    /// cache cannot change its bound without being thrown away.
    web: OnceLock<Arc<dyn WebResolver>>,
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
type RuntimeHook = Arc<OnceLock<Weak<WireRuntime>>>;

impl std::fmt::Debug for WireRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let current = self.current.read();
        f.debug_struct("WireRuntime")
            .field("file", &self.file)
            .field("configured", &current.agent_loop.is_some())
            .field("model", &current.model)
            .finish_non_exhaustive()
    }
}

/// Builds a runtime from `options`.
///
/// Fails only on settings that cannot be built at all — an unusable workspace,
/// an agent naming a container that is not installed. A missing provider or model is a
/// *state*: the runtime comes up unconfigured.
pub fn create_runtime(options: RuntimeOptions) -> Result<Arc<WireRuntime>> {
    WireRuntime::new(options)
}

impl WireRuntime {
    /// See [`create_runtime`].
    pub fn new(options: RuntimeOptions) -> Result<Arc<WireRuntime>> {
        let env = options
            .env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        let clock: Arc<dyn Clock> = options
            .clock
            .clone()
            .unwrap_or_else(|| Arc::new(SystemClock));

        let loaded = load_config(LoadConfigOptions {
            paths: ResolveWirePaths {
                root: options.home.clone(),
                workspaces: options.workspaces.clone(),
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
        let runtime = Arc::new(WireRuntime {
            options,
            env,
            clock,
            file: loaded.file,
            store,
            workspaces,
            tools,
            steering: Arc::new(SteeringQueue::new()),
            web: OnceLock::new(),
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

        // Reads the live config back through the runtime, so a settings save
        // moves an agent's provider and user agent without emptying the caches.
        attach_web(&runtime, &loaded.config);

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
    pub fn paths(&self) -> WirePaths {
        self.current.read().paths.clone()
    }

    /// Where the sandbox service listens for this install.
    fn sandbox_socket(&self) -> std::path::PathBuf {
        darkwire_environment::service::socket_path(
            self.env.get("DARKWIRE_SANDBOX_SOCKET").map(String::as_str),
            &self.paths(),
        )
    }

    /// Management calls use the same service transport as container tools.
    pub async fn sandbox_request(&self, value: serde_json::Value) -> Result<serde_json::Value> {
        let request: SandboxRequest = serde_json::from_value(value)
            .map_err(|e| WireError::new(ErrorKind::InvalidInput, e.to_string()))?;
        if matches!(request, SandboxRequest::Exec { .. }) {
            return Err(WireError::new(
                ErrorKind::PermissionDenied,
                "Tool execution is not a management operation",
            ));
        }
        darkwire_environment::service::SandboxClient::new(self.sandbox_socket())
            .request(request, &tokio_util::sync::CancellationToken::new())
            .await
    }

    /// The config file that was read, or would have been.
    pub fn file(&self) -> &std::path::Path {
        &self.file
    }

    /// Where the conversation lives.
    /// A fresh conversation key under `prefix`, as `prefix-<uuid>`.
    ///
    /// Minted the way the store mints its own ids — UUIDv7 off the injected
    /// clock and the OS CSPRNG — so a test that pauses time still gets
    /// distinct keys and nothing here reaches for ambient randomness.
    ///
    /// Hyphens and hex, and no punctuation that has to be escaped: a key is
    /// half of a URL the moment the same conversation is opened in a browser,
    /// and `cli:default` reads as `cli%3Adefault` there.
    #[must_use]
    pub fn new_session_key(&self, prefix: &str) -> String {
        let mut random = [0u8; 10];
        OsRandom.fill(&mut random);
        let id = new_uuid(u64::try_from(self.clock.now_ms()).unwrap_or(0), &random);
        format!("{prefix}-{id}")
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
        Err(WireError::new(kind, message))
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
    /// need it and neither belongs here: `darkwire serve` collects the channel
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
            WireError::new(
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
            paths: ResolveWirePaths {
                root: self.options.home.clone(),
                workspaces: self.options.workspaces.clone(),
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
            Some(previous) if previous.paths.workspaces_dir == paths.workspaces_dir => {
                Arc::clone(&previous.jails)
            }
            _ => Arc::new(JailCache::new(paths.clone())?),
        };

        // Before the mutations below, because every failure it can produce — an
        // container that is not installed, a manifest that does not parse, a network
        // request above its ceiling — must leave the runtime serving on the
        // settings that worked a moment ago.
        Self::resolve_policies(&agents, &paths)?;

        // Include every centrally registered tool name plus subagent tools.
        for agent in &agents {
            let mut advertised: Vec<String> = agent.tools.keys().cloned().collect();
            advertised.extend(
                agent
                    .subagents
                    .iter()
                    .map(|binding| binding.tool_name.clone()),
            );
            // The door takes no permission, so it is never in the map; an
            // override for it is placed whenever the short list is on.
            if agent.settings.lazy_discovery {
                advertised.push(TOOL_SEARCH_NAME.to_owned());
            }
            warnings.extend(tool_prompt_warnings(agent, &advertised));
        }
        // Read off the stored entries rather than the resolved agents: these
        // are fields the resolve step deliberately drops, and being told they
        // stopped being placed is the only thing left to do with them.
        for (id, entry) in &config.agents.list {
            warnings.extend(retired_prompt_warnings(id, entry));
        }

        // Past here nothing fails, so the mutations below cannot leave the
        // registry describing a runtime that failed to build.
        self.tools
            .set_timeout_ms(default_agent.settings.tool_timeout_ms);
        // Exact by source: a scheduler switched off in the settings panel has
        // to take `automation` out of the definitions the model sees, and MCP
        // and extension tools registered on this same registry must survive
        // that. An install with no scheduler should not advertise a way to
        // schedule; a tool that can only answer "this installation has no
        // scheduler" costs a turn to learn what its absence would have said for
        // free.
        self.tools.unregister_by_source(ToolSource::Builtin);
        if self.options.tools {
            register_builtins(
                &self.tools,
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
        let (factory, cache_resolver) =
            self.loop_factory(config.clone(), paths.clone(), Arc::clone(&jails));
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

impl WireRuntime {
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
    /// the default. `darkwire chat --model x` is a statement about this process,
    /// and an agent that quietly ignored it would be the more surprising rule.
    fn resolve_provider(
        &self,
        config: &Config,
        agent: &EffectiveAgent,
        paths: &WirePaths,
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

    /// Resolve every environment an enabled agent names.
    fn resolve_policies(agents: &[EffectiveAgent], paths: &WirePaths) -> Result<()> {
        let policies = PolicyStore::new(paths.policy_dir.clone());
        for agent in agents
            .iter()
            .filter(|agent| !agent.environment.name.is_empty())
        {
            let environment = policies.require_environment(&agent.environment.name)?;
            if agent.environment.network.mode == NetworkMode::Allowlist {
                assert_gateway_compatible(&environment.definition)?;
            }
        }
        Ok(())
    }
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

/// The host, or a container reached through the isolated service.
///
/// The composition root's half of [`EnvironmentResolver`]: the loop declares
/// what it needs and this decides which backend answers, so no agent and no
/// tool is written against one. A turn naming no environment runs here, which
/// is what an install with no container engine does for every agent.
///
/// It also reads the definition's own wording for what is installed, which is
/// why it holds a store. That read belongs here rather than in the loop for the
/// same reason the backend choice does: opening a policy file is
/// composition-root work, and a loop that could do it would be a loop that
/// knows where the policy directory is.
struct ServiceEnvironments {
    socket: std::path::PathBuf,
    policies: PolicyStore,
}

impl darkwire_tools::EnvironmentResolver for ServiceEnvironments {
    fn for_turn(&self, request: &darkwire_tools::PlacementRequest) -> Placed {
        if request.environment.is_empty() {
            return Placed::host();
        }
        // A definition that has gone missing or stopped parsing since boot is
        // not this function's to refuse. The command itself is still guarded,
        // and the service re-reads the definition before it runs anything. What
        // is lost is the prompt section, which is the right thing to lose: an
        // unreadable definition should not cost the turn its tools.
        let prompt = self
            .policies
            .require_environment(&request.environment)
            .ok()
            .and_then(|installed| installed.definition.prompt)
            .unwrap_or_default();
        Placed {
            environment: Arc::new(darkwire_environment::service::ContainerEnvironment::new(
                darkwire_environment::service::SandboxClient::new(self.socket.clone()),
                request.clone(),
            )),
            prompt,
        }
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

impl WireRuntime {
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
        paths: WirePaths,
        jails: Arc<JailCache>,
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
                Arc::clone(&bound) as Arc<dyn LoopResolver>,
            )
        });
        (factory, resolver)
    }
}

impl WireRuntime {
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
        paths: &WirePaths,
        jails: &Arc<JailCache>,
        resolver: Arc<dyn LoopResolver>,
    ) -> Result<Option<AgentLoop>> {
        let agent = resolve_agent(config, Some(agent_id))?;
        let endpoint = self.resolve_provider(config, &agent, paths)?;
        let Some(provider) = endpoint.provider else {
            return Ok(None);
        };

        // Every agent gets a permission-filtered view of the one shared
        // registry. Built-ins, MCP and extension tools retain their source
        // identities and lifecycle on that registry.
        if !agent.environment.name.is_empty() {
            PolicyStore::new(paths.policy_dir.clone())
                .require_environment(&agent.environment.name)?;
        }
        let scope = self.tools.select(agent.tools.clone());

        let mut options = AgentLoopOptions::new(
            provider,
            scope,
            Arc::clone(&self.store),
            Arc::clone(jails) as Arc<dyn JailResolver>,
        );
        options.model = Some(endpoint.model);
        options.config = agent.settings.clone();
        options.environment = agent.environment.clone();
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
        options.web = self.web.get().map(Arc::clone);
        options.environments = Some(Arc::new(ServiceEnvironments {
            socket: self.sandbox_socket(),
            policies: PolicyStore::new(paths.policy_dir.clone()),
        }));
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
            // Passed through as the operator wrote it. Which built-in an
            // empty override inherits is decided per turn, in the loop, because
            // a subagent runs where its caller's reference says it does and
            // this value is resolved once per agent.
            platform_prompt: Some(agent.platform_prompt.clone()),
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
        // model that cannot call `read`, the index is unusable and the prose
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
        // After memory, and in the *runtime* half rather than the cached one: a
        // plan changes while a turn runs, which is what the section is for.
        // Gated on the tool for the reason memory is — denying `todo` has to
        // take the section with it, or an agent that cannot write a plan still
        // pays for one in every request.
        if agent.settings.tools_enabled && granted(&agent.tools, "todo") {
            contributors.push(Arc::new(TasksContributor::new(Arc::clone(&self.store))));
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
    paths: &WirePaths,
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
    paths: &WirePaths,
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
    paths: &WirePaths,
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
