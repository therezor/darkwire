//! A whole server over a scripted model, behind the `testkit` feature.
//!
//! Three doubles, and each replaces the one thing a transport test has no
//! business standing up:
//!
//!  - [`FakeRuntime`] is a [`ServerRuntime`] with no provider, no vault and no
//!    config file. It holds a *real* session store and a *real* workspace jail,
//!    because those are the two things the routes actually exercise and fakes
//!    of them would be reimplementations of behaviour worth testing against.
//!  - [`scripted_runner`] is a turn that answers instantly, built over the
//!    agent crate's own scripted provider rather than over a hand-written
//!    stand-in — so a route test drives the real loop, and the hub sees the
//!    real event stream.
//!  - [`TestServer`] wires those into [`create_server`] and hands back a router
//!    a test drives with `tower::ServiceExt::oneshot`, plus the bearer token
//!    that satisfies the manifest's `Required` routes.
//!
//! Behind a feature rather than in `tests/`, because `packages/e2e` and
//! `darkwire-runtime` both want the composition and neither can reach a test
//! directory. Excluded from coverage for the same reason every testkit is: it
//! runs on every test and would pad whichever crate held it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use darkwire_agent::testkit::{ScriptedProvider, ScriptedTurn};
use darkwire_agent::{
    AgentLoop, AgentLoopOptions, PromptPreview, PromptPreviewInput, SteeringQueue, TurnInput,
};
use darkwire_core::ids::{DEFAULT_AGENT_ID, DEFAULT_WORKSPACE_ID};
use darkwire_core::paths::{ResolveWirePaths, WirePaths, workspace_dir_for};
use darkwire_core::testkit::ManualClock;
use darkwire_core::{Clock, Database, ErrorKind, Result, SessionStore, WireError, WorkspaceStore};
use darkwire_protocol::config::{Config, ConfigPatch};
use darkwire_protocol::environment::EnvironmentDefinition;
use darkwire_protocol::rest::SetCredentialRequest;
use darkwire_protocol::tools::ToolDefinition;
use darkwire_providers::BoxFuture;
use darkwire_security::EnvironmentListing;
use darkwire_security::jail::{JailOptions, WorkspaceJail, single_jail};
use darkwire_security::random::RandomSource;
use darkwire_tools::{ToolRegistry, ToolScope};
use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::app::{ServerOptions, create_server};
use crate::approvals::{HubApprovalGate, HubApprovalGateOptions};
use crate::auth_store::PasswordHasher;
use crate::hub::{
    AgentMissReason, AgentResolution, SessionHub, SessionHubOptions, TurnHandle, TurnRunner,
};
use crate::runtime::{AgentSummary, AgentView, ExtensionCounts, ServerRuntime};
use crate::scheduler::SchedulerPort;
use crate::ui::UiRoot;

/// The instant every double starts at, so a test that asserts on a timestamp
/// asserts on a number rather than on "recently".
pub const NOW: i64 = 1_700_000_000_000;

// Randomness

/// Bytes a test can predict.
///
/// Never usable in production — a session token from a counter is the same as
/// no token at all — which is exactly why the real source is a trait and this
/// lives behind a feature.
#[derive(Debug, Default)]
pub struct CountingRandom {
    next: Mutex<u8>,
}

impl RandomSource for CountingRandom {
    fn fill(&self, buf: &mut [u8]) {
        let mut next = self.next.lock();
        for byte in buf.iter_mut() {
            *byte = *next;
            *next = next.wrapping_add(1);
        }
    }
}

// The password hasher

/// A hasher that does no work.
///
/// argon2id is deliberately around 50 ms per call, which is right for a login
/// and wrong for a test that logs in forty times. The encoding is deliberately
/// not a valid PHC string, so a fixture cannot be mistaken for a real digest if
/// one ever ends up in a database somebody keeps.
#[derive(Debug, Default, Clone, Copy)]
pub struct FakeHasher;

impl PasswordHasher for FakeHasher {
    fn hash(&self, password: &str) -> Result<String> {
        Ok(format!("fake:{password}"))
    }

    fn verify(&self, digest: &str, password: &str) -> bool {
        digest == format!("fake:{password}")
    }
}

// The turn

/// A turn that says one thing and ends.
///
/// Built over the agent crate's scripted provider rather than over a
/// hand-written generator, so the events the hub sees are the ones a real turn
/// emits — in the order, and with the fields, the loop actually produces.
pub struct ScriptedRunner {
    agent_loop: AgentLoop,
    /// Every turn this runner was asked to run, in order.
    inputs: Mutex<Vec<String>>,
    /// Every steer, as `(session key, content)`.
    steers: Mutex<Vec<(String, String)>>,
}

impl std::fmt::Debug for ScriptedRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedRunner")
            .field("inputs", &self.inputs.lock().len())
            .finish_non_exhaustive()
    }
}

impl ScriptedRunner {
    /// The session key of every turn this runner started, in order.
    pub fn inputs(&self) -> Vec<String> {
        self.inputs.lock().clone()
    }

    /// Every steer this runner was handed, in order.
    pub fn steers(&self) -> Vec<(String, String)> {
        self.steers.lock().clone()
    }
}

impl TurnRunner for ScriptedRunner {
    fn run(&self, input: TurnInput, parent: &CancellationToken) -> Box<dyn TurnHandle> {
        self.inputs.lock().push(input.session_key.clone());
        Box::new(self.agent_loop.run(input, parent))
    }

    fn steer(&self, session_key: &str, content: &str) {
        self.steers
            .lock()
            .push((session_key.to_owned(), content.to_owned()));
        self.agent_loop.steer(session_key, content);
    }
}

/// A runner whose model answers with each of `answers` in turn and stops.
///
/// `hold_ms` delays the model's first token, which is the only way a route test
/// reaches the guards that exist *while* a session is busy — branching under a
/// running answer, clearing a transcript mid-turn. An instant turn is finished
/// before the assertion runs, so those branches are otherwise unreachable from
/// the transport.
pub fn scripted_runner(
    store: &Arc<SessionStore>,
    jail: &Arc<WorkspaceJail>,
    clock: &Arc<dyn Clock>,
    answers: Vec<&str>,
    hold_ms: Option<u64>,
) -> Result<Arc<ScriptedRunner>> {
    let turns: Vec<ScriptedTurn> = answers
        .into_iter()
        .map(|answer| match hold_ms {
            Some(delay) => ScriptedTurn::text(answer).after(delay),
            None => ScriptedTurn::text(answer),
        })
        .collect();
    let provider = ScriptedProvider::new(turns);
    let registry = Arc::new(ToolRegistry::default());
    let scope: Arc<dyn ToolScope> = registry.select(IndexMap::new());

    let agent_loop = AgentLoop::new(AgentLoopOptions {
        // The scripted provider answers whatever is asked of it, but a loop
        // still refuses to exist without a model named — which is the guard
        // that turns "nothing is configured" into a setup prompt rather than a
        // turn that fails at the provider.
        model: Some("test-model".to_owned()),
        clock: Arc::clone(clock),
        steering: Arc::new(SteeringQueue::new()),
        time_zone: Some(Arc::new(|| "UTC".to_owned())),
        ..AgentLoopOptions::new(
            provider,
            scope,
            Arc::clone(store),
            Arc::new(single_jail(Arc::clone(jail))),
        )
    })?;

    Ok(Arc::new(ScriptedRunner {
        agent_loop,
        inputs: Mutex::new(Vec::new()),
        steers: Mutex::new(Vec::new()),
    }))
}

// The runtime

/// How a [`FakeRuntime`] differs from the default one.
#[derive(Debug, Default, Clone)]
pub struct FakeRuntimeOptions {
    /// The settings tree. Defaults to a parsed empty config.
    pub config: Option<Config>,
    /// The provider instance id the agent view reports.
    pub provider: Option<String>,
    /// The model the agent view reports.
    pub model: Option<String>,
    /// `false` drives the routes as a fresh install with no provider or model.
    pub configured: Option<bool>,
    /// What the default agent advertises — the context inspector's list.
    pub tools: Vec<ToolDefinition>,
    /// What the registry holds, which is a superset in every real install.
    ///
    /// Defaults to `tools`, so a test that only cares that a definition reaches
    /// a route says it once. A test about the *difference* — the tool-list
    /// route offering something no agent currently holds — sets both.
    pub registered_tools: Option<Vec<ToolDefinition>>,
    /// Which credentials the settings panel should show as configured.
    pub credentials_present: IndexMap<String, bool>,
    /// The static half of the prompt the context route reports.
    pub system_prompt: Option<String>,
    /// The trailing turn the loop appends after the history.
    pub runtime_block: Option<String>,
    /// Independently installed container definitions.
    pub environments: Vec<EnvironmentListing>,
}

/// One agent's view, over a real jail and a real workspace tree.
pub struct FakeAgentView {
    id: String,
    label: String,
    provider: String,
    model: String,
    configured: bool,
    tools: Vec<ToolDefinition>,
    context_window_tokens: u32,
    paths: WirePaths,
    jails: Mutex<HashMap<String, Arc<WorkspaceJail>>>,
    system_prompt: String,
    runtime_block: String,
}

impl FakeAgentView {
    fn jail_or_build(&self, workspace_id: &str) -> Result<Arc<WorkspaceJail>> {
        if let Some(cached) = self.jails.lock().get(workspace_id) {
            return Ok(Arc::clone(cached));
        }
        let root = workspace_dir_for(&self.paths, workspace_id)?;
        let made = Arc::new(WorkspaceJail::new(JailOptions::new(root))?);
        self.jails
            .lock()
            .insert(workspace_id.to_owned(), Arc::clone(&made));
        Ok(made)
    }
}

impl AgentView for FakeAgentView {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn provider(&self) -> &str {
        &self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn configured(&self) -> bool {
        self.configured
    }

    fn jail(&self) -> Arc<WorkspaceJail> {
        // The default workspace's tree exists by the time any route asks:
        // `FakeRuntime::new` builds it, which is also what makes this
        // infallible where `jail_for` is not.
        self.jail_or_build(DEFAULT_WORKSPACE_ID)
            .unwrap_or_else(|_| unreachable_jail())
    }

    fn jail_for(&self, workspace_id: &str) -> Result<Arc<WorkspaceJail>> {
        self.jail_or_build(workspace_id)
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        self.tools.clone()
    }

    fn context_window_tokens(&self) -> u32 {
        self.context_window_tokens
    }

    fn system_prompt<'a>(
        &'a self,
        input: &'a PromptPreviewInput,
    ) -> BoxFuture<'a, Result<PromptPreview>> {
        let preview = PromptPreview {
            static_prompt: self.system_prompt.replace("{session}", &input.session_key),
            runtime_block: self.runtime_block.clone(),
        };
        Box::pin(async move { Ok(preview) })
    }
}

/// The default workspace's jail is built during construction, so a failure here
/// cannot happen; this keeps the infallible getter honest without a panic.
fn unreachable_jail() -> Arc<WorkspaceJail> {
    // A jail on the process's temporary directory: never reached, and still a
    // real jail rather than a panic, so a misuse degrades to a refusal.
    Arc::new(
        WorkspaceJail::new(JailOptions::new(std::env::temp_dir()))
            .unwrap_or_else(|_| unreachable!("the temporary directory is always a usable root")),
    )
}

/// A [`ServerRuntime`] with no provider, no vault and no config file.
pub struct FakeRuntime {
    config: Mutex<Config>,
    store: Arc<SessionStore>,
    workspaces: Arc<WorkspaceStore>,
    agent: Arc<FakeAgentView>,
    registered_tools: Vec<ToolDefinition>,
    /// Behind a lock because the environment routes write it: a save has to
    /// show up in the list the same response returns, or a round-trip test
    /// asserts against a snapshot taken before the write.
    environments: Mutex<Vec<EnvironmentListing>>,
    credentials: Mutex<IndexMap<String, bool>>,
    /// Every patch this runtime was asked to apply, in order.
    patches: Mutex<Vec<ConfigPatch>>,
    /// Every credential write, with the value it was handed.
    credential_writes: Mutex<Vec<SetCredentialRequest>>,
    /// Every workspace id a route asked to be forgotten.
    released: Mutex<Vec<String>>,
    /// How many times a reload was asked for.
    reloads: Mutex<usize>,
}

impl FakeRuntime {
    /// A runtime over `database`, with its workspaces tree under `root`.
    pub fn new(
        database: &Database,
        root: &std::path::Path,
        clock: &Arc<dyn Clock>,
        new_id: darkwire_core::session_store::IdSource,
        options: &FakeRuntimeOptions,
    ) -> Result<Arc<FakeRuntime>> {
        let config = options.config.clone().unwrap_or_default();
        let paths = WirePaths::resolve(ResolveWirePaths {
            root: Some(root.to_string_lossy().into_owned()),
            home: Some(root.to_path_buf()),
            ..ResolveWirePaths::default()
        })?;
        std::fs::create_dir_all(workspace_dir_for(&paths, DEFAULT_WORKSPACE_ID)?).map_err(
            |error| {
                WireError::new(
                    ErrorKind::Storage,
                    format!("Could not create the default workspace: {error}"),
                )
            },
        )?;

        let store = Arc::new(SessionStore::new(
            database.clone(),
            Arc::clone(clock),
            new_id,
        )?);
        let workspaces = Arc::new(WorkspaceStore::new(
            database.clone(),
            paths.clone(),
            Arc::clone(clock),
        )?);

        let agent = Arc::new(FakeAgentView {
            id: DEFAULT_AGENT_ID.to_owned(),
            label: DEFAULT_AGENT_ID.to_owned(),
            provider: options
                .provider
                .clone()
                .unwrap_or_else(|| "openai".to_owned()),
            model: options
                .model
                .clone()
                .unwrap_or_else(|| "gpt-test".to_owned()),
            // A route test is about the route, and a fixture that defaulted to
            // unconfigured would make every one of them assert around a setup
            // banner.
            configured: options.configured.unwrap_or(true),
            tools: options.tools.clone(),
            context_window_tokens: config
                .agents
                .list
                .get(DEFAULT_AGENT_ID)
                .map_or(65_536, |entry| {
                    u32::try_from(entry.settings.context_window_tokens).unwrap_or(u32::MAX)
                }),
            paths,
            jails: Mutex::new(HashMap::new()),
            system_prompt: options
                .system_prompt
                .clone()
                .unwrap_or_else(|| "# DarkWire\n\nSession: {session}".to_owned()),
            runtime_block: options
                .runtime_block
                .clone()
                .unwrap_or_else(|| "## Live state\n\nCurrent time: whenever".to_owned()),
        });
        // Built eagerly so the infallible getter is telling the truth.
        agent.jail_or_build(DEFAULT_WORKSPACE_ID)?;

        Ok(Arc::new(FakeRuntime {
            config: Mutex::new(config),
            store,
            workspaces,
            registered_tools: options
                .registered_tools
                .clone()
                .unwrap_or_else(|| options.tools.clone()),
            environments: Mutex::new(options.environments.clone()),
            agent,
            credentials: Mutex::new(options.credentials_present.clone()),
            patches: Mutex::new(Vec::new()),
            credential_writes: Mutex::new(Vec::new()),
            released: Mutex::new(Vec::new()),
            reloads: Mutex::new(0),
        }))
    }

    /// Every patch a settings route applied, in order.
    pub fn patches(&self) -> Vec<ConfigPatch> {
        self.patches.lock().clone()
    }

    /// Every credential write, with the value it was handed.
    pub fn credential_writes(&self) -> Vec<SetCredentialRequest> {
        self.credential_writes.lock().clone()
    }

    /// Every workspace id a route asked to be forgotten.
    pub fn released(&self) -> Vec<String> {
        self.released.lock().clone()
    }

    /// How many reloads were asked for.
    pub fn reloads(&self) -> usize {
        *self.reloads.lock()
    }
}

impl ServerRuntime for FakeRuntime {
    fn config(&self) -> Config {
        self.config.lock().clone()
    }

    fn apply_settings(&self, patch: ConfigPatch) -> Result<Config> {
        self.patches.lock().push(patch.clone());
        let mut config = self.config.lock();
        *config = merged(&config, &patch)?;
        Ok(config.clone())
    }

    fn reload(&self) -> Result<Config> {
        *self.reloads.lock() += 1;
        Ok(self.config.lock().clone())
    }

    fn credentials_present(&self) -> IndexMap<String, bool> {
        self.credentials.lock().clone()
    }

    fn set_credential(&self, request: &SetCredentialRequest) -> Result<()> {
        self.credential_writes.lock().push(request.clone());
        if request.namespace == darkwire_protocol::rest::CredentialNamespace::Providers {
            // `false`, not a removal: the settings panel distinguishes "no key"
            // from "never asked".
            self.credentials
                .lock()
                .insert(request.key.clone(), request.value.is_some());
        }
        Ok(())
    }

    fn load_error(&self) -> Option<String> {
        None
    }

    fn store(&self) -> Arc<SessionStore> {
        Arc::clone(&self.store)
    }

    fn workspaces_dir(&self) -> PathBuf {
        self.agent.paths.workspaces_dir.clone()
    }

    fn workspaces(&self) -> Arc<WorkspaceStore> {
        Arc::clone(&self.workspaces)
    }

    fn release_workspace(&self, workspace_id: &str) {
        self.released.lock().push(workspace_id.to_owned());
        self.agent.jails.lock().remove(workspace_id);
    }

    fn agent(&self, agent_id: Option<&str>) -> Result<Arc<dyn AgentView>> {
        match agent_id {
            None => Ok(Arc::clone(&self.agent) as Arc<dyn AgentView>),
            Some(id) if id == DEFAULT_AGENT_ID => Ok(Arc::clone(&self.agent) as Arc<dyn AgentView>),
            Some(id) if self.agents().iter().any(|entry| entry.id == id) => {
                Ok(Arc::clone(&self.agent) as Arc<dyn AgentView>)
            }
            Some(id) => Err(WireError::new(
                ErrorKind::NotFound,
                format!("No agent named \"{id}\""),
            )),
        }
    }

    fn registered_tools(&self) -> Vec<ToolDefinition> {
        self.registered_tools.clone()
    }

    fn agents(&self) -> Vec<AgentSummary> {
        // Driven by the settings tree so a route test that wants a second agent
        // sets one the same way an operator would, rather than through a second
        // knob that could disagree with the tree.
        let config = self.config.lock();
        let mut agents = vec![AgentSummary {
            id: DEFAULT_AGENT_ID.to_owned(),
            label: DEFAULT_AGENT_ID.to_owned(),
            model: self.agent.model.clone(),
            provider: self.agent.provider.clone(),
            reasoning_effort: None,
        }];
        for (id, entry) in &config.agents.list {
            if id == DEFAULT_AGENT_ID || !entry.enabled {
                continue;
            }
            agents.push(AgentSummary {
                id: id.clone(),
                label: if entry.label.is_empty() {
                    id.clone()
                } else {
                    entry.label.clone()
                },
                model: entry.settings.model.clone(),
                provider: self.agent.provider.clone(),
                reasoning_effort: None,
            });
        }
        agents
    }

    fn environments(&self) -> Vec<EnvironmentListing> {
        self.environments.lock().clone()
    }

    /// Records the write, running the one policy check a route test needs.
    ///
    /// The full check set belongs to `PolicyStore` and is tested there. The
    /// digest pin is repeated here because it is the refusal an operator meets
    /// most often, and because the *status* it comes back as is the route's
    /// decision rather than the store's: without a refusing double, every route
    /// test passes while a policy rejection is reported as a 500.
    fn save_environment(&self, definition: &EnvironmentDefinition) -> Result<String> {
        if !definition.image.starts_with("sha256:") && !definition.image.contains("@sha256:") {
            return Err(WireError::new(
                ErrorKind::Config,
                format!(
                    "Container \"{}\" must pin its image by digest, not by tag: {}",
                    definition.name, definition.image
                ),
            ));
        }
        let mut environments = self.environments.lock();
        environments.retain(|listing| listing.name != definition.name);
        environments.push(EnvironmentListing {
            name: definition.name.clone(),
            path: std::path::PathBuf::from(format!("/policy/{}.yaml", definition.name)),
            value: Some(definition.clone()),
            problem: None,
        });
        environments.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(format!("sha256:{}", "f".repeat(64)))
    }

    fn remove_environment(&self, name: &str) -> Result<()> {
        let mut environments = self.environments.lock();
        let before = environments.len();
        environments.retain(|listing| listing.name != name);
        if environments.len() == before {
            return Err(WireError::new(
                ErrorKind::Config,
                format!("No environment is installed under \"{name}\"."),
            ));
        }
        Ok(())
    }

    fn extensions(&self) -> ExtensionCounts {
        ExtensionCounts::default()
    }
}

/// A deep merge over JSON, then a re-parse.
///
/// The composition root's real merge lives in `darkwire-runtime`, which this
/// crate deliberately cannot reach. A route test does not need that rule — it
/// needs the settings tree to *move* when a patch is applied, so a test that
/// changes the model and reads it back sees the change. `null` removes a key,
/// which is the real merge's rule for the records an operator adds to and
/// removes from; without it a patch that deletes an agent would store a literal
/// null where an entry belongs and fail the re-parse.
fn merged(config: &Config, patch: &ConfigPatch) -> Result<Config> {
    let mut base = serde_json::to_value(config)
        .map_err(|error| WireError::new(ErrorKind::Internal, error.to_string()))?;
    let overlay = serde_json::to_value(patch)
        .map_err(|error| WireError::new(ErrorKind::Internal, error.to_string()))?;
    merge_value(&mut base, overlay);
    darkwire_protocol::config::parse_config(base)
        .map_err(|error| WireError::new(ErrorKind::Config, error.to_string()))
}

fn merge_value(base: &mut serde_json::Value, overlay: serde_json::Value) {
    let serde_json::Value::Object(source) = overlay else {
        *base = overlay;
        return;
    };
    let Some(target) = base.as_object_mut() else {
        *base = serde_json::Value::Object(source);
        return;
    };
    for (key, value) in source {
        if value.is_null() {
            target.shift_remove(&key);
            continue;
        }
        match target.get_mut(&key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                merge_value(existing, value);
            }
            _ => {
                target.insert(key, value);
            }
        }
    }
}

// The server

/// A built server, its doubles, and the credential that reaches its routes.
pub struct TestServer {
    /// The router, driven with `tower::ServiceExt::oneshot`.
    pub router: Router,
    /// The runtime behind it, for the assertions that are about what a route
    /// asked the runtime to do.
    pub runtime: Arc<FakeRuntime>,
    /// The hub the socket route serves.
    pub hub: Arc<SessionHub>,
    /// The turn every session runs.
    pub runner: Arc<ScriptedRunner>,
    /// A bearer token that satisfies every `Required` route.
    pub token: String,
    /// The auth store, so a test can mint a second token.
    ///
    /// Needed more often than it looks: `POST /api/auth/logout` revokes the
    /// session it was presented, so a test that walks more than one route with
    /// one token is testing its own bookkeeping rather than the routes.
    pub auth: Arc<crate::auth_store::AuthStore>,
    /// The clock, so a test that needs two rows to carry different timestamps
    /// can move it.
    pub clock: Arc<ManualClock>,
    /// The shared connection, kept alive for the life of the server.
    pub database: Database,
    /// The temporary home, kept alive so the workspace tree outlives the test.
    pub home: tempfile::TempDir,
}

impl std::fmt::Debug for TestServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Enough to name it in an `expect_err`, and no field whose `Debug`
        // would print a session token.
        f.debug_struct("TestServer")
            .field("sessions", &self.hub.session_count())
            .finish_non_exhaustive()
    }
}

/// How a [`TestServer`] differs from the default one.
#[derive(Default)]
pub struct TestServerOptions {
    /// The runtime's shape.
    pub runtime: FakeRuntimeOptions,
    /// The boot settings. Defaults to the runtime's.
    pub config: Option<Config>,
    /// What the scripted model answers with, turn by turn.
    pub answers: Vec<String>,
    /// The UI to serve. Defaults to none, which is what a route test wants.
    pub ui: UiRoot,
    /// The scheduling engine.
    ///
    /// `None` is the default and is a real state, not a gap: a build with no
    /// engine still serves the automation CRUD routes so an operator can author
    /// jobs, and only the forced run refuses. A test about the engine supplies
    /// a double here rather than standing the whole server up by hand.
    pub scheduler: Option<Arc<dyn SchedulerPort>>,
    /// The password to set at boot. Defaults to one the token is minted over.
    pub password: Option<String>,
    /// Delays the scripted model's first token, so a turn is still running when
    /// the next request arrives. See [`scripted_runner`].
    pub hold_ms: Option<u64>,
}

/// Stands a whole server up over the doubles.
///
/// Long because it is a composition root: every seam the real one wires is
/// wired here too, and hiding half of them behind helpers would make the thing
/// a test is standing up harder to read rather than easier.
#[allow(
    clippy::too_many_lines,
    reason = "a composition root reads better as one list than as five helpers"
)]
pub fn start_test_server(options: TestServerOptions) -> Result<TestServer> {
    let home = tempfile::tempdir().map_err(|error| {
        WireError::new(
            ErrorKind::Storage,
            format!("Could not create a temporary home: {error}"),
        )
    })?;
    let database = Database::in_memory()?;
    let clock = Arc::new(ManualClock::at(NOW));
    let dyn_clock: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;

    let counter = Arc::new(Mutex::new(0u64));
    let new_id: darkwire_core::session_store::IdSource = {
        let counter = Arc::clone(&counter);
        Box::new(move || {
            let mut next = counter.lock();
            *next += 1;
            format!("id-{next}")
        })
    };

    let config = options
        .config
        .clone()
        .or_else(|| options.runtime.config.clone())
        .unwrap_or_default();
    let runtime = FakeRuntime::new(
        &database,
        home.path(),
        &dyn_clock,
        new_id,
        &FakeRuntimeOptions {
            config: Some(config.clone()),
            ..options.runtime.clone()
        },
    )?;

    let jail = runtime.agent.jail_or_build(DEFAULT_WORKSPACE_ID)?;
    let answers: Vec<&str> = if options.answers.is_empty() {
        vec!["ok"]
    } else {
        options.answers.iter().map(String::as_str).collect()
    };
    let runner = scripted_runner(&runtime.store, &jail, &dyn_clock, answers, options.hold_ms)?;

    let approvals = Arc::new(HubApprovalGate::new(HubApprovalGateOptions {
        clock: Some(Arc::clone(&dyn_clock)),
        ..HubApprovalGateOptions::default()
    }));
    let hub = SessionHub::new(SessionHubOptions {
        config: config.clone(),
        loop_for: {
            let runner: Arc<dyn TurnRunner> = Arc::clone(&runner) as Arc<dyn TurnRunner>;
            Arc::new(move |_| Ok(Some(Arc::clone(&runner))))
        },
        // The real rule, off the config the test supplied, so a test that sets
        // up an agent the way an operator would gets the behaviour an operator
        // would. Reimplementing it as "everything resolves" would make the
        // fallback the one thing these tests could never see.
        resolve_agent_id: {
            let config = config.clone();
            Arc::new(move |agent_id| {
                let id = agent_id
                    .filter(|id| !id.is_empty())
                    .unwrap_or(DEFAULT_AGENT_ID);
                if id == DEFAULT_AGENT_ID {
                    return AgentResolution {
                        agent_id: id.to_owned(),
                        miss: None,
                    };
                }
                match config.agents.list.get(id) {
                    Some(entry) if entry.enabled => AgentResolution {
                        agent_id: id.to_owned(),
                        miss: None,
                    },
                    Some(_) => AgentResolution {
                        agent_id: DEFAULT_AGENT_ID.to_owned(),
                        miss: Some(AgentMissReason::Disabled),
                    },
                    None => AgentResolution {
                        agent_id: DEFAULT_AGENT_ID.to_owned(),
                        miss: Some(AgentMissReason::Unknown),
                    },
                }
            })
        },
        store: Arc::clone(&runtime.store),
        approvals,
        clock: Some(Arc::clone(&dyn_clock)),
        new_id: None,
        max_queue_depth: None,
        max_sessions: None,
    });

    let password = options
        .password
        .unwrap_or_else(|| "correct horse battery staple".to_owned());
    let built = create_server(ServerOptions {
        config,
        runtime: Arc::clone(&runtime) as Arc<dyn ServerRuntime>,
        hub: Arc::clone(&hub),
        ui: options.ui,
        database: database.clone(),
        scheduler: options.scheduler.clone(),
        clock: Arc::clone(&dyn_clock),
        random: Arc::new(CountingRandom::default()),
        password: Some(password),
        username: None,
        hasher: Some(Arc::new(FakeHasher)),
    })?;

    let token = built.auth.issue("test")?.token;

    Ok(TestServer {
        router: built.router,
        runtime,
        hub,
        runner,
        auth: Arc::clone(&built.auth),
        token,
        clock,
        database,
        home,
    })
}
