//! The read-only catalogues, and the two extension writes that are not
//! settings patches.
//!
//! Agents, tools, containers and MCP servers are all read-only over HTTP for the
//! same reason: each is either a subtree of the settings tree or live state
//! that has nowhere to live in a file, and a second write surface over the same
//! state would need its own merge rules and its own answer to what a partial
//! write means.
//!
//! The one that earns its own tests is `GET /api/tools`. It answers with the
//! *registry*, not one agent's advertised subset, and the difference is the
//! bug it was changed to fix: a tool the default agent did not hold had no row
//! on any agent, so it could never be granted to any agent — `automation`,
//! absent from the default agent on purpose, was invisible everywhere.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ghostai_protocol::config::Config;
use ghostai_protocol::environment::EnvironmentDefinition;
use ghostai_protocol::tools::{ToolDefinition, ToolRisk, ToolSource};
use ghostai_security::EnvironmentListing;
use ghostai_server::testkit::{
    FakeRuntimeOptions, TestServer, TestServerOptions, start_test_server,
};
use indexmap::IndexMap;
use serde_json::{Value, json};
use tower::ServiceExt as _;

fn server(options: TestServerOptions) -> TestServer {
    start_test_server(options).expect("a test server")
}

fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: format!("The {name} tool"),
        parameters: IndexMap::new(),
        risk: ToolRisk::Safe,
        source: ToolSource::default(),
        annotations: None,
    }
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

// GET /api/agents

#[tokio::test]
async fn an_install_that_named_no_agents_still_lists_the_default() {
    let test = server(TestServerOptions::default());
    let (status, body) = send(&test, Method::GET, "/api/agents", None).await;

    assert_eq!(status, StatusCode::OK);
    let agents = body["agents"].as_array().expect("an agent list");
    // The default agent is resolvable whether or not it has an entry, and
    // nothing downstream can do without it.
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["id"], "default");
}

#[tokio::test]
async fn the_operators_agents_come_after_the_default() {
    let test = server(TestServerOptions {
        config: Some(config_with(json!({
            "agents": {"list": {"reviewer": {"label": "Reviewer", "model": "llama3"}}},
        }))),
        ..TestServerOptions::default()
    });

    let (_, body) = send(&test, Method::GET, "/api/agents", None).await;
    let agents = body["agents"].as_array().expect("an agent list");
    // Already ordered, so a picker never has to sort and never renders a
    // different order twice.
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0]["id"], "default");
    assert_eq!(agents[1]["id"], "reviewer");
    assert_eq!(agents[1]["label"], "Reviewer");
    assert_eq!(agents[1]["model"], "llama3");
}

#[tokio::test]
async fn a_disabled_agent_is_left_out() {
    // It cannot run a turn, and this list is "every agent that can".
    let test = server(TestServerOptions {
        config: Some(config_with(json!({
            "agents": {"list": {"retired": {"label": "Retired", "enabled": false}}},
        }))),
        ..TestServerOptions::default()
    });

    let (_, body) = send(&test, Method::GET, "/api/agents", None).await;
    let agents = body["agents"].as_array().expect("an agent list");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["id"], "default");
}

#[tokio::test]
async fn an_agent_with_no_label_falls_back_to_its_id() {
    let test = server(TestServerOptions {
        config: Some(config_with(json!({
            "agents": {"list": {"reviewer": {"model": "llama3"}}},
        }))),
        ..TestServerOptions::default()
    });

    let (_, body) = send(&test, Method::GET, "/api/agents", None).await;
    let agents = body["agents"].as_array().expect("an agent list");
    assert_eq!(agents[1]["label"], "reviewer");
}

// GET /api/tools

#[tokio::test]
async fn it_offers_a_registered_tool_the_default_agent_does_not_hold() {
    let test = server(TestServerOptions {
        runtime: FakeRuntimeOptions {
            tools: vec![tool("read_file")],
            registered_tools: Some(vec![tool("automation"), tool("read_file")]),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (status, body) = send(&test, Method::GET, "/api/tools", None).await;
    assert_eq!(status, StatusCode::OK);

    let names: Vec<&str> = body["tools"]
        .as_array()
        .expect("a tool list")
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    // The one tool nobody starts with was, for a while, the one tool nobody
    // could grant.
    assert!(names.contains(&"automation"));
    assert!(names.contains(&"read_file"));
}

#[tokio::test]
async fn it_answers_with_an_empty_list_when_nothing_is_registered() {
    let test = server(TestServerOptions::default());
    let (status, body) = send(&test, Method::GET, "/api/tools", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tools"], json!([]));
}

#[tokio::test]
async fn a_tool_carries_the_schema_the_editor_draws_a_row_from() {
    let test = server(TestServerOptions {
        runtime: FakeRuntimeOptions {
            registered_tools: Some(vec![tool("exec")]),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    });

    let (_, body) = send(&test, Method::GET, "/api/tools", None).await;
    let entry = &body["tools"][0];
    assert_eq!(entry["name"], "exec");
    assert_eq!(entry["description"], "The exec tool");
    assert_eq!(entry["risk"], "safe");
    assert!(entry["parameters"].is_object());
}

#[tokio::test]
async fn sandbox_management_rejects_tool_execution_and_unknown_fields_at_http_boundary() {
    let test = server(TestServerOptions::default());
    for body in [
        json!({"op":"exec","argv":["true"]}),
        json!({"op":"stop","instance":"one","image":"untrusted"}),
    ] {
        let (status, _) = send(&test, Method::POST, "/api/sandboxes", Some(body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}

// GET /api/mcp

#[tokio::test]
async fn a_build_with_no_mcp_client_answers_with_an_empty_list() {
    let test = server(TestServerOptions::default());
    let (status, body) = send(&test, Method::GET, "/api/mcp", None).await;
    // Not a 501: "which MCP servers do you have" has a true answer here, and it
    // is "none".
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["servers"], json!([]));
}

// GET /api/extensions and the two writes

#[tokio::test]
async fn a_build_with_no_extension_host_lists_nothing_rather_than_refusing() {
    let test = server(TestServerOptions::default());
    let (status, body) = send(&test, Method::GET, "/api/extensions", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["extensions"], json!([]));
}

#[tokio::test]
async fn approving_on_a_build_with_no_host_is_a_404_where_the_listing_is_not() {
    let test = server(TestServerOptions::default());
    let (status, body) = send(
        &test,
        Method::POST,
        "/api/extensions/anything/approve",
        None,
    )
    .await;
    // The listing has a true answer for "which extensions do you have"; this
    // does not — there is nothing to approve anything *with*.
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn revoking_on_a_build_with_no_host_refuses_the_same_way() {
    let test = server(TestServerOptions::default());
    let (status, _) = send(&test, Method::POST, "/api/extensions/anything/revoke", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn both_extension_writes_need_a_session() {
    let test = server(TestServerOptions::default());
    for uri in [
        "/api/extensions/anything/approve",
        "/api/extensions/anything/revoke",
    ] {
        let request = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .body(Body::empty())
            .expect("a well-formed request");
        let response = test
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("the router answered");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}

// The command routes

#[tokio::test]
async fn an_install_with_no_extensions_contributes_no_commands() {
    let test = server(TestServerOptions::default());
    let (status, body) = send(&test, Method::GET, "/api/commands", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["commands"], json!([]));
}

#[tokio::test]
async fn a_command_that_does_not_exist_is_a_404() {
    let test = server(TestServerOptions::default());
    let (status, body) = send(
        &test,
        Method::POST,
        "/api/commands/slack-post",
        Some(json!({})),
    )
    .await;
    // A command that does not *exist* is the client asking for something wrong,
    // which is a 404 — unlike a command that runs and reports failure.
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("slack-post")
    );
}

#[tokio::test]
async fn a_command_body_that_is_not_json_is_a_400() {
    let test = server(TestServerOptions::default());
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/commands/anything")
        .header("authorization", format!("Bearer {}", test.token))
        .header("content-type", "application/json")
        .body(Body::from("{"))
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    // The body is read before the command is looked up, so a malformed one is
    // a 400 rather than the 404 the id would eventually have earned.
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// Containers, as an operator reviews them

/// A container definition, likewise built from JSON.
fn definition(overrides: &Value) -> EnvironmentDefinition {
    let mut value = json!({
        "schema": "ghostai.environment/1",
        "name": "dev",
        "image": format!("sha256:{}", "0".repeat(64)),
    });
    if let (Some(base), Some(extra)) = (value.as_object_mut(), overrides.as_object()) {
        for (key, item) in extra {
            base.insert(key.clone(), item.clone());
        }
    }
    serde_json::from_value(value).expect("a definition the schema accepts")
}

fn with_containers(definitions: Vec<EnvironmentDefinition>) -> TestServer {
    server(TestServerOptions {
        runtime: FakeRuntimeOptions {
            environments: definitions
                .into_iter()
                .map(|definition| EnvironmentListing {
                    name: definition.name.clone(),
                    path: PathBuf::from(format!("/containers/{}.yaml", definition.name)),
                    value: Some(definition),
                    problem: None,
                })
                .collect(),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    })
}

#[tokio::test]
async fn container_definitions_are_listed() {
    let test = with_containers(vec![definition(&json!({"shared": true}))]);
    let (_, body) = send(&test, Method::GET, "/api/environments", None).await;
    let entry = &body["environments"][0];
    assert_eq!(entry["name"], "dev");
    assert_eq!(entry["shared"], true);
    // A container reports where and how commands run.
    assert_eq!(entry["runtime"], "runc");
    assert_eq!(entry["workdir"], "/workspace");
    assert_eq!(entry["user"], "1000:1000");
    assert!(entry["limits"]["memoryMb"].is_number());
    // Nothing is loosened by default, and no egress request could be refused
    // by a container that weakens nothing.
    assert_eq!(entry["weakened"], json!([]));
    assert_eq!(entry["capsAdded"], json!([]));
    assert!(entry.get("gatewayProblem").is_none());
}

#[tokio::test]
async fn a_definition_that_weakens_the_environment_says_which_defences_it_drops() {
    let test = with_containers(vec![definition(&json!({
        "security": {
            "noNewPrivileges": false,
            "seccomp": "unconfined",
            "readOnlyRoot": false,
        },
        "caps": {"add": ["SYS_PTRACE"]},
    }))]);

    let (_, body) = send(&test, Method::GET, "/api/environments", None).await;
    let entry = &body["environments"][0];
    let weakened = entry["weakened"].as_array().expect("a weakened list");
    // The same list the terminal's review prints, so a browser and a terminal
    // cannot disagree about what a container is asking for.
    assert!(!weakened.is_empty(), "{weakened:?}");
    assert_eq!(entry["capsAdded"], json!(["SYS_PTRACE"]));
}

#[tokio::test]
async fn a_definition_that_names_no_user_is_itself_a_weakening() {
    // Naming no user leaves the image's default, which is commonly root. That
    // is worth saying out loud on the review screen even though the definition
    // asked for nothing: an operator approving it is approving that too.
    let test = with_containers(vec![definition(&json!({"user": ""}))]);

    let (_, body) = send(&test, Method::GET, "/api/environments", None).await;
    let weakened = body["environments"][0]["weakened"]
        .as_array()
        .expect("a weakened list");
    assert_eq!(weakened.len(), 1, "{weakened:?}");
    assert!(
        weakened[0].as_str().expect("a sentence").contains("root"),
        "{weakened:?}"
    );
}

#[tokio::test]
async fn an_environment_that_could_not_host_a_restricted_allow_list_says_so_in_advance() {
    // Shown before a save fails: an operator picking a container for an agent
    // that scopes its egress needs to know which ones cannot carry it.
    let test = with_containers(vec![definition(&json!({"user": "0:0"}))]);

    let (_, body) = send(&test, Method::GET, "/api/environments", None).await;
    let problem = body["environments"][0]["gatewayProblem"]
        .as_str()
        .expect("a gateway problem");
    assert!(problem.contains("restricted allow-list"), "{problem}");
}
