//! One router on one port, and the two things it refuses to be built at all.
//!
//! Most of what `create_server` does is covered where it belongs — the auth
//! matrix walks the manifest, `tests/ui.rs` covers the bundle in isolation,
//! `tests/boot.rs` covers the policy predicate. What is left, and what this
//! file is for, is the wiring: the order of construction, the fallback
//! underneath the router, and the two configurations that produce an error
//! instead of a server.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;
use std::path::Path;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use darkwire_protocol::config::Config;
use darkwire_server::testkit::{TestServer, TestServerOptions, start_test_server};
use darkwire_server::ui::{INDEX_FILE, UiRoot};
use darkwire_server::version::SERVER_VERSION;
use serde_json::Value;
use tower::ServiceExt as _;

/// A `dist/` as a bundler would leave one: a shell and a hashed asset.
fn bundle(root: &Path) {
    fs::write(
        root.join(INDEX_FILE),
        "<!doctype html><title>DarkWire</title>",
    )
    .expect("a shell on disk");
    fs::create_dir_all(root.join("assets")).expect("an assets directory");
    fs::write(root.join("assets/app-abc123.js"), "console.log(1);\n").expect("an asset on disk");
}

fn server(options: TestServerOptions) -> TestServer {
    start_test_server(options).expect("a test server")
}

struct Answer {
    status: StatusCode,
    content_type: String,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("a JSON body")
    }
}

async fn request(test: &TestServer, method: Method, uri: &str, token: Option<&str>) -> Answer {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = test
        .router
        .clone()
        .oneshot(builder.body(Body::empty()).expect("a well-formed request"))
        .await
        .expect("the router answered");
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("a body");
    Answer {
        status,
        content_type,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

// The single-page app underneath the router

#[tokio::test]
async fn the_shell_is_served_at_the_root() {
    let dir = tempfile::tempdir().expect("a temporary bundle");
    bundle(dir.path());
    let test = server(TestServerOptions {
        ui: UiRoot::Dir(dir.path().to_path_buf()),
        ..TestServerOptions::default()
    });

    let answer = request(&test, Method::GET, "/", None).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert!(answer.body.contains("<title>DarkWire</title>"));
    assert_eq!(answer.content_type, "text/html; charset=utf-8");
}

#[tokio::test]
async fn a_path_only_the_client_knows_about_becomes_the_shell() {
    let dir = tempfile::tempdir().expect("a temporary bundle");
    bundle(dir.path());
    let test = server(TestServerOptions {
        ui: UiRoot::Dir(dir.path().to_path_buf()),
        ..TestServerOptions::default()
    });

    // A single-page app owns URLs the server has never heard of, so the router
    // matching none of them is what "the client routed it" looks like here.
    for uri in ["/settings", "/session/abc-123", "/workspaces/research"] {
        let answer = request(&test, Method::GET, uri, None).await;
        assert_eq!(answer.status, StatusCode::OK, "{uri}");
        assert!(answer.body.contains("<title>DarkWire</title>"), "{uri}");
    }
}

#[tokio::test]
async fn the_asset_bundle_is_served_from_the_same_origin() {
    let dir = tempfile::tempdir().expect("a temporary bundle");
    bundle(dir.path());
    let test = server(TestServerOptions {
        ui: UiRoot::Dir(dir.path().to_path_buf()),
        ..TestServerOptions::default()
    });

    let answer = request(&test, Method::GET, "/assets/app-abc123.js", None).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body, "console.log(1);\n");
    assert_eq!(answer.content_type, "text/javascript; charset=utf-8");
}

#[tokio::test]
async fn the_shell_is_served_without_a_credential() {
    let dir = tempfile::tempdir().expect("a temporary bundle");
    bundle(dir.path());
    let test = server(TestServerOptions {
        ui: UiRoot::Dir(dir.path().to_path_buf()),
        ..TestServerOptions::default()
    });

    // The UI is a static asset; every byte of data behind it is authenticated,
    // and a login screen that needed a session to load could never be reached.
    let answer = request(&test, Method::GET, "/settings", None).await;
    assert_eq!(answer.status, StatusCode::OK);
}

#[tokio::test]
async fn an_unknown_api_path_stays_a_json_404_even_with_a_bundle() {
    let dir = tempfile::tempdir().expect("a temporary bundle");
    bundle(dir.path());
    let test = server(TestServerOptions {
        ui: UiRoot::Dir(dir.path().to_path_buf()),
        ..TestServerOptions::default()
    });

    let answer = request(&test, Method::GET, "/api/nope", Some(&test.token)).await;
    // Answering it with HTML turns "no such route" into "the JSON parser
    // failed" somewhere entirely unrelated.
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.json()["error"]["code"], "not_found");
}

#[tokio::test]
async fn an_unknown_socket_path_stays_a_json_404_too() {
    let dir = tempfile::tempdir().expect("a temporary bundle");
    bundle(dir.path());
    let test = server(TestServerOptions {
        ui: UiRoot::Dir(dir.path().to_path_buf()),
        ..TestServerOptions::default()
    });

    let answer = request(&test, Method::GET, "/ws/nope", Some(&test.token)).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.json()["error"]["code"], "not_found");
}

#[tokio::test]
async fn a_post_to_an_unknown_path_is_never_answered_with_the_shell() {
    let dir = tempfile::tempdir().expect("a temporary bundle");
    bundle(dir.path());
    let test = server(TestServerOptions {
        ui: UiRoot::Dir(dir.path().to_path_buf()),
        ..TestServerOptions::default()
    });

    let answer = request(&test, Method::POST, "/anything", Some(&test.token)).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.json()["error"]["code"], "not_found");
}

#[tokio::test]
async fn a_build_with_no_bundle_answers_the_root_with_a_json_404() {
    // A headless install is honest rather than broken: `/api` works and `GET /`
    // says there is nothing there.
    let test = server(TestServerOptions::default());
    let answer = request(&test, Method::GET, "/", None).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.json()["error"]["code"], "not_found");
}

#[tokio::test]
async fn the_not_found_message_names_the_method_and_the_path() {
    let test = server(TestServerOptions::default());
    let answer = request(&test, Method::GET, "/api/nope", Some(&test.token)).await;
    let message = answer.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(message.contains("GET"), "{message}");
    assert!(message.contains("/api/nope"), "{message}");
}

// The two refusals that happen before a listener exists

#[test]
fn a_non_loopback_bind_with_authentication_off_is_refused_rather_than_served() {
    let mut config = Config::default();
    config.server.host = "0.0.0.0".to_owned();
    config.server.auth.enabled = false;

    let outcome = start_test_server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    });

    // A refusal, not a warning, and before there is anything to unwind.
    match outcome {
        Ok(_) => panic!("an unauthenticated LAN bind must not produce a server"),
        Err(refusal) => assert_eq!(refusal.kind, darkwire_core::ErrorKind::Config),
    }
}

#[test]
fn a_loopback_bind_with_authentication_off_is_served() {
    let mut config = Config::default();
    config.server.host = "127.0.0.1".to_owned();
    config.server.auth.enabled = false;
    start_test_server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    })
    .expect("a loopback bind has no network boundary to cross");
}

#[test]
fn a_password_set_at_boot_produces_a_server() {
    // The store opens its tables and the password is written *before* the boot
    // policy asks whether a login could ever succeed.
    start_test_server(TestServerOptions {
        password: Some("a-long-enough-password".to_owned()),
        ..TestServerOptions::default()
    })
    .expect("a server with a password");
}

// One version for the whole workspace

#[test]
fn the_server_version_is_the_crate_version() {
    // The hand-edited literal is gone; there is one number and it comes from
    // the crate metadata.
    assert_eq!(SERVER_VERSION, env!("CARGO_PKG_VERSION"));
    assert!(!SERVER_VERSION.is_empty());
}

#[tokio::test]
async fn the_error_envelope_is_the_same_shape_whatever_produced_it() {
    let test = server(TestServerOptions::default());

    // A 404 from the fallback and a 401 from the auth layer are produced in
    // completely different places and must be indistinguishable in shape.
    let missing = request(&test, Method::GET, "/api/nope", Some(&test.token)).await;
    let refused = request(&test, Method::GET, "/api/status", None).await;

    for answer in [&missing, &refused] {
        let body = answer.json();
        assert!(body["error"]["code"].is_string());
        assert!(body["error"]["message"].is_string());
        assert_eq!(
            body.as_object().expect("an object").len(),
            1,
            "the envelope grew a second top-level key"
        );
    }
}
