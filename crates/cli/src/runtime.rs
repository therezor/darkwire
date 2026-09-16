//! The CLI's view of `ghostai-runtime`.
//!
//! There is nothing CLI-shaped left in the composition itself: it moved to its
//! own crate the moment a second consumer needed it — the server shares one
//! [`Database`] with its auth store and reconfigures without dropping the
//! session store, neither of which a terminal ever asks for — and duplicating
//! the wiring is how the Python original ended up with three implementations of
//! the same startup.
//!
//! What remains is the naming, the one place a global option becomes a path
//! resolution, and `save_settings`, which is the one thing a terminal does to
//! the settings tree that the server's own port does differently.

use std::collections::HashMap;
use std::sync::Arc;

use ghostai_core::paths::ResolveGhostPaths;
use ghostai_core::{
    ErrorKind, GhostError, LogLevel, LogSink, LoggerOptions, Result, create_logger, save_config,
};
use ghostai_protocol::DEFAULT_AGENT_ID;
use ghostai_protocol::config::{AgentEntry, AgentSettings, Config, ConfigPatch};
use ghostai_runtime::{GhostRuntime, RuntimeOptions, create_runtime};

use crate::i18n::Env;
use crate::program::Globals;

/// The environment as the core crates want it.
///
/// They take an owned map rather than reading the process, for the reason every
/// seam here is injected: a test that had to mutate the process environment
/// would be a test that cannot run beside another one.
#[must_use]
pub fn env_map(env: &Env) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for name in ENV_NAMES {
        if let Some(value) = env.get(name) {
            map.insert((*name).to_owned(), value.to_owned());
        }
    }
    map
}

/// The variables the core crates read out of an environment.
///
/// Named rather than copied wholesale, because the map is what a jail, a
/// provider and a keychain all consult, and handing them the whole process
/// environment would make "which variable moved this" unanswerable.
const ENV_NAMES: &[&str] = &[
    "GHOSTAI_HOME",
    "GHOSTAI_LANG",
    "GHOSTAI_LOG_LEVEL",
    "GHOSTAI_CATALOGUE",
    "GHOSTAI_PRESETS_DIR",
    "HOME",
    // `exec`'s environment allow-list names `PATH` first, and the allow-list
    // filters *this* map — so a `PATH` that never arrives here is a child
    // process with no `PATH` at all, and every command named without a leading
    // slash fails with "No such file or directory". `node --version` is the
    // shape of it: a command an operator would call ordinary.
    "PATH",
    "LOG_LEVEL",
    "TERM",
    "NO_COLOR",
    "FORCE_COLOR",
    "TZ",
];

/// How a global `--home` and a per-command `--workspace` become paths.
///
/// One function so the answer cannot differ between the command that loads a
/// config to read it and the one that builds a whole runtime over it.
#[must_use]
pub fn load_options(globals: &Globals, workspace: Option<&str>, env: &Env) -> ResolveGhostPaths {
    let mut map = env_map(env);
    // Every provider's key variable, which resolution consults when no config
    // names a provider. Copied on demand rather than listed above, because the
    // set is the provider table's to decide.
    for spec in ghostai_providers::PROVIDERS.iter() {
        let Some(key) = spec.env_key.as_deref() else {
            continue;
        };
        if let Some(value) = env.get(key) {
            map.insert(key.to_owned(), value.to_owned());
        }
    }
    ResolveGhostPaths {
        root: globals.home.clone(),
        workspace: workspace.map(str::to_owned),
        env: Some(map),
        home: None,
    }
}

/// The log, written where the answer is not.
///
/// The core logger defaults to stdout, which is right for a library and wrong
/// for this program: stdout carries the *answer* — a turn's text, the `--json`
/// event stream, the listening record — and a log line interleaved with it
/// corrupts whatever is reading. `ghostai serve 2>ghost.log` is the shape an
/// operator expects, and it only works if the log was on the other stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct StderrSink;

impl LogSink for StderrSink {
    fn write_line(&self, line: &str) {
        use std::io::Write as _;
        let stderr = std::io::stderr();
        let mut handle = stderr.lock();
        // A closed stderr is not a reason to stop the process that was logging
        // to it; there is nowhere left to report the failure anyway.
        let _ = writeln!(handle, "{line}");
    }
}

/// Installs the logger for this process.
///
/// Once, and only here: the layer is global, and a second install is a silent
/// no-op that would leave a command wondering why its level was ignored. The
/// attempt is therefore allowed to fail quietly — a test that has already
/// installed one is the only way it can.
pub fn install_logger(level: LogLevel, env: &Env) {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let layer = create_logger(LoggerOptions {
        level: Some(level),
        name: Some("ghostai".to_owned()),
        sink: Some(Arc::new(StderrSink)),
        env: Some(env_map(env)),
        ..LoggerOptions::default()
    });
    let _ = tracing_subscriber::registry().with(layer).try_init();
}

/// The runtime `ghostai chat` builds.
pub type ChatRuntime = Arc<GhostRuntime>;

/// Builds a runtime, which is what every command that runs a turn needs.
pub fn create_chat_runtime(options: RuntimeOptions) -> Result<ChatRuntime> {
    create_runtime(options)
}

/// Applies a settings patch and writes it to `config.yaml`.
///
/// The server's own port is the same two steps plus a credential sweep, and
/// this is deliberately not a call into that: the REPL never builds a
/// `ServerRuntime` — it is an HTTP-shaped port with container approvals and
/// credential writes on it — and the sweep only has anything to do when a patch
/// *removes a provider instance*, which the patches a chat prompt sends never
/// do.
///
/// A failure here is the operator's file, not the operator's typing: a
/// `config.yaml` that is read-only, or a home directory that is not writable.
/// A `GhostError` is what the slash-command runner already catches and renders
/// as a warning, so the prompt says why and stays open rather than unwinding
/// over a half-applied change. The reconfigure has already landed at that
/// point, which is the honest outcome to report: this run moved, the file did
/// not.
pub fn save_settings(runtime: &ChatRuntime, patch: &ConfigPatch) -> Result<Config> {
    let merged = runtime.apply_patch(patch)?;
    save_config(runtime.file(), &merged).map_err(|error| {
        GhostError::new(
            ErrorKind::Storage,
            format!(
                "The change is live for this run, but {} could not be written",
                runtime.file().display()
            ),
        )
        .with_detail("file", runtime.file().to_string_lossy())
        .with_source(error)
    })?;
    Ok(merged)
}

/// One agent's settings, resolved.
///
/// A thin read, and it exists so the callers that want a budget rather than a
/// loop do not each write the fallback. Nothing is inherited: an id naming no
/// agent gets the schema's answer, which is the same thing an entry that names
/// none of these fields would have got.
#[must_use]
pub fn settings_of(config: &Config, agent_id: Option<&str>) -> AgentSettings {
    let id = agent_id
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_AGENT_ID);
    config.agents.list.get(id).map_or_else(
        || AgentEntry::default().settings,
        |entry| entry.settings.clone(),
    )
}
