//! `GhostRuntime` as the server's routes want it.
//!
//! `ghostai-server` states a narrow [`ServerRuntime`] port rather than
//! depending on the composition root, so that a route test needs neither a
//! provider nor a vault nor a workspace. This is the adapter on the other side
//! of that port, and it lives here because `ghostai serve` is where the two
//! halves are wired together in the first place.
//!
//! It is not a pass-through. Five things the port promises are implemented
//! *here* rather than in the runtime:
//!
//!  - **A settings save persists.** The runtime deliberately does not write
//!    `config.yaml` — previewing a patch and saving one are different
//!    operations — so `apply_settings` is reconfigure-then-write. The write
//!    runs after the rebuild, so a patch that cannot be built leaves both the
//!    running server and the file on the settings that worked.
//!  - **Deleting a provider instance takes its credential with it.** The config
//!    is only half of an instance; the other half is a vault entry keyed by the
//!    same id, and leaving it behind means the next instance to reuse that id
//!    silently inherits somebody else's key.
//!  - **A credential written over HTTP is usable on the next turn.** The vault
//!    is written and then the runtime is rebuilt with an empty patch, which
//!    re-reads it: the provider adapter is keyed on a digest of the key, so a
//!    new key is a new adapter and the turn after the save uses it.
//!  - **The vault is opened only when there is one, or when one is being
//!    written.** Resolving a vault key mints a keychain entry the first time it
//!    runs, and an install that talks to a local model and never stores a
//!    credential should not acquire one because someone opened the settings
//!    panel.
//!  - **Model lists come from the endpoints themselves.** That work belongs to
//!    the model catalogue rather than to this file, which is why it arrives as
//!    [`ModelSource`]: the terminal's `/model` asks the same question and there
//!    is one implementation of the answer.

use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use ghostai_agent::{PromptPreview, PromptPreviewInput};
use ghostai_core::{
    Database, ErrorKind, GhostError, Result, SessionStore, SystemClock, WorkspaceStore, save_config,
};
use ghostai_protocol::DEFAULT_AGENT_ID;
use ghostai_protocol::config::{Config, ConfigPatch};
use ghostai_protocol::rest::{
    ChannelStatus, ConfigWarning, CredentialNamespace, ExtensionCommand, ExtensionStatus,
    McpServerStatus, ModelsResponse, ProviderTestRequest, ProviderTestResponse, RunCommandRequest,
    RunCommandResponse, SetCredentialRequest,
};
use ghostai_protocol::tools::ToolDefinition;
use ghostai_providers::{ChatRequest, ChatResult, PROVIDERS, list_instances};
use ghostai_runtime::{
    GhostRuntime, PROVIDER_CREDENTIAL_NAMESPACE, VaultChoice, open_vault, resolve_agent,
};
use ghostai_security::jail::WorkspaceJail;
use ghostai_security::policy_store::{EnvironmentListing, PolicyStore};
use ghostai_security::{CredentialVault, ExtensionStore};
use ghostai_server::runtime::DirectChatInput;
use ghostai_server::{AgentSummary, AgentView, ExtensionCounts, ServerRuntime};
use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

/// What the adapter needs of a model catalogue.
///
/// Narrow on purpose. The catalogue caches, times out and classifies failures;
/// none of that is this file's business, and stating the whole of it here would
/// make the settings routes depend on how a model list is fetched.
pub trait ModelSource: Send + Sync {
    /// Every reachable model. `refresh` discards whatever was cached.
    fn list(&self, refresh: bool) -> BoxFuture<'_, Result<ModelsResponse>>;

    /// Whether one connection can be reached, and with which models.
    ///
    /// Answers rather than fails, because every outcome here is a *result*:
    /// "the key was rejected" and "nothing is listening there" are the two
    /// things the operator came to find out, so they travel as a `reason` on a
    /// 200 rather than as an error envelope the client would have to unpick.
    fn test<'a>(
        &'a self,
        request: &'a ProviderTestRequest,
    ) -> BoxFuture<'a, Result<ProviderTestResponse>>;

    /// Drops the cached catalogue.
    ///
    /// Called whenever something has happened that the cache cannot know about:
    /// a settings save, a credential written, a reload. Without it the panel
    /// would go on serving a list that predates the fix the operator has just
    /// made.
    fn invalidate(&self);
}

/// The real catalogue, as the narrow port above.
///
/// An adapter rather than an implementation on `ModelCatalogue` itself: the
/// catalogue is also what the terminal's `/model` reads, and that caller has no
/// use for the provider-test shape a route needs.
struct CatalogueSource {
    catalogue: crate::models::ModelCatalogue,
    credentials: Arc<Credentials>,
}

impl ModelSource for CatalogueSource {
    fn list(&self, refresh: bool) -> BoxFuture<'_, Result<ModelsResponse>> {
        Box::pin(self.catalogue.list(refresh))
    }

    /// One connection, asked whether it answers and with what.
    ///
    /// An omitted key means "whatever is stored", which is how a saved row
    /// re-tests without the client having to hold the credential to do it. An
    /// *empty* one is a different question — "does this answer with no key at
    /// all" — and both are ones an operator asks, so they are not collapsed.
    fn test<'a>(
        &'a self,
        request: &'a ProviderTestRequest,
    ) -> BoxFuture<'a, Result<ProviderTestResponse>> {
        Box::pin(async move {
            let Some(spec) = ghostai_providers::find_provider(&request.kind, &PROVIDERS) else {
                return Ok(ProviderTestResponse {
                    ok: false,
                    models: Vec::new(),
                    reason: Some("unsupported".to_owned()),
                    message: Some(format!(
                        "There is no provider type called \u{201c}{}\u{201d}.",
                        request.kind
                    )),
                });
            };

            let config = ghostai_protocol::ProviderConfig {
                kind: request.kind.clone(),
                label: String::new(),
                api_base: Some(request.api_base.clone()),
                extra_headers: request.extra_headers.clone(),
                models: Vec::new(),
                enabled: true,
            };
            let connection = ghostai_providers::resolve_connection(spec, Some(&config));
            // An omitted key means "whatever is stored"; an *empty* one is the
            // different question "does this answer with no key at all", and
            // both are ones an operator asks.
            let api_key = match request.api_key.clone() {
                Some(key) => Some(key).filter(|key| !key.is_empty()),
                None => request
                    .instance_id
                    .as_deref()
                    .and_then(|id| self.credentials.read(id, spec.env_key.as_deref())),
            };
            let target = crate::models::ProbeTarget {
                spec: spec.clone(),
                api_base: connection.api_base,
                extra_headers: connection.extra_headers,
                api_key,
            };

            match self.catalogue.probe(&target).await {
                crate::models::ProbeResult::Failed { reason, message } => {
                    Ok(ProviderTestResponse {
                        ok: false,
                        models: Vec::new(),
                        reason: Some(reason),
                        message: Some(message),
                    })
                }
                crate::models::ProbeResult::Models(models) => {
                    // A successful probe has just learned this endpoint's
                    // catalogue first hand, which makes the cached one out of
                    // date by definition. Without dropping it the agent panel
                    // would go on offering a list that predates the endpoint
                    // the operator just fixed.
                    self.catalogue.invalidate();
                    // Ids only. The probe's job is "can this be reached, and
                    // what is on it"; the shaped catalogue is what the model
                    // list serves.
                    Ok(ProviderTestResponse {
                        ok: true,
                        models: models.into_iter().map(|model| model.id).collect(),
                        reason: None,
                        message: None,
                    })
                }
            }
        })
    }

    fn invalidate(&self) {
        self.catalogue.invalidate();
    }
}

/// Rebuilds what only `ghostai serve` owns, after an extension was loaded.
///
/// The knot this unties: approving an extension can bring a channel factory
/// with it, and the channel manager fixes its factories at construction. So the
/// composition root passes the callback in, the same late binding the scheduler
/// uses for the same knot.
pub type ExtensionsChanged = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// Rebuilds the channel manager after a write that can move a channel.
///
/// Two writes can, and only two: a settings save, which is where
/// `config.channels` lives, and a channel's own credential, which is where a
/// bot token lives. Both mean a *new* manager rather than a restart, because
/// [`ChannelManager`](ghostai_channels::ChannelManager) fixes its factories at
/// construction — and only the composition root knows a manager exists, which
/// is why this arrives as a callback instead of being done here.
pub type ChannelsChanged = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// Everything the adapter is injected with.
#[derive(Default)]
pub struct ServerRuntimeOptions {
    /// The credential vault. `None` opens `<root>/vault.json` on demand, and
    /// only when it already exists.
    pub vault: Option<Arc<Mutex<CredentialVault>>>,
    /// The model catalogue. `None` means this build has nothing to ask, and the
    /// routes fall back to what the settings tree names.
    pub models: Option<Arc<dyn ModelSource>>,
    /// Called after an extension approval has been reconciled.
    pub extensions_changed: Option<ExtensionsChanged>,
    /// Called after a settings save, or after a channel credential was written.
    pub channels_changed: Option<ChannelsChanged>,
    /// Provider key variables, read for the presence flags.
    pub env: std::collections::HashMap<String, String>,
    /// Reports one channel's live state.
    ///
    /// A function rather than a list, because the composition root replaces the
    /// channel manager whenever the settings that configure it are saved, and a
    /// snapshot taken at construction would report the manager that has since
    /// been replaced.
    pub channels: Option<Arc<dyn Fn() -> Vec<ChannelStatus> + Send + Sync>>,
}

/// Where a provider key is read from, and when the vault is opened to do it.
///
/// Its own object rather than a pair of methods on the adapter, because two
/// things need exactly this and one of them is built by the other: the routes
/// ask "is a credential present", and the model catalogue asks "what is it" —
/// and a catalogue that opened its own vault would mint a second keychain entry
/// for the same install.
pub struct Credentials {
    runtime: Arc<GhostRuntime>,
    env: std::collections::HashMap<String, String>,
    /// Opened lazily, and only when there is one or one is being written.
    vault: Mutex<Option<Arc<Mutex<CredentialVault>>>>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").finish_non_exhaustive()
    }
}

impl Credentials {
    /// A reader over one install's vault and environment.
    pub fn new(
        runtime: Arc<GhostRuntime>,
        env: std::collections::HashMap<String, String>,
        vault: Option<Arc<Mutex<CredentialVault>>>,
    ) -> Arc<Credentials> {
        Arc::new(Credentials {
            runtime,
            env,
            vault: Mutex::new(vault),
        })
    }

    /// `create` is what separates reading presence from storing a key.
    ///
    /// Resolving a vault key mints a keychain entry the first time it runs, and
    /// an install that talks to a local model and never stores a credential
    /// should not acquire one because someone opened the settings panel.
    pub fn open(&self, create: bool) -> Option<Arc<Mutex<CredentialVault>>> {
        let mut held = self.vault.lock();
        if let Some(vault) = held.as_ref() {
            return Some(Arc::clone(vault));
        }
        let paths = self.runtime.paths();
        if !create && !paths.vault_file.exists() {
            return None;
        }
        let opened = Arc::new(Mutex::new(open_vault(&paths).ok()?));
        *held = Some(Arc::clone(&opened));
        Some(opened)
    }

    /// One instance's key: the vault first, the environment second.
    pub fn read(&self, instance_id: &str, env_key: Option<&str>) -> Option<String> {
        let stored = self
            .open(false)
            .and_then(|vault| {
                vault
                    .lock()
                    .get(PROVIDER_CREDENTIAL_NAMESPACE, instance_id)
                    .map(str::to_owned)
            })
            .filter(|value| !value.is_empty());
        stored.or_else(|| {
            env_key
                .and_then(|key| self.env.get(key))
                .filter(|value| !value.is_empty())
                .cloned()
        })
    }

    /// Whether an instance has a key at all, without reading one.
    ///
    /// Booleans, never values: the vault is write-only over HTTP, and this is
    /// the shape that lets a settings panel show "configured" without a key
    /// crossing the network.
    pub fn present(&self, instance_id: &str, env_key: Option<&str>) -> bool {
        let from_env = env_key
            .and_then(|key| self.env.get(key))
            .is_some_and(|value| !value.is_empty());
        from_env
            || self
                .open(false)
                .is_some_and(|vault| vault.lock().has(PROVIDER_CREDENTIAL_NAMESPACE, instance_id))
    }
}

/// The adapter.
pub struct CliServerRuntime {
    runtime: Arc<GhostRuntime>,
    options: ServerRuntimeOptions,
    credentials: Arc<Credentials>,
    /// The catalogue the model routes and a provider test both go through.
    models: Arc<dyn ModelSource>,
}

impl std::fmt::Debug for CliServerRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CliServerRuntime").finish_non_exhaustive()
    }
}

impl CliServerRuntime {
    /// Wraps a runtime in the port the routes speak.
    /// The model catalogue is built here rather than passed in, because it needs
    /// the same credential reader the presence flags do and a second one would
    /// open a second vault. A caller that wants a different catalogue — a test,
    /// which must not dial anything — supplies one in the options.
    pub fn new(runtime: Arc<GhostRuntime>, options: ServerRuntimeOptions) -> Arc<CliServerRuntime> {
        let credentials = Credentials::new(
            Arc::clone(&runtime),
            options.env.clone(),
            options.vault.clone(),
        );
        let models = options.models.clone().unwrap_or_else(|| {
            let reader = Arc::clone(&credentials);
            let catalogue = crate::models::create_model_catalogue(
                Arc::clone(&runtime),
                crate::models::ModelCatalogueOptions {
                    credential_for: {
                        let reader = Arc::clone(&reader);
                        Arc::new(move |instance| {
                            reader.read(&instance.id, instance.spec.env_key.as_deref())
                        })
                    },
                    timeout_ms: None,
                    clock: None,
                },
            );
            Arc::new(CatalogueSource {
                catalogue,
                credentials: reader,
            })
        });
        Arc::new(CliServerRuntime {
            runtime,
            options,
            credentials,
            models,
        })
    }

    /// The runtime underneath, for the composition root that built it.
    pub fn runtime(&self) -> &Arc<GhostRuntime> {
        &self.runtime
    }

    /// A store of its own rather than the host's, for the reason the container
    /// store is constructed per use: it is a thin object over the shared
    /// connection, and handing the host's out would make the approval gate
    /// reachable from anything holding a host.
    fn extension_store(&self) -> Result<ExtensionStore> {
        ExtensionStore::new(
            self.database(),
            self.runtime.paths().extensions_dir,
            Arc::new(SystemClock),
        )
    }

    fn database(&self) -> Database {
        self.runtime.store().database().clone()
    }

    fn invalidate_models(&self) {
        self.models.invalidate();
    }

    /// Loads what the approval just changed, then lets the caller catch up.
    ///
    /// Two steps, and they are not interchangeable: the runtime has to
    /// reconcile first, because the channel factories the composition root is
    /// about to collect only exist once the extension has been activated.
    fn rebuild_channels(&self) {
        let Some(changed) = self.options.channels_changed.as_ref() else {
            return;
        };
        let future = changed();
        // Started and not awaited, deliberately: both callers answer a request
        // that is still open, and a save that blocked on a Telegram round trip
        // would be a settings panel that hangs on somebody else's network. The
        // panel reads the channel's state back on its next fetch.
        //
        // `try_current` rather than `tokio::spawn`, which panics off a runtime:
        // every real caller is a route handler, but a direct caller in a test
        // is not, and it should get the rebuild inline rather than an abort.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(future);
            }
            Err(_) => futures::executor::block_on(future),
        }
    }

    fn reload_extensions(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.runtime.reload_extensions();
            if let Some(changed) = self.options.extensions_changed.as_ref() {
                changed().await;
            }
        })
    }
}

impl ServerRuntime for CliServerRuntime {
    fn config(&self) -> Config {
        self.runtime.config()
    }

    fn apply_settings(&self, patch: ConfigPatch) -> Result<Config> {
        let before: Vec<String> = self.runtime.config().providers.keys().cloned().collect();
        let merged = self.runtime.apply_patch(&patch)?;

        // After the rebuild, so a patch that could not be built has not already
        // destroyed a credential on its way to failing.
        let removed: Vec<&String> = before
            .iter()
            .filter(|id| !merged.providers.contains_key(*id))
            .collect();
        if !removed.is_empty() {
            if let Some(vault) = self.credentials.open(false) {
                for id in removed {
                    let _ = vault.lock().delete(PROVIDER_CREDENTIAL_NAMESPACE, id);
                }
            }
            // The catalogue named instances that no longer exist.
            self.invalidate_models();
        }

        save_config(self.runtime.file(), &merged)?;
        // After the write, so a patch that could not be saved does not bounce a
        // bot that is running perfectly well on the settings that are still on
        // disk.
        self.rebuild_channels();
        Ok(merged)
    }

    fn reload(&self) -> Result<Config> {
        let next = self.runtime.reload()?;
        // The file is the source here, so there is nothing to write back — and
        // writing would turn a reload into a save, which is how a config edited
        // by hand gets reformatted by the button that was meant to read it.
        //
        // The catalogue goes, though: a reload is how an operator picks up an
        // endpoint that moved or a model they have just pulled.
        self.invalidate_models();
        Ok(next)
    }

    fn credentials_present(&self) -> IndexMap<String, bool> {
        let mut present = IndexMap::new();
        // By instance, not by provider type: two endpoints of one type can hold
        // different keys, and reporting the type would light both up for one.
        for instance in list_instances(&self.runtime.config().providers, &PROVIDERS) {
            let configured = self
                .credentials
                .present(&instance.id, instance.spec.env_key.as_deref());
            present.insert(instance.id.clone(), configured);
        }
        present
    }

    fn set_credential(&self, request: &SetCredentialRequest) -> Result<()> {
        let Some(vault) = self.credentials.open(true) else {
            return Err(GhostError::new(
                ErrorKind::Storage,
                "The credential vault could not be opened",
            ));
        };
        {
            let mut vault = vault.lock();
            let namespace = namespace_name(request.namespace);
            match request.value.as_deref() {
                None => {
                    vault.delete(namespace, &request.key)?;
                }
                Some(value) => vault.set(namespace, &request.key, value)?,
            }
        }
        // An empty patch is not a no-op: the rebuild re-reads the credential,
        // and without it the loop keeps the provider it was built with — so a
        // key saved in the UI would not take effect until a restart.
        self.runtime.apply_patch(&ConfigPatch::default())?;
        // A key is often what stood between an endpoint and its catalogue.
        self.invalidate_models();
        // Only a channel's own credential moves a channel. A provider key save
        // must not bounce a bot that has nothing to do with it.
        if request.namespace == CredentialNamespace::Channels {
            self.rebuild_channels();
        }
        Ok(())
    }

    /// A declared capability with no source.
    ///
    /// Loading refuses an unreadable file, so a running server has no load
    /// error to report — there is no tolerant-boot path for it to describe.
    fn load_error(&self) -> Option<String> {
        None
    }

    fn config_warnings(&self) -> Vec<ConfigWarning> {
        self.runtime
            .config_warnings()
            .iter()
            .map(ghostai_runtime::AgentConfigWarning::to_dto)
            .collect()
    }

    fn store(&self) -> Arc<SessionStore> {
        Arc::clone(self.runtime.store())
    }

    fn workspaces(&self) -> Arc<WorkspaceStore> {
        Arc::clone(self.runtime.workspaces())
    }

    /// Read from disk on every call, deliberately.
    ///
    /// A definition edited after approval must stop reporting as usable the
    /// moment it changes, and a list cached at boot would keep saying it was
    /// fine until a restart. Constructing the store per call is a directory
    /// read.
    fn environments(&self) -> Vec<EnvironmentListing> {
        PolicyStore::new(self.runtime.paths().policy_dir).list_environments()
    }

    fn sandbox_request(
        &self,
        request: serde_json::Value,
    ) -> BoxFuture<'_, Result<serde_json::Value>> {
        Box::pin(self.runtime.sandbox_request(request))
    }

    fn release_workspace(&self, workspace_id: &str) {
        self.runtime.evict_workspace(workspace_id);
    }

    fn agent(&self, agent_id: Option<&str>) -> Result<Arc<dyn AgentView>> {
        // Resolution refuses an id naming nothing runnable, which the route
        // turns into a 404 — the alternative, silently describing the default,
        // would report tools and a prompt for an agent nobody asked about.
        let config = self.runtime.config();
        let agent = resolve_agent(&config, agent_id)?;
        let agent_loop = self.runtime.loop_for(agent_id)?;
        // The spec id either way, which is what a loop reports: a fallback
        // spelling the instance id instead would make the field mean one thing
        // on a configured install and another on an unconfigured one.
        let provider = agent_loop.as_ref().map_or_else(
            || self.runtime.spec().map(|spec| spec.id).unwrap_or_default(),
            |one| one.provider().to_owned(),
        );
        Ok(Arc::new(CliAgentView {
            provider,
            runtime: Arc::clone(&self.runtime),
            id: agent.id.clone(),
            label: agent.label.clone(),
            is_default: agent.id == DEFAULT_AGENT_ID,
            tools_enabled: agent.settings.tools_enabled,
            context_window_tokens: u32::try_from(agent.settings.context_window_tokens)
                .unwrap_or(u32::MAX),
            tools: agent.tools.clone(),
            agent_loop,
        }))
    }

    fn registered_tools(&self) -> Vec<ToolDefinition> {
        // The bare registry, narrowed by nobody. Built-ins, MCP registrations
        // and extension tools — everything an agent could be granted.
        self.runtime.tools().definitions().to_vec()
    }

    fn agents(&self) -> Vec<AgentSummary> {
        let provider = self
            .runtime
            .instance()
            .map(|instance| instance.id)
            .unwrap_or_default();
        self.runtime
            .agents()
            .into_iter()
            .map(|agent| AgentSummary {
                // After inheritance, and after any process-wide pin, so a
                // picker shows what a turn would actually use rather than what
                // the file says.
                model: self
                    .runtime
                    .loop_for(Some(&agent.id))
                    .ok()
                    .flatten()
                    .map_or_else(
                        || agent.settings.model.clone(),
                        |one| one.model().to_owned(),
                    ),
                provider: provider.clone(),
                reasoning_effort: agent.settings.reasoning_effort,
                id: agent.id,
                label: agent.label,
            })
            .collect()
    }

    fn extensions(&self) -> ExtensionCounts {
        ExtensionCounts {
            // `ready` and not merely "configured": the count answers "how many
            // servers could a turn actually reach", which is the question the
            // status line is asking. A server that is retrying is not one.
            mcp_servers_connected: u32::try_from(
                self.runtime
                    .mcp_servers()
                    .iter()
                    .filter(|server| server.state == ghostai_protocol::McpServerState::Ready)
                    .count(),
            )
            .unwrap_or(u32::MAX),
            extensions_loaded: self.runtime.extensions().map_or(0, |host| {
                u32::try_from(host.loaded_count()).unwrap_or(u32::MAX)
            }),
        }
    }

    fn models(&self, refresh: bool) -> Option<BoxFuture<'_, Result<ModelsResponse>>> {
        Some(self.models.list(refresh))
    }

    fn channels(&self) -> Vec<ChannelStatus> {
        self.options
            .channels
            .as_ref()
            .map(|report| report())
            .unwrap_or_default()
    }

    fn test_provider<'a>(
        &'a self,
        request: &'a ProviderTestRequest,
    ) -> Option<BoxFuture<'a, Result<ProviderTestResponse>>> {
        Some(self.models.test(request))
    }

    fn mcp_servers(&self) -> Vec<McpServerStatus> {
        self.runtime.mcp_servers()
    }

    fn extension_statuses(&self) -> Vec<ExtensionStatus> {
        self.runtime
            .extensions()
            .map(|host| host.status())
            .unwrap_or_default()
    }

    fn approve_extension<'a>(&'a self, id: &'a str) -> Option<BoxFuture<'a, Result<()>>> {
        Some(Box::pin(async move {
            self.extension_store()?.approve(id)?;
            self.reload_extensions().await;
            Ok(())
        }))
    }

    fn revoke_extension<'a>(&'a self, id: &'a str) -> Option<BoxFuture<'a, Result<()>>> {
        Some(Box::pin(async move {
            self.extension_store()?.revoke(id)?;
            self.reload_extensions().await;
            Ok(())
        }))
    }

    fn commands(&self) -> Vec<ExtensionCommand> {
        self.runtime
            .extensions()
            .map(|host| host.commands())
            .unwrap_or_default()
    }

    fn run_command<'a>(
        &'a self,
        id: &'a str,
        request: &'a RunCommandRequest,
        token: CancellationToken,
    ) -> Option<BoxFuture<'a, Result<RunCommandResponse>>> {
        let host = self.runtime.extensions()?.clone();
        Some(Box::pin(async move {
            let outcome = host
                .run_command(id, &request.args, request.session_key.as_deref(), &token)
                .await?;
            Ok(RunCommandResponse {
                message: outcome.message,
                ok: outcome.ok,
            })
        }))
    }

    /// One provider request outside a turn — the heartbeat's forced
    /// `skip | run` decision, and nothing else.
    ///
    /// It refuses rather than falling back on an unconfigured install: a
    /// heartbeat that could not ask has to be a recorded error, because the
    /// alternative is guessing "run" and starting an unbounded turn every
    /// interval forever.
    fn chat(&self, input: DirectChatInput) -> Option<BoxFuture<'_, Result<ChatResult>>> {
        Some(Box::pin(async move {
            let resolved = self
                .runtime
                .provider_for(input.agent_id.as_deref(), input.model.as_deref())?
                .ok_or_else(|| {
                    GhostError::new(
                        ErrorKind::NotFound,
                        "No provider is configured to answer with.",
                    )
                })?;
            let (provider, model) = resolved;
            let request = ChatRequest {
                tools: input.tools,
                tool_choice: Some(input.tool_choice),
                max_tokens: input.max_tokens.map(u64::from),
                ..ChatRequest::new(model, input.messages)
            };
            provider.chat(&request, &input.token).await
        }))
    }
}

/// One agent, as the status and context routes see it.
struct CliAgentView {
    runtime: Arc<GhostRuntime>,
    id: String,
    label: String,
    is_default: bool,
    tools_enabled: bool,
    context_window_tokens: u32,
    tools: ghostai_protocol::ToolPermissions,
    agent_loop: Option<ghostai_agent::AgentLoop>,
    /// The endpoint a turn would reach, resolved once at construction.
    ///
    /// The loop's when there is one, and the *resolved instance's* when there
    /// is not — and the second half is the part that earns the field. An
    /// install whose endpoint resolves but whose agent names no model has no
    /// loop, and reporting nothing for it makes it indistinguishable from an
    /// install with no endpoint at all. The setup wizard branches on exactly
    /// that difference: one of them needs a model and the other needs a
    /// provider, and asking the wrong question reads as the app having
    /// forgotten what it was told.
    provider: String,
}

impl AgentView for CliAgentView {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn provider(&self) -> &str {
        // Empty only when nothing resolved at all: `configured` is the flag to
        // branch on for "can this take a turn", so nothing has to read meaning
        // into a string beyond "is there an endpoint".
        &self.provider
    }

    fn model(&self) -> &str {
        self.agent_loop
            .as_ref()
            .map_or("", ghostai_agent::AgentLoop::model)
    }

    fn configured(&self) -> bool {
        if self.is_default {
            self.runtime.configured()
        } else {
            self.agent_loop.is_some()
        }
    }

    fn jail(&self) -> Arc<WorkspaceJail> {
        self.runtime.jail()
    }

    fn jail_for(&self, workspace_id: &str) -> Result<Arc<WorkspaceJail>> {
        Ok(self.runtime.jails().for_workspace(workspace_id))
    }

    /// The loop's own list, for the reason the prompt preview defers to it too.
    ///
    /// Narrowing the registry by the agent's allow-list is correct as far as it
    /// goes and blind to the container tools composed on top of that scope when
    /// the agent has a container — rebuilt here, a container agent's tools were
    /// absent from the context inspector and from its token count.
    ///
    /// The fallback covers an unconfigured install, where there is no loop and
    /// so no turn to describe; the narrowed registry is the honest answer there
    /// — except when the agent advertises no tools at all, which the registry
    /// cannot know and which would otherwise have the panel list a toolset no
    /// turn on this agent would ever send.
    fn tools(&self) -> Vec<ToolDefinition> {
        match self.agent_loop.as_ref() {
            Some(one) => one.tool_definitions(),
            None if self.tools_enabled => self
                .runtime
                .tools()
                .select(self.tools.clone())
                .definitions()
                .to_vec(),
            None => Vec::new(),
        }
    }

    fn context_window_tokens(&self) -> u32 {
        self.context_window_tokens
    }

    /// The loop's own composition, not a second assembly of it: memory and
    /// skills arrive as contributors attached to that object, and a
    /// reimplementation here could not see them.
    fn system_prompt<'a>(
        &'a self,
        input: &'a PromptPreviewInput,
    ) -> ghostai_providers::BoxFuture<'a, Result<PromptPreview>> {
        Box::pin(async move {
            let Some(one) = self.agent_loop.as_ref() else {
                // The context route asks for this to show what a turn would
                // carry. With no model there is no turn and no prompt, and
                // failing would make one unconfigured panel break a screen that
                // otherwise works.
                return Ok(PromptPreview {
                    static_prompt:
                        "No model is configured, so no system prompt has been assembled yet."
                            .to_owned(),
                    runtime_block: String::new(),
                });
            };
            one.preview_prompt(input).await
        })
    }
}

/// The vault namespace a credential request names, as the vault spells it.
///
/// Matched rather than serialised through JSON: the vault is keyed by a plain
/// string, and round-tripping an enum through a serialiser to get one back
/// would make the key depend on a `rename_all` attribute three crates away.
fn namespace_name(namespace: CredentialNamespace) -> &'static str {
    match namespace {
        CredentialNamespace::Providers => PROVIDER_CREDENTIAL_NAMESPACE,
        CredentialNamespace::Tools => "tools",
        CredentialNamespace::Audio => "audio",
        CredentialNamespace::McpServers => "mcpServers",
        CredentialNamespace::Extensions => "extensions",
        CredentialNamespace::Channels => "channels",
    }
}

/// Whether a vault exists at `path` without opening it.
///
/// Stated as a function so the "open only when there is one" rule has one
/// spelling rather than an `exists` call at each site that has to remember it.
#[must_use]
pub fn vault_exists(path: &Path) -> bool {
    path.exists()
}

/// The default vault wiring: open `<root>/vault.json` on demand.
///
/// Named so the composition root can say which of the three states it wants
/// without spelling the enum at the call site.
#[must_use]
pub fn default_vault_choice() -> VaultChoice {
    VaultChoice::Default
}
