//! Liveness, what is running, and the generated document, over HTTP.
//!
//! `GET /api/status` is the one route whose whole job is to answer "what would
//! a turn do right now" rather than "what does the config file say". The
//! difference is only visible when the two disagree, so that is what most of
//! this file arranges: a settings save that moved the model, an install with no
//! model at all, a boot flag that a later patch must not appear to have
//! changed.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ghostai_protocol::config::Config;
use ghostai_server::testkit::{
    FakeRuntimeOptions, TestServer, TestServerOptions, start_test_server,
};
use ghostai_server::version::SERVER_VERSION;
use serde_json::{Value, json};
use tower::ServiceExt as _;

fn server(options: TestServerOptions) -> TestServer {
    start_test_server(options).expect("a test server")
}

async fn get(test: &TestServer, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .uri(uri)
        .header("authorization", format!("Bearer {}", test.token))
        .body(Body::empty())
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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("a JSON body")
    };
    (status, body)
}

// GET /api/health

#[tokio::test]
async fn health_reports_ok_while_the_database_answers() {
    let test = server(TestServerOptions::default());
    let request = Request::builder()
        .uri("/api/health")
        .body(Body::empty())
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("a body");
    let body: Value = serde_json::from_slice(&bytes).expect("a JSON body");
    assert_eq!(body["status"], "ok");
    // One named check rather than a bare boolean: a probe that says only "not
    // ok" leaves an operator with nowhere to start.
    assert_eq!(body["checks"][0]["name"], "database");
    assert_eq!(body["checks"][0]["status"], "ok");
    assert_eq!(body["checks"][0]["detail"], "");
}

#[tokio::test]
async fn health_is_the_one_route_that_needs_no_credential() {
    // A liveness probe is run by something that has no session and never will.
    let test = server(TestServerOptions::default());
    let request = Request::builder()
        .uri("/api/health")
        .body(Body::empty())
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
}

// GET /api/status

#[tokio::test]
async fn status_reports_what_a_turn_would_use_right_now() {
    let test = server(TestServerOptions {
        runtime: FakeRuntimeOptions {
            provider: Some("ollama".to_owned()),
            model: Some("llama3".to_owned()),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (status, body) = get(&test, "/api/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["version"], SERVER_VERSION);
    assert_eq!(body["provider"], "ollama");
    assert_eq!(body["model"], "llama3");
    assert_eq!(body["configured"], true);
    assert_eq!(body["authEnabled"], true);
    assert!(body["protocolVersion"].is_number());
}

#[tokio::test]
async fn status_reports_a_fresh_install_as_unconfigured_without_failing() {
    // Every other route works in this state, so reporting it must not be an
    // error — the client's answer is to offer setup.
    let test = server(TestServerOptions {
        runtime: FakeRuntimeOptions {
            configured: Some(false),
            provider: Some(String::new()),
            model: Some(String::new()),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (status, body) = get(&test, "/api/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], false);
    assert_eq!(body["model"], "");
    assert_eq!(body["provider"], "");
}

#[tokio::test]
async fn status_reports_uptime_from_the_monotonic_clock() {
    let test = server(TestServerOptions::default());

    let (_, before) = get(&test, "/api/status").await;
    assert_eq!(before["uptimeMs"], 0);

    test.clock.advance(std::time::Duration::from_secs(5));
    let (_, after) = get(&test, "/api/status").await;
    assert_eq!(after["uptimeMs"], 5_000);
}

#[tokio::test]
async fn status_uptime_survives_a_wall_clock_step_backwards() {
    // An NTP correction must not make a process look like it started in the
    // future, which is why the figure is monotonic rather than a subtraction of
    // two wall-clock readings.
    let test = server(TestServerOptions::default());
    test.clock.advance(std::time::Duration::from_secs(3));
    test.clock.set_now_ms(0);

    let (_, body) = get(&test, "/api/status").await;
    assert_eq!(body["uptimeMs"], 3_000);
}

#[tokio::test]
async fn status_reports_a_workspace_id_and_a_count_never_a_host_path() {
    let test = server(TestServerOptions::default());

    let (_, body) = get(&test, "/api/status").await;
    assert_eq!(body["workspaceId"], "default");
    // An absolute host path handed to every authenticated client names the
    // operator's account and directory layout, which is the one string that
    // turns a blind traversal attempt into a targeted one.
    assert!(body.get("workspacePath").is_none());
    let rendered = body.to_string();
    assert!(!rendered.contains(&test.home.path().to_string_lossy().into_owned()));
    assert!(body["workspaceCount"].as_u64().unwrap_or(0) >= 1);
}

#[tokio::test]
async fn status_counts_the_tools_the_agent_advertises_not_the_registry() {
    let tool = |name: &str| ghostai_protocol::tools::ToolDefinition {
        name: name.to_owned(),
        description: String::new(),
        parameters: indexmap::IndexMap::new(),
        risk: ghostai_protocol::tools::ToolRisk::Safe,
        source: ghostai_protocol::tools::ToolSource::default(),
        annotations: None,
    };
    let test = server(TestServerOptions {
        runtime: FakeRuntimeOptions {
            tools: vec![tool("read_file")],
            registered_tools: Some(vec![tool("read_file"), tool("automation")]),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (_, body) = get(&test, "/api/status").await;
    // What a turn would send, not the catalogue the agent editor draws rows
    // from.
    assert_eq!(body["toolCount"], 1);
}

#[tokio::test]
async fn status_reports_the_boot_auth_flag_rather_than_the_live_one() {
    let mut config = Config::default();
    config.server.auth.enabled = false;
    let test = server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    });

    let (_, body) = get(&test, "/api/status").await;
    // This reports whether the *running listener* authenticates, and that is
    // not something a settings save can change under an already-authenticated
    // session.
    assert_eq!(body["authEnabled"], false);
}

#[tokio::test]
async fn status_reports_zero_extensions_on_a_build_that_has_none() {
    let test = server(TestServerOptions::default());
    let (_, body) = get(&test, "/api/status").await;
    assert_eq!(body["mcpServersConnected"], 0);
    assert_eq!(body["extensionsLoaded"], 0);
}

// GET /api/openapi.json

#[tokio::test]
async fn the_openapi_route_serves_the_generated_document() {
    // What the document *says* is asserted in `tests/openapi.rs`, against the
    // generator rather than through a socket. This is only about the route
    // handing it over.
    let test = server(TestServerOptions::default());
    let (status, body) = get(&test, "/api/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, ghostai_server::openapi::openapi_document());
}

#[tokio::test]
async fn the_openapi_route_needs_a_session() {
    // The document names every route and every shape, which is a map of the
    // surface rather than a public brochure.
    let test = server(TestServerOptions::default());
    let request = Request::builder()
        .uri("/api/openapi.json")
        .body(Body::empty())
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_document_declares_this_build_s_version() {
    let test = server(TestServerOptions::default());
    let (_, document) = get(&test, "/api/openapi.json").await;
    let (_, status) = get(&test, "/api/status").await;
    // One version for the whole workspace: the document and the status line
    // read the same constant, so they cannot drift.
    assert_eq!(document["info"]["version"], status["version"]);
    assert_eq!(document["info"]["version"], json!(SERVER_VERSION));
}
