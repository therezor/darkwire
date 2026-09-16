//! The workspace manager over HTTP.
//!
//! Two invariants are worth testing at this level rather than in the store, and
//! both are about what a client can say. **No path ever crosses this
//! boundary**: a workspace is created by name, gets a derived slug, and lives
//! under the workspace root — so the tests that matter are the ones that send
//! something path-shaped and watch it be refused. And **deleting detaches
//! rather than removes**: there is no undo for a recursive delete of a tree
//! someone has been working in, so the files stay, the row goes, and a
//! workspace whose conversations still name it refuses with the count the UI
//! renders in its "move them first" affordance.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture or a response that cannot be read is a failing test either way"
)]

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use darkwire_core::session_store::CreateSession;
use darkwire_core::workspace_store::CreateWorkspace;
use darkwire_server::runtime::ServerRuntime as _;
use darkwire_server::testkit::{TestServer, TestServerOptions, start_test_server};
use serde_json::{Value, json};
use tower::ServiceExt as _;

// Harness

fn server() -> TestServer {
    start_test_server(TestServerOptions::default()).expect("a test server")
}

struct Answer {
    status: StatusCode,
    body: Value,
}

impl Answer {
    fn json(&self) -> &Value {
        &self.body
    }
}

async fn send(test: &TestServer, method: &str, uri: &str, body: Option<Value>) -> Answer {
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
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("a body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer { status, body }
}

async fn get(test: &TestServer, uri: &str) -> Answer {
    send(test, "GET", uri, None).await
}

async fn post(test: &TestServer, uri: &str, body: Value) -> Answer {
    send(test, "POST", uri, Some(body)).await
}

async fn patch(test: &TestServer, uri: &str, body: Value) -> Answer {
    send(test, "PATCH", uri, Some(body)).await
}

async fn delete(test: &TestServer, uri: &str) -> Answer {
    send(test, "DELETE", uri, None).await
}

/// The directory one workspace slug sits in.
fn folder(test: &TestServer, id: &str) -> std::path::PathBuf {
    test.home.path().join("workspace").join(id)
}

fn make_workspace(test: &TestServer, name: &str) -> String {
    let record = test
        .runtime
        .workspaces()
        .create(CreateWorkspace {
            name: name.to_owned(),
            ..CreateWorkspace::default()
        })
        .expect("the workspace was created");
    test.clock.advance(Duration::from_millis(1));
    record.id
}

fn bind_session(test: &TestServer, key: &str, workspace_id: &str) {
    test.runtime
        .store()
        .ensure_session(
            key,
            CreateSession {
                workspace_id: Some(workspace_id.to_owned()),
                ..CreateSession::default()
            },
        )
        .expect("the session was created");
    test.clock.advance(Duration::from_millis(1));
}

// Listing

#[tokio::test]
async fn the_listing_always_has_a_default_and_it_is_first() {
    let test = server();
    make_workspace(&test, "Research");

    let answer = get(&test, "/api/workspaces").await;
    assert_eq!(answer.status, StatusCode::OK);
    let rows = answer.json()["workspaces"].as_array().unwrap();
    assert_eq!(rows[0]["id"], "default");
    assert_eq!(rows[0]["isDefault"], true);
    assert_eq!(rows.len(), 2);
}

#[tokio::test]
async fn the_listing_reports_the_session_count_a_delete_would_have_to_move() {
    let test = server();
    let id = make_workspace(&test, "Research");
    bind_session(&test, "s-1", &id);
    bind_session(&test, "s-2", &id);

    let answer = get(&test, "/api/workspaces").await;
    let row = answer.json()["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id.as_str())
        .expect("the workspace is listed");
    assert_eq!(row["sessionCount"], 2);
}

#[tokio::test]
async fn the_listing_never_reports_a_path() {
    let test = server();
    make_workspace(&test, "Research");

    let answer = get(&test, "/api/workspaces").await;
    // A response that carried one would tell a client where the server keeps
    // its files, and nothing above this layer can use an absolute path.
    let rendered = answer.json().to_string();
    assert!(!rendered.contains(&test.home.path().to_string_lossy().into_owned()));
    for row in answer.json()["workspaces"].as_array().unwrap() {
        assert!(row.get("path").is_none());
        assert!(row.get("root").is_none());
    }
}

// Creating

#[tokio::test]
async fn creating_makes_a_workspace_and_its_folder_from_a_name_alone() {
    let test = server();
    let answer = post(&test, "/api/workspaces", json!({"name": "Client Work"})).await;

    assert_eq!(answer.status, StatusCode::CREATED);
    assert_eq!(answer.json()["name"], "Client Work");
    assert_eq!(answer.json()["id"], "client-work");
    assert_eq!(answer.json()["isDefault"], false);
    assert_eq!(answer.json()["sessionCount"], 0);
    assert!(folder(&test, "client-work").is_dir());
}

#[tokio::test]
async fn an_id_that_could_be_a_path_is_refused() {
    let test = server();
    // The first request that sent `/` would otherwise hand an authenticated
    // caller the whole filesystem, vault included.
    for id in ["../escape", "a/b", "/abs"] {
        let answer = post(&test, "/api/workspaces", json!({"name": "X", "id": id})).await;
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY, "{id}");
    }
}

#[tokio::test]
async fn a_reserved_id_is_refused() {
    let test = server();
    let answer = post(
        &test,
        "/api/workspaces",
        json!({"name": "Default", "id": "default"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_duplicate_is_refused() {
    let test = server();
    make_workspace(&test, "Research");
    let answer = post(
        &test,
        "/api/workspaces",
        json!({"name": "Research", "id": "research"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_blank_name_is_refused() {
    let test = server();
    let answer = post(&test, "/api/workspaces", json!({"name": "   "})).await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn creating_adopts_an_existing_folder() {
    let test = server();
    // This is what makes delete-then-recreate work: a detached directory is
    // re-adopted by creating a workspace with the same name.
    std::fs::create_dir_all(folder(&test, "research")).unwrap();
    std::fs::write(folder(&test, "research").join("notes.md"), "kept").unwrap();

    let answer = post(
        &test,
        "/api/workspaces",
        json!({"name": "Research", "id": "research"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CREATED);
    assert_eq!(
        std::fs::read_to_string(folder(&test, "research").join("notes.md")).unwrap(),
        "kept"
    );
}

#[tokio::test]
async fn a_slug_that_collides_with_a_file_is_refused() {
    let test = server();
    std::fs::create_dir_all(test.home.path().join("workspace")).unwrap();
    std::fs::write(folder(&test, "research"), "not a folder").unwrap();

    let answer = post(
        &test,
        "/api/workspaces",
        json!({"name": "Research", "id": "research"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
}

// Renaming and moving

#[tokio::test]
async fn renaming_moves_nothing_on_disk() {
    let test = server();
    let id = make_workspace(&test, "Research");
    std::fs::write(folder(&test, &id).join("notes.md"), "kept").unwrap();

    let answer = patch(
        &test,
        &format!("/api/workspaces/{id}"),
        json!({"name": "Deep Research"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["name"], "Deep Research");
    assert_eq!(answer.json()["id"], id);
    assert!(folder(&test, &id).join("notes.md").is_file());
}

#[tokio::test]
async fn moving_the_folder_takes_the_sessions_and_the_cached_jail_with_it() {
    let test = server();
    let id = make_workspace(&test, "Research");
    std::fs::write(folder(&test, &id).join("notes.md"), "kept").unwrap();
    bind_session(&test, "s-1", &id);

    let answer = patch(
        &test,
        &format!("/api/workspaces/{id}"),
        json!({"id": "archive"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["id"], "archive");

    assert!(folder(&test, "archive").join("notes.md").is_file());
    assert!(!folder(&test, &id).exists());
    assert_eq!(
        test.runtime
            .store()
            .get_session("s-1")
            .unwrap()
            .unwrap()
            .workspace_id,
        "archive"
    );
    // A jail canonicalises its root once, so an entry keyed on a directory that
    // has just been renamed away holds a path that is no longer there — and
    // would be handed to the *next* workspace created on that freed name.
    assert_eq!(test.runtime.released(), [id]);
}

#[tokio::test]
async fn a_name_and_a_folder_arrive_in_one_request() {
    let test = server();
    let id = make_workspace(&test, "Research");

    let answer = patch(
        &test,
        &format!("/api/workspaces/{id}"),
        json!({"name": "Archive", "id": "archive"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["name"], "Archive");
    assert_eq!(answer.json()["id"], "archive");
}

#[tokio::test]
async fn the_default_cannot_be_moved() {
    let test = server();
    // Its directory *is* the workspace root and the parent of every other
    // workspace; there is no rename of it that does not mean relocating the
    // entire tree.
    let answer = patch(&test, "/api/workspaces/default", json!({"id": "elsewhere"})).await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_folder_that_could_be_a_path_or_that_is_taken_is_refused() {
    let test = server();
    let id = make_workspace(&test, "Research");
    make_workspace(&test, "Archive");

    let traversal = patch(
        &test,
        &format!("/api/workspaces/{id}"),
        json!({"id": "../escape"}),
    )
    .await;
    assert_eq!(traversal.status, StatusCode::UNPROCESSABLE_ENTITY);

    let taken = patch(
        &test,
        &format!("/api/workspaces/{id}"),
        json!({"id": "archive"}),
    )
    .await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_folder_equal_to_the_current_one_is_a_no_op_rather_than_a_move() {
    let test = server();
    let id = make_workspace(&test, "Research");

    let answer = patch(
        &test,
        &format!("/api/workspaces/{id}"),
        json!({"id": id.clone()}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["id"], id);
    assert!(
        test.runtime.released().is_empty(),
        "nothing moved, so nothing had to be forgotten"
    );
}

#[tokio::test]
async fn patching_a_workspace_that_is_not_there_is_a_not_found() {
    let test = server();
    let answer = patch(&test, "/api/workspaces/ghost", json!({"name": "X"})).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_empty_patch_answers_with_the_row_unchanged() {
    let test = server();
    let id = make_workspace(&test, "Research");
    let answer = patch(&test, &format!("/api/workspaces/{id}"), json!({})).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["name"], "Research");
}

// Deleting

#[tokio::test]
async fn deleting_detaches_and_keeps_the_files() {
    let test = server();
    let id = make_workspace(&test, "Research");
    std::fs::write(folder(&test, &id).join("notes.md"), "kept").unwrap();

    let answer = delete(&test, &format!("/api/workspaces/{id}")).await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    assert!(test.runtime.workspaces().get(&id).unwrap().is_none());
    // A delete in a web UI is one click away from a misclick and there is no
    // undo for a recursive remove of a tree someone has been working in.
    assert!(folder(&test, &id).join("notes.md").is_file());
}

#[tokio::test]
async fn deleting_is_refused_while_sessions_still_point_at_it() {
    let test = server();
    let id = make_workspace(&test, "Research");
    bind_session(&test, "s-1", &id);
    bind_session(&test, "s-2", &id);

    let answer = delete(&test, &format!("/api/workspaces/{id}")).await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
    // The count belongs in the error rather than only in the message: it is
    // what the UI renders in its "move them to Default first" affordance.
    let details = &answer.json()["error"]["details"];
    assert_eq!(details["sessionCount"], 2);
    assert_eq!(details["workspaceId"], id);
    assert!(test.runtime.workspaces().get(&id).unwrap().is_some());
}

#[tokio::test]
async fn the_refusal_says_session_in_the_singular_for_one() {
    let test = server();
    let id = make_workspace(&test, "Research");
    bind_session(&test, "s-1", &id);

    let answer = delete(&test, &format!("/api/workspaces/{id}")).await;
    let message = answer.json()["error"]["message"].as_str().unwrap();
    assert!(message.contains("1 session still"), "{message}");
}

#[tokio::test]
async fn the_default_cannot_be_deleted() {
    let test = server();
    let answer = delete(&test, "/api/workspaces/default").await;
    assert_ne!(answer.status, StatusCode::NO_CONTENT);
    assert!(test.runtime.workspaces().get("default").unwrap().is_some());
}

#[tokio::test]
async fn deleting_a_workspace_that_is_not_there_is_a_not_found() {
    let test = server();
    assert_eq!(
        delete(&test, "/api/workspaces/ghost").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn deleting_goes_through_once_the_sessions_have_been_moved() {
    let test = server();
    let id = make_workspace(&test, "Research");
    bind_session(&test, "s-1", &id);

    let moved = post(
        &test,
        &format!("/api/workspaces/{id}/sessions/move"),
        json!({"to": "default"}),
    )
    .await;
    assert_eq!(moved.status, StatusCode::OK);
    assert_eq!(moved.json()["moved"], 1);

    assert_eq!(
        delete(&test, &format!("/api/workspaces/{id}")).await.status,
        StatusCode::NO_CONTENT
    );
}

// Moving sessions

#[tokio::test]
async fn moving_into_a_destination_that_does_not_exist_is_a_not_found() {
    let test = server();
    let id = make_workspace(&test, "Research");
    bind_session(&test, "s-1", &id);

    // Moving into a workspace nobody can name would strand the conversations
    // somewhere the UI cannot show them, which is worse than the delete this
    // was meant to unblock.
    let answer = post(
        &test,
        &format!("/api/workspaces/{id}/sessions/move"),
        json!({"to": "ghost"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(
        test.runtime
            .store()
            .get_session("s-1")
            .unwrap()
            .unwrap()
            .workspace_id,
        id
    );
}

#[tokio::test]
async fn moving_from_a_source_that_does_not_exist_is_a_not_found() {
    let test = server();
    let answer = post(
        &test,
        "/api/workspaces/ghost/sessions/move",
        json!({"to": "default"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_move_into_itself_is_refused() {
    let test = server();
    let id = make_workspace(&test, "Research");
    let answer = post(
        &test,
        &format!("/api/workspaces/{id}/sessions/move"),
        json!({"to": id.clone()}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(answer.json()["error"]["details"]["workspaceId"], id);
}

#[tokio::test]
async fn moving_an_empty_workspace_reports_nothing_moved() {
    let test = server();
    let id = make_workspace(&test, "Research");
    let answer = post(
        &test,
        &format!("/api/workspaces/{id}/sessions/move"),
        json!({"to": "default"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["moved"], 0);
}

// Sessions and workspaces together

#[tokio::test]
async fn a_session_cannot_be_opened_in_a_workspace_that_does_not_exist() {
    let test = server();
    let answer = post(
        &test,
        "/api/sessions",
        json!({"key": "s-1", "workspaceId": "ghost"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert!(test.runtime.store().get_session("s-1").unwrap().is_none());
}

#[tokio::test]
async fn a_session_records_the_workspace_and_reports_it_back() {
    let test = server();
    let id = make_workspace(&test, "Research");
    let answer = post(
        &test,
        "/api/sessions",
        json!({"key": "s-1", "workspaceId": id.clone()}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CREATED);
    assert_eq!(answer.json()["workspaceId"], id);
}
