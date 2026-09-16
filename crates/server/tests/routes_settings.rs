//! The settings tree over HTTP, and the credentials that are deliberately not
//! part of it.
//!
//! Two rules carry most of this file. A credential goes *in* through `PUT
//! /api/settings/credentials` and never comes back out, so the tests that
//! matter most are the ones asserting a key is absent from a response rather
//! than present in a store. And a patch that could not be served is refused at
//! save time, because the alternative is an operator with a config file whose
//! next boot is a refusal — a failure that surfaces on restart, long after the
//! change that caused it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use darkwire_protocol::config::Config;
use darkwire_server::testkit::{
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

async fn get(test: &TestServer) -> Value {
    let (status, body) = send(test, Method::GET, "/api/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    body
}

/// A raw body, so a malformed one can be sent at all.
async fn send_raw(test: &TestServer, method: Method, uri: &str, body: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {}", test.token))
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("a body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("a JSON body")
    };
    (status, value)
}

/// A config with one provider instance an operator would recognise.
fn with_provider() -> Config {
    let raw = json!({
        "providers": {
            "local": {"type": "ollama", "models": ["llama3"]},
        },
    });
    darkwire_protocol::config::parse_config(raw).expect("a parseable config")
}

// GET /api/settings

#[tokio::test]
async fn it_returns_the_settings_tree_and_the_presence_flags() {
    let mut present = indexmap::IndexMap::new();
    present.insert("local".to_owned(), true);
    let test = server(TestServerOptions {
        config: Some(with_provider()),
        runtime: FakeRuntimeOptions {
            credentials_present: present,
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let body = get(&test).await;
    assert_eq!(body["config"]["providers"]["local"]["type"], "ollama");
    assert_eq!(body["credentialsPresent"]["local"], true);
}

#[tokio::test]
async fn it_never_returns_a_credential() {
    let mut present = indexmap::IndexMap::new();
    present.insert("local".to_owned(), true);
    let test = server(TestServerOptions {
        config: Some(with_provider()),
        runtime: FakeRuntimeOptions {
            credentials_present: present,
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });
    // Store one the way a client would, then read the tree back.
    let (status, _) = send(
        &test,
        Method::PUT,
        "/api/settings/credentials",
        Some(json!({"namespace": "providers", "key": "local", "value": "sk-secret-value"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let rendered = get(&test).await.to_string();
    // Nothing relies on response serialisation to enforce this — it is enforced
    // by the response simply not containing one.
    assert!(
        !rendered.contains("sk-secret-value"),
        "the settings tree carried a credential"
    );
    assert!(rendered.contains("credentialsPresent"));
}

#[tokio::test]
async fn a_healthy_install_reports_no_load_error_and_no_warnings() {
    let test = server(TestServerOptions::default());
    let body = get(&test).await;
    // Absent rather than null: `loadError` means the file did not parse *at
    // all*, which is one string and one alert.
    assert!(body.get("loadError").is_none() || body["loadError"].is_null());
    assert_eq!(body["warnings"], json!([]));
}

#[tokio::test]
async fn it_reports_no_channels_on_a_build_that_ships_none() {
    let test = server(TestServerOptions::default());
    assert_eq!(get(&test).await["channels"], json!([]));
}

// PATCH /api/settings

#[tokio::test]
async fn a_deep_patch_leaves_untouched_fields_alone() {
    let test = server(TestServerOptions {
        config: Some(with_provider()),
        ..TestServerOptions::default()
    });

    let (status, body) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"providers": {"local": {"label": "Laptop"}}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["config"]["providers"]["local"]["label"], "Laptop");
    // A shallow partial would leave each field's default in place, so saving
    // one panel would rewrite every untouched field back to its default.
    assert_eq!(body["config"]["providers"]["local"]["type"], "ollama");
    assert_eq!(
        body["config"]["providers"]["local"]["models"],
        json!(["llama3"])
    );
}

#[tokio::test]
async fn the_patched_settings_are_what_the_next_read_serves() {
    let test = server(TestServerOptions {
        config: Some(with_provider()),
        ..TestServerOptions::default()
    });

    send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"providers": {"local": {"label": "Laptop"}}})),
    )
    .await;

    assert_eq!(
        get(&test).await["config"]["providers"]["local"]["label"],
        "Laptop"
    );
}

#[tokio::test]
async fn the_patch_reaches_the_runtime_rather_than_being_applied_here() {
    let test = server(TestServerOptions {
        config: Some(with_provider()),
        ..TestServerOptions::default()
    });

    send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"providers": {"local": {"label": "Laptop"}}})),
    )
    .await;

    // One patch, recorded once: the route is a transport, and rebuilding the
    // provider and the loops is the runtime's job.
    assert_eq!(test.runtime.patches().len(), 1);
}

#[tokio::test]
async fn a_patch_whose_settings_could_never_boot_is_refused() {
    let test = server(TestServerOptions::default());

    let (status, body) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"server": {"host": "0.0.0.0", "auth": {"enabled": false}}})),
    )
    .await;

    // Saving this would leave an operator with a config file whose next boot is
    // a refusal.
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
    // Refused means nothing was applied.
    assert!(test.runtime.patches().is_empty());
}

#[tokio::test]
async fn a_loopback_bind_with_authentication_off_is_still_savable() {
    // The refusal is about *exposure*, not about authentication: a loopback
    // bind has no network boundary to cross.
    let test = server(TestServerOptions::default());
    let (status, _) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"server": {"host": "127.0.0.1", "auth": {"enabled": false}}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_malformed_patch_is_a_422_pointing_at_the_field() {
    let test = server(TestServerOptions::default());

    let (status, body) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"server": {"port": "not a number"}})),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "bad_request");
    let details = body["error"]["details"]
        .as_object()
        .expect("a 422 carries details");
    assert!(
        details.keys().any(|key| key.contains("port")),
        "no pointer named the field that failed: {details:?}"
    );
}

#[tokio::test]
async fn a_body_that_is_not_json_at_all_is_a_400_rather_than_a_500() {
    let test = server(TestServerOptions::default());
    let (status, body) = send_raw(&test, Method::PATCH, "/api/settings", "{not json").await;
    // "Fix the request" rather than "something broke here".
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
}

// Renaming an agent, which travels on the settings body

/// A config with one operator-authored agent beside the default.
fn with_agent(id: &str) -> Config {
    let raw = json!({
        "agents": {
            "list": {
                id: {"label": "Reviewer", "model": "llama3"},
            },
        },
    });
    darkwire_protocol::config::parse_config(raw).expect("a parseable config")
}

#[tokio::test]
async fn a_rename_moves_the_agent_to_its_new_id() {
    let test = server(TestServerOptions {
        config: Some(with_agent("reviewer")),
        ..TestServerOptions::default()
    });

    let (status, body) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"renameAgents": [{"from": "reviewer", "to": "code-review"}]})),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["config"]["agents"]["list"]["code-review"].is_object());
    assert!(body["config"]["agents"]["list"].get("reviewer").is_none());
    assert_eq!(
        body["config"]["agents"]["list"]["code-review"]["label"],
        "Reviewer"
    );
}

#[tokio::test]
async fn a_rename_to_the_same_id_is_accepted_without_complaint() {
    let test = server(TestServerOptions {
        config: Some(with_agent("reviewer")),
        ..TestServerOptions::default()
    });

    let (status, body) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"renameAgents": [{"from": "reviewer", "to": "reviewer"}]})),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["config"]["agents"]["list"]["reviewer"].is_object());
}

#[tokio::test]
async fn a_rename_of_an_agent_that_does_not_exist_is_a_404() {
    let test = server(TestServerOptions::default());
    let (status, _) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"renameAgents": [{"from": "ghost", "to": "spectre"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_default_agent_cannot_be_renamed() {
    // It is resolvable whether or not it has an entry, and nothing downstream
    // can do without it.
    let test = server(TestServerOptions::default());
    let (status, body) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"renameAgents": [{"from": "default", "to": "primary"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let details = body["error"]["details"]
        .as_object()
        .expect("a 422 carries details");
    assert_eq!(details["/renameAgents/from"], "default");
}

#[tokio::test]
async fn a_rename_onto_a_taken_id_is_a_conflict() {
    let raw = json!({
        "agents": {
            "list": {
                "reviewer": {"label": "Reviewer"},
                "writer": {"label": "Writer"},
            },
        },
    });
    let config = darkwire_protocol::config::parse_config(raw).expect("a parseable config");
    let test = server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    });

    let (status, _) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"renameAgents": [{"from": "reviewer", "to": "writer"}]})),
    )
    .await;
    // "Look again and decide", not "fix the request".
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_rename_to_an_unusable_id_is_refused_with_the_field_named() {
    let test = server(TestServerOptions {
        config: Some(with_agent("reviewer")),
        ..TestServerOptions::default()
    });

    let (status, body) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"renameAgents": [{"from": "reviewer", "to": "Not A Slug"}]})),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let details = body["error"]["details"]
        .as_object()
        .expect("a 422 carries details");
    assert_eq!(details["/renameAgents/to"], "Not A Slug");
}

#[tokio::test]
async fn a_refused_rename_changes_nothing_at_all() {
    let test = server(TestServerOptions {
        config: Some(with_agent("reviewer")),
        ..TestServerOptions::default()
    });

    send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({
            "renameAgents": [{"from": "reviewer", "to": "default"}],
            "providers": {"local": {"type": "ollama"}},
        })),
    )
    .await;

    // The rename and the edit travel together precisely so one cannot land
    // without the other.
    assert!(test.runtime.patches().is_empty());
    let body = get(&test).await;
    assert!(body["config"]["agents"]["list"]["reviewer"].is_object());
    assert!(body["config"]["providers"].get("local").is_none());
}

#[tokio::test]
async fn a_rename_and_an_entry_edit_land_as_one_write() {
    let test = server(TestServerOptions {
        config: Some(with_agent("reviewer")),
        ..TestServerOptions::default()
    });

    let (status, body) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({
            "renameAgents": [{"from": "reviewer", "to": "code-review"}],
            "agents": {"list": {"code-review": {"model": "llama3.2"}}},
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    // Last-writer-wins on `agents.list`: the caller's entry is the edited one
    // and the rename's is the copy.
    assert_eq!(
        body["config"]["agents"]["list"]["code-review"]["model"],
        "llama3.2"
    );
    assert_eq!(test.runtime.patches().len(), 1, "two writes, not one");
}

#[tokio::test]
async fn two_renames_move_in_one_save() {
    let raw = json!({
        "agents": {
            "list": {
                "reviewer": {"label": "Reviewer"},
                "writer": {"label": "Writer"},
            },
        },
    });
    let config = darkwire_protocol::config::parse_config(raw).expect("a parseable config");
    let test = server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    });

    let (status, body) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"renameAgents": [
            {"from": "reviewer", "to": "code-review"},
            {"from": "writer", "to": "prose"},
        ]})),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let list = &body["config"]["agents"]["list"];
    assert!(list["code-review"].is_object());
    assert!(list["prose"].is_object());
    assert!(list.get("reviewer").is_none());
    assert!(list.get("writer").is_none());
}

#[tokio::test]
async fn a_delegation_to_a_renamed_agent_follows_it() {
    let raw = json!({
        "agents": {
            "list": {
                "reviewer": {"label": "Reviewer"},
                "lead": {"label": "Lead", "subagents": [{"id": "reviewer"}]},
            },
        },
    });
    let config = darkwire_protocol::config::parse_config(raw).expect("a parseable config");
    let test = server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    });

    let (status, body) = send(
        &test,
        Method::PATCH,
        "/api/settings",
        Some(json!({"renameAgents": [{"from": "reviewer", "to": "code-review"}]})),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    // Every intermediate state would have a dangling delegation, which is why
    // all three edits are one patch.
    assert_eq!(
        body["config"]["agents"]["list"]["lead"]["subagents"][0]["id"],
        "code-review"
    );
}

// POST /api/settings/reload

#[tokio::test]
async fn a_reload_answers_with_what_it_is_now_serving() {
    let test = server(TestServerOptions {
        config: Some(with_provider()),
        ..TestServerOptions::default()
    });

    let (status, body) = send(&test, Method::POST, "/api/settings/reload", None).await;

    assert_eq!(status, StatusCode::OK);
    // Not "did it work" but "what is it running now": a body of `{"ok": true}`
    // would send every caller straight back for the answer.
    assert_eq!(body["config"]["providers"]["local"]["type"], "ollama");
    assert_eq!(test.runtime.reloads(), 1);
}

#[tokio::test]
async fn a_reload_is_the_other_direction_from_a_patch() {
    let test = server(TestServerOptions::default());
    send(&test, Method::POST, "/api/settings/reload", None).await;
    // It takes what the *file* says and leaves it alone, so it records no
    // patch.
    assert!(test.runtime.patches().is_empty());
    assert_eq!(test.runtime.reloads(), 1);
}

// PUT /api/settings/credentials

#[tokio::test]
async fn storing_a_credential_answers_with_no_body() {
    let test = server(TestServerOptions::default());

    let (status, body) = send(
        &test,
        Method::PUT,
        "/api/settings/credentials",
        Some(json!({"namespace": "providers", "key": "local", "value": "sk-live"})),
    )
    .await;

    // A route that echoed what it stored would be a read path for a store that
    // has none.
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);

    let writes = test.runtime.credential_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].key, "local");
    assert_eq!(writes[0].value.as_deref(), Some("sk-live"));
}

#[tokio::test]
async fn a_null_value_deletes_the_entry() {
    let test = server(TestServerOptions::default());
    send(
        &test,
        Method::PUT,
        "/api/settings/credentials",
        Some(json!({"namespace": "providers", "key": "local", "value": "sk-live"})),
    )
    .await;

    let (status, _) = send(
        &test,
        Method::PUT,
        "/api/settings/credentials",
        Some(json!({"namespace": "providers", "key": "local", "value": null})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // `false`, not a removal: the settings panel distinguishes "no key" from
    // "never asked".
    assert_eq!(get(&test).await["credentialsPresent"]["local"], false);
}

#[tokio::test]
async fn a_stored_credential_shows_as_present_without_its_value() {
    let test = server(TestServerOptions::default());
    send(
        &test,
        Method::PUT,
        "/api/settings/credentials",
        Some(json!({"namespace": "providers", "key": "local", "value": "sk-live"})),
    )
    .await;

    let body = get(&test).await;
    assert_eq!(body["credentialsPresent"]["local"], true);
    assert!(!body.to_string().contains("sk-live"));
}

#[tokio::test]
async fn a_namespace_outside_the_known_set_is_refused() {
    let test = server(TestServerOptions::default());
    let (status, _) = send(
        &test,
        Method::PUT,
        "/api/settings/credentials",
        Some(json!({"namespace": "nowhere", "key": "local", "value": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(test.runtime.credential_writes().is_empty());
}

#[tokio::test]
async fn an_empty_key_is_refused_before_it_reaches_the_vault() {
    let test = server(TestServerOptions::default());
    let (status, _) = send(
        &test,
        Method::PUT,
        "/api/settings/credentials",
        Some(json!({"namespace": "providers", "key": "", "value": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(test.runtime.credential_writes().is_empty());
}

#[tokio::test]
async fn a_credential_write_that_is_not_json_is_a_400() {
    let test = server(TestServerOptions::default());
    let (status, _) = send_raw(&test, Method::PUT, "/api/settings/credentials", "]").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
