//! The settings tree.
//!
//! Conventions:
//!
//! - **`0` means "no limit"** on every `*_timeout_ms` and `*_per_minute` field,
//!   so a limit can be disabled without a separate nullable flag.
//! - **Durations are named with their unit.** A bare `tool_timeout: 40` is
//!   ambiguous between seconds and milliseconds at every call site;
//!   `tool_timeout_ms` is not.
//! - **Every nested block has a `Default` that is the empty object parsed
//!   through it**, so `Config::default()` is a fully populated tree and a
//!   missing block in a file means "all defaults", not "nothing".
//! - **Unknown keys are stripped, not refused**, everywhere except the patch:
//!   a config file from a newer build still loads on an older one. The patch is
//!   the opposite, for the reason given on [`ConfigPatch`].
//! - **No normalisation at parse time.** Trimming an `api_base`, expanding `~`:
//!   that happens at load time in the core crate, which keeps the serialised
//!   and deserialised shapes identical and every type here representable as
//!   JSON Schema.

use garde::Validate;
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_with::rust::double_option;

use crate::ids::DEFAULT_AGENT_ID;
use crate::json::{LooseObject, MAX_SAFE_INTEGER, Nullable, positive, prefault, yes};
use crate::tools::{ExecRule, ToolPermission, ToolPermissions, ToolPromptOverrides};

/// How hard to ask the model to think, where `off` is a value and unset is not.
///
/// Unset means the request carries no reasoning parameter and the provider
/// applies its own — the only thing that works against an endpoint that
/// rejects the field outright. `off` is a statement: this model thinks by
/// default and I do not want it to, so send whatever this wire spells that as.
/// `xhigh` is not invented here: it is what Qwen3.8 calls its top rung, and it
/// goes to the wire as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// Ask the model not to think.
    Off,
    /// The least a provider offers.
    Minimal,
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
    /// The top rung, where a model has one.
    Xhigh,
}

/// How an agent's system prompt is assembled.
///
/// `template` is the two-half assembly: an identity template and a live-state
/// template, with the platform note and the tool-output policy filled in as
/// sections the operator may also replace. It
/// keeps a provider's prompt cache working, because everything that changes
/// between requests sits in the tail. `raw` hands the whole system message to
/// one template and places nothing: "you own the prompt" and "you own the
/// prompt as long as you fill in our sections" are different claims, and only
/// the first is worth making.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PromptMode {
    /// Two halves, with the loop's sections placed around them.
    #[default]
    Template,
    /// One template is the whole system message.
    Raw,
}

/// How much of the model's reasoning is shown before anybody asks.
///
/// See [`UiConfig::reasoning`] for why there are three of these and not two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningDisplay {
    /// Never rendered. The strong reading of "hide the thinking".
    Hidden,
    /// Rendered as one labelled row that opens on a keystroke.
    #[default]
    Collapsed,
    /// Rendered in full, as it arrives.
    Expanded,
}

// One agent's settings

/// The working folder every agent shares.
///
/// Root-level rather than on an agent, because an agent *works in* a workspace
/// and does not own one: several agents with separate identities opening the
/// same one is the thing this is built around. Empty means
/// `~/DarkWire/workspaces`; a literal default would write one machine's home
/// directory into a file meant to be portable. A relative path is resolved
/// against the root, never against the working directory. Merged per field
/// like every other root key, so an omitted key preserves it.
///
/// `DARKWIRE_WORKSPACES` wins over whatever this says, so a container or a
/// test can relocate the tree without editing the file it mounted.
pub type WorkspacesPath = String;

/// What one agent sends, and what it costs.
///
/// Every agent states its own. There is no inheritance layer above this: a
/// field an entry does not name is filled by the type's own default, not by
/// another agent's answer. That is why the two fields below with no default —
/// `temperature` and `reasoning_effort` — mean "send nothing and let the
/// provider decide" rather than "look somewhere else".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentSettings {
    /// Empty means *unconfigured*, not "pick one for me". There is no
    /// model-picking code anywhere: an empty model refuses every turn, which is
    /// the fresh-install state and a question the UI asks the operator.
    #[serde(default)]
    pub model: String,
    /// `auto` runs the resolution order; otherwise a provider *instance* id. A
    /// bare provider type is still accepted and means "any instance of that
    /// type, or a default one if none is configured".
    #[serde(default = "default_provider")]
    #[garde(length(utf16, min = 1))]
    pub provider: String,
    /// The completion cap.
    #[serde(default = "default_max_tokens")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub max_tokens: u64,
    /// The context window the history is trimmed to.
    #[serde(default = "default_context_window_tokens")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub context_window_tokens: u64,
    /// Unset is not the same as `0`: unset sends no `temperature` at all and
    /// the provider applies its own, which is the only correct answer for the
    /// models that reject the parameter outright.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 0.0, max = 2.0))]
    pub temperature: Option<f64>,
    /// Tool round-trips per turn.
    #[serde(default = "default_max_tool_iterations")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub max_tool_iterations: u64,
    /// Per-call cap. `0` disables it.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub tool_timeout_ms: u64,
    /// Wall-clock cap on one turn, checked at the top of each iteration.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub loop_wall_timeout_ms: u64,
    /// Cap on one delegated run. `0` disables it.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub subagent_timeout_ms: u64,
    /// How hard to think. Unset sends nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Whether attached images are sent to the model as images.
    ///
    /// Off, an attachment still reaches the model as the path line it always
    /// carries, but never as an `image` part — the difference between a
    /// text-only model answering "let me open it" and the request being
    /// rejected outright.
    #[serde(default = "yes")]
    pub vision_enabled: bool,
    /// Whether the request advertises any tools at all.
    ///
    /// Off is not the same as denying every tool: the agent's permissions are
    /// left as configured and simply not offered to *this* model. It has no
    /// reactive counterpart, because the degradation ladder never strips
    /// `tools` — a turn where the model answers from memory is a wrong answer
    /// rather than a failed request.
    #[serde(default = "yes")]
    pub tools_enabled: bool,
    /// What the `exec` tool may run for this agent, and for how long.
    ///
    /// Whether the agent has `exec` at all is the permission map's answer,
    /// like every other tool; there is no second switch here to disagree.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub exec: ExecToolConfig,
    /// How long to wait for a decision before treating an `ask` call as
    /// denied.
    #[serde(default = "default_approval_timeout_ms")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub approval_timeout_ms: u64,
    /// Head+tail truncation budget for a single tool result.
    ///
    /// **Positive, and 0 does not mean "no limit" here**, unlike every
    /// duration in this tree. This is also an *allocation* bound: `read`
    /// sizes its read from it, so 0 would make it read one byte of every file,
    /// and lifting that would remove the only thing stopping one call from
    /// allocating a multi-gigabyte buffer. An operator who wants effectively no
    /// cap sets a large number, which is bounded and says what it means.
    #[serde(default = "default_max_output_chars")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub max_output_chars: u64,
    /// Send the model `tool_search` plus the pinned tools, and nothing else.
    ///
    /// Off sends every tool the agent permits. On, the rest are reachable by
    /// name through `tool_search` and stay in the list for the rest of the
    /// session once activated.
    #[serde(default)]
    pub lazy_discovery: bool,
    /// Tools that stay in the list while `lazy_discovery` is on.
    ///
    /// Names, not permissions: a pin widens nothing, and a tool this agent
    /// denies is still not sent. Replaced whole on a patch, so a pin can be
    /// removed. `tool_search` itself is never pinned or hidden.
    #[serde(default)]
    pub pinned_tools: Vec<String>,
}

fn default_provider() -> String {
    "auto".to_owned()
}

fn default_max_tokens() -> u64 {
    8192
}

fn default_context_window_tokens() -> u64 {
    65_536
}

fn default_max_tool_iterations() -> u64 {
    40
}

fn default_approval_timeout_ms() -> u64 {
    5 * 60 * 1000
}

fn default_max_output_chars() -> u64 {
    8192
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            model: String::new(),
            provider: default_provider(),
            max_tokens: default_max_tokens(),
            context_window_tokens: default_context_window_tokens(),
            temperature: None,
            max_tool_iterations: default_max_tool_iterations(),
            tool_timeout_ms: 0,
            loop_wall_timeout_ms: 0,
            subagent_timeout_ms: 0,
            reasoning_effort: None,
            vision_enabled: true,
            tools_enabled: true,
            exec: ExecToolConfig::default(),
            approval_timeout_ms: default_approval_timeout_ms(),
            max_output_chars: default_max_output_chars(),
            lazy_discovery: false,
            pinned_tools: Vec::new(),
        }
    }
}

// Providers

/// One configured endpoint. API keys are deliberately absent: they live in the
/// encrypted vault under the `providers` namespace, keyed by the *instance*
/// id, so a `config.yaml` is safe to commit or paste into a bug report.
///
/// `type` is what makes an instance distinct from a provider. Two Ollama
/// servers — a laptop and a GPU box — are two entries with the same type and
/// different `api_base`. It is validated against the provider registry
/// upstream of this crate, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ProviderConfig {
    /// A provider registry id — `ollama`, `openai`, `custom`.
    #[serde(rename = "type")]
    #[garde(length(utf16, min = 1))]
    pub kind: String,
    /// Shown in the UI. Empty falls back to the type's display name.
    #[serde(default)]
    pub label: String,
    /// The endpoint. Absent means the type's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// Headers sent with every request.
    #[serde(default)]
    pub extra_headers: IndexMap<String, String>,
    /// Models to offer for this instance: a fallback rather than the
    /// catalogue, for an endpoint that does not answer `GET /models`, or while
    /// one is unreachable.
    #[serde(default)]
    pub models: Vec<String>,
    /// A disabled instance is kept, and skipped by resolution and model listing.
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// Keyed by *instance* id, which is an operator's label rather than a provider
/// id.
///
/// An old file's keys *are* provider ids, so adding `type` = the key is the
/// whole of moving one over, and every credential already in the vault keeps
/// resolving under the same string. There is no automatic migration — a file
/// without `type` is an error naming the key.
pub type ProvidersConfig = IndexMap<String, ProviderConfig>;

// Server

/// Who may reach the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AuthConfig {
    /// Disabling auth on a non-loopback bind is a startup *error*, not a
    /// warning — see [`is_loopback_host`]. A warning scrolls past, and the
    /// result is an unauthenticated shell-capable agent on a LAN address.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// How long a login lasts.
    #[serde(default = "default_session_ttl_ms")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub session_ttl_ms: u64,
    /// Login attempts per minute. `0` disables the limit.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub rate_limit_per_minute: u64,
    /// Lifetime of the HMAC-signed URLs that serve workspace media to `<img>`.
    #[serde(default = "default_signed_url_ttl_ms")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub signed_url_ttl_ms: u64,
}

fn default_session_ttl_ms() -> u64 {
    30 * 24 * 60 * 60 * 1000
}

fn default_signed_url_ttl_ms() -> u64 {
    10 * 60 * 1000
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            session_ttl_ms: default_session_ttl_ms(),
            rate_limit_per_minute: 0,
            signed_url_ttl_ms: default_signed_url_ttl_ms(),
        }
    }
}

/// The HTTP and WebSocket server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ServerConfig {
    /// The bind address.
    #[serde(default = "default_host")]
    #[garde(length(utf16, min = 1))]
    pub host: String,
    /// One port for the API, the WebSocket and the static UI. DarkWire is
    /// single-process; nothing in it is heavy enough to justify the
    /// reconnect-and-HTTP-fallback client a split-process topology would need.
    #[serde(default = "default_port")]
    #[garde(range(min = 1, max = 65_535))]
    pub port: u16,
    /// Host names the server answers to, beyond the ones it always does.
    ///
    /// Every request's `Host` is checked, so a page on a name that was pointed
    /// at this machine cannot read from it (DNS rebinding). `localhost`, the
    /// loopback addresses, any IP literal, the bind host and the machine's
    /// hostname are always accepted. A name here is for a reverse proxy or a
    /// DNS alias. An entry with a port matches only that port.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Who may reach it.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub auth: AuthConfig,
    /// How many server events to retain per session so a reconnecting tab can
    /// replay an in-flight turn from its last `seq`.
    ///
    /// A count of *frames*, and a turn that streams a long answer spends one
    /// per token — so this is not the knob that decides whether a reload comes
    /// back to the whole turn; `turn_log_max_bytes` is. This one decides how
    /// far back a *reconnect* can pick up across turns.
    #[serde(default = "default_replay_buffer_size")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub replay_buffer_size: u64,
    /// The budget for retaining the turn that is running, whole, so a reload
    /// comes back to all of it — including every nested subagent step.
    ///
    /// Bytes rather than frames because frames are not the cost: the log
    /// merges adjacent deltas of the same part, so a long answer is one entry,
    /// and what is actually retained is tool output. Held only while a turn is
    /// open, so the ceiling is the number of concurrent turns. Past it the log
    /// stops retaining and says so; `0` disables it.
    #[serde(default = "default_turn_log_max_bytes")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub turn_log_max_bytes: u64,
}

fn default_host() -> String {
    "127.0.0.1".to_owned()
}

fn default_port() -> u16 {
    3000
}

fn default_replay_buffer_size() -> u64 {
    512
}

fn default_turn_log_max_bytes() -> u64 {
    16 * 1024 * 1024
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            allowed_hosts: Vec::new(),
            auth: AuthConfig::default(),
            replay_buffer_size: default_replay_buffer_size(),
            turn_log_max_bytes: default_turn_log_max_bytes(),
        }
    }
}

/// Whether `host` binds only to the local machine.
///
/// A predicate rather than a validation rule on the config, because the caller
/// needs to explain *why* startup was refused, and a cross-field rule would
/// also make the schema unrepresentable. `0.0.0.0` and `::` are the wildcard
/// binds that must count as remote.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_lowercase();
    let h = h.strip_prefix('[').unwrap_or(&h);
    let h = h.strip_suffix(']').unwrap_or(h);
    if h == "localhost" || h == "::1" {
        return true;
    }
    if h == "0.0.0.0" || h == "::" || h.is_empty() {
        return false;
    }
    // 127.0.0.0/8 — any of the 16 million loopback addresses, not just .0.1.
    let mut octets = h.split('.');
    octets.next() == Some("127")
        && (0..3).all(|_| {
            octets.next().is_some_and(|o| {
                (1..=3).contains(&o.len()) && o.bytes().all(|b| b.is_ascii_digit())
            })
        })
        && octets.next().is_none()
}

// Tools

/// The `exec` tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ExecToolConfig {
    /// Per-command cap. `0` disables it.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub timeout_ms: u64,
    /// Appended to the child's `PATH`.
    #[serde(default)]
    pub path_append: String,
    /// Command rules. See [`ExecRule`] for how they match.
    #[serde(default)]
    #[garde(dive)]
    pub rules: Vec<ExecRule>,
    /// The ceiling for a call whose program is a shell. A wildcard rule and
    /// the tool's own permission are capped at this, and `deny` refuses a
    /// shell outright. Only a rule naming the exact command decides one on its
    /// own.
    #[serde(default = "default_shell")]
    pub shell: ToolPermission,
    /// Environment variables passed through to the child.
    #[serde(default = "default_env_allowlist")]
    pub env_allowlist: Vec<String>,
    /// Output kept per command.
    #[serde(default = "default_max_output_bytes")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub max_output_bytes: u64,
}

fn default_shell() -> ToolPermission {
    ToolPermission::Ask
}

fn default_env_allowlist() -> Vec<String> {
    ["PATH", "HOME", "LANG", "TZ"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn default_max_output_bytes() -> u64 {
    1024 * 1024
}

impl Default for ExecToolConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 0,
            path_append: String::new(),
            rules: Vec::new(),
            shell: default_shell(),
            env_allowlist: default_env_allowlist(),
            max_output_bytes: default_max_output_bytes(),
        }
    }
}

/// OAuth for an MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct McpOAuthConfig {
    /// The authorisation endpoint.
    #[garde(length(utf16, min = 1))]
    pub auth_url: String,
    /// The token endpoint.
    #[garde(length(utf16, min = 1))]
    pub token_url: String,
    /// The client id.
    #[garde(length(utf16, min = 1))]
    pub client_id: String,
    /// Scopes to request.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// How long to wait for the browser to come back. `0` disables the limit.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub callback_timeout_ms: u64,
}

/// How an MCP server is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum McpTransport {
    /// A child process.
    Stdio,
    /// Server-sent events.
    Sse,
    /// Streamable HTTP.
    StreamableHttp,
}

/// One MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct McpServerConfig {
    /// Inferred from `command` vs `url` when omitted.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<McpTransport>,
    /// The program, for `stdio`.
    #[serde(default)]
    pub command: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Its environment.
    #[serde(default)]
    pub env: IndexMap<String, String>,
    /// The endpoint, for `sse` and `streamableHttp`.
    #[serde(default)]
    pub url: String,
    /// Headers sent with every request.
    #[serde(default)]
    pub headers: IndexMap<String, String>,
    /// OAuth, when the server needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub oauth: Option<McpOAuthConfig>,
    /// Per-call cap. `0` disables it.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub tool_timeout_ms: u64,
    /// `["*"]` exposes everything the server advertises.
    #[serde(default = "default_enabled_tools")]
    pub enabled_tools: Vec<String>,
    /// A disabled server is kept and not connected.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn default_enabled_tools() -> Vec<String> {
    vec!["*".to_owned()]
}

/// The tool layer's install-wide half, which is the MCP servers and nothing
/// else. Everything about how a tool runs for an agent, from `exec` to the
/// result budget, is on the agent (`AgentSettings`), because two agents on one
/// install can reasonably want different answers to all of it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolsConfig {
    /// MCP servers, by id.
    #[serde(default)]
    #[garde(custom(crate::json::validate_map_values))]
    pub mcp_servers: IndexMap<String, McpServerConfig>,
    /// How this install reaches the web.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub web: WebToolsConfig,
}

/// Which backend answers a search.
///
/// Both are keyless, which is the point: nothing here asks an operator to hold
/// an account with a search company. `auto` costs nothing and promises nothing;
/// `searxng` is the one that actually holds, and the instance is theirs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchProvider {
    /// Scraped front doors in rotation, then Hacker News. No key, no setup and
    /// no promises: see the tools documentation.
    #[default]
    Auto,
    /// A SearXNG instance the operator runs, named by `searchUrl`.
    Searxng,
}

/// How this install reaches the web, for `web_fetch` and `web_search`.
///
/// Install-wide rather than per agent, unlike `exec` and the result budget.
/// These describe the shape of an outbound connection and the machine making
/// it: which backend answers a search, how this install identifies itself, what
/// every request is bounded by, and one cache shared by everything. None of it
/// is something one agent should be able to answer differently from another.
///
/// **No API keys, deliberately.** Both backends are keyless. What an agent may
/// *reach* is still the agent's, in its
/// [`EnvironmentNetwork`](EnvironmentNetwork) allow-list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct WebToolsConfig {
    /// Which backend answers `web_search`.
    #[serde(default)]
    pub search_provider: WebSearchProvider,
    /// The SearXNG instance, for `searxng`. Ignored otherwise.
    #[serde(default)]
    pub search_url: String,
    /// Sent verbatim, with no client hints.
    ///
    /// Empty is the built-in browser profile, which is what gets past most bot
    /// walls. A value here is the operator choosing to be identifiable, and the
    /// client hints are dropped with it: a hint set naming Chrome beside a
    /// custom agent is a contradiction that gives the whole thing away.
    #[serde(default)]
    pub user_agent: String,
    /// Per fetch.
    #[serde(default = "default_web_timeout_seconds")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub timeout_seconds: u64,
    /// Per page read inside a search, so one slow origin cannot eat the batch.
    #[serde(default = "default_web_read_timeout_seconds")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub read_timeout_seconds: u64,
    /// The streaming body cap, counted after decompression.
    #[serde(default = "default_web_max_bytes")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub max_bytes: u64,
    /// Pages and result sets held in memory. `0` disables the cache.
    #[serde(default = "default_web_cache_entries")]
    #[garde(range(min = 0, max = MAX_SAFE_INTEGER))]
    pub cache_entries: u64,
    /// How long a cached entry stays usable. `0` disables the cache.
    #[serde(default = "default_web_cache_ttl_seconds")]
    #[garde(range(min = 0, max = MAX_SAFE_INTEGER))]
    pub cache_ttl_seconds: u64,
}

const fn default_web_timeout_seconds() -> u64 {
    20
}

const fn default_web_read_timeout_seconds() -> u64 {
    15
}

const fn default_web_max_bytes() -> u64 {
    5 * 1024 * 1024
}

const fn default_web_cache_entries() -> u64 {
    64
}

/// Fifteen minutes. A fetched page is worth minutes, not restarts.
const fn default_web_cache_ttl_seconds() -> u64 {
    900
}

impl Default for WebToolsConfig {
    fn default() -> WebToolsConfig {
        WebToolsConfig {
            search_provider: WebSearchProvider::default(),
            search_url: String::new(),
            user_agent: String::new(),
            timeout_seconds: default_web_timeout_seconds(),
            read_timeout_seconds: default_web_read_timeout_seconds(),
            max_bytes: default_web_max_bytes(),
            cache_entries: default_web_cache_entries(),
            cache_ttl_seconds: default_web_cache_ttl_seconds(),
        }
    }
}

// Agents

/// What a newly created agent starts with: the built-in tools, at the
/// permission their risk band implies.
///
/// The one place a risk band still turns into a permission, and it happens
/// once, at creation, where an operator can see the result and change it.
/// Seeding rather than starting empty because an agent that can do nothing
/// looks broken to whoever just made it. `memory` and `skill` are seeded on
/// because an agent that silently fails to remember reads as broken rather
/// than as unconfigured; an install that predates them has neither until an
/// operator grants it.
pub const DEFAULT_AGENT_TOOLS: &[(&str, ToolPermission)] = &[
    ("read", ToolPermission::Allow),
    ("ls", ToolPermission::Allow),
    ("grep", ToolPermission::Allow),
    ("find", ToolPermission::Allow),
    ("write", ToolPermission::Allow),
    ("edit", ToolPermission::Allow),
    ("exec", ToolPermission::Ask),
    ("memory", ToolPermission::Allow),
    ("skill", ToolPermission::Allow),
    // The plan a long turn runs on, and the switch for the Tasks section of the
    // prompt. Seeded on: an agent that cannot say what it is doing is the thing
    // this exists to fix.
    ("todo", ToolPermission::Allow),
];

/// [`DEFAULT_AGENT_TOOLS`] as the map an entry holds.
pub fn default_agent_tools() -> ToolPermissions {
    DEFAULT_AGENT_TOOLS
        .iter()
        .map(|(name, permission)| ((*name).to_owned(), *permission))
        .collect()
}

/// How much of the network an agent's environment reaches.
///
/// The one place egress is configured. An environment definition decides whether
/// a restricted gateway *can* be built — a non-root numeric uid, no-new-privs,
/// no packet-forging capability — and this decides what that gateway permits.
/// Splitting the two across both files is what produced a "ceiling" nobody
/// could find the other half of.
///
/// One list, whatever the destination looks like. An entry is a CIDR block, an
/// address, a name, or a name with a leading dot covering its subdomains.
/// Blocks and addresses are enforced by the gateway's packet filter; names are
/// enforced by the egress proxy, which sees the name rather than the address a
/// name resolved to, and so is the one thing DNS rebinding cannot defeat.
///
/// Nothing inside an environment resolves a name. The proxy does it, on the
/// engine's side of the boundary, which is why there is no resolver to
/// configure and why a name is reachable only over HTTP and HTTPS.
///
/// Unknown keys are refused here, unlike most of this file. This field decides
/// what an agent can reach, and a key that was silently dropped would read as
/// an allow-list that had been applied.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct EnvironmentNetwork {
    /// How much network to permit at all.
    #[serde(default)]
    pub mode: NetworkMode,
    /// What `allowlist` permits: blocks, addresses, names, `.suffix` names.
    #[serde(default)]
    pub allow: Vec<String>,
}

/// How much network an agent asks for. Ordered weakest to strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// No network at all.
    #[default]
    None,
    /// Only what `allow` names.
    Allowlist,
    /// Anything.
    Open,
}

/// Where this agent's command operations run, and what they can reach.
///
/// An empty name runs command operations on the machine running DarkWire,
/// inside the workspace jail, where a network request means nothing and is
/// refused rather than ignored. A named environment routes them through the
/// sandbox service instead.
///
/// The image, capabilities, hardening and sharing live in the installed
/// definition and have no representation here. The network *does* live here:
/// egress is the one thing an operator configures per agent rather than per
/// image, and a single place to configure it is worth more than a second
/// ceiling nobody could locate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentEnvironment {
    /// An installed environment name, or empty to run on the host.
    #[serde(default)]
    pub name: String,
    /// What this agent's environment may reach.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub network: EnvironmentNetwork,
    /// Whether this agent brings its own environment when something delegates
    /// to it.
    ///
    /// Off, the default, means a delegated turn runs where its caller does:
    /// work handed down stays inside the boundary the operator chose rather
    /// than falling back to the host halfway down a chain. At the top of a
    /// chain there is no caller, so the agent runs in the environment named
    /// above either way.
    ///
    /// On pins it to that environment whoever called. A web-search agent with a
    /// browser in its image is the case: it is useless anywhere else, and the
    /// caller cannot be expected to know that.
    ///
    /// It is a property of the agent rather than of one delegation because an
    /// agent that needs its own toolchain needs it from every caller.
    #[serde(default)]
    pub always_use_own: bool,
}

/// Another agent this one may hand a task to.
///
/// A subagent is an ordinary entry in `agents.list` that some other entry
/// points at: a researcher is configured, tested and used on its own, and being
/// someone's subagent is a relationship rather than a mode. `prompt` is the
/// tool description the model reads — the only part of this feature that
/// decides when it fires. `permission` sits here rather than in `tools`
/// because a subagent is not an installed tool and would render under a "not
/// installed" badge there. `allow` is the default because delegation is the
/// feature; the tools the *subagent* runs are gated by its own map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SubagentRef {
    /// An id in `agents.list`.
    pub id: String,
    /// The operator's guidance. Empty means the built-in sentence.
    #[serde(default)]
    pub prompt: String,
    /// Whether delegation runs unattended.
    #[serde(default = "allow")]
    pub permission: ToolPermission,
}

fn allow() -> ToolPermission {
    ToolPermission::Allow
}

/// One named agent, complete.
///
/// Built on [`AgentSettings`] rather than on a patch of it, so an entry that
/// names three fields parses into an agent that states all of them; the rest
/// come from the defaults. Nothing is inherited from anywhere, which is what
/// lets a reader answer "what does this agent run on" from the entry alone.
/// `workspace` is deliberately absent: the working folder is root-level and
/// shared.
///
/// Every prompt field has the same three-state meaning: empty inherits the
/// built-in template, a single space deletes the section, anything else
/// replaces it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentEntry {
    /// What the agent sends, and what it costs.
    #[serde(flatten)]
    #[garde(dive)]
    pub settings: AgentSettings,
    /// Shown in the UI. Empty falls back to the id.
    #[serde(default)]
    pub label: String,
    /// The whole static system prompt, as a template. Empty means the
    /// built-in, which is what keeps an install that never customised a prompt
    /// receiving improvements to it on upgrade.
    #[serde(default)]
    pub system_prompt: String,
    /// The per-iteration half's live-state section. Never cached, so every
    /// line is re-sent on every request of every turn.
    #[serde(default)]
    pub live_prompt: String,
    /// What is appended in the last few iterations of a turn.
    #[serde(default)]
    pub wrap_up_prompt: String,
    /// Whether `system_prompt` is the static half or the entire system message.
    ///
    /// `raw` stops *placing* anything; the section templates below still decide
    /// what the placeholders render *to*, so raw controls the layout rather
    /// than discarding the wording. `live_prompt` is the one field raw ignores
    /// outright, since a raw template names `{{time}}` and `{{wrapUp}}` itself.
    #[serde(default)]
    pub prompt_mode: PromptMode,
    /// The `## Running commands` section: where commands run, and what is
    /// there.
    ///
    /// **Empty inherits a built-in that depends on placement**, decided per
    /// turn because a subagent runs where its caller's reference says. On the
    /// host that is the rule the exec guard enforces. In a container it is the
    /// environment definition's own `prompt`, because only the image knows what
    /// it holds and saying it once per image beats restating it on every agent
    /// that uses one; an image that says nothing places no section at all.
    ///
    /// One field rather than two, and the editor seeds the box with whichever
    /// built-in applies, so an agent granted three of an image's ten tools
    /// opens the box on the image's list and deletes seven.
    ///
    /// Editing it does not widen anything: where a command may reach is decided
    /// by the exec guard and the jail, neither of which reads the prompt.
    #[serde(default)]
    pub platform_prompt: String,
    /// Read by nothing. `platform_prompt` above is the one section about
    /// placement, and in a container its built-in is already the definition's
    /// own words.
    ///
    /// Still parsed so an agent that set it can be told its wording is not
    /// placed, rather than losing it in silence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_prompt: Option<String>,
    /// The `## Tool output policy` section. The envelopes are emitted and the
    /// nonce regenerated whatever this says — this is the *explanation* of a
    /// defence, not the defence.
    #[serde(default)]
    pub tool_policy_prompt: String,
    /// The `## Memory` section. Only rendered while the agent may call
    /// `memory` and the workspace has at least one.
    #[serde(default)]
    pub memory_prompt: String,
    /// The `## Skills` section. Only rendered while the agent may call `skill`
    /// and the workspace has at least one.
    #[serde(default)]
    pub skills_prompt: String,
    /// Per-tool replacements for the description and the parameter
    /// descriptions the model is sent. Keyed by advertised tool name, so it
    /// reaches built-ins, MCP and extension tools and
    /// `ask_<id>` subagent tools alike. A key naming no advertised tool is a
    /// warning, not an error.
    #[serde(default)]
    #[garde(custom(crate::json::validate_map_values))]
    pub tool_prompts: ToolPromptOverrides,
    /// A disabled agent is kept and refused a turn.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Replaces, never merges. An entry that names three tools has three tools
    /// — the seed is what a *new* agent gets, not a floor every agent stands
    /// on, or switching a tool off would be impossible to express.
    #[serde(default = "default_agent_tools")]
    pub tools: ToolPermissions,
    /// Where built-in command execution runs.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub environment: AgentEnvironment,
    /// Agents this one may delegate to. Order is the order the model sees
    /// them, which is why this is a list and not a map.
    #[serde(default)]
    #[garde(dive)]
    pub subagents: Vec<SubagentRef>,
}

impl Default for AgentEntry {
    fn default() -> Self {
        Self {
            settings: AgentSettings::default(),
            label: String::new(),
            system_prompt: String::new(),
            live_prompt: String::new(),
            wrap_up_prompt: String::new(),
            prompt_mode: PromptMode::Template,
            platform_prompt: String::new(),
            environment_prompt: None,
            tool_policy_prompt: String::new(),
            memory_prompt: String::new(),
            skills_prompt: String::new(),
            tool_prompts: IndexMap::new(),
            enabled: true,
            tools: default_agent_tools(),
            environment: AgentEnvironment::default(),
            subagents: Vec::new(),
        }
    }
}

/// Every agent this install has. There is nothing above them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentsConfig {
    /// Keyed by an id the operator chooses, following the workspace id rules.
    ///
    /// `default` is always present because it is the agent every unbound
    /// conversation runs on, and with no settings layer above it there is
    /// nothing else for it to resolve from. A fresh install therefore has
    /// exactly one agent, complete and unconfigured, rather than none.
    #[serde(default = "default_agent_list")]
    #[schemars(transform = prefault_agent_list)]
    #[garde(custom(crate::json::validate_map_values))]
    pub list: IndexMap<String, AgentEntry>,
}

fn default_agent_list() -> IndexMap<String, AgentEntry> {
    let mut list = IndexMap::new();
    list.insert(DEFAULT_AGENT_ID.to_owned(), AgentEntry::default());
    list
}

fn prefault_agent_list(schema: &mut schemars::Schema) {
    schema.insert(
        "default".into(),
        serde_json::json!({ DEFAULT_AGENT_ID: {} }),
    );
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            list: default_agent_list(),
        }
    }
}

// Scheduler, channels, extensions

/// The engine, and nothing about any one job.
///
/// Every key here is true of the *scheduler*; none describes a task. A
/// heartbeat **is** a job: its interval is the job's schedule, its file and
/// model are the job's payload, and its on/off is the job's own flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SchedulerConfig {
    /// Whether anything fires.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Concurrent automation runs. Two keeps a slow job from blocking the queue.
    #[serde(default = "default_concurrency")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub concurrency: u64,
    /// Run `at` jobs whose time passed while the process was down.
    #[serde(default = "yes")]
    pub catch_up_on_boot: bool,
    /// Runs kept per job, trimmed on write. Per job rather than a global cap:
    /// a nightly job's year of history must not be evicted by a five-minute
    /// job's afternoon.
    #[serde(default = "default_run_retention")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub run_retention: u64,
}

fn default_concurrency() -> u64 {
    2
}

fn default_run_retention() -> u64 {
    200
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            concurrency: default_concurrency(),
            catch_up_on_boot: true,
            run_retention: default_run_retention(),
        }
    }
}

/// Channel settings. Loose by design: each channel — built-in or from an
/// extension — parses its own block, so installing a channel does not require
/// a schema change here. Unknown keys are kept, in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ChannelsConfig {
    /// Whether channels relay progress while a turn runs.
    #[serde(default = "yes")]
    pub send_progress: bool,
    /// Whether channels mention tool calls.
    #[serde(default)]
    pub send_tool_hints: bool,
    /// Every channel's own block, keyed by channel id.
    #[serde(flatten)]
    pub extra: LooseObject,
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            send_progress: true,
            send_tool_hints: false,
            extra: IndexMap::new(),
        }
    }
}

/// Extension settings.
///
/// Per-extension configuration is a `settings` sub-object rather than the
/// loose top level channels use, because this block already has keys of its
/// own, and an extension whose id happened to be `load` or `disabled` would
/// silently overwrite one. Inside `settings` each block is loose: the extension
/// parses its own.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ExtensionsConfig {
    /// Extra directories to load from, beside `~/.darkwire/extensions`. A path,
    /// never a package spec: nothing here fetches, which is what keeps an
    /// air-gapped install air-gapped.
    #[serde(default)]
    pub load: Vec<String>,
    /// Extensions kept installed and not run.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Lets a later-discovered extension shadow an earlier id instead of
    /// erroring.
    #[serde(default)]
    pub allow_override: bool,
    /// Each extension's own block, by id.
    #[serde(default)]
    pub settings: IndexMap<String, LooseObject>,
}

/// What the install looks and reads like, for both surfaces.
///
/// Its own section rather than a field on `server`, because nothing here is
/// transport. Both fields are plain strings on purpose: an enum would have to
/// enumerate the shipped languages or the IANA database, and would turn a
/// config naming a value this build does not carry into a parse failure that
/// takes the whole file down. Each is narrowed where it is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct UiConfig {
    /// A BCP-47 tag. Unknown values fall back rather than failing to parse.
    #[serde(default = "default_locale")]
    pub locale: String,
    /// How much of the model's reasoning a reader is shown by default.
    ///
    /// Three states rather than a switch, and the middle one is why. `hidden`
    /// stops the reasoning reaching a surface at all, which is what somebody
    /// who never wants to see it means. `collapsed` is the default: the run is
    /// there, labelled, one row, and opening it is a keystroke. A switch would
    /// have had to pick one of those two to be "off", and both readings are
    /// reasonable.
    ///
    /// It governs both surfaces, because it is a property of the install rather
    /// than of the terminal. The browser collapses reasoning already, so only
    /// `hidden` changes anything there.
    #[serde(default)]
    pub reasoning: ReasoningDisplay,
    /// Whether a tool's output arrives open.
    ///
    /// A switch and not three states, unlike reasoning above: the text is
    /// written either way, so there is no third thing for "off" to mean.
    #[serde(default)]
    pub expand_tool_output: bool,
    /// Whether the terminal shows what a turn cost as it finishes.
    ///
    /// Worth having, and not worth a row under every answer. A switch for the
    /// same reason as `expand_tool_output`: the line is written either way and
    /// a keystroke reveals it, so there is no third thing for "off" to mean.
    ///
    /// The terminal only. The browser puts the same figures in a turn-info
    /// popover, which is already out of the way.
    #[serde(default)]
    pub expand_turn_stats: bool,
    /// The one zone this install reads and writes clock times in.
    ///
    /// Everything is *stored* in UTC, so this is not a storage format; it is
    /// the answer to "whose clock", and deliberately a single install-wide
    /// answer rather than one per job. A concrete IANA name, never a rule:
    /// storing `system` would mean the server resolved it to the host zone
    /// while a browser resolved it to the viewer's. UTC rather than the host
    /// zone as the default, because a server's zone moves when the box moves.
    #[serde(default = "default_timezone")]
    #[garde(length(utf16, min = 1))]
    pub timezone: String,
}

fn default_locale() -> String {
    "en".to_owned()
}

fn default_timezone() -> String {
    "UTC".to_owned()
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            locale: default_locale(),
            reasoning: ReasoningDisplay::default(),
            expand_tool_output: false,
            // Spelled out rather than left to the derive, which is not used
            // here, and `false` is the wanted answer either way: a turn's cost
            // arrives folded.
            expand_turn_stats: false,
            timezone: default_timezone(),
        }
    }
}

// Root

/// The whole settings tree.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct Config {
    /// The folder the workspaces live in. See [`WorkspacesPath`].
    #[serde(default)]
    pub workspaces: WorkspacesPath,
    /// Every agent.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub agents: AgentsConfig,
    /// Every provider instance.
    #[serde(default)]
    #[garde(custom(crate::json::validate_map_values))]
    pub providers: ProvidersConfig,
    /// The server.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub server: ServerConfig,
    /// The tool layer.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub tools: ToolsConfig,
    /// Channels.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub channels: ChannelsConfig,
    /// The scheduler.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub scheduler: SchedulerConfig,
    /// Extensions.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub extensions: ExtensionsConfig,
    /// Locale and time zone.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub ui: UiConfig,
}

// Patches
//
// A patch type is its config type with every field optional **and stripped of
// its default**. Optional alone would be actively wrong: a patch is deep-merged
// into the live config, so a field that defaulted itself on parse would
// silently rewrite every setting the client never mentioned back to its
// default the moment one settings panel was saved.

/// A patch over [`AgentSettings`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentSettingsPatch {
    /// See [`AgentSettings::model`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// See [`AgentSettings::provider`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub provider: Option<String>,
    /// See [`AgentSettings::max_tokens`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub max_tokens: Option<u64>,
    /// See [`AgentSettings::context_window_tokens`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub context_window_tokens: Option<u64>,
    /// See [`AgentSettings::temperature`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 0.0, max = 2.0))]
    pub temperature: Option<f64>,
    /// See [`AgentSettings::max_tool_iterations`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub max_tool_iterations: Option<u64>,
    /// See [`AgentSettings::tool_timeout_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub tool_timeout_ms: Option<u64>,
    /// See [`AgentSettings::loop_wall_timeout_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub loop_wall_timeout_ms: Option<u64>,
    /// See [`AgentSettings::subagent_timeout_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub subagent_timeout_ms: Option<u64>,
    /// See [`AgentSettings::reasoning_effort`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// See [`AgentSettings::vision_enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision_enabled: Option<bool>,
    /// See [`AgentSettings::tools_enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_enabled: Option<bool>,
    /// See [`AgentSettings::exec`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub exec: Option<ExecToolConfigPatch>,
    /// See [`AgentSettings::approval_timeout_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub approval_timeout_ms: Option<u64>,
    /// See [`AgentSettings::max_output_chars`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub max_output_chars: Option<u64>,
    /// See [`AgentSettings::lazy_discovery`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lazy_discovery: Option<bool>,
    /// See [`AgentSettings::pinned_tools`]. Replaces the whole list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_tools: Option<Vec<String>>,
}

impl From<AgentSettings> for AgentSettingsPatch {
    fn from(settings: AgentSettings) -> Self {
        Self {
            model: Some(settings.model),
            provider: Some(settings.provider),
            max_tokens: Some(settings.max_tokens),
            context_window_tokens: Some(settings.context_window_tokens),
            temperature: settings.temperature,
            max_tool_iterations: Some(settings.max_tool_iterations),
            tool_timeout_ms: Some(settings.tool_timeout_ms),
            loop_wall_timeout_ms: Some(settings.loop_wall_timeout_ms),
            subagent_timeout_ms: Some(settings.subagent_timeout_ms),
            reasoning_effort: settings.reasoning_effort,
            vision_enabled: Some(settings.vision_enabled),
            tools_enabled: Some(settings.tools_enabled),
            exec: Some(settings.exec.into()),
            approval_timeout_ms: Some(settings.approval_timeout_ms),
            max_output_chars: Some(settings.max_output_chars),
            lazy_discovery: Some(settings.lazy_discovery),
            pinned_tools: Some(settings.pinned_tools),
        }
    }
}

/// A patch over [`ExecToolConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ExecToolConfigPatch {
    /// See [`ExecToolConfig::timeout_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub timeout_ms: Option<u64>,
    /// See [`ExecToolConfig::path_append`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_append: Option<String>,
    /// See [`ExecToolConfig::rules`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub rules: Option<Vec<ExecRule>>,
    /// See [`ExecToolConfig::shell`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ToolPermission>,
    /// See [`ExecToolConfig::env_allowlist`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_allowlist: Option<Vec<String>>,
    /// See [`ExecToolConfig::max_output_bytes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub max_output_bytes: Option<u64>,
}

impl From<ExecToolConfig> for ExecToolConfigPatch {
    fn from(exec: ExecToolConfig) -> Self {
        Self {
            timeout_ms: Some(exec.timeout_ms),
            path_append: Some(exec.path_append),
            rules: Some(exec.rules),
            shell: Some(exec.shell),
            env_allowlist: Some(exec.env_allowlist),
            max_output_bytes: Some(exec.max_output_bytes),
        }
    }
}

/// A patch over [`EnvironmentNetwork`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct EnvironmentNetworkPatch {
    /// See [`EnvironmentNetwork::mode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<NetworkMode>,
    /// See [`EnvironmentNetwork::allow`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
}

/// A patch over [`AgentEnvironment`]. `network` is itself a patch, so a save
/// that only changes the mode does not have to resend `allow` — which is how a
/// settings panel silently clears the allow-list it never rendered.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentEnvironmentPatch {
    /// See [`AgentEnvironment::name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// See [`AgentEnvironment::network`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub network: Option<EnvironmentNetworkPatch>,
    /// See [`AgentEnvironment::always_use_own`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_use_own: Option<bool>,
}

/// A patch over [`AgentEntry`].
///
/// `tools` and `subagents` are not patches: the map and the list replace
/// wholesale, because a merge that went key by key could add a tool and change
/// a permission but never remove one.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentEntryPatch {
    /// See [`AgentEntry::settings`].
    #[serde(flatten)]
    #[garde(dive)]
    pub settings: AgentSettingsPatch,
    /// See [`AgentEntry::label`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// See [`AgentEntry::system_prompt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// See [`AgentEntry::live_prompt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_prompt: Option<String>,
    /// See [`AgentEntry::wrap_up_prompt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap_up_prompt: Option<String>,
    /// See [`AgentEntry::prompt_mode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_mode: Option<PromptMode>,
    /// See [`AgentEntry::platform_prompt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_prompt: Option<String>,
    /// See [`AgentEntry::environment_prompt`]. Read by nothing; carried so a
    /// patch round-trips an entry that still sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_prompt: Option<String>,
    /// See [`AgentEntry::tool_policy_prompt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy_prompt: Option<String>,
    /// See [`AgentEntry::memory_prompt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_prompt: Option<String>,
    /// See [`AgentEntry::skills_prompt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_prompt: Option<String>,
    /// See [`AgentEntry::tool_prompts`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_prompts: Option<ToolPromptOverrides>,
    /// See [`AgentEntry::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// See [`AgentEntry::tools`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolPermissions>,
    /// See [`AgentEntry::environment`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub environment: Option<AgentEnvironmentPatch>,
    /// See [`AgentEntry::subagents`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub subagents: Option<Vec<SubagentRef>>,
}

impl From<AgentEntry> for AgentEntryPatch {
    fn from(entry: AgentEntry) -> Self {
        Self {
            settings: entry.settings.into(),
            label: Some(entry.label),
            system_prompt: Some(entry.system_prompt),
            live_prompt: Some(entry.live_prompt),
            wrap_up_prompt: Some(entry.wrap_up_prompt),
            prompt_mode: Some(entry.prompt_mode),
            platform_prompt: Some(entry.platform_prompt),
            environment_prompt: entry.environment_prompt,
            tool_policy_prompt: Some(entry.tool_policy_prompt),
            memory_prompt: Some(entry.memory_prompt),
            skills_prompt: Some(entry.skills_prompt),
            tool_prompts: Some(entry.tool_prompts),
            enabled: Some(entry.enabled),
            tools: Some(entry.tools),
            environment: Some(AgentEnvironmentPatch {
                name: Some(entry.environment.name),
                network: Some(EnvironmentNetworkPatch {
                    mode: Some(entry.environment.network.mode),
                    allow: Some(entry.environment.network.allow),
                }),
                always_use_own: Some(entry.environment.always_use_own),
            }),
            subagents: Some(entry.subagents),
        }
    }
}

/// A patch that adds one `exec` rule to an agent and changes nothing else.
///
/// Built from the whole stored entry, because `agents.list.*` replaces
/// wholesale: a patch naming only the rules would reset every other field.
/// A rule the agent already has is not added twice.
pub fn with_exec_rule(config: &Config, agent_id: &str, rule: ExecRule) -> ConfigPatch {
    let mut entry = config
        .agents
        .list
        .get(agent_id)
        .cloned()
        .unwrap_or_default();
    if !entry.settings.exec.rules.contains(&rule) {
        entry.settings.exec.rules.push(rule);
    }
    ConfigPatch {
        agents: Some(AgentsConfigPatch {
            list: Some(IndexMap::from([(
                agent_id.to_owned(),
                Some(AgentEntryPatch::from(entry)),
            )])),
        }),
        ..ConfigPatch::default()
    }
}

/// The `agents` half of a patch. Refuses unknown keys, for the reason given on
/// [`ConfigPatch`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct AgentsConfigPatch {
    /// `null` deletes the agent; an object creates or updates one. An absent
    /// key means "not mentioned", so removing an agent needs a syntax the
    /// merge can tell apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "IndexMap<String, Nullable<AgentEntryPatch>>")]
    #[garde(custom(crate::json::validate_optional_map_options))]
    pub list: Option<IndexMap<String, Option<AgentEntryPatch>>>,
}

/// A patch over [`ProviderConfig`]. `type` is optional here, which is right
/// for editing an instance that already has one; creating an instance without
/// naming a type fails the merged tree's re-parse.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ProviderConfigPatch {
    /// See [`ProviderConfig::kind`].
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub kind: Option<String>,
    /// See [`ProviderConfig::label`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// See [`ProviderConfig::api_base`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// See [`ProviderConfig::extra_headers`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_headers: Option<IndexMap<String, String>>,
    /// See [`ProviderConfig::models`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    /// See [`ProviderConfig::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// A patch over [`AuthConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AuthConfigPatch {
    /// See [`AuthConfig::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// See [`AuthConfig::session_ttl_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub session_ttl_ms: Option<u64>,
    /// See [`AuthConfig::rate_limit_per_minute`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub rate_limit_per_minute: Option<u64>,
    /// See [`AuthConfig::signed_url_ttl_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub signed_url_ttl_ms: Option<u64>,
}

/// A patch over [`ServerConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ServerConfigPatch {
    /// See [`ServerConfig::host`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub host: Option<String>,
    /// See [`ServerConfig::port`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = 65_535))]
    pub port: Option<u16>,
    /// See [`ServerConfig::allowed_hosts`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_hosts: Option<Vec<String>>,
    /// See [`ServerConfig::auth`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub auth: Option<AuthConfigPatch>,
    /// See [`ServerConfig::replay_buffer_size`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub replay_buffer_size: Option<u64>,
    /// See [`ServerConfig::turn_log_max_bytes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub turn_log_max_bytes: Option<u64>,
}

/// A patch over [`McpServerConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct McpServerConfigPatch {
    /// See [`McpServerConfig::kind`].
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<McpTransport>,
    /// See [`McpServerConfig::command`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// See [`McpServerConfig::args`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// See [`McpServerConfig::env`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<IndexMap<String, String>>,
    /// See [`McpServerConfig::url`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// See [`McpServerConfig::headers`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    /// `null` says this server does not use OAuth. Needed because `oauth` is
    /// genuinely optional rather than defaulted: "unset" is a real state, and
    /// an absent key already means "not mentioned".
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    #[schemars(with = "Nullable<McpOAuthConfig>")]
    #[garde(dive)]
    pub oauth: Option<Option<McpOAuthConfig>>,
    /// See [`McpServerConfig::tool_timeout_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub tool_timeout_ms: Option<u64>,
    /// See [`McpServerConfig::enabled_tools`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_tools: Option<Vec<String>>,
    /// See [`McpServerConfig::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// A patch over [`ToolsConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolsConfigPatch {
    /// `null` deletes the server, exactly as it does for a provider instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "IndexMap<String, Nullable<McpServerConfigPatch>>")]
    #[garde(custom(crate::json::validate_optional_map_options))]
    pub mcp_servers: Option<IndexMap<String, Option<McpServerConfigPatch>>>,
    /// See [`ToolsConfig::web`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub web: Option<WebToolsConfigPatch>,
}

/// A patch over [`WebToolsConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct WebToolsConfigPatch {
    /// See [`WebToolsConfig::search_provider`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_provider: Option<WebSearchProvider>,
    /// See [`WebToolsConfig::search_url`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_url: Option<String>,
    /// See [`WebToolsConfig::user_agent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// See [`WebToolsConfig::timeout_seconds`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub timeout_seconds: Option<u64>,
    /// See [`WebToolsConfig::read_timeout_seconds`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub read_timeout_seconds: Option<u64>,
    /// See [`WebToolsConfig::max_bytes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub max_bytes: Option<u64>,
    /// See [`WebToolsConfig::cache_entries`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 0, max = MAX_SAFE_INTEGER))]
    pub cache_entries: Option<u64>,
    /// See [`WebToolsConfig::cache_ttl_seconds`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 0, max = MAX_SAFE_INTEGER))]
    pub cache_ttl_seconds: Option<u64>,
}

/// A patch over [`ChannelsConfig`]. Loose, unlike the rest: an extension
/// channel's config block is an unknown key here, and a stripping patch would
/// drop it on every save.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ChannelsConfigPatch {
    /// See [`ChannelsConfig::send_progress`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_progress: Option<bool>,
    /// See [`ChannelsConfig::send_tool_hints`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_tool_hints: Option<bool>,
    /// Every channel's own block.
    #[serde(flatten)]
    pub extra: LooseObject,
}

/// A patch over [`SchedulerConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SchedulerConfigPatch {
    /// See [`SchedulerConfig::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// See [`SchedulerConfig::concurrency`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub concurrency: Option<u64>,
    /// See [`SchedulerConfig::catch_up_on_boot`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up_on_boot: Option<bool>,
    /// See [`SchedulerConfig::run_retention`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub run_retention: Option<u64>,
}

/// A patch over [`ExtensionsConfig`]. An extension's settings block has to be
/// deletable, so a `null` value deletes one.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ExtensionsConfigPatch {
    /// See [`ExtensionsConfig::load`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<Vec<String>>,
    /// See [`ExtensionsConfig::disabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<Vec<String>>,
    /// See [`ExtensionsConfig::allow_override`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_override: Option<bool>,
    /// `null` deletes one extension's block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "IndexMap<String, Nullable<LooseObject>>")]
    pub settings: Option<IndexMap<String, Option<LooseObject>>>,
}

/// A patch over [`UiConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct UiConfigPatch {
    /// See [`UiConfig::locale`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// See [`UiConfig::reasoning`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningDisplay>,
    /// See [`UiConfig::expand_tool_output`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand_tool_output: Option<bool>,
    /// See [`UiConfig::expand_turn_stats`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand_turn_stats: Option<bool>,
    /// See [`UiConfig::timezone`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub timezone: Option<String>,
}

/// A settings patch from the UI or CLI.
///
/// Deep-partial: the settings panel saves one section at a time, so
/// `{agents: {list: {x: {model: "y"}}}}` must validate without restating the
/// sibling fields — and must not invent them.
///
/// **Refuses unknown keys, and that is the point.** A key-stripping patch
/// would give a client writing a section this build no longer has a 200 and a
/// save that changed nothing; refusing names the key, which is the only signal
/// that reaches an old client. Strict here and on `agents` alone, where the two
/// shapes a client can be wrong about live: a whole section, and a block inside
/// it. The nested patches stay loose deliberately — `extensions.settings`
/// holds shapes this layer cannot know, and a strict `agents.list.*` would
/// refuse the entry the settings panel reads back and sends whole.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct ConfigPatch {
    /// Agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub agents: Option<AgentsConfigPatch>,
    /// `null` deletes the instance; an object creates or updates one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "IndexMap<String, Nullable<ProviderConfigPatch>>")]
    #[garde(custom(crate::json::validate_optional_map_options))]
    pub providers: Option<IndexMap<String, Option<ProviderConfigPatch>>>,
    /// The server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub server: Option<ServerConfigPatch>,
    /// The tool layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub tools: Option<ToolsConfigPatch>,
    /// Channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub channels: Option<ChannelsConfigPatch>,
    /// The scheduler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub scheduler: Option<SchedulerConfigPatch>,
    /// Extensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub extensions: Option<ExtensionsConfigPatch>,
    /// Locale and time zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub ui: Option<UiConfigPatch>,
    /// The folder the workspaces live in. Stripped of its default like
    /// everything else here: a patch parsed from `{}` must not carry
    /// `workspaces: ""`, or every settings save would reset a configured
    /// folder to "unset".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<String>,
}

// Editing one agent's model and sampling settings

/// The fields a chat surface can move without opening the agent editor.
///
/// `model` and `provider` are one setting and travel together: a model sent
/// without the instance that offers it leaves `provider` naming an endpoint
/// that has never heard of the model. `Some(None)` — JSON `null` — means
/// *clear this*, and only the two genuinely optional fields accept it: cleared
/// means the request carries no such parameter and the provider applies its
/// own.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettingsChange {
    /// A new model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// A new provider instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// A new temperature, or `null` to clear it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub temperature: Option<Option<f64>>,
    /// A new reasoning effort, or `null` to clear it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub reasoning_effort: Option<Option<ReasoningEffort>>,
}

/// A patch that moves one agent's model or sampling settings and nothing else.
///
/// Written once because of the rule it encodes: **`agents.list.*` replaces
/// wholesale, so the patch *is* the agent.** A patch naming `model` alone does
/// not set one field — it replaces the entry and takes the label, the system
/// prompt, the tools, the environment and the subagent roster with it. So the
/// stored entry is read and sent back whole, and clearing a field means
/// omitting the key rather than nulling it, since a `null` would reach the
/// entry as a value and be rejected.
///
/// An id with no entry yields a patch that creates one: the honest answer for
/// an agent deleted underneath a conversation, since the turn falls back to the
/// default agent anyway and a half-agent under a dead id would be worse than a
/// whole one.
pub fn agent_settings_patch(
    config: &Config,
    agent_id: &str,
    changes: &AgentSettingsChange,
) -> ConfigPatch {
    let mut next: AgentEntryPatch = config
        .agents
        .list
        .get(agent_id)
        .cloned()
        .unwrap_or_default()
        .into();
    if let Some(model) = &changes.model {
        next.settings.model = Some(model.clone());
    }
    if let Some(provider) = &changes.provider {
        next.settings.provider = Some(provider.clone());
    }
    if let Some(temperature) = changes.temperature {
        next.settings.temperature = temperature;
    }
    if let Some(effort) = changes.reasoning_effort {
        next.settings.reasoning_effort = effort;
    }

    let mut list = IndexMap::new();
    list.insert(agent_id.to_owned(), Some(next));
    ConfigPatch {
        agents: Some(AgentsConfigPatch { list: Some(list) }),
        ..ConfigPatch::default()
    }
}

/// A JSON object that parses as a whole [`Config`], for callers holding one
/// as a value.
pub fn parse_config(value: Value) -> Result<Config, serde_json::Error> {
    serde_json::from_value(value)
}
