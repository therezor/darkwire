//! What models this install can actually reach.
//!
//! One catalogue, two callers. The REPL's `/model` wants a list of models and
//! nothing else; the server's adapter wants the same list plus a way to probe
//! one connection an operator has typed and not yet saved. Building the whole
//! server-facing port to answer either question was how this ended up inside a
//! closure once, so it lives here and both consume it.
//!
//! Three decisions carried over unchanged, because each of them was already
//! right:
//!
//!  - **A provider that cannot be reached is a normal state.** A laptop closes,
//!    a key expires. One endpoint going quiet must not fail the whole list, so
//!    a failure becomes an entry in [`ModelsResponse::errors`] beside the models
//!    that did arrive — which is what lets a caller say *which* endpoint went
//!    quiet rather than silently showing a shorter list.
//!  - **The adapter is built bare, outside the runtime's provider cache.** That
//!    cache is keyed by model as well as connection, and a catalogue has no
//!    model; going through it would let a settings refresh evict the adapter the
//!    next turn is about to want. Resilience is off for the same reason: retries
//!    and degradation are for a turn, and a catalogue that does not answer
//!    promptly should say so rather than spend fifteen seconds insisting.
//!  - **Credentials are read through an injected callback.** Reading one means
//!    opening the vault, and opening the vault mints a keychain entry the first
//!    time — a decision that belongs to whoever knows whether this install has
//!    one, not to the thing listing models.
//!
//! The two seams a test moves are the base URL, which points at a scripted
//! server instead of an endpoint, and the [`Clock`], which is what the cache's
//! time-to-live is measured against.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use darkwire_core::{Clock, Result, SystemClock, WireError};
use darkwire_protocol::config::ProvidersConfig;
use darkwire_protocol::{ModelInfo, ModelsResponse};
use darkwire_providers::{
    CreateProviderOptions, PROVIDERS, ProviderError, ProviderInstance, ProviderRef, ProviderSpec,
    Resilience, create_provider, list_instances, resolve_connection,
};
use darkwire_runtime::WireRuntime;
use indexmap::IndexMap;
use tokio_util::sync::CancellationToken;

/// How long a fetched catalogue is served before the endpoints are asked again.
///
/// Long enough that opening the settings panel twice does not reach a local
/// model server twice; short enough that pulling a new model and coming back is
/// not a puzzle. A refresh bypasses it outright, which is what the refresh
/// button in the UI is for.
pub const MODEL_CACHE_TTL_MS: u64 = 60_000;

/// How long one endpoint gets to answer before it is reported as unreachable.
///
/// Short on purpose: the whole list is only as fast as its slowest member, and
/// a laptop that has closed since the config was written must not make the
/// settings panel look hung. A timeout lands in `errors` beside a real refusal,
/// which is the honest place for it.
pub const MODEL_FETCH_TIMEOUT_MS: u64 = 5000;

/// One endpoint, resolved far enough to dial.
///
/// Carried as a value rather than an instance id because the two callers want
/// different things: the listing asks about something the config names, and a
/// provider test asks about something an operator has typed and not saved.
#[derive(Debug, Clone)]
pub struct ProbeTarget {
    /// Which provider type is being spoken to.
    pub spec: ProviderSpec,
    /// The effective base URL.
    pub api_base: String,
    /// The spec's headers with the configured ones over them.
    pub extra_headers: IndexMap<String, String>,
    /// The credential, when this install has one for the endpoint.
    pub api_key: Option<String>,
}

/// A catalogue, or why there is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    /// What the endpoint published.
    Models(Vec<ModelInfo>),
    /// Why it did not.
    ///
    /// `reason` is the whole value of this: `auth` means the endpoint answered
    /// and refused the key, `transport` means nothing answered at all, and
    /// those send an operator to two entirely different places. The provider
    /// crate classifies from the status and the socket error, never from
    /// message text, so this passes the verdict along rather than re-deriving
    /// one.
    Failed {
        /// The provider crate's classification.
        reason: String,
        /// The sentence.
        message: String,
    },
}

/// Reads the credential for one provider instance, or answers that there is
/// none.
///
/// A callback rather than a vault, because reading a credential can *create*
/// the vault — and an install that talks to a local model and never stores a key
/// should not acquire a keychain entry because something listed models.
pub type CredentialFor = Arc<dyn Fn(&ProviderInstance) -> Option<String> + Send + Sync>;

/// Everything the catalogue is injected with.
pub struct ModelCatalogueOptions {
    /// The key for one provider instance, if this install has one.
    pub credential_for: CredentialFor,
    /// Injected so a test does not wait out a real timeout.
    pub timeout_ms: Option<u64>,
    /// What the cache's age is measured against.
    pub clock: Option<Arc<dyn Clock>>,
}

impl std::fmt::Debug for ModelCatalogueOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelCatalogueOptions")
            .field("timeout_ms", &self.timeout_ms)
            .finish_non_exhaustive()
    }
}

/// The models this install can reach, cached for [`MODEL_CACHE_TTL_MS`].
pub struct ModelCatalogue {
    runtime: Arc<WireRuntime>,
    credential_for: CredentialFor,
    timeout_ms: u64,
    clock: Arc<dyn Clock>,
    cached: Mutex<Option<(Duration, ModelsResponse)>>,
}

impl std::fmt::Debug for ModelCatalogue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelCatalogue")
            .field("timeout_ms", &self.timeout_ms)
            .finish_non_exhaustive()
    }
}

/// Builds a catalogue over one runtime.
pub fn create_model_catalogue(
    runtime: Arc<WireRuntime>,
    options: ModelCatalogueOptions,
) -> ModelCatalogue {
    ModelCatalogue {
        runtime,
        credential_for: options.credential_for,
        timeout_ms: options.timeout_ms.unwrap_or(MODEL_FETCH_TIMEOUT_MS),
        clock: options.clock.unwrap_or_else(|| Arc::new(SystemClock)),
        cached: Mutex::new(None),
    }
}

/// One connection, asked for its catalogue. The only thing here that dials out.
///
/// A free function rather than a method so that the listing can run every
/// endpoint at once: each call owns everything it needs, which is what lets it
/// be spawned rather than awaited in turn.
pub async fn probe_endpoint(
    target: ProbeTarget,
    timeout_ms: u64,
    clock: Arc<dyn Clock>,
) -> ProbeResult {
    if !target.spec.supports_model_listing {
        return ProbeResult::Failed {
            reason: "unsupported".to_owned(),
            message: format!(
                "{} does not publish a model list, so there is nothing to ask it.",
                target.spec.display_name
            ),
        };
    }

    let built = create_provider(CreateProviderOptions {
        api_key: target.api_key.clone(),
        api_base: Some(target.api_base.clone()),
        extra_headers: target.extra_headers.clone(),
        clock: Some(clock),
        // Retries and degradation are for a turn.
        resilience: Resilience::Disabled,
        ..CreateProviderOptions::new(ProviderRef::Spec(Box::new(target.spec.clone())))
    });
    let provider = match built {
        Ok(provider) => provider,
        Err(error) => return describe_failure(&error),
    };

    let token = CancellationToken::new();
    let deadline = Duration::from_millis(timeout_ms);
    let outcome = tokio::time::timeout(deadline, provider.list_models(&token)).await;
    // Cancelled whatever happened: a request still in flight when the deadline
    // passed has to be told to stop, or the connection it holds outlives the
    // answer nobody is waiting for any more.
    token.cancel();
    provider.close().await;

    match outcome {
        Ok(Ok(models)) => ProbeResult::Models(models),
        Ok(Err(error)) => describe_failure(&error),
        Err(_) => ProbeResult::Failed {
            reason: "timeout".to_owned(),
            message: format!("{} did not answer within {timeout_ms}ms.", target.api_base),
        },
    }
}

impl ModelCatalogue {
    /// One connection, asked for its catalogue, with this catalogue's timeout.
    pub async fn probe(&self, target: &ProbeTarget) -> ProbeResult {
        probe_endpoint(target.clone(), self.timeout_ms, Arc::clone(&self.clock)).await
    }

    /// Drops the cached catalogue.
    ///
    /// Called whenever something has happened that the cache cannot know about:
    /// a settings save, a credential written, a successful probe that has just
    /// learned an endpoint's catalogue first hand. Without it the panel would go
    /// on serving a minute-old list that predates the fix the operator has just
    /// made.
    pub fn invalidate(&self) {
        if let Ok(mut cached) = self.cached.lock() {
            *cached = None;
        }
    }

    /// Every reachable model, cached for [`MODEL_CACHE_TTL_MS`].
    pub async fn list(&self, refresh: bool) -> Result<ModelsResponse> {
        let now = self.clock.monotonic();
        if !refresh && let Some(response) = self.fresh_enough(now) {
            return Ok(response);
        }

        let config = self.runtime.config();
        let listable: Vec<ProviderInstance> = list_instances(&config.providers, &PROVIDERS)
            .into_iter()
            .filter(|instance| instance.config.enabled && instance.spec.supports_model_listing)
            .collect();

        // All at once: the list is as slow as its slowest endpoint either way,
        // and in sequence it would be as slow as their sum. The credential is
        // read here, before anything is started, because reading one is the
        // caller's decision and must not happen on a task nobody is holding.
        let mut running = tokio::task::JoinSet::new();
        for (at, instance) in listable.iter().enumerate() {
            let target = self.target_for(&config.providers, instance);
            let timeout_ms = self.timeout_ms;
            let clock = Arc::clone(&self.clock);
            running.spawn(async move { (at, probe_endpoint(target, timeout_ms, clock).await) });
        }

        // Collected by position rather than by completion order, so the list is
        // the config's order however the endpoints happen to answer.
        let mut fetched: Vec<Option<ProbeResult>> = (0..listable.len()).map(|_| None).collect();
        while let Some(joined) = running.join_next().await {
            match joined {
                Ok((at, result)) => fetched[at] = Some(result),
                // A panic in a probe is this crate's bug, not the endpoint's.
                // It is reported as that endpoint going quiet rather than
                // taking the whole listing down with it.
                Err(error) => tracing::error!(%error, "a model probe did not finish"),
            }
        }

        let mut models: Vec<ModelInfo> = Vec::new();
        let mut errors: IndexMap<String, String> = IndexMap::new();
        for (instance, result) in listable.iter().zip(fetched) {
            match result {
                // `errors` is a map of prose, so the reason is dropped here
                // rather than carried: the response reports a list that came up
                // short, and the question "why exactly" is what a provider test
                // exists to answer.
                Some(ProbeResult::Failed { message, .. }) => {
                    errors.insert(instance.id.clone(), message);
                }
                Some(ProbeResult::Models(found)) => models.extend(tagged(found, instance)),
                None => {
                    errors.insert(
                        instance.id.clone(),
                        "The model probe did not finish.".to_owned(),
                    );
                }
            }
        }

        let response = ModelsResponse { models, errors };
        if let Ok(mut cached) = self.cached.lock() {
            *cached = Some((now, response.clone()));
        }
        Ok(response)
    }

    /// The cached response, when it is younger than the time-to-live.
    fn fresh_enough(&self, now: Duration) -> Option<ModelsResponse> {
        let cached = self.cached.lock().ok()?;
        let (at, response) = cached.as_ref()?;
        let age = now.checked_sub(*at)?;
        if age < Duration::from_millis(MODEL_CACHE_TTL_MS) {
            Some(response.clone())
        } else {
            None
        }
    }

    /// One instance, resolved far enough to dial, credential included.
    fn target_for(&self, providers: &ProvidersConfig, instance: &ProviderInstance) -> ProbeTarget {
        let connection = resolve_connection(&instance.spec, providers.get(&instance.id));
        ProbeTarget {
            spec: instance.spec.clone(),
            api_base: connection.api_base,
            extra_headers: connection.extra_headers,
            api_key: (self.credential_for)(instance),
        }
    }
}

/// Tags one instance's models with the instance and the type they came from.
///
/// The endpoint answers with ids alone; which of two endpoints of the same type
/// published a model is the catalogue's to record, and a picker that could not
/// say would offer the same id twice with nothing to tell them apart.
fn tagged(models: Vec<ModelInfo>, instance: &ProviderInstance) -> Vec<ModelInfo> {
    models
        .into_iter()
        .map(|model| ModelInfo {
            provider_id: instance.id.clone(),
            provider_type: Some(instance.spec.id.clone()),
            ..model
        })
        .collect()
}

/// Why a probe did not produce a catalogue, keeping the classification.
fn describe_failure(error: &WireError) -> ProbeResult {
    let reason = if ProviderError::is_provider_error(error) {
        ProviderError::reason_of(error).as_str().to_owned()
    } else {
        // The fallback for a failure the provider crate never classified: a
        // request the caller has to fix rather than an endpoint that went
        // quiet.
        "invalid_request".to_owned()
    };
    ProbeResult::Failed {
        reason,
        message: error.message.clone(),
    }
}
