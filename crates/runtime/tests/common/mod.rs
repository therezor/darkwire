//! One temporary install, and the seams every suite injects into it.
//!
//! Nothing here touches a keychain, a daemon or a socket: the vault is switched
//! off, the MCP client and the extension host are off unless a suite asks, and
//! the provider factory is a scripted double. That is the whole point of the
//! runtime's injection surface, and a harness that quietly took the defaults
//! would prove the opposite of what these suites are for.

#![allow(
    dead_code,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a shared harness is used by a subset of its consumers, and a fixture that will not load is a failing test either way"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use darkwire_core::testkit::ManualClock;
use darkwire_core::{Clock, Database, Result, WireError};
use darkwire_protocol::{Config, ToolDefinition, ToolRisk};
use darkwire_providers::testkit::ScriptedProvider;
use darkwire_providers::{ChatProvider, CreateProviderOptions, ProviderSpec, WireProtocol};
use darkwire_runtime::provider_cache::{ProviderCache, ProviderFactory};
use darkwire_runtime::{
    ExtensionChoice, McpChoice, RuntimeOptions, VaultChoice, WireRuntime, create_runtime,
};
use darkwire_tools::{AnyTool, BoxFuture, Tool, ToolContext, ToolExecution};
use serde_json::{Value, json};
use tempfile::TempDir;

/// The frozen wall clock every fixture starts at.
pub const NOW: i64 = 1_700_000_000_000;

/// A temporary `DARKWIRE_HOME` with a config file in it.
pub struct Install {
    /// Kept so the directory outlives the test.
    pub temp: TempDir,
    /// The root every path is derived from.
    pub root: PathBuf,
    /// The shared connection, so a suite can prove the runtime did not take it.
    pub database: Database,
    /// Frozen, so an idle sweep only happens when a test moves it.
    pub clock: Arc<ManualClock>,
}

impl Install {
    /// An install whose `config.yaml` is `config`.
    pub fn with(config: &Value) -> Install {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        std::fs::write(
            root.join("config.yaml"),
            serde_json::to_string_pretty(config).unwrap(),
        )
        .unwrap();
        Install {
            temp,
            root,
            database: Database::in_memory().unwrap(),
            clock: Arc::new(ManualClock::at(NOW)),
        }
    }

    /// An install with no config file at all, which is a fresh machine.
    pub fn bare() -> Install {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        Install {
            temp,
            root,
            database: Database::in_memory().unwrap(),
            clock: Arc::new(ManualClock::at(NOW)),
        }
    }

    /// Rewrites `config.yaml`, for the suites that prove a reload reads the file.
    pub fn write_config(&self, config: &Value) {
        std::fs::write(
            self.root.join("config.yaml"),
            serde_json::to_string_pretty(config).unwrap(),
        )
        .unwrap();
    }

    /// The config file's path.
    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.yaml")
    }

    /// Options over this install with every outward seam closed.
    pub fn options(&self) -> RuntimeOptions {
        RuntimeOptions {
            home: Some(self.root.to_string_lossy().into_owned()),
            env: Some(HashMap::new()),
            // An explicit "no vault" rather than the default: the default opens
            // one on demand, and opening one writes a key to the OS keychain.
            vault: VaultChoice::None,
            mcp: McpChoice::Off,
            extensions: ExtensionChoice::Off,
            database: Some(self.database.clone()),
            clock: Some(Arc::clone(&self.clock) as Arc<dyn Clock>),
            ..RuntimeOptions::default()
        }
    }

    /// A runtime over this install.
    pub fn runtime(&self) -> Result<Arc<WireRuntime>> {
        create_runtime(self.options())
    }
}

/// A provider cache that counts constructions and opens no sockets.
pub struct CountingProviders {
    /// The cache to hand the runtime.
    pub cache: Arc<ProviderCache>,
    built: Arc<AtomicUsize>,
}

impl CountingProviders {
    /// A cache over a scripted provider, bounded at `max`.
    pub fn new(max: usize) -> CountingProviders {
        let built = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&built);
        let factory: ProviderFactory = Arc::new(move |options: CreateProviderOptions| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(scripted(&spec_of(&options)) as Arc<dyn ChatProvider>)
        });
        CountingProviders {
            cache: Arc::new(ProviderCache::with_factory(max, factory)),
            built,
        }
    }

    /// How many adapters have been constructed.
    pub fn built(&self) -> usize {
        self.built.load(Ordering::SeqCst)
    }
}

/// The spec a factory call names, whichever way it was addressed.
fn spec_of(options: &CreateProviderOptions) -> ProviderSpec {
    match &options.provider {
        darkwire_providers::ProviderRef::Spec(spec) => (**spec).clone(),
        darkwire_providers::ProviderRef::Id(id) => local_spec(id),
    }
}

/// A provider that answers one fixed sentence and never reaches the network.
pub fn scripted(spec: &ProviderSpec) -> Arc<ScriptedProvider> {
    ScriptedProvider::new(spec.clone(), Vec::new())
}

/// A local, credential-free provider type, which is what a test wants by
/// default: no key is needed, so nothing consults a vault.
pub fn local_spec(id: &str) -> ProviderSpec {
    let mut spec = ProviderSpec {
        id: id.to_owned(),
        display_name: id.to_owned(),
        wire: WireProtocol::OpenaiChat,
        is_local: true,
        default_api_base: Some(format!("http://{id}.test/v1")),
        ..darkwire_providers::PROVIDERS
            .iter()
            .find(|spec| spec.id == "ollama")
            .unwrap()
            .clone()
    };
    spec.env_key = None;
    spec
}

/// The settings of an install that can run a turn against a local endpoint.
pub fn configured(model: &str) -> Value {
    json!({
        "agents": {"list": {"default": {"model": model, "provider": "ollama"}}},
        "providers": {"ollama": {"type": "ollama", "apiBase": "http://ollama.test/v1"}},
    })
}

/// One error out of a result, or a failure naming what came instead.
pub fn err<T: std::fmt::Debug>(result: Result<T>) -> WireError {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error,
    }
}

/// The config as a value, for a test that asserts on one field.
pub fn value_of(config: &Config) -> Value {
    serde_json::to_value(config).unwrap()
}

/// Writes a file and everything above it.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// A tool that exists to be registered and never called.
///
/// A double rather than a built-in: these suites are about who *holds* a name,
/// and a real tool would drag a workspace and a jail in behind it.
pub struct Named(pub ToolDefinition);

impl Tool for Named {
    fn definition(&self) -> &ToolDefinition {
        &self.0
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Safe
    }

    fn execute<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolExecution> {
        let _ = (args, ctx);
        Box::pin(async { ToolExecution::ok("") })
    }
}

/// One [`Named`] tool, advertised under `name`.
pub fn tool(name: &str) -> AnyTool {
    Arc::new(Named(ToolDefinition {
        name: name.to_owned(),
        description: String::new(),
        parameters: indexmap::IndexMap::new(),
        risk: ToolRisk::Safe,
        source: darkwire_protocol::ToolSource::Builtin,
        annotations: None,
    }))
}

/// Polls `ready` until it holds or the deadline passes.
///
/// A condition rather than a fixed number of yields: a count passes on an idle
/// machine and fails on a loaded one, which is the flake this repo has already
/// paid for once.
pub async fn eventually(within: std::time::Duration, ready: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        if ready() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    ready()
}
