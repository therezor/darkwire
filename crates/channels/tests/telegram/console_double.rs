//! A `TelegramConsole` over real stores.
//!
//! Real `SessionStore` and `WorkspaceStore` rather than stubs: those two are the
//! parts worth testing against, because a command's whole job is what it does to
//! them, and a stub that returns what the test already assumes proves nothing.
//! The stores are in-memory SQLite over a temporary directory, so a suite still
//! finishes in milliseconds.
//!
//! Only the four provider-shaped members are canned, because there is no
//! provider here to answer them.

use std::sync::Arc;

use darkwire_channels::channel::BoxFuture;
use darkwire_channels::telegram::console::{
    MemoryState, SkillSummary, SkillsState, TelegramConsole,
};
use darkwire_core::clock::Clock;
use darkwire_core::paths::{ResolveWirePaths, WirePaths};
use darkwire_core::testkit::ManualClock;
use darkwire_core::{Database, Result, SessionStore, WorkspaceStore};
use darkwire_protocol::{AgentSummary, ContextResponse, ModelInfo, ModelsResponse};
use indexmap::IndexMap;
use parking_lot::Mutex;
use tempfile::TempDir;

/// The frozen wall clock every fixture starts at.
pub const NOW: i64 = 1_700_000_000_000;

/// Deterministic ids, so a listing's order is the order it was written in.
fn counter_ids(prefix: &'static str) -> darkwire_core::session_store::IdSource {
    let next = std::sync::atomic::AtomicU64::new(0);
    Box::new(move || {
        format!(
            "{prefix}{}",
            next.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
        )
    })
}

struct Canned {
    agents: Vec<AgentSummary>,
    models: ModelsResponse,
    context: Option<ContextResponse>,
    memory: MemoryState,
    skills: SkillsState,
    models_set: Vec<String>,
}

/// A console whose stores are real and whose provider answers are scripted.
pub struct FakeConsole {
    store: SessionStore,
    workspaces: WorkspaceStore,
    canned: Mutex<Canned>,
    clock: Arc<ManualClock>,
    /// Kept alive: dropping it removes the workspace root under the stores.
    dir: TempDir,
}

fn default_agents() -> Vec<AgentSummary> {
    vec![
        AgentSummary {
            id: "default".to_owned(),
            label: "Default".to_owned(),
            model: "gpt-4o".to_owned(),
            provider: "openai".to_owned(),
            reasoning_effort: None,
        },
        AgentSummary {
            id: "researcher".to_owned(),
            label: "Researcher".to_owned(),
            model: "o3".to_owned(),
            provider: "openai".to_owned(),
            reasoning_effort: None,
        },
    ]
}

fn model(id: &str) -> ModelInfo {
    ModelInfo {
        id: id.to_owned(),
        provider_id: "openai".to_owned(),
        provider_type: None,
        display_name: None,
        context_window_tokens: None,
        supports_tools: None,
        supports_vision: None,
        supports_reasoning: None,
    }
}

fn default_models() -> ModelsResponse {
    ModelsResponse {
        models: vec![model("gpt-4o"), model("o3")],
        errors: IndexMap::new(),
    }
}

impl FakeConsole {
    /// A console over fresh stores.
    pub fn new() -> Result<Arc<FakeConsole>> {
        let dir = tempfile::tempdir().map_err(|error| {
            darkwire_core::WireError::new(
                darkwire_core::ErrorKind::Storage,
                format!("no temporary directory: {error}"),
            )
        })?;
        let root = dir.path().to_string_lossy().into_owned();
        let clock = Arc::new(ManualClock::at(NOW));
        let database = Database::in_memory()?;
        let store = SessionStore::new(
            database.clone(),
            Arc::clone(&clock) as Arc<dyn Clock>,
            counter_ids("m"),
        )?;
        // A real store needs somewhere to make a workspace's directory. The
        // temporary directory is both the install root and the workspace root.
        let paths = WirePaths::resolve(ResolveWirePaths {
            root: Some(root.clone()),
            workspace: Some(root),
            env: Some(std::collections::HashMap::new()),
            home: Some(dir.path().to_path_buf()),
        })?;
        let workspaces =
            WorkspaceStore::new(database, paths, Arc::clone(&clock) as Arc<dyn Clock>)?;

        Ok(Arc::new(FakeConsole {
            store,
            workspaces,
            canned: Mutex::new(Canned {
                agents: default_agents(),
                models: default_models(),
                context: None,
                memory: MemoryState {
                    granted: true,
                    count: 0,
                    tokens: 0,
                },
                skills: SkillsState {
                    granted: true,
                    skills: Vec::new(),
                },
                models_set: Vec::new(),
            }),
            clock,
            dir,
        }))
    }

    /// The clock both stores read.
    pub fn clock(&self) -> Arc<ManualClock> {
        Arc::clone(&self.clock)
    }

    /// The install root, so a test can assert a workspace directory was made.
    pub fn root(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// Models `set_model` was asked for, in order.
    pub fn models_set(&self) -> Vec<String> {
        self.canned.lock().models_set.clone()
    }

    /// Replaces what `models()` answers.
    pub fn set_models(&self, ids: &[&str]) {
        self.canned.lock().models = ModelsResponse {
            models: ids.iter().map(|id| model(id)).collect(),
            errors: IndexMap::new(),
        };
    }

    /// Replaces what `agents()` answers.
    pub fn set_agents(&self, agents: Vec<AgentSummary>) {
        self.canned.lock().agents = agents;
    }

    /// Replaces what `context()` answers.
    pub fn set_context(&self, report: Option<ContextResponse>) {
        self.canned.lock().context = report;
    }

    /// Replaces what `memory()` answers.
    pub fn set_memory(&self, state: MemoryState) {
        self.canned.lock().memory = state;
    }

    /// Replaces what `skills()` answers.
    pub fn set_skills(&self, granted: bool, skills: Vec<SkillSummary>) {
        self.canned.lock().skills = SkillsState { granted, skills };
    }
}

impl TelegramConsole for FakeConsole {
    fn store(&self) -> &SessionStore {
        &self.store
    }

    fn workspaces(&self) -> &WorkspaceStore {
        &self.workspaces
    }

    fn agents(&self) -> Vec<AgentSummary> {
        self.canned.lock().agents.clone()
    }

    fn models(&self) -> BoxFuture<'_, Result<ModelsResponse>> {
        let models = self.canned.lock().models.clone();
        Box::pin(std::future::ready(Ok(models)))
    }

    fn set_model(&self, id: &str) {
        self.canned.lock().models_set.push(id.to_owned());
    }

    fn context<'a>(
        &'a self,
        _session_key: &'a str,
    ) -> BoxFuture<'a, Result<Option<ContextResponse>>> {
        let report = self.canned.lock().context.clone();
        Box::pin(std::future::ready(Ok(report)))
    }

    fn memory<'a>(&'a self, _session_key: &'a str) -> BoxFuture<'a, Result<MemoryState>> {
        let state = self.canned.lock().memory;
        Box::pin(std::future::ready(Ok(state)))
    }

    fn skills<'a>(&'a self, _session_key: &'a str) -> BoxFuture<'a, Result<SkillsState>> {
        let state = self.canned.lock().skills.clone();
        Box::pin(std::future::ready(Ok(state)))
    }
}
