//! The provider registry: one table, and the resolution order over it.
//!
//! Every provider is metadata. Only the *wire protocol* needs code, and one
//! adapter (`openai-chat`) covers Ollama, LM Studio, llama.cpp, vLLM, OpenAI,
//! OpenRouter, DeepSeek, Groq, xAI and Gemini's compatibility endpoint. So
//! adding a provider is a table entry, not a type.
//!
//! What this module describes is a provider *type*. What an operator
//! configures is an *instance* of one; see `instances`. The two were the same
//! thing while the settings tree allowed one endpoint per provider; they are
//! not, and the distinction is why `providers.<key>` carries a `type` field.
//!
//! Order matters. The table is scanned in declaration order by
//! [`find_gateway`], so gateways come first: a key beginning `sk-or-` is
//! OpenRouter's whoever else might accept it, and detection must reach that
//! entry before a generic one.

use std::sync::LazyLock;

use darkwire_protocol::ProviderInfo;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The request/response shape a provider speaks.
///
/// Only `openai-chat` is implemented here. The other three are declared
/// because the table is the single source of truth for what a provider *is*,
/// and an entry that lies about its wire is worse than an entry that names a
/// wire whose adapter has not landed: the factory refuses the latter loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireProtocol {
    /// `POST /chat/completions`.
    OpenaiChat,
    /// Anthropic's Messages API.
    AnthropicMessages,
    /// Google's native `generateContent`.
    GeminiGenerate,
    /// OpenAI's Responses API.
    OpenaiResponses,
}

impl WireProtocol {
    /// The `kebab-case` spelling the table and the wire use.
    pub fn as_str(self) -> &'static str {
        match self {
            WireProtocol::OpenaiChat => "openai-chat",
            WireProtocol::AnthropicMessages => "anthropic-messages",
            WireProtocol::GeminiGenerate => "gemini-generate",
            WireProtocol::OpenaiResponses => "openai-responses",
        }
    }

    /// Parses the `kebab-case` spelling.
    pub fn parse(value: &str) -> Option<WireProtocol> {
        WIRE_PROTOCOLS
            .into_iter()
            .find(|wire| wire.as_str() == value)
    }
}

impl std::fmt::Display for WireProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The closed vocabulary a spec may name.
pub const WIRE_PROTOCOLS: [WireProtocol; 4] = [
    WireProtocol::OpenaiChat,
    WireProtocol::AnthropicMessages,
    WireProtocol::GeminiGenerate,
    WireProtocol::OpenaiResponses,
];

/// The name the token cap goes by on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensParam {
    /// The original name, accepted by the whole OpenAI-compatible range.
    #[default]
    MaxTokens,
    /// The longer name newer OpenAI models require.
    MaxCompletionTokens,
}

impl MaxTokensParam {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            MaxTokensParam::MaxTokens => "max_tokens",
            MaxTokensParam::MaxCompletionTokens => "max_completion_tokens",
        }
    }
}

/// A parameter override applied to models whose id contains `match`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOverride {
    /// The substring of the model id, matched case-insensitively.
    #[serde(rename = "match")]
    pub matches: String,
    /// Replaces the request's temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Replaces the request's token cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// One provider type.
///
/// Every field beyond the required four is optional and defaults to off, so a
/// new entry is as small as it deserves to be. An extension can supply one of
/// these at runtime, which is why the fields are owned rather than static.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "seven independent facts about a provider type, each read by a different caller"
)]
pub struct ProviderSpec {
    /// The registry id.
    pub id: String,
    /// For a person.
    pub display_name: String,
    /// Which adapter speaks to it.
    pub wire: WireProtocol,
    /// Substrings that identify this provider from a bare model name, so
    /// `claude-sonnet-4` resolves without the operator naming a provider.
    /// Matched case-insensitively with `-` and `_` treated as the same
    /// character.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Environment variable consulted when the vault holds no key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_key: Option<String>,
    /// Used when config supplies no `api_base`. Absent means one is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_api_base: Option<String>,
    /// Reachable without credentials, on this machine or the LAN.
    #[serde(default)]
    pub is_local: bool,
    /// Fronts many upstream models, so it is matched by key/base, not by
    /// model.
    #[serde(default)]
    pub is_gateway: bool,
    /// Credentials arrive from an OAuth flow rather than an API key.
    #[serde(default)]
    pub is_o_auth: bool,
    /// An API key prefix that identifies this provider unambiguously.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detect_by_key_prefix: Option<String>,
    /// A substring of `api_base` that identifies this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detect_by_base_keyword: Option<String>,
    /// The endpoint wants bare model ids: `openai/gpt-4o` is sent as `gpt-4o`.
    #[serde(default)]
    pub strip_model_prefix: bool,
    /// The prefix is part of the model id and must survive: `nvidia/foo`.
    #[serde(default)]
    pub preserve_model_prefix: bool,
    /// Newer OpenAI models reject `max_tokens` and require the longer name.
    #[serde(default)]
    pub max_tokens_param: MaxTokensParam,
    /// Headers every request carries: gateway attribution, API versions.
    #[serde(default)]
    pub default_headers: IndexMap<String, String>,
    /// Per-model parameter overrides.
    #[serde(default)]
    pub model_overrides: Vec<ModelOverride>,
    /// The endpoint understands `prompt_cache_key`.
    #[serde(default)]
    pub supports_prompt_caching: bool,
    /// What `reasoning_effort: off` becomes on this wire, merged into the body
    /// in place of `reasoning_effort`.
    ///
    /// Per entry because there is no agreed spelling. OpenAI added `none` to
    /// `reasoning_effort` itself, which is the default; OpenRouter takes a
    /// `reasoning` object instead and ignores the effort string entirely.
    /// Local servers are all over the place (Qwen3 under llama.cpp wants a
    /// template kwarg), so they are left on the default rather than guessed
    /// at, and an operator who knows better has `custom`.
    ///
    /// The honest limit: an endpoint that rejects whatever this sends falls to
    /// `drop_reasoning_effort`, which removes the parameter altogether. So
    /// `off` degrades to *unset*, the provider's own default, with the usual
    /// `degraded` notice saying so. There is no way to stop a model thinking
    /// from out here; this only ever asks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_off_body: Option<Map<String, Value>>,
    /// The endpoint answers `GET /models` with a catalogue.
    ///
    /// True for the whole OpenAI-compatible range, including every local
    /// server: Ollama, LM Studio, llama.cpp and vLLM all implement it, which
    /// is what makes an automatic model list possible. Declared per entry
    /// rather than inferred from `wire`, because the native wires landing
    /// later have their own catalogue endpoints on their own paths.
    #[serde(default)]
    pub supports_model_listing: bool,
}

impl ProviderSpec {
    /// The four required fields, everything else off.
    pub fn new(
        id: &str,
        display_name: &str,
        wire: WireProtocol,
        keywords: &[&str],
    ) -> ProviderSpec {
        ProviderSpec {
            id: id.to_owned(),
            display_name: display_name.to_owned(),
            wire,
            keywords: keywords
                .iter()
                .map(|keyword| (*keyword).to_owned())
                .collect(),
            env_key: None,
            default_api_base: None,
            is_local: false,
            is_gateway: false,
            is_o_auth: false,
            detect_by_key_prefix: None,
            detect_by_base_keyword: None,
            strip_model_prefix: false,
            preserve_model_prefix: false,
            max_tokens_param: MaxTokensParam::MaxTokens,
            default_headers: IndexMap::new(),
            model_overrides: Vec::new(),
            supports_prompt_caching: false,
            reasoning_off_body: None,
            supports_model_listing: false,
        }
    }
}

/// Gateways and local servers first, then direct providers.
fn build_table() -> Vec<ProviderSpec> {
    let chat = WireProtocol::OpenaiChat;
    vec![
        ProviderSpec {
            env_key: Some("OPENROUTER_API_KEY".to_owned()),
            default_api_base: Some("https://openrouter.ai/api/v1".to_owned()),
            is_gateway: true,
            detect_by_key_prefix: Some("sk-or-".to_owned()),
            detect_by_base_keyword: Some("openrouter".to_owned()),
            // OpenRouter ranks callers by attribution header; it is not
            // authentication.
            default_headers: IndexMap::from([("X-Title".to_owned(), "DarkWire".to_owned())]),
            supports_prompt_caching: true,
            supports_model_listing: true,
            // OpenRouter normalises reasoning across every upstream model
            // behind it, so this one object covers models that would each
            // spell it differently.
            reasoning_off_body: json!({"reasoning": {"enabled": false}})
                .as_object()
                .cloned(),
            ..ProviderSpec::new("openrouter", "OpenRouter", chat, &["openrouter"])
        },
        ProviderSpec {
            default_api_base: Some("http://127.0.0.1:11434/v1".to_owned()),
            is_local: true,
            detect_by_base_keyword: Some("11434".to_owned()),
            supports_model_listing: true,
            ..ProviderSpec::new("ollama", "Ollama", chat, &["ollama"])
        },
        ProviderSpec {
            default_api_base: Some("http://127.0.0.1:1234/v1".to_owned()),
            is_local: true,
            detect_by_base_keyword: Some("1234".to_owned()),
            supports_model_listing: true,
            ..ProviderSpec::new("lmstudio", "LM Studio", chat, &["lmstudio", "lm-studio"])
        },
        ProviderSpec {
            default_api_base: Some("http://127.0.0.1:8080/v1".to_owned()),
            is_local: true,
            supports_model_listing: true,
            ..ProviderSpec::new("llamacpp", "llama.cpp", chat, &["llamacpp", "llama-cpp"])
        },
        ProviderSpec {
            env_key: Some("VLLM_API_KEY".to_owned()),
            default_api_base: Some("http://127.0.0.1:8000/v1".to_owned()),
            is_local: true,
            supports_model_listing: true,
            ..ProviderSpec::new("vllm", "vLLM", chat, &["vllm"])
        },
        ProviderSpec {
            env_key: Some("OPENAI_API_KEY".to_owned()),
            default_api_base: Some("https://api.openai.com/v1".to_owned()),
            // `max_tokens` is rejected outright by the reasoning models rather
            // than being ignored, and the replacement is accepted by the rest
            // of the range.
            max_tokens_param: MaxTokensParam::MaxCompletionTokens,
            supports_prompt_caching: true,
            supports_model_listing: true,
            ..ProviderSpec::new("openai", "OpenAI", chat, &["gpt", "o1", "o3", "o4"])
        },
        ProviderSpec {
            env_key: Some("ANTHROPIC_API_KEY".to_owned()),
            default_api_base: Some("https://api.anthropic.com/v1".to_owned()),
            supports_prompt_caching: true,
            // No OpenAI-compatible endpoint worth depending on; the native wire
            // lands with the rest of the provider breadth. `create_provider`
            // says so plainly rather than letting a misconfiguration surface
            // as a 404 mid-turn.
            ..ProviderSpec::new(
                "anthropic",
                "Anthropic",
                WireProtocol::AnthropicMessages,
                &["claude", "anthropic"],
            )
        },
        ProviderSpec {
            env_key: Some("GEMINI_API_KEY".to_owned()),
            default_api_base: Some(
                "https://generativelanguage.googleapis.com/v1beta/openai".to_owned(),
            ),
            supports_model_listing: true,
            // Google's OpenAI compatibility layer. The native `gemini-generate`
            // wire is only needed for the features it does not expose.
            ..ProviderSpec::new("gemini", "Google Gemini", chat, &["gemini"])
        },
        ProviderSpec {
            env_key: Some("DEEPSEEK_API_KEY".to_owned()),
            default_api_base: Some("https://api.deepseek.com/v1".to_owned()),
            supports_prompt_caching: true,
            supports_model_listing: true,
            ..ProviderSpec::new("deepseek", "DeepSeek", chat, &["deepseek"])
        },
        ProviderSpec {
            env_key: Some("GROQ_API_KEY".to_owned()),
            default_api_base: Some("https://api.groq.com/openai/v1".to_owned()),
            detect_by_key_prefix: Some("gsk_".to_owned()),
            supports_model_listing: true,
            ..ProviderSpec::new("groq", "Groq", chat, &["groq"])
        },
        ProviderSpec {
            env_key: Some("XAI_API_KEY".to_owned()),
            default_api_base: Some("https://api.x.ai/v1".to_owned()),
            supports_model_listing: true,
            ..ProviderSpec::new("xai", "xAI", chat, &["grok", "xai"])
        },
        // The escape hatch: any OpenAI-compatible endpoint. No keywords and no
        // detection, so it is only ever selected by being named.
        ProviderSpec {
            env_key: Some("OPENAI_API_KEY".to_owned()),
            supports_model_listing: true,
            ..ProviderSpec::new("custom", "Custom", chat, &[])
        },
    ]
}

/// The built-in table, gateways and local servers first.
pub static PROVIDERS: LazyLock<Vec<ProviderSpec>> = LazyLock::new(build_table);

/// Every built-in id, in table order.
pub fn provider_ids() -> Vec<&'static str> {
    PROVIDERS.iter().map(|spec| spec.id.as_str()).collect()
}

/// Whether `value` names a built-in provider.
pub fn is_provider_id(value: &str) -> bool {
    PROVIDERS.iter().any(|spec| spec.id == value)
}

/// The spec one id names, in `specs`.
///
/// `specs` is the built-in table everywhere except where an extension's own
/// providers have to be visible: resolution and the settings listing.
pub fn find_provider<'a>(id: &str, specs: &'a [ProviderSpec]) -> Option<&'a ProviderSpec> {
    specs.iter().find(|spec| spec.id == id)
}

/// The built-in spec one id names.
pub fn find_builtin(id: &str) -> Option<&'static ProviderSpec> {
    find_provider(id, &PROVIDERS)
}

/// `-` and `_` are interchangeable in every provider and model id in the wild.
fn normalise(value: &str) -> String {
    value.to_lowercase().replace('-', "_")
}

/// The provider a bare model name implies.
///
/// Two passes, and the order is the point. An explicit `provider/model`
/// prefix is an assertion by whoever wrote the config and wins outright;
/// keyword matching is a guess and only runs when there is no assertion to
/// honour.
///
/// Gateways and local servers are skipped entirely. A gateway serves models
/// from everyone (`openrouter` would match nothing by keyword and match
/// everything by accident), so it is identified by key or base URL instead.
pub fn find_provider_by_model<'a>(
    model: &str,
    specs: &'a [ProviderSpec],
) -> Option<&'a ProviderSpec> {
    let direct = || {
        specs
            .iter()
            .filter(|spec| !spec.is_gateway && !spec.is_local)
    };
    let normalised = normalise(model);

    if let Some(slash) = normalised.find('/').filter(|slash| *slash > 0) {
        let prefix = &normalised[..slash];
        if let Some(named) = direct().find(|spec| normalise(&spec.id) == prefix) {
            return Some(named);
        }
    }

    direct().find(|spec| {
        spec.keywords
            .iter()
            .any(|keyword| normalised.contains(&normalise(keyword)))
    })
}

/// What [`find_gateway`] has to go on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatewayHints<'a> {
    /// A provider id the operator named.
    pub provider_id: Option<&'a str>,
    /// The API key, for prefix detection.
    pub api_key: Option<&'a str>,
    /// The base URL, for keyword detection.
    pub api_base: Option<&'a str>,
}

/// The gateway or local server implied by the credentials, not the model.
///
/// There is deliberately no fallback to "some local provider" when nothing
/// matches. Treating an unrecognised `api_base` as vLLM is how a direct
/// provider behind a corporate proxy ends up sending its requests somewhere
/// else entirely; returning `None` lets the caller fall through to model
/// matching, which is a better guess and an honest one.
pub fn find_gateway(hints: &GatewayHints<'_>) -> Option<&'static ProviderSpec> {
    if let Some(named) = hints.provider_id.and_then(find_builtin)
        && (named.is_gateway || named.is_local)
    {
        return Some(named);
    }

    let api_base = hints.api_base.map(str::to_lowercase);
    PROVIDERS.iter().find(|spec| {
        let by_key = spec
            .detect_by_key_prefix
            .as_deref()
            .zip(hints.api_key)
            .is_some_and(|(prefix, key)| key.starts_with(prefix));
        let by_base = spec
            .detect_by_base_keyword
            .as_deref()
            .zip(api_base.as_deref())
            .is_some_and(|(keyword, base)| base.contains(keyword));
        by_key || by_base
    })
}

/// What [`resolve_provider`] has to go on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveProviderOptions<'a> {
    /// From config. `auto`, empty or absent runs the full resolution order.
    pub provider: Option<&'a str>,
    /// The model name.
    pub model: Option<&'a str>,
    /// The API key.
    pub api_key: Option<&'a str>,
    /// The base URL.
    pub api_base: Option<&'a str>,
}

/// The provider to use, in the one order the whole system agrees on.
///
/// 1. An explicit id that exists: the operator said so.
/// 2. Gateway or local detection from the API key prefix or base URL.
/// 3. The model name.
///
/// Returns `None` rather than guessing when none of the three answer, so the
/// caller can report "no provider configured" instead of failing at the first
/// request with a 401 from somewhere unexpected.
pub fn resolve_provider(options: &ResolveProviderOptions<'_>) -> Option<&'static ProviderSpec> {
    let named = options.provider;
    if let Some(name) = named.filter(|name| !name.is_empty() && *name != "auto")
        && let Some(spec) = find_builtin(name)
    {
        return Some(spec);
    }

    if let Some(gateway) = find_gateway(&GatewayHints {
        provider_id: named,
        api_key: options.api_key,
        api_base: options.api_base,
    }) {
        return Some(gateway);
    }

    options
        .model
        .filter(|model| !model.is_empty())
        .and_then(|model| find_provider_by_model(model, &PROVIDERS))
}

/// The model id as this provider wants to receive it.
///
/// The stored model keeps its `provider/model` prefix so a session records
/// which provider produced it, but most endpoints reject a prefix they did
/// not issue. Three rules, in order:
///
/// - `preserve_model_prefix`: the prefix is part of the name upstream.
///   Untouched.
/// - `strip_model_prefix`: a gateway that wants bare ids. Everything before
///   the last `/` goes, so `openrouter/anthropic/claude` reduces correctly.
/// - otherwise, only a prefix naming *this* provider is removed. A gateway
///   model like `anthropic/claude-sonnet-4` keeps its prefix, because
///   upstream that prefix is the routing instruction.
pub fn resolve_model_id(spec: &ProviderSpec, model: &str) -> String {
    if spec.preserve_model_prefix {
        return model.to_owned();
    }
    let Some(slash) = model.find('/').filter(|slash| *slash > 0) else {
        return model.to_owned();
    };
    if spec.strip_model_prefix {
        return model.rsplit('/').next().unwrap_or(model).to_owned();
    }
    if normalise(&model[..slash]) == normalise(&spec.id) {
        model[slash + 1..].to_owned()
    } else {
        model.to_owned()
    }
}

/// A table entry as the settings UI sees it: the catalogue, not a configured
/// endpoint.
///
/// The projection lives here rather than in the HTTP layer so that adding a
/// provider stays a one-line table entry rather than a table entry plus a
/// route change. It carries no credential flag: a credential belongs to an
/// instance, and `describe_instance` is where that boolean is supplied.
pub fn describe_provider(spec: &ProviderSpec) -> ProviderInfo {
    ProviderInfo {
        id: spec.id.clone(),
        display_name: spec.display_name.clone(),
        wire: spec.wire.as_str().to_owned(),
        is_local: spec.is_local,
        is_gateway: spec.is_gateway,
        is_o_auth: spec.is_o_auth,
        default_api_base: spec.default_api_base.clone(),
        env_key: spec.env_key.clone(),
        supports_model_listing: spec.supports_model_listing,
    }
}

/// The first override whose `match` appears in the model id, if any.
pub fn model_override_for<'a>(spec: &'a ProviderSpec, model: &str) -> Option<&'a ModelOverride> {
    let needle = model.to_lowercase();
    spec.model_overrides
        .iter()
        .find(|override_| needle.contains(&override_.matches.to_lowercase()))
}
