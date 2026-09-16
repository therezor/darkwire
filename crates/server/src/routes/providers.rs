//! What can be talked to, and with which models.
//!
//! `GET /api/providers` answers two questions at once, because the settings
//! panel asks both: `types` is the catalogue an operator adds an endpoint from,
//! projected from the provider table, and `instances` is what they have
//! actually configured. Both projections live in `darkwire-providers` beside the
//! table itself, so adding a provider stays a one-line table entry rather than
//! a table entry plus a route change — and this route supplies the one thing
//! neither can know: whether a credential exists, which is the vault's business
//! and never leaves it as a value.

use std::collections::HashSet;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use darkwire_protocol::rest::{
    ModelInfo, ModelsResponse, ProviderTestRequest, ProviderTestResponse, ProvidersResponse,
};
use darkwire_providers::{PROVIDERS, describe_instance, describe_provider, list_instances};
use indexmap::IndexMap;

use crate::errors::HttpError;
use crate::routes::AppState;
use crate::schema::parse_body;

/// The provider catalogue, and every endpoint configured from it.
pub async fn list(State(state): State<AppState>) -> Result<Json<ProvidersResponse>, HttpError> {
    let present = state.runtime.credentials_present();
    let config = state.runtime.config();
    Ok(Json(ProvidersResponse {
        types: PROVIDERS.iter().map(describe_provider).collect(),
        instances: list_instances(&config.providers, &PROVIDERS)
            .iter()
            .map(|instance| {
                describe_instance(
                    instance,
                    present.get(&instance.id).copied().unwrap_or(false),
                )
            })
            .collect(),
    }))
}

/// Ask one provider connection whether it answers, and with what.
///
/// Degrades rather than refusing when the runtime cannot probe, for the same
/// reason the model list falls back to the configured catalogue: `ok: false`
/// with a reason *is* the answer to "can this be reached", and a client that
/// had to branch on the transport to find out would render an error where there
/// is only an absence.
pub async fn test(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<ProviderTestResponse>, HttpError> {
    let raw = serde_json::from_slice(&body)
        .map_err(|error| HttpError::bad_request(format!("Invalid JSON body: {error}")))?;
    let request: ProviderTestRequest = parse_body("body", raw)?;

    let Some(pending) = state.runtime.test_provider(&request) else {
        return Ok(Json(ProviderTestResponse {
            ok: false,
            models: Vec::new(),
            reason: Some("unsupported".to_owned()),
            message: Some("This server cannot test provider connections.".to_owned()),
        }));
    };
    Ok(Json(pending.await?))
}

/// Models available to the configured provider instances.
pub async fn models(State(state): State<AppState>) -> Result<Json<ModelsResponse>, HttpError> {
    Ok(Json(list_models(&state, false).await?))
}

/// Re-fetch every provider instance model list, ignoring the cache.
///
/// A `POST` because it has an effect: it discards the cached catalogue and
/// reaches every configured endpoint again. The `GET` is what a page load uses,
/// and it must not turn a render loop into a flood of requests at somebody's
/// local model server.
pub async fn refresh_models(
    State(state): State<AppState>,
) -> Result<Json<ModelsResponse>, HttpError> {
    Ok(Json(list_models(&state, true).await?))
}

/// The models the settings tree names, with no endpoint asked.
///
/// Still the fallback even now that instances can be enumerated live, and it
/// earns the place twice over: a provider that is unreachable must not empty
/// the picker of the model a turn is currently using, and an endpoint with no
/// catalogue route has nothing else to offer. A model an operator typed into
/// `providers.<id>.models` is not a guess — it is a statement of intent.
fn configured_models(state: &AppState) -> Result<ModelsResponse, HttpError> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut models: Vec<ModelInfo> = Vec::new();

    let mut add = |provider_id: &str, id: &str, provider_type: Option<&str>| {
        if id.is_empty() || !seen.insert(format!("{provider_id} {id}")) {
            return;
        }
        models.push(ModelInfo {
            id: id.to_owned(),
            provider_id: provider_id.to_owned(),
            provider_type: provider_type.map(str::to_owned),
            display_name: None,
            context_window_tokens: None,
            supports_tools: None,
            supports_vision: None,
            supports_reasoning: None,
        });
    };

    let config = state.runtime.config();
    for instance in list_instances(&config.providers, &PROVIDERS) {
        for model in &instance.config.models {
            add(&instance.id, model, Some(&instance.spec.id));
        }
    }

    let agent = state.runtime.agent(None)?;
    if agent.configured() {
        add(agent.provider(), agent.model(), None);
    }

    sort_models(&mut models);
    // Empty rather than one entry per provider saying "not fetched": `errors`
    // is for a list that was attempted and failed, and a client that renders it
    // would otherwise show a wall of failures for something nobody asked for.
    Ok(ModelsResponse {
        models,
        errors: IndexMap::new(),
    })
}

/// The live catalogue merged over the configured one.
///
/// The union rather than either alone. A fetch that succeeded is the better
/// answer and is listed first by the sort; a fetch that failed leaves whatever
/// the operator typed, so the picker does not empty itself the moment a laptop
/// closes. `errors` names the instances that could not be reached, which is
/// what lets the panel say *why* a list looks short.
async fn list_models(state: &AppState, refresh: bool) -> Result<ModelsResponse, HttpError> {
    let Some(pending) = state.runtime.models(refresh) else {
        return configured_models(state);
    };
    let fetched = pending.await?;

    let configured = configured_models(state)?;
    let seen: HashSet<String> = fetched
        .models
        .iter()
        .map(|model| format!("{} {}", model.provider_id, model.id))
        .collect();
    let mut models = fetched.models;
    models.extend(
        configured
            .models
            .into_iter()
            .filter(|model| !seen.contains(&format!("{} {}", model.provider_id, model.id))),
    );
    sort_models(&mut models);
    Ok(ModelsResponse {
        models,
        errors: fetched.errors,
    })
}

/// By instance, then by model id, so two renders of the same catalogue agree.
fn sort_models(models: &mut [ModelInfo]) {
    models.sort_by(|a, b| {
        a.provider_id
            .cmp(&b.provider_id)
            .then_with(|| a.id.cmp(&b.id))
    });
}
