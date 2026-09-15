//! What the routes need from below the transport, stated as a trait.
//!
//! `ghostai-runtime` is the composition root: it turns a config file into a
//! provider, a jail, a store, a registry and a loop. This crate deliberately
//! does not depend on it. The dependency would compile — nothing above the
//! server depends on the server — but it would put the whole wiring graph
//! behind every route test, and the server would then be untestable without a
//! provider, a workspace and a vault. So the server states the *narrow* set of
//! things a route actually touches, and `ghostai serve` supplies an adapter
//! over `GhostRuntime`.
//!
//! The shape is deliberately made of calls rather than fields. `PATCH
//! /api/settings` rebuilds the provider, the jail and the loop, so a route
//! holding a snapshot taken at boot would keep answering with the model the
//! operator just changed. Everything a settings save can move is read through a
//! method.
//!
//! The optional members are optional for one reason: a route test standing in
//! for a runtime has no business opening a socket or owning a channel manager,
//! and a required member would make every such double invent an answer. Each
//! one has a default that reports the honest empty state.

use std::sync::Arc;

use ghostai_agent::{PromptPreview, PromptPreviewInput};
use ghostai_core::{Result, SessionStore, WorkspaceStore};
use ghostai_protocol::config::{Config, ConfigPatch, ReasoningEffort};
use ghostai_protocol::messages::ChatMessage;
use ghostai_protocol::rest::{
    ChannelStatus, ConfigWarning, ExtensionCommand, ExtensionStatus, McpServerStatus,
    ModelsResponse, ProviderTestRequest, ProviderTestResponse, RunCommandRequest,
    RunCommandResponse, SetCredentialRequest,
};
use ghostai_protocol::tools::ToolDefinition;
use ghostai_providers::{BoxFuture, ChatResult, ToolChoice};
use ghostai_security::jail::WorkspaceJail;
use ghostai_security::policy_store::{ContainerListing, ToolboxListing};
use indexmap::IndexMap;
use tokio_util::sync::CancellationToken;

/// The agent as the status and context routes see it.
///
/// A snapshot, taken per request: `provider`, `model` and `jail` are all
/// replaced by a reconfigure, and the tool list changes when `exec` is switched
/// off in the settings panel or an MCP server connects.
pub trait AgentView: Send + Sync {
    /// Which agent this view describes. `default` unless one was asked for.
    fn id(&self) -> &str;

    /// Never empty: falls back to the id.
    fn label(&self) -> &str;

    /// The provider *instance* id, or empty when nothing is configured.
    fn provider(&self) -> &str;

    /// Empty when no model is configured.
    fn model(&self) -> &str;

    /// Whether a turn can run. False on a fresh install; every other route
    /// works.
    fn configured(&self) -> bool;

    /// The default workspace's tree — the one a request that names none gets.
    fn jail(&self) -> Arc<WorkspaceJail>;

    /// The tree one workspace owns.
    ///
    /// Resolves any legal slug, registry row or not: a *detached* workspace's
    /// sessions must keep reaching their own files. Deciding whether a
    /// workspace is one the caller may still name is the route's job, not this
    /// one's.
    fn jail_for(&self, workspace_id: &str) -> Result<Arc<WorkspaceJail>>;

    /// Sorted by name, as the model is offered them.
    ///
    /// This agent's, not the registry's: an agent with a tool subset must not
    /// be described by a context inspector that lists tools it cannot call.
    fn tools(&self) -> Vec<ToolDefinition>;

    /// This agent's budget, which is what the context meter is measured
    /// against.
    fn context_window_tokens(&self) -> u32;

    /// The prompt a turn on this session would carry, in its two halves.
    ///
    /// Comes from the loop itself rather than being reassembled here, so the
    /// preview and the turn cannot drift.
    fn system_prompt<'a>(
        &'a self,
        input: &'a PromptPreviewInput,
    ) -> BoxFuture<'a, Result<PromptPreview>>;
}

/// One agent, as a picker and the settings screen need it.
///
/// Deliberately not the whole resolved agent: the full settings tree already
/// reaches the client through `GET /api/settings`, and a second, subtly
/// different copy of it is how the two drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSummary {
    /// The agent's id.
    pub id: String,
    /// Never empty: falls back to the id.
    pub label: String,
    /// After inheritance, so a picker shows what a turn would actually use.
    pub model: String,
    /// The provider instance id.
    pub provider: String,
    /// Absent means this agent sends no such parameter, so the provider
    /// decides.
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Counts `GET /api/status` reports for the two pluggable subsystems.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtensionCounts {
    /// MCP servers currently connected.
    pub mcp_servers_connected: u32,
    /// Extensions currently loaded.
    pub extensions_loaded: u32,
}

/// The narrowest request shape the heartbeat's two decisions need.
pub struct DirectChatInput {
    /// Which agent's connection to use. `None` is the default agent.
    pub agent_id: Option<String>,
    /// Overrides the agent's own model — how a cheap heartbeat model is chosen.
    pub model: Option<String>,
    /// The whole conversation for this one request.
    pub messages: Vec<ChatMessage>,
    /// The tools to offer, which for the heartbeat is exactly one.
    pub tools: Vec<ToolDefinition>,
    /// How hard to push the model towards calling one.
    pub tool_choice: ToolChoice,
    /// A ceiling on the answer.
    pub max_tokens: Option<u32>,
    /// Cancels the request.
    pub token: CancellationToken,
}

/// Everything a route reaches for below the transport.
///
/// Implemented by the adapter `ghostai serve` builds over `GhostRuntime`, and
/// by the testkit double the route tests use.
pub trait ServerRuntime: Send + Sync {
    /// The live settings tree. Replaced wholesale by [`Self::apply_settings`].
    fn config(&self) -> Config;

    /// Applies a settings patch, rebuilds what depends on it, and persists it.
    ///
    /// Persistence is the adapter's job, not the runtime's: previewing a patch
    /// and saving one are different operations, and this is the saving one, so
    /// a UI that changes the model and reloads sees the change.
    ///
    /// Returns `Err` without applying anything when the merged settings cannot
    /// be built — an unknown provider, an unusable workspace — leaving the
    /// server answering on the settings that worked a moment ago.
    fn apply_settings(&self, patch: ConfigPatch) -> Result<Config>;

    /// Re-reads `config.json` from disk and rebuilds what depends on it.
    ///
    /// The other direction from [`Self::apply_settings`], which takes a patch
    /// from a client and writes it out. This one takes what the *file* says and
    /// leaves it alone, so it is the answer to every edit a running server
    /// cannot see: a config changed in an editor, an extension dropped in
    /// beside it, an endpoint that came back on a different port.
    ///
    /// Returns `Err` without applying anything when the file cannot be built,
    /// leaving the server on the settings it was already serving.
    fn reload(&self) -> Result<Config>;

    /// Provider id to whether a usable credential exists.
    ///
    /// Booleans, never values. The vault is write-only over HTTP, and this is
    /// the shape that lets a settings panel show "configured" without a key
    /// crossing the network.
    fn credentials_present(&self) -> IndexMap<String, bool>;

    /// Writes or clears one credential. A `None` value deletes the entry.
    fn set_credential(&self, request: &SetCredentialRequest) -> Result<()>;

    /// Set when `config.json` failed to parse and the defaults are in use.
    fn load_error(&self) -> Option<String>;

    /// Settings that parsed but could not be fully honoured. Empty is healthy.
    ///
    /// Distinct from [`Self::load_error`], which means nothing loaded at all.
    /// These are individually addressable — a delegation to an agent that was
    /// deleted, an entry stored under a key that is not a usable id — and the
    /// operator fixes them one at a time.
    fn config_warnings(&self) -> Vec<ConfigWarning> {
        Vec::new()
    }

    /// The conversation store.
    fn store(&self) -> Arc<SessionStore>;

    /// The workspace registry.
    fn workspaces(&self) -> Arc<WorkspaceStore>;

    /// Toolboxes installed on this machine, read fresh.
    ///
    /// A listing rather than a store or a path, for the reason the trait exists
    /// at all: the server should not know where toolboxes live on disk or how
    /// an approval is recorded, only what an operator is allowed to choose
    /// from. Read on every call because a manifest edited after approval stops
    /// being usable the moment it changes.
    fn toolboxes(&self) -> Vec<ToolboxListing> {
        Vec::new()
    }

    /// Independently installed container definitions, read fresh.
    fn containers(&self) -> Vec<ContainerListing> {
        Vec::new()
    }

    /// Operator lifecycle controls, never arbitrary tool execution.
    fn sandbox_request(
        &self,
        _request: serde_json::Value,
    ) -> BoxFuture<'_, Result<serde_json::Value>> {
        Box::pin(async {
            Err(ghostai_core::GhostError::new(
                ghostai_core::ErrorKind::Config,
                "Sandbox service is not configured",
            ))
        })
    }

    /// Forgets whatever is cached against one workspace id.
    ///
    /// There is exactly one caller: the folder move behind `PATCH
    /// /api/workspaces/:id`. A jail canonicalises its root once, when it is
    /// built, so an entry keyed on an id whose directory has just been renamed
    /// away holds a path that is no longer there — and would be handed to the
    /// *next* workspace created on that freed folder name.
    ///
    /// Defaulted because it is a cache detail rather than a capability: a
    /// runtime that keeps no jails has nothing to forget.
    fn release_workspace(&self, workspace_id: &str) {
        let _ = workspace_id;
    }

    /// One agent's view. `None` is the default agent.
    ///
    /// Takes an id because tools, prompt and context window all differ per
    /// agent — a status panel describing the default while a session runs on
    /// another would be describing something that is not happening.
    ///
    /// Returns `Err` for an id that names nothing runnable, which is a 404 at
    /// the route.
    fn agent(&self, agent_id: Option<&str>) -> Result<Arc<dyn AgentView>>;

    /// Every tool the registry holds, whoever may call it.
    ///
    /// The catalogue, not a grant. [`AgentView::tools`] is one agent's
    /// *advertised* subset — what a turn on that agent would send — and the two
    /// are different questions. Answering this one with the default agent's
    /// subset would leave the agent editor offering a tool only if that agent
    /// already held it, and `automation` is absent from its tools by design: the
    /// one tool nobody starts with would be the one tool nobody could grant.
    ///
    /// Sorted by name, as the registry keeps it.
    fn registered_tools(&self) -> Vec<ToolDefinition>;

    /// Every agent that can run a turn, the default one first.
    fn agents(&self) -> Vec<AgentSummary>;

    /// Counts of the two pluggable subsystems.
    fn extensions(&self) -> ExtensionCounts;

    /// The models to offer, fetched from the endpoints that can list them.
    ///
    /// `None` means this runtime has nothing to ask, and the routes fall back
    /// to what the settings tree names — which is honest: a model an operator
    /// typed into `providers.<id>.models` is a model they intend to use.
    ///
    /// `refresh` discards whatever the implementation cached. A page load must
    /// not reach every configured endpoint on every render, and an operator who
    /// has just pulled a new model must not have to wait out a TTL to see it.
    fn models(&self, refresh: bool) -> Option<BoxFuture<'_, Result<ModelsResponse>>> {
        let _ = refresh;
        None
    }

    /// The channels this build ships, and what each is doing.
    ///
    /// A method rather than a field, and that matters here more than elsewhere:
    /// the composition root builds the channel manager after it builds this
    /// port, and rebuilds it whenever the settings that configure it are saved.
    /// A snapshot taken at construction would report the manager that has since
    /// been replaced.
    fn channels(&self) -> Vec<ChannelStatus> {
        Vec::new()
    }

    /// Whether one connection can be reached, and with which models.
    ///
    /// Answers rather than fails, because every outcome here is a *result*:
    /// "the key was rejected" and "nothing is listening there" are the two
    /// things the operator came to find out, so they travel as a `reason` on a
    /// 200 rather than as an error envelope the client would have to unpick.
    fn test_provider<'a>(
        &'a self,
        request: &'a ProviderTestRequest,
    ) -> Option<BoxFuture<'a, Result<ProviderTestResponse>>> {
        let _ = request;
        None
    }

    /// Every configured MCP server and the state it is actually in.
    ///
    /// Live state, which is why it is not on [`Self::config`]: "unreachable
    /// since 12:04" is not something to write into `config.json`, and the
    /// settings tree is what gets written back.
    fn mcp_servers(&self) -> Vec<McpServerStatus> {
        Vec::new()
    }

    /// Every installed extension and the state it is actually in.
    fn extension_statuses(&self) -> Vec<ExtensionStatus> {
        Vec::new()
    }

    /// Records the digest of an extension's files as approved, and loads it.
    ///
    /// A route rather than a settings patch, because an approval is not
    /// configuration: it is a statement about the exact bytes on disk right
    /// now, and writing it into `config.json` would make it survive an edit to
    /// the very files it was about.
    ///
    /// `None` means this build has no extension host, which is a 404.
    fn approve_extension<'a>(&'a self, id: &'a str) -> Option<BoxFuture<'a, Result<()>>> {
        let _ = id;
        None
    }

    /// Forgets an extension's approval and unloads it.
    fn revoke_extension<'a>(&'a self, id: &'a str) -> Option<BoxFuture<'a, Result<()>>> {
        let _ = id;
        None
    }

    /// Every slash command extensions contribute.
    ///
    /// Served rather than compiled into each surface, which the built-in
    /// command tables are: there is one definition of an extension's command
    /// and more than one place it has to appear.
    fn commands(&self) -> Vec<ExtensionCommand> {
        Vec::new()
    }

    /// Runs one contributed command.
    fn run_command<'a>(
        &'a self,
        id: &'a str,
        request: &'a RunCommandRequest,
        token: CancellationToken,
    ) -> Option<BoxFuture<'a, Result<RunCommandResponse>>> {
        let _ = (id, request, token);
        None
    }

    /// One provider request that is **not** a turn.
    ///
    /// The one caller is the heartbeat's forced `skip | run` decision — a
    /// single request carrying one tool and no history, whose answer decides
    /// whether an expensive turn happens at all. Everything a turn gets is
    /// bypassed here: the tool registry never learns the tool exists, no
    /// approval is asked, no history is windowed and no turn-stats row is
    /// written. That is right for a classification and wrong for work, so a
    /// second caller reaching for this is a sign it actually wants a turn.
    fn chat(&self, input: DirectChatInput) -> Option<BoxFuture<'_, Result<ChatResult>>> {
        let _ = input;
        None
    }
}
