//! REST DTOs.
//!
//! These are the shapes the OpenAPI document is generated from, so the API
//! reference cannot drift from the routes it documents.
//!
//! Cursor pagination for anything read sequentially: sessions and messages are
//! append-only, so an offset shifts under a reader whenever a turn lands. The
//! three listings a numbered pager also reads — sessions, automation runs,
//! notifications — accept an `offset` as well, and carry a `total` a page of
//! rows cannot supply. The two modes are alternatives; see [`PaginationQuery`].

use std::borrow::Cow;

use garde::Validate;
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::automation::{AutomationJob, AutomationRun};
use crate::config::EnvironmentNetwork;
use crate::config::{
    AgentsConfigPatch, ChannelsConfigPatch, ExtensionsConfigPatch, ProviderConfigPatch,
    SchedulerConfigPatch, ServerConfigPatch, ToolsConfigPatch, UiConfigPatch,
};
use crate::config::{Config, ConfigPatch, McpTransport, ReasoningEffort};
use crate::environment::{ContainerLimits, ContainerRuntime, EnvironmentKind};
use crate::extension::ExtensionContribution;
use crate::json::{MAX_SAFE_INTEGER, Nullable, True, positive, yes};
use crate::messages::{StopReason, StoredMessage, Usage};
use crate::subagent::SubagentRunRef;
use crate::tools::ToolDefinition;
use crate::ws::NotificationLevel;

// Envelopes

/// The body of every non-2xx response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ErrorBody {
    /// A code from the same vocabulary as the WebSocket `error` event.
    #[garde(length(utf16, min = 1))]
    pub code: String,
    /// For a person.
    pub message: String,
    /// Field-level detail for a 422, keyed by JSON pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<IndexMap<String, Value>>,
}

/// The single error shape for every non-2xx response, so a client has one
/// branch to write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ErrorResponse {
    /// The error.
    #[garde(dive)]
    pub error: ErrorBody,
}

/// How a client asks for one page, in either of two ways.
///
/// **`cursor` and `offset` are alternatives, never a pair.** A cursor addresses
/// a position in the sort order and an offset counts rows from the top, so
/// sending both asks for a page relative to a page; the endpoints refuse the
/// combination. A sequential reader — the sidebar, an infinite scroll — wants
/// `cursor`, because these tables move under it; a numbered pager wants
/// `offset`, because "page 7" cannot be expressed as a position it has not
/// visited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct PaginationQuery {
    /// Rows per page.
    #[serde(default = "default_limit")]
    #[garde(range(min = 1, max = 200))]
    #[schemars(transform = positive)]
    pub limit: u64,
    /// Opaque; echo back `next_cursor` verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Rows to skip from the top. Mutually exclusive with `cursor`.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub offset: u64,
}

fn default_limit() -> u64 {
    50
}

impl Default for PaginationQuery {
    fn default() -> Self {
        Self {
            limit: default_limit(),
            cursor: None,
            offset: 0,
        }
    }
}

// Status

/// `GET /api/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct StatusResponse {
    /// The build.
    pub version: String,
    /// The wire protocol.
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub protocol_version: u64,
    /// Since boot.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub uptime_ms: u64,
    /// Resolved, not configured — what a turn would actually use now. Empty
    /// when nothing is configured yet; `configured` is the flag to branch on.
    pub model: String,
    /// The provider instance a turn would use.
    pub provider: String,
    /// Whether a turn can run at all. `false` on a fresh install: everything
    /// but chat is up until a provider and a model exist.
    pub configured: bool,
    /// The default workspace's id, never its path. An absolute host path
    /// would tell every authenticated client the operator's username and
    /// directory layout — the one string that turns a blind traversal attempt
    /// into a targeted one.
    #[garde(length(utf16, min = 1))]
    pub workspace_id: String,
    /// How many workspaces exist.
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub workspace_count: u64,
    /// Whether a credential is required.
    pub auth_enabled: bool,
    /// Registered tools.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub tool_count: u64,
    /// Connected MCP servers.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub mcp_servers_connected: u64,
    /// Running extensions.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub extensions_loaded: u64,
}

/// How one health check went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthCheckStatus {
    /// Fine.
    Ok,
    /// Worth a look.
    Warn,
    /// Broken.
    Fail,
    /// Not applicable here.
    Skipped,
}

/// One line of `ghostai doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct HealthCheck {
    /// What was checked.
    #[garde(length(utf16, min = 1))]
    pub name: String,
    /// How it went.
    pub status: HealthCheckStatus,
    /// Why.
    #[serde(default)]
    pub detail: String,
}

/// The overall verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// Fine.
    Ok,
    /// Something warned.
    Degraded,
    /// Something failed.
    Fail,
}

/// `GET /api/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct HealthResponse {
    /// The verdict.
    pub status: HealthStatus,
    /// Every check.
    #[garde(dive)]
    pub checks: Vec<HealthCheck>,
}

// Settings

/// Something the settings say that could not be honoured, but did not stop
/// the install from running.
///
/// The counterpart to a config error, which refuses the whole tree. An agent id
/// is user-authored and deletable, so a reference to one that has gone has to
/// be survivable — and the only alternative to a warning is discarding it
/// silently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ConfigWarning {
    /// What kind of problem.
    #[garde(length(utf16, min = 1))]
    pub code: String,
    /// For a person.
    pub message: String,
    /// The agent the warning is about, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// One channel, as the settings panel needs to see it.
///
/// Four fields rather than one, because "is my bot working" has four distinct
/// answers. `configured` exists because the vault is write-only over HTTP: the
/// panel can never read a token back, so a boolean is the only way it can say
/// "a token is saved" instead of showing an empty box over a working bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ChannelStatus {
    /// The channel id, which is also its `config.channels` key.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// What the settings say.
    pub enabled: bool,
    /// A credential is stored. Never the credential itself.
    pub configured: bool,
    /// The channel is connected right now.
    pub running: bool,
    /// The bot's username when connected, or why it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Config as served to the UI. Credentials never appear — the vault is
/// write-only over HTTP — so the panel gets a per-provider boolean instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SettingsResponse {
    /// The whole tree.
    #[garde(dive)]
    pub config: Config,
    /// Provider *instance* id → whether a usable key exists in the vault.
    pub credentials_present: IndexMap<String, bool>,
    /// Channels this build ships, whether configured or not. A separate field
    /// rather than more keys in `credentials_present`, which is indexed by
    /// provider instance id and would collide.
    #[serde(default)]
    #[garde(dive)]
    pub channels: Vec<ChannelStatus>,
    /// Set when the file on disk failed to parse and defaults are in use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_error: Option<String>,
    /// Non-fatal problems found resolving the settings. Empty is healthy. A
    /// sibling of `load_error` rather than a widening of it: that means the
    /// file did not parse *at all*, which is one string and one alert.
    #[serde(default)]
    #[garde(dive)]
    pub warnings: Vec<ConfigWarning>,
}

/// One agent moving to a new id, as part of a settings save.
///
/// A rename travels *with* the patch rather than through a route of its own
/// because it is not separable from one: the editor's Save can change an
/// agent's id and its model in the same gesture, and as two requests the first
/// can land and the second fail. And a patch alone cannot say which of two
/// things a key move *means* — `{reviewer: null, "code-review": {...}}`
/// describes a rename and a delete-and-create equally well, and the two are
/// opposites for the conversations bound to the old id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentRename {
    /// The current id.
    #[garde(length(utf16, min = 1))]
    pub from: String,
    /// The new id.
    #[garde(length(utf16, min = 1))]
    pub to: String,
}

/// The body of `PATCH /api/settings`: a config patch, plus what it means.
///
/// The [`ConfigPatch`] fields restated rather than nested, because the
/// patch refuses unknown keys and so must this — serde cannot flatten into a
/// type that does. `rename_agents` is not config and is never stored; it is
/// read and discarded by the route.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct SettingsPatchRequest {
    /// See [`ConfigPatch::agents`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub agents: Option<AgentsConfigPatch>,
    /// See [`ConfigPatch::providers`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "IndexMap<String, Nullable<ProviderConfigPatch>>")]
    #[garde(custom(crate::json::validate_optional_map_options))]
    pub providers: Option<IndexMap<String, Option<ProviderConfigPatch>>>,
    /// See [`ConfigPatch::server`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub server: Option<ServerConfigPatch>,
    /// See [`ConfigPatch::tools`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub tools: Option<ToolsConfigPatch>,
    /// See [`ConfigPatch::channels`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub channels: Option<ChannelsConfigPatch>,
    /// See [`ConfigPatch::scheduler`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub scheduler: Option<SchedulerConfigPatch>,
    /// See [`ConfigPatch::extensions`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub extensions: Option<ExtensionsConfigPatch>,
    /// See [`ConfigPatch::ui`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub ui: Option<UiConfigPatch>,
    /// See [`ConfigPatch::workspace`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Applied *before* the patch, so the patch addresses the new ids. A list,
    /// because there is no reason for the route to stop an operator renaming
    /// two agents in one save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub rename_agents: Option<Vec<AgentRename>>,
}

impl SettingsPatchRequest {
    /// The config patch and the renames, separated.
    pub fn into_parts(self) -> (ConfigPatch, Vec<AgentRename>) {
        let patch = ConfigPatch {
            agents: self.agents,
            providers: self.providers,
            server: self.server,
            tools: self.tools,
            channels: self.channels,
            scheduler: self.scheduler,
            extensions: self.extensions,
            ui: self.ui,
            workspace: self.workspace,
        };
        (patch, self.rename_agents.unwrap_or_default())
    }
}

/// Where a credential lives in the vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialNamespace {
    /// A provider instance's key.
    Providers,
    /// A tool's key.
    Tools,
    /// Speech services.
    Audio,
    /// An MCP server's key.
    McpServers,
    /// An extension's own credential, keyed by extension id.
    Extensions,
    /// A channel's own credential — a bot token, keyed by channel id.
    Channels,
}

/// Write-only credential update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SetCredentialRequest {
    /// Where it lives.
    pub namespace: CredentialNamespace,
    /// The key within the namespace.
    #[garde(length(utf16, min = 1))]
    pub key: String,
    /// The secret. `null` deletes the entry.
    #[schemars(with = "Nullable<String>")]
    pub value: Option<String>,
}

// Providers and models

/// A provider *type*: the catalogue an operator adds an endpoint from.
///
/// It carries no credential flag. A credential belongs to a configured
/// instance — two Ollama entries can have different tokens — so the boolean
/// lives on [`ProviderInstanceInfo`] and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent facts about a provider type"
)]
pub struct ProviderInfo {
    /// The registry id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// For a person.
    pub display_name: String,
    /// Which wire adapter drives it.
    pub wire: String,
    /// Runs on this machine.
    pub is_local: bool,
    /// Fronts other providers.
    pub is_gateway: bool,
    /// Authenticates with OAuth rather than a key.
    pub is_o_auth: bool,
    /// Where it lives when the operator names nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_api_base: Option<String>,
    /// The environment variable a key may be read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_key: Option<String>,
    /// The endpoint can be asked for its own model list.
    pub supports_model_listing: bool,
}

/// One configured endpoint. `type` names the [`ProviderInfo`] it was created
/// from; `id` is the operator's key for this particular endpoint, what an
/// agent's `provider` names and what the vault stores its credential under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "six independent facts about an instance"
)]
pub struct ProviderInstanceInfo {
    /// The instance id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// The provider type.
    #[serde(rename = "type")]
    #[garde(length(utf16, min = 1))]
    pub kind: String,
    /// Resolved for display: the instance's label, or the type's name.
    pub display_name: String,
    /// Effective, not configured — the default is folded in.
    pub api_base: String,
    /// Runs on this machine.
    pub is_local: bool,
    /// Fronts other providers.
    pub is_gateway: bool,
    /// Authenticates with OAuth.
    pub is_o_auth: bool,
    /// The environment variable a key may be read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_key: Option<String>,
    /// Whether resolution considers it.
    pub enabled: bool,
    /// The endpoint can be asked for its own model list.
    pub supports_model_listing: bool,
    /// A key is in the vault.
    pub credentials_present: bool,
}

/// `GET /api/providers`: both lists, because the panel needs both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ProvidersResponse {
    /// What an "Add provider" control offers.
    #[garde(dive)]
    pub types: Vec<ProviderInfo>,
    /// What the list below it renders.
    #[garde(dive)]
    pub instances: Vec<ProviderInstanceInfo>,
}

/// One model an instance offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ModelInfo {
    /// The model id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// The provider *instance* this model was offered by.
    #[garde(length(utf16, min = 1))]
    pub provider_id: String,
    /// The instance's type, for grouping and labelling. Absent on a bare list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_type: Option<String>,
    /// For a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The window it advertises.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub context_window_tokens: Option<u64>,
    /// Whether it can call tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
    /// Whether it can see images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    /// Whether it can think.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning: Option<bool>,
}

/// `GET /api/models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ModelsResponse {
    /// Every model every instance offered.
    #[garde(dive)]
    pub models: Vec<ModelInfo>,
    /// Instances whose model list could not be fetched, id → reason.
    #[serde(default)]
    pub errors: IndexMap<String, String>,
}

/// "Can this endpoint be talked to?", asked of a connection rather than of a
/// stored instance — the Add-provider dialog probes what the operator has
/// typed, before there is anything to store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ProviderTestRequest {
    /// A provider registry id.
    #[serde(rename = "type")]
    #[garde(length(utf16, min = 1))]
    pub kind: String,
    /// Empty means the type's own default endpoint.
    #[serde(default)]
    pub api_base: String,
    /// Headers sent with every request.
    #[serde(default)]
    pub extra_headers: IndexMap<String, String>,
    /// The key to probe *with*. Omitted means whatever is already stored for
    /// `instance_id`, which is how a saved row re-tests without the client
    /// ever having held the credential. Never echoed back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// The instance being tested, when one exists. Only used to find a key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
}

/// The result of one probe. `reason` is the field that matters: the
/// difference between `auth` and `transport` is two completely different
/// things for an operator to go and fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ProviderTestResponse {
    /// Whether it answered.
    pub ok: bool,
    /// Model ids the endpoint listed. Empty when `ok` is false.
    #[serde(default)]
    pub models: Vec<String>,
    /// A provider error reason, or `unsupported` when nothing could be asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// For a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// Sessions

/// A conversation, as a listing shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SessionSummary {
    /// The session key.
    #[garde(length(utf16, min = 1))]
    pub key: String,
    /// The title.
    pub title: String,
    /// How many messages it holds.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub message_count: u64,
    /// When it was created.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub created_at_ms: u64,
    /// When it last changed.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub updated_at_ms: u64,
    /// Channel that owns it — `web`, `telegram`, `automation`, an extension id.
    #[serde(default = "default_origin")]
    pub origin: String,
    /// The workspace this session's tools run in. Set at creation and moved
    /// only by `PATCH /api/sessions/:key`.
    #[serde(default = "default_workspace")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: String,
    /// The agent it is bound to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Everything it has cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub total_usage: Option<Usage>,
}

fn default_origin() -> String {
    "web".to_owned()
}

fn default_workspace() -> String {
    crate::ids::DEFAULT_WORKSPACE_ID.to_owned()
}

/// `GET /api/sessions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SessionListResponse {
    /// This page.
    #[garde(dive)]
    pub sessions: Vec<SessionSummary>,
    /// Where the next page starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Every session the filter matches, not the length of `sessions`.
    /// Required rather than optional: an optional total is a field every
    /// client has to branch on before it can render anything, to save one
    /// count over an indexed column.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub total: u64,
}

/// `GET /api/sessions/:key/messages`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SessionMessagesResponse {
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// This page.
    #[garde(dive)]
    pub messages: Vec<StoredMessage>,
    /// Where the next page starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// The delegations this history contains, by the call that made each. A
    /// subagent's steps live in the subagent's own session, so this is the one
    /// thing in a transcript the rows cannot describe.
    #[serde(default)]
    #[garde(custom(crate::json::validate_map_values))]
    pub subagent_runs: IndexMap<String, SubagentRunRef>,
    /// Why each failed turn failed, by turn id. A failed turn appends nothing,
    /// so a rebuilt transcript would otherwise show the question, no answer,
    /// and no indication anything went wrong.
    #[serde(default)]
    pub failures: IndexMap<String, String>,
}

/// `POST /api/sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct CreateSessionRequest {
    /// The key. The server generates one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub key: Option<String>,
    /// The title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Which workspace to open the conversation in. Defaults to `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: Option<String>,
    /// The agent to bind it to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// `PATCH /api/sessions/:key`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct UpdateSessionRequest {
    /// A new title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub title: Option<String>,
    /// A new agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Moves the conversation to another workspace. The only path that moves
    /// one: a socket frame naming a workspace can only ever *create*, so a
    /// crafted frame cannot point an open conversation's tools at another
    /// workspace's files. Takes effect from the next turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: Option<String>,
}

/// What the agent would actually send to the model, for the context
/// inspector — the panel that makes the token budget legible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ContextResponse {
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The cached prefix: the system message, without the per-iteration tail.
    pub system_prompt: String,
    /// The trailing turn the loop appends after the history — live state, the
    /// turn's delimiter, a correction. Separate from `system_prompt` because
    /// the two are billed differently: this is the only section re-read at
    /// full price on every iteration.
    #[serde(default)]
    pub runtime_block: String,
    /// The definitions as the provider would receive them, so the `tools` row
    /// can be opened.
    #[serde(default)]
    #[garde(dive)]
    pub tools: Vec<ToolDefinition>,
    /// The history.
    #[garde(dive)]
    pub messages: Vec<StoredMessage>,
    /// What the next request would cost.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub estimated_tokens: u64,
    /// The window it has to fit.
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub context_window_tokens: u64,
    /// Section name → token cost, so an oversized block is visible.
    #[serde(default)]
    pub breakdown: IndexMap<String, f64>,
    /// The agent these figures describe — the one a turn would actually run
    /// on, which is not always the session's binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Set only when the binding did not resolve, naming what it asked for.
    /// Absent is the healthy state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_agent_id: Option<String>,
}

/// What one turn cost, recorded when it ended.
///
/// Fetched rather than streamed, because a conversation you did not watch
/// happen has no live events to have carried it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct TurnStats {
    /// The turn.
    #[garde(length(utf16, min = 1))]
    pub turn_id: String,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The agent that ran it.
    #[serde(default)]
    pub agent_id: String,
    /// Which workspace this turn ran in — the files it could actually reach.
    /// Not the session's current workspace: a conversation can be moved.
    #[serde(default = "default_workspace")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: String,
    /// The provider instance.
    pub provider: String,
    /// The model.
    pub model: String,
    /// When it started.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub started_at_ms: u64,
    /// When it ended.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub ended_at_ms: u64,
    /// Tool round-trips.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub iterations: u64,
    /// Why it stopped.
    pub stop_reason: StopReason,
    /// What it cost.
    #[garde(dive)]
    pub usage: Usage,
    /// Time spent generating. Absent on any turn recorded before it was
    /// measured, which is what the rate's fallback to the wall clock is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub generation_ms: Option<u64>,
    /// The tokens produced inside `generation_ms`, and only those.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub generation_tokens: Option<u64>,
    /// How long it waited to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub first_token_ms: Option<u64>,
    /// Why it stopped, when `stop_reason` is `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `GET /api/sessions/:key/turns`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct TurnStatsResponse {
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// Every recorded turn.
    #[garde(dive)]
    pub turns: Vec<TurnStats>,
}

/// Fork a conversation at a point. REST rather than a socket frame: this
/// creates a resource and starts no turn, and the caller needs the new key
/// back to navigate to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct BranchSessionRequest {
    /// Copy everything at or below this `seq`. `0` forks an empty
    /// conversation.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub seq: u64,
    /// The new key. The server generates one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub key: Option<String>,
    /// The new title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

// Agents

/// One agent, as a picker needs it.
///
/// Deliberately thin. The full settings tree already reaches the client
/// through `GET /api/settings`; what this adds is the part settings cannot
/// answer: the model and effort a turn would actually use, after any
/// process-wide pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentSummary {
    /// The id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// Never empty: falls back to the id.
    #[garde(length(utf16, min = 1))]
    pub label: String,
    /// The model a turn would use.
    pub model: String,
    /// The provider instance a turn would use.
    pub provider: String,
    /// The effort in force, absent when this agent states none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// `GET /api/agents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentListResponse {
    /// The default agent first, then the operator's own order.
    #[garde(dive)]
    pub agents: Vec<AgentSummary>,
}

// Tools

/// `GET /api/tools`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolListResponse {
    /// Every registered tool.
    #[garde(dive)]
    pub tools: Vec<ToolDefinition>,
}

/// `GET /api/environments`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct EnvironmentListResponse {
    /// Independently selectable execution environments.
    #[garde(dive)]
    pub environments: Vec<EnvironmentSummary>,
}

/// One installed environment definition.
///
/// Carries everything an operator weighs before selecting one for an agent:
/// the image, who it runs as, what it is allowed to spend, and whether any
/// hardening was switched off. `gatewayProblem` is the sentence a restricted
/// egress request would fail with, resolved once here so the editor can warn
/// while the network is still being chosen rather than on save.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct EnvironmentSummary {
    /// Operator-installed identifier.
    pub name: String,
    /// What kind of place this is.
    #[serde(default)]
    pub kind: EnvironmentKind,
    /// The definition's prompt section, so the agent editor can show what an
    /// agent inherits before it decides whether to override it.
    #[serde(default)]
    pub prompt: String,
    /// Immutable image reference.
    pub image: String,
    /// Reused across agents and conversations in one workspace.
    pub shared: bool,
    /// The OCI runtime.
    pub runtime: ContainerRuntime,
    /// Where the workspace is mounted.
    pub workdir: String,
    /// `uid:gid` inside.
    pub user: String,
    /// The resource budget.
    #[garde(dive)]
    pub limits: ContainerLimits,
    /// Capabilities added to the otherwise dropped set.
    pub caps_added: Vec<String>,
    /// Non-default hardening choices, named so they can be shown as warnings.
    pub weakened: Vec<String>,
    /// Why a restricted egress request could not be honoured here, when it
    /// could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_problem: Option<String>,
    /// Why selection is unavailable, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// One container managed by the isolated environment service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxInstanceSummary {
    /// Opaque lifecycle identifier.
    pub id: String,
    /// Registered workspace identifier.
    pub workspace: String,
    /// Approved environment definition backing the instance.
    pub environment: String,
    /// Shared across authorized agents and conversations in this workspace.
    pub shared: bool,
    /// Active operations.
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub busy: u64,
    /// Last activity on the service clock.
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub last_used_ms: u64,
    /// Agents that have resolved this instance.
    #[serde(default)]
    pub agents: Vec<String>,
}

/// `GET /api/sandboxes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxListResponse {
    /// Live instances only.
    pub instances: Vec<SandboxInstanceSummary>,
}

/// Everything the app may ask the sandbox service for.
///
/// One type for the whole boundary rather than one for the socket and another
/// for `POST /api/sandboxes`: the HTTP route deserialises this, refuses the
/// variants an operator may not send, and forwards the same value. Two enums
/// meant a field could be added to one and silently dropped re-serialising
/// through the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub enum SandboxRequest {
    /// Probe the service and container engine.
    Health,
    /// Live instances.
    List,
    /// Run one guarded command. Refused over HTTP because a model's tool call
    /// reaches the service through the agent loop. It carries argv rather than a plan: `cwd` and the
    /// resolved environment of the caller's process mean nothing inside a
    /// environment, and the service re-guards regardless. The caller guards
    /// first so a refusal reaches the model quickly; the service guards again
    /// and its answer is the one that counts.
    #[serde(rename_all = "camelCase")]
    Exec {
        /// Installed environment to run it in.
        environment: String,
        /// Registered workspace ID.
        workspace: String,
        /// The calling agent.
        agent: String,
        /// The conversation.
        session: String,
        /// Program and arguments, already split. Never a shell string.
        argv: Vec<String>,
        /// Requested wall-clock ceiling, clamped service-side. 0 is no limit.
        #[serde(default)]
        #[garde(range(max = MAX_SAFE_INTEGER))]
        timeout_ms: u64,
        /// Requested output ceiling, clamped service-side.
        #[serde(default)]
        #[garde(range(max = MAX_SAFE_INTEGER))]
        max_output_bytes: u64,
        /// What the agent's environment may reach.
        #[serde(default)]
        #[schemars(transform = crate::json::prefault)]
        network: EnvironmentNetwork,
    },
    /// Warm an approved environment.
    Start {
        /// Approved environment name.
        environment: String,
        /// Registered workspace ID.
        workspace: String,
        /// Audit identity.
        agent: String,
        /// Audit/session identity.
        session: String,
        /// What the instance may reach. Part of its identity: two agents
        /// asking for different egress never share one container.
        #[serde(default)]
        #[schemars(transform = crate::json::prefault)]
        network: EnvironmentNetwork,
    },
    /// Stop an instance.
    Stop {
        /// Opaque instance ID.
        instance: String,
    },
    /// Restart an instance.
    Restart {
        /// Opaque instance ID.
        instance: String,
    },
}

// MCP servers

/// Where one configured MCP server is right now.
///
/// A *live* state, which is why it is here and not in the settings tree: the
/// config says what should be connected, and this says what is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpServerState {
    /// Connecting.
    Connecting,
    /// Connected and advertising tools.
    Ready,
    /// Waiting for the operator to complete OAuth.
    NeedsAuthorization,
    /// Could not connect.
    Failed,
    /// Switched off in the settings.
    Disabled,
}

/// One MCP server's live state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct McpServerStatus {
    /// The config key.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// Resolved, so a config that left it to inference still reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<McpTransport>,
    /// Where it is.
    pub state: McpServerState,
    /// What the settings say.
    pub enabled: bool,
    /// The flattened names the model sees. Sorted.
    #[serde(default)]
    pub tools: Vec<String>,
    /// What the server advertises and `enabled_tools` filtered out.
    #[serde(default)]
    pub filtered_tools: Vec<String>,
    /// What the server calls itself.
    #[serde(default)]
    pub server_name: String,
    /// Its version.
    #[serde(default)]
    pub server_version: String,
    /// Why it is not connected, phrased for the operator. A field on the row
    /// rather than a config warning, because those are properties of the
    /// settings tree and a closed laptop is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When it last connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub last_connected_at_ms: Option<u64>,
    /// Where the operator must go while `state` is `needs_authorization`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    /// Problems that did not stop the server working.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// `GET /api/mcp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct McpStatusResponse {
    /// Every configured server.
    #[garde(dive)]
    pub servers: Vec<McpServerStatus>,
}

// Extensions

/// What an extension is doing right now.
///
/// Four of the five are reasons it is *not* running, and each is distinct
/// because each has a different fix: approve it, re-approve it, enable it, or
/// repair it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionState {
    /// Loaded and activated.
    Ready,
    /// Discovered, never approved.
    Unapproved,
    /// Approved once; the bytes on disk have changed since.
    Drifted,
    /// Named in `extensions.disabled`.
    Disabled,
    /// Approved and enabled, and it failed to start.
    Failed,
}

/// One extension's live state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ExtensionStatus {
    /// The id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// Where it is.
    pub state: ExtensionState,
    /// The manifest's version.
    #[serde(default)]
    pub version: String,
    /// For a person.
    #[serde(default)]
    pub label: String,
    /// One sentence.
    #[serde(default)]
    pub description: String,
    /// What the manifest declares. Empty on an extension that failed to parse.
    #[serde(default)]
    pub contributes: Vec<ExtensionContribution>,
    /// The tool names it registered, flattened and sorted.
    #[serde(default)]
    pub tools: Vec<String>,
    /// The channels it registered.
    #[serde(default)]
    pub channels: Vec<String>,
    /// The providers it registered.
    #[serde(default)]
    pub providers: Vec<String>,
    /// The commands it registered.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Present once it has been approved, so the panel can show what it holds.
    #[serde(default)]
    pub digest: String,
    /// When it was approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub approved_at_ms: Option<u64>,
    /// Why it is not running, phrased for the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Problems that did not stop it loading.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// `GET /api/extensions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ExtensionListResponse {
    /// Every discovered extension.
    #[garde(dive)]
    pub extensions: Vec<ExtensionStatus>,
}

/// A slash command an extension contributes.
///
/// Fetched rather than compiled in, because there is exactly one definition
/// of it and more than one place it has to appear; and it answers with text
/// rather than a resource key, since its copy ships with the extension and
/// never reaches a locale bundle. Two surfaces, not three: the composer and the
/// terminal reach these, Telegram does not, because its command names cannot
/// spell a namespaced `slack-post`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ExtensionCommand {
    /// `<extensionId>` or `<extensionId>-<suffix>`.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// The extension that owns it.
    #[garde(length(utf16, min = 1))]
    pub extension_id: String,
    /// The line the autocomplete shows.
    #[serde(default)]
    pub description: String,
    /// What to write after the name, in prose. Empty means it takes none.
    #[serde(default)]
    pub args_hint: String,
}

/// `GET /api/commands`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct CommandListResponse {
    /// Every contributed command.
    #[garde(dive)]
    pub commands: Vec<ExtensionCommand>,
}

/// `POST /api/commands/:id`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct RunCommandRequest {
    /// Everything the operator typed after the command name.
    #[serde(default)]
    pub args: String,
    /// The conversation it was typed in, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
}

/// What a command answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct RunCommandResponse {
    /// What to show the operator, verbatim. Not a resource key: an
    /// extension's copy ships with the extension.
    #[serde(default)]
    pub message: String,
    /// `false` renders the message as an error rather than a note.
    #[serde(default = "yes")]
    pub ok: bool,
}

impl Default for RunCommandResponse {
    fn default() -> Self {
        Self {
            message: String::new(),
            ok: true,
        }
    }
}

// Files

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct FileEntry {
    /// Workspace-relative, always. Absolute paths never cross this boundary.
    #[garde(length(utf16, min = 1))]
    pub path: String,
    /// The last segment.
    #[garde(length(utf16, min = 1))]
    pub name: String,
    /// Whether it is a directory.
    pub is_directory: bool,
    /// Size on disk.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub size_bytes: u64,
    /// Modification time.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub modified_at_ms: u64,
    /// The guessed media type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// `GET /api/files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct FileListResponse {
    /// The directory listed.
    pub path: String,
    /// Its entries.
    #[garde(dive)]
    pub entries: Vec<FileEntry>,
}

/// An HMAC-signed, expiring URL.
///
/// `<img src>` cannot carry an Authorization header, and making the file
/// endpoint public would turn it into anonymous read access to everything
/// under the workspace. A short-lived signature satisfies the browser instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SignedUrl {
    /// The URL.
    #[garde(length(utf16, min = 1))]
    pub url: String,
    /// When it stops working.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub expires_at_ms: u64,
}

/// Asking for a signed URL. A body rather than a query parameter, because a
/// workspace path in a URL is written to every access log between the browser
/// and the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SignedUrlRequest {
    /// Workspace-relative.
    #[garde(length(utf16, min = 1))]
    pub path: String,
    /// Which workspace the path is relative to. Defaults to `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: Option<String>,
}

/// `POST /api/files/upload`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct UploadResponse {
    /// Where it landed, workspace-relative.
    #[garde(length(utf16, min = 1))]
    pub path: String,
    /// Size on disk.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub size_bytes: u64,
    /// The media type.
    pub mime_type: String,
    /// A URL a browser can draw it from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub signed_url: Option<SignedUrl>,
}

/// One text file, as an editor needs it: the characters in a JSON string,
/// which render in a text area and execute nowhere, and `modified_at_ms`,
/// which is what makes a save conflict detectable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct FileTextResponse {
    /// Workspace-relative.
    #[garde(length(utf16, min = 1))]
    pub path: String,
    /// The text.
    pub content: String,
    /// The file's size on disk. Larger than `content` when `truncated`.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub size_bytes: u64,
    /// Modification time.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub modified_at_ms: u64,
    /// The file was longer than the read limit and `content` is a prefix. An
    /// editor that saved a prefix would delete the rest of the file, so this
    /// is the flag that makes the panel read-only.
    pub truncated: bool,
}

/// Saving a text file.
///
/// `expected_modified_at_ms` is the reason this is not just a `PUT` of the
/// body: the workspace is a tree a model writes to while a person is looking
/// at it, so "the agent rewrote the file under the open editor" is ordinary.
/// Sending back the timestamp the editor loaded turns that into a 409 the
/// panel can explain. Absent means "write it regardless", which is what
/// creating a new file is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct FileWriteRequest {
    /// Workspace-relative.
    #[garde(length(utf16, min = 1))]
    pub path: String,
    /// The new text.
    pub content: String,
    /// Which workspace the path is relative to. Defaults to `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: Option<String>,
    /// The modification time the editor loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub expected_modified_at_ms: Option<u64>,
}

/// `POST /api/files/mkdir`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct CreateDirectoryRequest {
    /// Workspace-relative.
    #[garde(length(utf16, min = 1))]
    pub path: String,
    /// Which workspace the path is relative to. Defaults to `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: Option<String>,
}

/// Moving a file or a directory within one workspace.
///
/// **Two full paths, not a name.** A rename and a move are the same filesystem
/// operation, and a `{path, newName}` shape would have to grow a second
/// endpoint the first time anybody wants to drag a file into a folder. There
/// is no `workspace_id` per side on purpose: moving *between* workspaces would
/// cross a boundary the jail exists to hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct MoveFileRequest {
    /// Where it is.
    #[garde(length(utf16, min = 1))]
    pub from: String,
    /// Where it goes.
    #[garde(length(utf16, min = 1))]
    pub to: String,
    /// Which workspace both paths are relative to. Defaults to `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: Option<String>,
}

// Workspaces

/// A workspace as the switcher and the manager see it.
///
/// **No path field.** A workspace is `<root>/workspace/<id>` and the id is
/// the only thing that crosses the wire; accepting a directory would turn
/// "managed directories only" from a fact into a convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct WorkspaceSummary {
    /// The id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// For a person.
    pub name: String,
    /// True for exactly one, which cannot be deleted and contains all the
    /// others.
    pub is_default: bool,
    /// When it was created.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub created_at_ms: u64,
    /// When it last changed.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub updated_at_ms: u64,
    /// What a delete would have to move first.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub session_count: u64,
}

/// `GET /api/workspaces`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct WorkspaceListResponse {
    /// Every workspace.
    #[garde(dive)]
    pub workspaces: Vec<WorkspaceSummary>,
}

/// `POST /api/workspaces`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct CreateWorkspaceRequest {
    /// For a person.
    #[garde(length(utf16, min = 1, max = 60))]
    pub name: String,
    /// Derived from the name when absent. Lowercase; also the folder name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1, max = 40))]
    pub id: Option<String>,
}

/// `PATCH /api/workspaces/:id`: the label, the folder, or both. A body with
/// neither is a no-op, which is what lets the editor send one request for
/// whichever boxes were touched.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct UpdateWorkspaceRequest {
    /// A new label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1, max = 60))]
    pub name: Option<String>,
    /// The folder to move it to. Refused for the default workspace, whose
    /// folder is the parent of every other one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1, max = 40))]
    pub id: Option<String>,
}

/// The way through a delete that was refused for having sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct MoveSessionsRequest {
    /// The workspace to move them to.
    #[garde(length(utf16, min = 1))]
    pub to: String,
}

/// How many sessions moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct MoveSessionsResponse {
    /// The count.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub moved: u64,
}

// Notifications

/// A notification, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct Notification {
    /// The row id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// The headline.
    pub title: String,
    /// The detail.
    pub body: String,
    /// How loud.
    #[serde(default)]
    pub level: NotificationLevel,
    /// When it was raised.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub created_at_ms: u64,
    /// When it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub read_at_ms: Option<u64>,
    /// The conversation it is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// The automation job that raised it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
}

/// `GET /api/notifications`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct NotificationListResponse {
    /// This page.
    #[garde(dive)]
    pub notifications: Vec<Notification>,
    /// How many are unread. The bell wants this.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub unread_count: u64,
    /// Where the next page starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Every notification the filter matches — which is *not* `unread_count`
    /// unless only unread ones were asked for. The pager wants this. Two
    /// numbers because they are two questions.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub total: u64,
}

// Automation

/// `GET /api/automation/jobs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AutomationJobListResponse {
    /// Every job.
    #[garde(dive)]
    pub jobs: Vec<AutomationJob>,
}

/// `GET /api/automation/jobs/:id/runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AutomationRunListResponse {
    /// This page.
    #[garde(dive)]
    pub runs: Vec<AutomationRun>,
    /// Where the next page starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Every run the job has kept, bounded by its retention knob.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub total: u64,
}

// Auth

/// The login name an install starts with.
///
/// A default rather than a required choice, because the first credential a
/// fresh install needs is a *password* — asking for a username in the same
/// breath adds a second thing to invent at the one moment the operator has
/// least context. The sign-in form prefills it and the CLI names it in help
/// text; changing it is done from the same form that changes the password.
pub const DEFAULT_USERNAME: &str = "ghost";

/// The shortest login name.
pub const USERNAME_MIN_LENGTH: usize = 1;
/// The longest login name.
pub const USERNAME_MAX_LENGTH: usize = 64;

/// The shortest new password. Twelve rather than the eight a login form
/// usually settles for, because what sits behind this one is an agent that can
/// read files and run commands on the host.
pub const PASSWORD_MIN_LENGTH: usize = 12;
/// The longest password: not a strength ceiling but a work ceiling. argon2id
/// will happily chew through a megabyte of input, and an unauthenticated
/// caller must not be able to ask it to.
pub const PASSWORD_MAX_LENGTH: usize = 256;

/// The characters a login name may hold, and where it may start.
pub const USERNAME_PATTERN: &str = "^[a-z0-9][a-z0-9._-]*$";

/// A login name, as it is compared.
///
/// Trimmed and lower-cased on the way in rather than by each caller, so the
/// value that reaches storage is the value that reaches a comparison. A name
/// that matched on the way in and failed on the way back — because one path
/// folded case and the other did not — is a lockout with no error message. The
/// character class is narrow on purpose: this is a single local account, and
/// every character it does not accept is one that cannot turn up in a log
/// line, a shell completion or a URL as something other than itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema, Validate)]
#[serde(transparent)]
pub struct Username(
    #[garde(length(utf16, min = 1, max = 64), pattern(r"^[a-z0-9][a-z0-9._-]*$"))] String,
);

impl Username {
    /// Normalises the way the schema does: trimmed and lower-cased. Validity
    /// is a separate step, so a caller can report *why* a name was refused.
    pub fn new(raw: &str) -> Self {
        Self(crate::json::js_trim(raw).to_lowercase())
    }

    /// The normalised name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Username {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = <Cow<'de, str>>::deserialize(d)?;
        Ok(Self::new(&raw))
    }
}

/// A new password, as it is accepted.
///
/// Deliberately not trimmed. A leading or trailing space is a character the
/// person chose, and silently removing it would mean storing a digest of
/// something they never typed — after which the password manager that replays
/// it verbatim can never sign in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(transparent)]
pub struct NewPassword(#[garde(length(utf16, min = 12, max = 256))] pub String);

/// A password being *presented*, which is a different shape from one being
/// set.
///
/// The bounds a new password must clear are a policy, and applying a policy to
/// an attempt would turn the login into an oracle: a 422 for "too short" and a
/// 401 for "wrong" tell an attacker which guesses are not worth making. Only
/// the upper bound survives, and only because it caps the work an anonymous
/// caller can ask argon2id to do.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(transparent)]
pub struct PresentedPassword(#[garde(length(utf16, min = 1, max = 256))] pub String);

/// `POST /api/auth/login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct LoginRequest {
    /// Who.
    #[garde(dive)]
    pub username: Username,
    /// The proof.
    #[garde(dive)]
    pub password: PresentedPassword,
}

/// No token in the body. Browsers get an `httpOnly; Secure; SameSite=Strict`
/// cookie set by the response: a token readable from script is
/// XSS-exfiltratable, and this app's whole job is rendering model-authored
/// markdown. CLI and CI use a `Bearer` token minted out of band.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct LoginResponse {
    /// Always `true`.
    pub ok: True,
    /// When the session ends.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub expires_at_ms: u64,
}

/// Who the caller is, as far as the server is concerned.
///
/// `auth_enabled` is here so the UI has one request to make before deciding
/// whether to render the login overlay. With auth off every caller is
/// authenticated and there is no session behind it, which is why
/// `expires_at_ms` is optional rather than a sentinel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AuthSessionResponse {
    /// Whether the caller may proceed.
    pub authenticated: bool,
    /// Whether a credential is required at all.
    pub auth_enabled: bool,
    /// When the session ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub expires_at_ms: Option<u64>,
    /// Who the caller is signed in as. Only on an authenticated response: a
    /// public route that answered "the account here is called `admin`" would
    /// be handing out half of the credential to anyone who asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

// First-run setup

/// Whether this install still has to be claimed.
///
/// Public, and deliberately says nothing else. An unauthenticated caller learns
/// one bit — that no password has been set — which they would learn anyway by
/// watching every login fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SetupStatusResponse {
    /// Whether setup is still needed.
    pub required: bool,
}

/// The one-time code printed to the console on first launch.
///
/// Both alternatives are worse: refusing to start without a password leaves the
/// UI that would set one unreachable, and starting unauthenticated is a
/// shell-capable agent answering to whoever reaches the port first. A code only
/// the operator's own terminal can see closes that gap without either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SetupClaimRequest {
    /// The code from the console.
    #[garde(length(utf16, min = 1))]
    pub code: String,
}

/// Setting the password, and — the same request, later in an install's life —
/// changing it.
///
/// One route for both because they are one operation with one precondition
/// that differs: a claim has no current password to prove, and a rotation
/// does. `current_password` is optional *in the shape* and mandatory *in the
/// handler* whenever a password already exists; a session alone is not enough
/// for a rotation, because the failure being closed is an injection that
/// changes the password and locks the operator out, and knowing the old one is
/// the thing a stolen session does not confer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SetupPasswordRequest {
    /// The new password.
    #[garde(dive)]
    pub password: NewPassword,
    /// Proof that the caller knows the password they are replacing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub current_password: Option<PresentedPassword>,
    /// Absent leaves the login name alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub username: Option<Username>,
}
