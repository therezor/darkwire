//! An extension: a manifest, the code it names, and what it says it
//! contributes.
//!
//! The shape is deliberately the toolbox manifest's, because the two solve the
//! same problem. An extension's capabilities are not *settings* — they are the
//! boundary that decides what running it means — so they live in a file outside
//! the config tree, and `config.extensions` carries an id, an on/off and a
//! block of the extension's own settings, none of which can widen anything.
//!
//! ## Two schema versions, one shape
//!
//! `ghostai.extension/1` named `entry`: a module the host loaded into its own
//! process. `ghostai.extension/2` names `command`: an argv the host spawns as a
//! child process speaking JSON-RPC over its stdio. The difference is a process
//! boundary, so the two cannot be run by the same host — a v1 bundle reaching a
//! host that spawns lands on its row as `failed` with a sentence saying so.
//!
//! Both versions deserialise into *one* struct rather than a discriminated
//! union, and that is deliberate: a manifest whose version this build cannot run
//! still has to produce an id, a label and a `contributes` list, because those
//! are what the row explaining the refusal is made of. A union would make the
//! refusal a parse error, and a parse error has no id to hang a row on.
//!
//! Four fields are load-bearing. **`id` is the directory name, and the prefix
//! for everything**: a channel, a provider, a command and a tool all have to be
//! named `<id>` or `<id>-<suffix>`, so one namespace check covers four
//! registries. **`command` is an argv, never a shell line**, whose first element
//! is either a bare program name resolved on `PATH` or a path the security layer
//! resolves and refuses if it escapes the install directory — the same rule
//! `entry` had, because the approval digest covers that directory. **`env` is an
//! allow-list of host variable *names*, never values**, so the provider key in
//! the host's own environment does not land inside third-party code.
//! **`contributes` is disclosure, not enforcement**: it is what the approval
//! screen shows, and the host drops a registration whose kind is not listed.
//!
//! What is deliberately absent is a `permissions` block. A tool an extension
//! registers is granted exactly the way every other tool is: per agent, where
//! absent means disabled. A second permission vocabulary reachable from a
//! manifest would be a way to grant something the operator never enabled.

use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use indexmap::IndexMap;

use crate::json::prefault;

/// The registries an extension may write into.
///
/// `context` is the system-prompt contributor seam — the one memory and skills
/// arrive through — and is named for the interface rather than for "prompt",
/// because what it contributes is a section of context and the prompt is what
/// the sections add up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionContribution {
    /// Tools the model can call.
    Tools,
    /// Chat channels.
    Channels,
    /// Model providers.
    Providers,
    /// System-prompt sections.
    Context,
    /// Slash commands.
    Commands,
}

/// A semver range this build must satisfy.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ExtensionEngines {
    /// A semver range this build must satisfy. Empty means any.
    #[serde(default)]
    pub ghostai: String,
}

/// The manifest format tag.
///
/// An enum of the two rather than a literal of the current one, so a v1 bundle
/// still parses far enough to be *described* on the row that refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ExtensionSchemaVersion {
    /// A module loaded in-process. No host in this build can run one.
    #[serde(rename = "ghostai.extension/1")]
    V1,
    /// A child process speaking JSON-RPC over its stdio.
    #[serde(rename = "ghostai.extension/2")]
    V2,
}

/// The host variables a child inherits when the manifest names none.
///
/// Enough to find a program and behave like one run from a terminal, and
/// nothing that could be a credential. The same reasoning, and very nearly the
/// same list, as the MCP stdio connector's inherited set.
pub const DEFAULT_EXTENSION_ENV: &[&str] = &["PATH", "HOME", "LANG", "TMPDIR"];

/// Which parameter carries the output cap on a provider's wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionMaxTokensParam {
    /// What every OpenAI-compatible endpoint has always taken.
    #[default]
    MaxTokens,
    /// What newer OpenAI models require instead.
    MaxCompletionTokens,
}

/// A provider type an extension contributes, as manifest data.
///
/// Data rather than code, and that is the whole design: registering a provider
/// by handing the host a wire adapter — a function — is something an
/// out-of-process extension cannot do, and would route every generated token
/// through two extra hops if it could. An OpenAI-compatible endpoint
/// needs no code at all, which is the common case, so what an extension
/// contributes is the table entry and the host supplies the adapter it ships.
///
/// `wire` is a plain string, not an enum, and that is the load-bearing part: a
/// manifest naming a wire this build has no adapter for has to *parse*, so the
/// host can put "declares a provider on an unknown wire" on the extension's row
/// and register the rest. An enum would make it a manifest that does not load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the provider table's own independent flags, field for field"
)]
pub struct ExtensionProviderSpec {
    /// The registry id. Namespaced to the extension, like every other id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// For a person.
    #[serde(default)]
    pub display_name: String,
    /// Which adapter speaks to it. Unknown here is a warning, not a refusal.
    #[serde(default = "default_wire")]
    pub wire: String,
    /// Substrings that identify this provider from a bare model name.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Environment variable consulted when the vault holds no key.
    #[serde(default)]
    pub env_key: String,
    /// Used when config supplies no `api_base`. Empty means one is required.
    #[serde(default)]
    pub default_api_base: String,
    /// Reachable without credentials, on this machine or the LAN.
    #[serde(default)]
    pub is_local: bool,
    /// Fronts many upstream models, so it is matched by key/base, not model.
    #[serde(default)]
    pub is_gateway: bool,
    /// Credentials arrive from an OAuth flow rather than an API key.
    #[serde(default)]
    pub is_o_auth: bool,
    /// An API key prefix that identifies this provider unambiguously.
    #[serde(default)]
    pub detect_by_key_prefix: String,
    /// A substring of `api_base` that identifies this provider.
    #[serde(default)]
    pub detect_by_base_keyword: String,
    /// The endpoint wants bare model ids: `openai/gpt-4o` is sent as `gpt-4o`.
    #[serde(default)]
    pub strip_model_prefix: bool,
    /// The prefix is part of the model id and must survive: `nvidia/foo`.
    #[serde(default)]
    pub preserve_model_prefix: bool,
    /// Newer OpenAI models reject `max_tokens` and require the longer name.
    #[serde(default)]
    pub max_tokens_param: ExtensionMaxTokensParam,
    /// Headers every request carries: gateway attribution, API versions.
    #[serde(default)]
    pub default_headers: IndexMap<String, String>,
    /// The endpoint understands `prompt_cache_key`.
    #[serde(default)]
    pub supports_prompt_caching: bool,
    /// The endpoint answers `GET /models` with a catalogue.
    #[serde(default)]
    pub supports_model_listing: bool,
}

/// An extension manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ExtensionManifest {
    /// Which of the two contracts above this manifest is written against.
    pub schema: ExtensionSchemaVersion,
    /// Also the directory name, and the prefix every contributed id carries.
    #[garde(length(utf16, min = 1, max = 40))]
    pub id: String,
    /// The extension's own version string.
    #[serde(default = "default_version")]
    pub version: String,
    /// Shown in the UI. Empty falls back to the id.
    #[serde(default)]
    pub label: String,
    /// One sentence, shown beside the Approve button.
    #[serde(default)]
    pub description: String,
    /// The module a `ghostai.extension/1` host imported. Read on v1 only.
    ///
    /// Kept so that a v1 manifest still describes itself on the row that
    /// refuses it; a v2 manifest leaves it at its default and nothing reads it.
    #[serde(default = "default_entry")]
    #[garde(length(utf16, min = 1))]
    pub entry: String,
    /// The argv a `ghostai.extension/2` host spawns. Read on v2 only.
    ///
    /// Never a shell line: element zero is the program and the rest are its
    /// arguments, exactly as they reach `execve`. Empty on a v2 manifest is a
    /// refusal with a sentence, from the security layer, rather than a parse
    /// error — because the row explaining it still needs the id.
    #[serde(default)]
    pub command: Vec<String>,
    /// Host environment variable *names* the child may additionally inherit.
    ///
    /// Names, never values: a manifest cannot set a variable, only ask for one
    /// the host already has. See the module docs for what the default four buy.
    #[serde(default = "default_env")]
    pub env: Vec<String>,
    /// Provider types this extension contributes, as data.
    #[serde(default)]
    #[garde(dive)]
    pub providers: Vec<ExtensionProviderSpec>,
    /// What the operator is approving. See the module docs.
    #[serde(default)]
    pub contributes: Vec<ExtensionContribution>,
    /// Version constraints on the host.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub engines: ExtensionEngines,
}

fn default_version() -> String {
    "0.0.0".to_owned()
}

fn default_entry() -> String {
    "dist/index.js".to_owned()
}

fn default_wire() -> String {
    "openai-chat".to_owned()
}

fn default_env() -> Vec<String> {
    DEFAULT_EXTENSION_ENV
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}
