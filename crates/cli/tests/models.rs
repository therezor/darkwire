//! `src/models.rs` — probing one endpoint, and the cache over the whole list.
//!
//! Everything here goes through [`probe_endpoint`], which owns what a listing
//! actually does: build a bare adapter, ask for a catalogue, classify what came
//! back. The listing above it is the same call once per enabled instance plus a
//! time-to-live, and both halves are exercised without a socket leaving the
//! machine — a scripted server stands in for the endpoint, and a hand-moved
//! clock stands in for time passing.

// `allow-unwrap-in-tests` covers a `#[test]` body; the fixture helpers beside
// them are ordinary functions, and a helper that returned `Result` would make
// every case carry a `?` for a temporary directory that cannot fail in a way
// the case could act on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture helpers in an integration test"
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use darkwire::models::{
    MODEL_CACHE_TTL_MS, MODEL_FETCH_TIMEOUT_MS, ModelCatalogue, ModelCatalogueOptions, ProbeResult,
    ProbeTarget, create_model_catalogue, probe_endpoint,
};
use darkwire_core::testkit::ManualClock;
use darkwire_core::{Clock, Database, SystemClock};
use darkwire_providers::testkit::{ScriptedResponse, ScriptedServer, models_body};
use darkwire_providers::{ProviderSpec, find_builtin};
use darkwire_runtime::{
    ExtensionChoice, McpChoice, RuntimeOptions, VaultChoice, WireRuntime, create_runtime,
};
use indexmap::IndexMap;
use serde_json::json;
use tempfile::TempDir;

/// A built-in spec, cloned so a test can bend one field of it.
fn spec_of(id: &str) -> ProviderSpec {
    find_builtin(id)
        .unwrap_or_else(|| panic!("no built-in provider {id}"))
        .clone()
}

fn target(spec: ProviderSpec, api_base: &str) -> ProbeTarget {
    ProbeTarget {
        spec,
        api_base: api_base.to_owned(),
        extra_headers: IndexMap::new(),
        api_key: None,
    }
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

#[tokio::test]
async fn answers_with_the_catalogue_the_endpoint_published() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(
        200,
        &models_body(&["qwen3", "llama3"]),
    ));

    let result = probe_endpoint(
        target(spec_of("ollama"), &server.base_url()),
        MODEL_FETCH_TIMEOUT_MS,
        clock(),
    )
    .await;

    match result {
        ProbeResult::Models(models) => {
            let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
            assert_eq!(ids, ["qwen3", "llama3"]);
        }
        ProbeResult::Failed { reason, message } => {
            panic!("expected a catalogue, got {reason}: {message}")
        }
    }
}

#[tokio::test]
async fn refuses_to_ask_an_endpoint_that_publishes_no_list() {
    // Its own reason rather than a transport failure: nothing was dialled, and
    // an operator told "could not connect" would go looking for a network
    // problem that does not exist.
    let mut spec = spec_of("ollama");
    spec.supports_model_listing = false;

    let result = probe_endpoint(target(spec, "http://127.0.0.1:1"), 50, clock()).await;

    match result {
        ProbeResult::Failed { reason, message } => {
            assert_eq!(reason, "unsupported");
            assert!(message.contains("does not publish a model list"));
        }
        ProbeResult::Models(_) => panic!("nothing should have been asked"),
    }
}

#[tokio::test]
async fn keeps_the_provider_crate_s_classification_of_a_refusal() {
    // `auth` and `transport` send an operator to two entirely different places,
    // so the verdict is passed along rather than re-derived from the message.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(
        401,
        &json!({"error": {"message": "invalid api key"}}),
    ));

    let result = probe_endpoint(
        target(spec_of("openai"), &server.base_url()),
        MODEL_FETCH_TIMEOUT_MS,
        clock(),
    )
    .await;

    match result {
        ProbeResult::Failed { reason, message } => {
            assert_eq!(reason, "auth");
            assert!(!message.is_empty());
        }
        ProbeResult::Models(_) => panic!("a 401 is not a catalogue"),
    }
}

#[tokio::test]
async fn reports_an_endpoint_that_never_answers_rather_than_waiting_on_it() {
    // The whole list is only as fast as its slowest member, and a laptop that
    // has closed since the config was written must not make a settings panel
    // look hung.
    let result = probe_endpoint(
        // A port nothing is listening on: the connection is refused rather than
        // hanging, which is the same branch a real unreachable endpoint takes.
        target(spec_of("ollama"), "http://127.0.0.1:1"),
        50,
        clock(),
    )
    .await;

    match result {
        ProbeResult::Failed { reason, .. } => {
            assert!(
                reason == "transport" || reason == "timeout",
                "an endpoint that did not answer should be transport or timeout, got {reason}"
            );
        }
        ProbeResult::Models(_) => panic!("nothing was listening"),
    }
}

#[tokio::test]
async fn gives_up_on_an_endpoint_that_accepts_and_then_says_nothing() {
    let server = ScriptedServer::start().await;
    server.push(
        ScriptedResponse::json(200, &models_body(&["qwen3"])).before(|| {
            // Longer than the deadline below, so the request is still in flight
            // when it passes.
            std::thread::sleep(Duration::from_millis(300));
        }),
    );

    let result = probe_endpoint(target(spec_of("ollama"), &server.base_url()), 40, clock()).await;

    match result {
        ProbeResult::Failed { reason, message } => {
            assert_eq!(reason, "timeout");
            assert!(message.contains("40ms"));
        }
        ProbeResult::Models(_) => panic!("the deadline passed first"),
    }
}

#[test]
fn the_manual_clock_measures_the_cache_rather_than_the_wall() {
    // The seam the time-to-live is written against: a test that had to wait a
    // minute to prove a cache expires is a test nobody runs.
    let clock = ManualClock::at(0);
    assert_eq!(clock.monotonic(), Duration::ZERO);
    clock.advance(Duration::from_secs(61));
    assert_eq!(clock.monotonic(), Duration::from_secs(61));
}

// ------------------------------------------------------------ the listing

/// An install whose `config.yaml` is the value given.
fn install(config: &serde_json::Value) -> (TempDir, Arc<WireRuntime>) {
    let temp = TempDir::new().expect("a temporary home");
    std::fs::write(
        temp.path().join("config.yaml"),
        serde_json::to_string_pretty(config).expect("the fixture config serialises"),
    )
    .expect("the config is written");
    let runtime = create_runtime(RuntimeOptions {
        home: Some(temp.path().to_string_lossy().into_owned()),
        env: Some(HashMap::new()),
        // Explicit rather than defaulted: the default opens a vault on demand,
        // and opening one writes a key to the OS keychain.
        vault: VaultChoice::None,
        mcp: McpChoice::Off,
        extensions: ExtensionChoice::Off,
        database: Some(Database::in_memory().expect("an in-memory database")),
        ..RuntimeOptions::default()
    })
    .expect("the runtime builds over a temporary home");
    (temp, runtime)
}

/// A catalogue over that runtime with no credentials and a hand-moved clock.
fn catalogue(runtime: &Arc<WireRuntime>, clock: &Arc<ManualClock>) -> ModelCatalogue {
    create_model_catalogue(
        Arc::clone(runtime),
        ModelCatalogueOptions {
            // No vault is opened: a listing that minted a keychain entry would
            // be a side effect of reading a list.
            credential_for: Arc::new(|_| None),
            timeout_ms: Some(2000),
            clock: Some(Arc::clone(clock) as Arc<dyn Clock>),
        },
    )
}

#[tokio::test]
async fn tags_every_model_with_the_instance_and_the_type_it_came_from() {
    // The endpoint answers with ids alone. Two instances of the same type would
    // otherwise offer the same id twice with nothing to tell them apart.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(200, &models_body(&["qwen3"])));

    let (_home, runtime) = install(&json!({
        "providers": {"laptop": {"type": "ollama", "apiBase": server.base_url()}}
    }));
    let clock = Arc::new(ManualClock::at(0));
    let listed = catalogue(&runtime, &clock).list(false).await.unwrap();

    assert_eq!(listed.models.len(), 1);
    assert_eq!(listed.models[0].id, "qwen3");
    assert_eq!(listed.models[0].provider_id, "laptop");
    assert_eq!(listed.models[0].provider_type.as_deref(), Some("ollama"));
    assert!(listed.errors.is_empty());
}

#[tokio::test]
async fn keeps_the_config_order_however_the_endpoints_answer() {
    // Collected by position rather than by completion order: the slow endpoint
    // is first in the file and has to stay first in the list.
    let slow = ScriptedServer::start().await;
    slow.push(
        ScriptedResponse::json(200, &models_body(&["slow-model"])).before(|| {
            std::thread::sleep(Duration::from_millis(120));
        }),
    );
    let quick = ScriptedServer::start().await;
    quick.push(ScriptedResponse::json(200, &models_body(&["quick-model"])));

    let (_home, runtime) = install(&json!({
        "providers": {
            "slow": {"type": "ollama", "apiBase": slow.base_url()},
            "quick": {"type": "ollama", "apiBase": quick.base_url()},
        }
    }));
    let clock = Arc::new(ManualClock::at(0));
    let listed = catalogue(&runtime, &clock).list(false).await.unwrap();

    let ids: Vec<&str> = listed
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(ids, ["slow-model", "quick-model"]);
}

#[tokio::test]
async fn one_endpoint_going_quiet_does_not_shorten_the_list_silently() {
    // The whole point of `errors`: a caller can say *which* endpoint went quiet
    // rather than showing a list that is mysteriously short.
    let working = ScriptedServer::start().await;
    working.push(ScriptedResponse::json(200, &models_body(&["reachable"])));
    let refusing = ScriptedServer::start().await;
    refusing.push(ScriptedResponse::json(
        401,
        &json!({"error": {"message": "invalid api key"}}),
    ));

    let (_home, runtime) = install(&json!({
        "providers": {
            "good": {"type": "ollama", "apiBase": working.base_url()},
            "bad": {"type": "openai", "apiBase": refusing.base_url()},
        }
    }));
    let clock = Arc::new(ManualClock::at(0));
    let listed = catalogue(&runtime, &clock).list(false).await.unwrap();

    assert_eq!(listed.models.len(), 1);
    assert_eq!(listed.models[0].provider_id, "good");
    assert!(listed.errors.contains_key("bad"), "{:?}", listed.errors);
    assert!(!listed.errors["bad"].is_empty());
}

#[tokio::test]
async fn never_asks_a_disabled_instance() {
    // A disabled instance is kept in the file and skipped by resolution; a
    // listing that dialled it anyway would wake a machine the operator turned
    // off on purpose.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(200, &models_body(&["unwanted"])));

    let (_home, runtime) = install(&json!({
        "providers": {
            "off": {"type": "ollama", "apiBase": server.base_url(), "enabled": false}
        }
    }));
    let clock = Arc::new(ManualClock::at(0));
    let listed = catalogue(&runtime, &clock).list(false).await.unwrap();

    assert!(listed.models.is_empty());
    assert!(listed.errors.is_empty());
    assert!(
        server.calls().is_empty(),
        "nothing should have been dialled"
    );
}

#[tokio::test]
async fn serves_the_cached_list_until_the_time_to_live_passes() {
    // Opening the settings panel twice must not reach a local model server
    // twice. Only one response is scripted, so a second request would be a 500
    // and show up as an error rather than as the same list.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(200, &models_body(&["cached"])));

    let (_home, runtime) = install(&json!({
        "providers": {"laptop": {"type": "ollama", "apiBase": server.base_url()}}
    }));
    let clock = Arc::new(ManualClock::at(0));
    let catalogue = catalogue(&runtime, &clock);

    let first = catalogue.list(false).await.unwrap();
    clock.advance(Duration::from_millis(MODEL_CACHE_TTL_MS - 1));
    let second = catalogue.list(false).await.unwrap();

    assert_eq!(first.models.len(), 1);
    assert_eq!(second.models.len(), 1);
    assert_eq!(server.calls().len(), 1);
}

#[tokio::test]
async fn asks_again_once_the_list_is_older_than_the_time_to_live() {
    // Pulling a new model and coming back should not be a puzzle.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(200, &models_body(&["first"])));
    server.push(ScriptedResponse::json(200, &models_body(&["first", "new"])));

    let (_home, runtime) = install(&json!({
        "providers": {"laptop": {"type": "ollama", "apiBase": server.base_url()}}
    }));
    let clock = Arc::new(ManualClock::at(0));
    let catalogue = catalogue(&runtime, &clock);

    assert_eq!(catalogue.list(false).await.unwrap().models.len(), 1);
    clock.advance(Duration::from_millis(MODEL_CACHE_TTL_MS + 1));
    assert_eq!(catalogue.list(false).await.unwrap().models.len(), 2);
    assert_eq!(server.calls().len(), 2);
}

#[tokio::test]
async fn a_refresh_bypasses_the_cache_outright() {
    // What the refresh button in the panel is for: the operator knows something
    // the time-to-live does not.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(200, &models_body(&["first"])));
    server.push(ScriptedResponse::json(200, &models_body(&["first", "new"])));

    let (_home, runtime) = install(&json!({
        "providers": {"laptop": {"type": "ollama", "apiBase": server.base_url()}}
    }));
    let clock = Arc::new(ManualClock::at(0));
    let catalogue = catalogue(&runtime, &clock);

    assert_eq!(catalogue.list(false).await.unwrap().models.len(), 1);
    // No time has passed at all, so only the flag can explain a second request.
    assert_eq!(catalogue.list(true).await.unwrap().models.len(), 2);
    assert_eq!(server.calls().len(), 2);
}

#[tokio::test]
async fn invalidating_drops_a_list_that_is_still_within_its_time_to_live() {
    // Called when something has happened the cache cannot know about — a
    // settings save, a credential written, a probe that just learned an
    // endpoint's catalogue first hand.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(200, &models_body(&["before"])));
    server.push(ScriptedResponse::json(200, &models_body(&["after"])));

    let (_home, runtime) = install(&json!({
        "providers": {"laptop": {"type": "ollama", "apiBase": server.base_url()}}
    }));
    let clock = Arc::new(ManualClock::at(0));
    let catalogue = catalogue(&runtime, &clock);

    assert_eq!(catalogue.list(false).await.unwrap().models[0].id, "before");
    catalogue.invalidate();
    assert_eq!(catalogue.list(false).await.unwrap().models[0].id, "after");
}

#[tokio::test]
async fn a_probe_through_the_catalogue_uses_the_catalogue_s_own_timeout() {
    // The method exists so the server's provider test and the listing agree on
    // how long an endpoint gets, rather than each choosing.
    let (_home, runtime) = install(&json!({"providers": {}}));
    let clock = Arc::new(ManualClock::at(0));
    let catalogue = catalogue(&runtime, &clock);

    let result = catalogue
        .probe(&target(spec_of("ollama"), "http://127.0.0.1:1"))
        .await;

    match result {
        ProbeResult::Failed { reason, .. } => {
            assert!(
                reason == "transport" || reason == "timeout",
                "nothing was listening, got {reason}"
            );
        }
        ProbeResult::Models(_) => panic!("nothing was listening"),
    }
}

#[test]
fn the_injected_seams_stay_out_of_the_debug_output() {
    // A credential callback and a clock have no useful rendering, and a
    // catalogue printed into a log line must not invite one to be added.
    let (_home, runtime) = install(&json!({"providers": {}}));
    let clock = Arc::new(ManualClock::at(0));
    let rendered = format!("{:?}", catalogue(&runtime, &clock));

    assert!(rendered.contains("ModelCatalogue"), "{rendered}");
    assert!(rendered.contains("timeout_ms"), "{rendered}");
    assert!(!rendered.contains("credential"), "{rendered}");
}

#[test]
fn the_options_render_the_one_field_worth_reading() {
    let options = ModelCatalogueOptions {
        credential_for: Arc::new(|_| None),
        timeout_ms: Some(42),
        clock: None,
    };
    let rendered = format!("{options:?}");
    assert!(rendered.contains("42"), "{rendered}");
}
