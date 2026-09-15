//! From configuration to a working provider.
//!
//! The one place that turns a registry entry plus connection settings into a
//! `ChatProvider`, so every caller (the agent loop, the model-list endpoint,
//! the scheduler's cheap heartbeat model) gets the same resilience behaviour
//! without remembering to ask for it.
//!
//! A wire without an adapter is a loud `config` error naming the provider,
//! not a silent fallback to the OpenAI shape. Pointing `anthropic` at
//! `/chat/completions` would produce a 404 in the middle of a turn, which
//! reads as "the model is gone" rather than "this provider is not implemented
//! yet".
//!
//! Which adapters exist is a lookup rather than an `if`, and `wires` is how an
//! extension fills a gap in it. That is the whole of the seam: an extension
//! hands over a `ProviderSpec` (data) and, when this build has no adapter for
//! the wire it names, a `WireAdapter` (code), and both still go through
//! `with_resilience` here, so an extension's provider inherits retry, backoff
//! and timeout classification rather than having to remember to ask for them.

use std::sync::Arc;

use ghostai_core::{Clock, ErrorKind, GhostError, Result};
use ghostai_protocol::ProviderConfig;
use ghostai_security::RandomSource;
use indexmap::IndexMap;

use crate::registry::{ProviderSpec, find_builtin};
use crate::resilience::{ResilienceOptions, with_resilience};
use crate::types::{ChatProvider, WireAdapterOptions};
use crate::wires::{WireAdapters, wire_adapter_for};

/// Which provider to build: a registry id, or a spec directly, which is how
/// an extension supplies its own.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderRef {
    /// A built-in id.
    Id(String),
    /// A spec, built-in or not. Boxed: a spec is a page of table data and
    /// an id is a short string.
    Spec(Box<ProviderSpec>),
}

impl From<&str> for ProviderRef {
    fn from(id: &str) -> ProviderRef {
        ProviderRef::Id(id.to_owned())
    }
}

impl From<ProviderSpec> for ProviderRef {
    fn from(spec: ProviderSpec) -> ProviderRef {
        ProviderRef::Spec(Box::new(spec))
    }
}

/// Whether, and how, the provider is wrapped.
#[derive(Clone, Default)]
pub enum Resilience {
    /// The default decorator.
    #[default]
    Default,
    /// The bare adapter, for tests that assert wire behaviour.
    Disabled,
    /// The decorator with these settings.
    Configured(ResilienceOptions),
}

/// Everything [`create_provider`] needs.
#[derive(Clone)]
pub struct CreateProviderOptions {
    /// Which provider.
    pub provider: ProviderRef,
    /// From the credential vault. Never read from `config.yaml`.
    pub api_key: Option<String>,
    /// Overrides the spec's default base URL.
    pub api_base: Option<String>,
    /// Headers every request carries, over the spec's own.
    pub extra_headers: IndexMap<String, String>,
    /// Time to first response header.
    pub request_timeout_ms: Option<u64>,
    /// Longest gap between stream chunks.
    pub stream_idle_timeout_ms: Option<u64>,
    /// Reaches the adapter's tool-call ids and the retry jitter.
    pub random: Option<Arc<dyn RandomSource>>,
    /// Reaches the adapter's stream timings.
    pub clock: Option<Arc<dyn Clock>>,
    /// Whether the result is wrapped.
    pub resilience: Resilience,
    /// Wire adapters beyond the built-in one, supplied by extensions.
    pub wires: Option<WireAdapters>,
}

impl CreateProviderOptions {
    /// Options for `provider` with every setting at its default.
    pub fn new(provider: impl Into<ProviderRef>) -> CreateProviderOptions {
        CreateProviderOptions {
            provider: provider.into(),
            api_key: None,
            api_base: None,
            extra_headers: IndexMap::new(),
            request_timeout_ms: None,
            stream_idle_timeout_ms: None,
            random: None,
            clock: None,
            resilience: Resilience::Default,
            wires: None,
        }
    }
}

impl std::fmt::Debug for CreateProviderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateProviderOptions")
            .field("provider", &self.provider)
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

/// Builds the provider `options` describes, wrapped in resilience unless
/// told otherwise.
pub fn create_provider(options: CreateProviderOptions) -> Result<Arc<dyn ChatProvider>> {
    let spec = match options.provider {
        ProviderRef::Spec(spec) => *spec,
        ProviderRef::Id(id) => find_builtin(&id).cloned().ok_or_else(|| {
            GhostError::new(ErrorKind::Config, format!("Unknown provider \"{id}\""))
        })?,
    };

    let Some(adapter) = wire_adapter_for(spec.wire, options.wires.as_ref()) else {
        return Err(GhostError::new(
            ErrorKind::Config,
            format!(
                "Provider \"{}\" speaks the {wire} wire, which this build has no adapter for. Use \
                 an OpenAI-compatible provider, an endpoint that exposes one, or install an \
                 extension that contributes the {wire} wire.",
                spec.id,
                wire = spec.wire
            ),
        ));
    };

    let provider = adapter(WireAdapterOptions {
        spec,
        api_key: options.api_key,
        api_base: options.api_base,
        extra_headers: options.extra_headers,
        request_timeout_ms: options.request_timeout_ms,
        stream_idle_timeout_ms: options.stream_idle_timeout_ms,
        random: options.random.clone(),
        clock: options.clock,
    })?;

    Ok(match options.resilience {
        Resilience::Disabled => provider,
        Resilience::Default => with_resilience(
            provider,
            ResilienceOptions {
                random: options.random,
                ..ResilienceOptions::default()
            },
        ),
        Resilience::Configured(mut resilience) => {
            // Only supplies the default, so one `random` on the outer options
            // means one source for the whole stack, and an explicit
            // `resilience.random` still wins.
            resilience.random = resilience.random.or(options.random);
            with_resilience(provider, resilience)
        }
    })
}

/// The connection settings for one provider, with config layered over the
/// table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// The effective base URL. Empty when neither config nor the table has one.
    pub api_base: String,
    /// The spec's headers with the configured ones over them.
    pub extra_headers: IndexMap<String, String>,
}

/// The effective connection settings for `spec` under `config`.
///
/// Kept separate from [`create_provider`] because the settings UI needs to
/// show the effective values (which base URL a provider will actually use)
/// without opening a connection to find out.
pub fn resolve_connection(spec: &ProviderSpec, config: Option<&ProviderConfig>) -> Connection {
    // An empty string in config means "unset", not "the empty base URL": a
    // cleared text field in the settings panel must fall back to the default
    // rather than producing a provider that cannot resolve its own endpoint.
    let configured = config
        .and_then(|config| config.api_base.as_deref())
        .map(str::trim)
        .filter(|base| !base.is_empty());
    let mut extra_headers = spec.default_headers.clone();
    if let Some(config) = config {
        for (name, value) in &config.extra_headers {
            extra_headers.insert(name.clone(), value.clone());
        }
    }
    Connection {
        api_base: configured
            .map(str::to_owned)
            .or_else(|| spec.default_api_base.clone())
            .unwrap_or_default(),
        extra_headers,
    }
}
