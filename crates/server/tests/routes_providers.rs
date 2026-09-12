//! What can be talked to, and with which models.
//!
//! The theme running through this file is that an absence is an answer. A
//! runtime with no adapter cannot probe a connection and cannot enumerate a
//! catalogue, and in both cases the honest reply is a *result* rather than an
//! error envelope: a client that had to branch on the transport to find out
//! would render a failure where there is only something not configured yet.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ghostai_protocol::config::Config;
use ghostai_server::testkit::{
    FakeRuntimeOptions, TestServer, TestServerOptions, start_test_server,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

fn server(options: TestServerOptions) -> TestServer {
    start_test_server(options).expect("a test server")
}

async fn send(
    test: &TestServer,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {}", test.token))
        .header("content-type", "application/json")
        .body(match &body {
            Some(value) => Body::from(value.to_string()),
            None => Body::empty(),
        })
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("a body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("a JSON body")
    };
    (status, value)
}

fn config_with(raw: Value) -> Config {
    ghostai_protocol::config::parse_config(raw).expect("a parseable config")
}

// GET /api/providers

#[tokio::test]
async fn it_describes_every_provider_type_in_the_registry() {
    let test = server(TestServerOptions::default());
    let (status, body) = send(&test, Method::GET, "/api/providers", None).await;

    assert_eq!(status, StatusCode::OK);
    let types = body["types"].as_array().expect("a catalogue");
    // A provider is data, so the catalogue is the table rather than a list this
    // route keeps in step with it by hand.
    assert_eq!(types.len(), ghostai_providers::PROVIDERS.len());
    for spec in ghostai_providers::PROVIDERS.iter() {
        assert!(
            types.iter().any(|entry| entry["id"] == spec.id),
            "{} is missing from the catalogue",
            spec.id
        );
    }
}

#[tokio::test]
async fn a_provider_type_carries_no_credential_flag() {
    let test = server(TestServerOptions::default());
    let (_, body) = send(&test, Method::GET, "/api/providers", None).await;
    // A credential belongs to a configured instance — two Ollama entries can
    // have different tokens — so the boolean lives on the instance and nowhere
    // else.
    for entry in body["types"].as_array().expect("a catalogue") {
        assert!(
            entry.get("credentialsPresent").is_none(),
            "a provider type claimed a credential"
        );
    }
}

#[tokio::test]
async fn it_lists_the_configured_instances_with_a_credential_flag_each() {
    let mut present = indexmap::IndexMap::new();
    present.insert("keyed".to_owned(), true);
    let test = server(TestServerOptions {
        config: Some(config_with(json!({
            "providers": {
                "keyed": {"type": "openai"},
                "bare": {"type": "ollama"},
            },
        }))),
        runtime: FakeRuntimeOptions {
            credentials_present: present,
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (_, body) = send(&test, Method::GET, "/api/providers", None).await;
    let instances = body["instances"].as_array().expect("the configured list");

    let keyed = instances
        .iter()
        .find(|entry| entry["id"] == "keyed")
        .expect("the keyed instance");
    let bare = instances
        .iter()
        .find(|entry| entry["id"] == "bare")
        .expect("the bare instance");
    assert_eq!(keyed["credentialsPresent"], true);
    assert_eq!(bare["credentialsPresent"], false);
}

#[tokio::test]
async fn an_install_that_configured_nothing_lists_no_instances() {
    let test = server(TestServerOptions {
        config: Some(config_with(json!({"providers": {}}))),
        ..TestServerOptions::default()
    });
    let (_, body) = send(&test, Method::GET, "/api/providers", None).await;
    assert_eq!(body["instances"], json!([]));
    // The catalogue is still there: it is what an operator adds an endpoint
    // *from*.
    assert!(!body["types"].as_array().expect("a catalogue").is_empty());
}

// POST /api/providers/test

#[tokio::test]
async fn a_runtime_that_cannot_probe_degrades_rather_than_failing() {
    let test = server(TestServerOptions::default());

    let (status, body) = send(
        &test,
        Method::POST,
        "/api/providers/test",
        Some(json!({"type": "ollama"})),
    )
    .await;

    // `ok: false` with a reason *is* the answer to "can this be reached".
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], false);
    assert_eq!(body["reason"], "unsupported");
    assert!(body["message"].is_string());
    assert_eq!(body["models"], json!([]));
}

#[tokio::test]
async fn a_malformed_test_body_is_a_422() {
    let test = server(TestServerOptions::default());
    let (status, body) = send(
        &test,
        Method::POST,
        "/api/providers/test",
        Some(json!({"type": 17})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn a_test_body_that_is_not_json_is_a_400() {
    let test = server(TestServerOptions::default());
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/providers/test")
        .header("authorization", format!("Bearer {}", test.token))
        .header("content-type", "application/json")
        .body(Body::from("<<<"))
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// GET /api/models and POST /api/models/refresh

#[tokio::test]
async fn it_lists_what_the_settings_name_plus_the_model_in_use() {
    let test = server(TestServerOptions {
        config: Some(config_with(json!({
            "providers": {"local": {"type": "ollama", "models": ["llama3", "mistral"]}},
        }))),
        runtime: FakeRuntimeOptions {
            provider: Some("local".to_owned()),
            model: Some("llama3".to_owned()),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (status, body) = send(&test, Method::GET, "/api/models", None).await;
    assert_eq!(status, StatusCode::OK);

    let ids: Vec<&str> = body["models"]
        .as_array()
        .expect("a model list")
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect();
    // A model an operator typed into `providers.<id>.models` is not a guess —
    // it is a statement of intent.
    assert!(ids.contains(&"llama3"));
    assert!(ids.contains(&"mistral"));
}

#[tokio::test]
async fn the_model_in_use_is_never_missing_from_the_picker() {
    // A provider that is unreachable must not empty the picker of the model a
    // turn is currently using.
    let test = server(TestServerOptions {
        config: Some(config_with(json!({"providers": {}}))),
        runtime: FakeRuntimeOptions {
            provider: Some("local".to_owned()),
            model: Some("llama3".to_owned()),
            configured: Some(true),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (_, body) = send(&test, Method::GET, "/api/models", None).await;
    let models = body["models"].as_array().expect("a model list");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["id"], "llama3");
    assert_eq!(models[0]["providerId"], "local");
}

#[tokio::test]
async fn an_unconfigured_install_offers_nothing_for_the_agent() {
    let test = server(TestServerOptions {
        config: Some(config_with(json!({"providers": {}}))),
        runtime: FakeRuntimeOptions {
            configured: Some(false),
            provider: Some(String::new()),
            model: Some(String::new()),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (status, body) = send(&test, Method::GET, "/api/models", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["models"], json!([]));
}

#[tokio::test]
async fn a_list_nobody_attempted_reports_no_errors() {
    let test = server(TestServerOptions::default());
    let (_, body) = send(&test, Method::GET, "/api/models", None).await;
    // `errors` is for a list that was attempted and failed; a client that
    // renders it would otherwise show a wall of failures for something nobody
    // asked for.
    assert_eq!(body["errors"], json!({}));
}

#[tokio::test]
async fn the_catalogue_is_sorted_so_two_renders_agree() {
    let test = server(TestServerOptions {
        config: Some(config_with(json!({
            "providers": {
                "zeta": {"type": "ollama", "models": ["b-model", "a-model"]},
                "alpha": {"type": "ollama", "models": ["z-model"]},
            },
        }))),
        runtime: FakeRuntimeOptions {
            configured: Some(false),
            provider: Some(String::new()),
            model: Some(String::new()),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (_, body) = send(&test, Method::GET, "/api/models", None).await;
    let pairs: Vec<(String, String)> = body["models"]
        .as_array()
        .expect("a model list")
        .iter()
        .map(|model| {
            (
                model["providerId"].as_str().unwrap_or_default().to_owned(),
                model["id"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let mut sorted = pairs.clone();
    sorted.sort();
    assert_eq!(pairs, sorted);
}

#[tokio::test]
async fn the_same_model_named_twice_is_listed_once() {
    let test = server(TestServerOptions {
        config: Some(config_with(json!({
            "providers": {"local": {"type": "ollama", "models": ["llama3", "llama3"]}},
        }))),
        runtime: FakeRuntimeOptions {
            provider: Some("local".to_owned()),
            model: Some("llama3".to_owned()),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (_, body) = send(&test, Method::GET, "/api/models", None).await;
    let llama = body["models"]
        .as_array()
        .expect("a model list")
        .iter()
        .filter(|model| model["id"] == "llama3")
        .count();
    assert_eq!(llama, 1);
}

#[tokio::test]
async fn a_refresh_answers_the_same_shape_as_the_listing() {
    let test = server(TestServerOptions {
        config: Some(config_with(json!({
            "providers": {"local": {"type": "ollama", "models": ["llama3"]}},
        }))),
        runtime: FakeRuntimeOptions {
            provider: Some("local".to_owned()),
            model: Some("llama3".to_owned()),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (listed_status, listed) = send(&test, Method::GET, "/api/models", None).await;
    let (refreshed_status, refreshed) =
        send(&test, Method::POST, "/api/models/refresh", None).await;

    assert_eq!(listed_status, StatusCode::OK);
    assert_eq!(refreshed_status, StatusCode::OK);
    // The `POST` exists because it has an effect — it discards the cache — not
    // because it answers differently.
    assert_eq!(listed, refreshed);
}
