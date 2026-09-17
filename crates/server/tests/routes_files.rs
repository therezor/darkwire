//! The workspace over HTTP.
//!
//! Every path in these routes arrives as a client-supplied string, which makes
//! this the largest attack surface the server has. Three rules carry the file
//! tests, and each of them is here because the alternative is a real bypass:
//!
//!  - A path that escapes is **clamped** by the jail's lexical normalisation,
//!    and one that escapes *through a symlink* is refused with a 403 — never a
//!    404, because the difference between the two answers is a way to map the
//!    filesystem by probing.
//!  - A recursive delete happens only when the caller said the word that means
//!    exactly that; a mistyped path cannot empty a tree.
//!  - A signed media URL is a bearer credential with one job, and the response
//!    refuses to render anything a browser would execute in this origin. The
//!    workspace is a tree a language model writes to, so "the agent produced an
//!    HTML file and the user opened it" is a path that needs closing rather
//!    than a hypothetical.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture or a response that cannot be read is a failing test either way"
)]

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use darkwire_core::Clock as _;
use darkwire_core::workspace_store::CreateWorkspace;
use darkwire_server::runtime::ServerRuntime as _;
use darkwire_server::signing::{MEDIA_SECRET_NAME, MediaClaim, sign_media_token};
use darkwire_server::testkit::{TestServer, TestServerOptions, start_test_server};
use serde_json::{Value, json};
use tower::ServiceExt as _;

// Harness

fn server() -> TestServer {
    start_test_server(TestServerOptions::default()).expect("a test server")
}

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    bytes: Vec<u8>,
    body: Value,
}

impl Answer {
    fn json(&self) -> &Value {
        &self.body
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    fn header(&self, name: header::HeaderName) -> String {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }
}

async fn raw(test: &TestServer, method: &str, uri: &str, body: Body, authenticate: bool) -> Answer {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if authenticate {
        builder = builder.header("authorization", format!("Bearer {}", test.token));
    }
    let request = builder.body(body).expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
        .await
        .expect("a body")
        .to_vec();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answer {
        status,
        headers,
        bytes,
        body,
    }
}

async fn send(test: &TestServer, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let body = match &body {
        Some(value) => Body::from(value.to_string()),
        None => Body::empty(),
    };
    raw(test, method, uri, body, true).await
}

async fn get(test: &TestServer, uri: &str) -> Answer {
    send(test, "GET", uri, None).await
}

async fn post(test: &TestServer, uri: &str, body: Value) -> Answer {
    send(test, "POST", uri, Some(body)).await
}

async fn put(test: &TestServer, uri: &str, body: Value) -> Answer {
    send(test, "PUT", uri, Some(body)).await
}

async fn delete(test: &TestServer, uri: &str) -> Answer {
    send(test, "DELETE", uri, None).await
}

/// The directory one workspace slug sits in. Every id is a folder, `default`
/// included, and they are siblings.
fn workspace_dir(test: &TestServer, id: &str) -> std::path::PathBuf {
    test.home.path().join("DarkWire/workspaces").join(id)
}

fn write_file(test: &TestServer, workspace: &str, relative: &str, content: &[u8]) {
    let path = workspace_dir(test, workspace).join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the parent directory");
    }
    std::fs::write(path, content).expect("the file was written");
}

fn make_workspace(test: &TestServer, name: &str) -> String {
    test.runtime
        .workspaces()
        .create(CreateWorkspace {
            name: name.to_owned(),
            ..CreateWorkspace::default()
        })
        .expect("the workspace was created")
        .id
}

/// The signing secret this server actually uses.
///
/// Read off the shared connection rather than handed over by the harness: the
/// secret is minted lazily by the first route that signs anything, so a
/// throwaway sign is what brings it into existence, and the row is then the
/// only place it lives.
async fn secret(test: &TestServer) -> String {
    write_file(test, "default", ".signing-probe", b"x");
    let minted = post(
        test,
        "/api/files/signed-url",
        json!({"path": ".signing-probe"}),
    )
    .await;
    assert_eq!(minted.status, StatusCode::OK, "the probe was signed");

    let guard = test.database.lock();
    guard
        .query_row(
            "SELECT value FROM auth_secrets WHERE name = ?",
            [MEDIA_SECRET_NAME],
            |row| row.get::<_, String>(0),
        )
        .expect("the signing secret was stored")
}

/// A token for one path in one workspace, good for a minute.
async fn token_for(test: &TestServer, path: &str, workspace_id: &str) -> String {
    sign_media_token(
        &secret(test).await,
        &MediaClaim {
            path: path.to_owned(),
            workspace_id: workspace_id.to_owned(),
            expires_at_ms: test.clock.now_ms() + 60_000,
        },
    )
}

// GET /api/files

#[tokio::test]
async fn the_listing_answers_the_workspace_root_by_default_directories_first() {
    let test = server();
    write_file(&test, "default", "b.txt", b"b");
    write_file(&test, "default", "a.txt", b"a");
    std::fs::create_dir_all(workspace_dir(&test, "default").join("z-dir")).unwrap();
    std::fs::create_dir_all(workspace_dir(&test, "default").join("a-dir")).unwrap();

    let answer = get(&test, "/api/files").await;
    assert_eq!(answer.status, StatusCode::OK);
    let names: Vec<&str> = answer.json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a-dir", "z-dir", "a.txt", "b.txt"]);
    // The relative path the jail agreed to, echoed back rather than the input.
    assert_eq!(answer.json()["path"], "");
}

#[tokio::test]
async fn the_listing_answers_a_subdirectory() {
    let test = server();
    write_file(&test, "default", "notes/todo.md", b"todo");

    let answer = get(&test, "/api/files?path=notes").await;
    assert_eq!(answer.status, StatusCode::OK);
    let entries = answer.json()["entries"].as_array().unwrap().clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "todo.md");
    // Workspace-relative, always: an absolute path tells a client where the
    // server keeps its files.
    assert_eq!(entries[0]["path"], "notes/todo.md");
    assert_eq!(entries[0]["isDirectory"], false);
    assert_eq!(entries[0]["sizeBytes"], 4);
}

#[tokio::test]
async fn a_dot_path_and_a_bare_path_are_the_same_directory() {
    let test = server();
    write_file(&test, "default", "notes/todo.md", b"todo");
    let dotted = get(&test, "/api/files?path=./notes/").await;
    let bare = get(&test, "/api/files?path=notes").await;
    assert_eq!(dotted.json()["path"], bare.json()["path"]);
}

#[tokio::test]
#[cfg(unix)]
async fn a_symlink_that_leads_out_of_the_workspace_never_appears() {
    let test = server();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "not yours").unwrap();
    write_file(&test, "default", "inside.txt", b"yours");
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        workspace_dir(&test, "default").join("escape.txt"),
    )
    .unwrap();

    let answer = get(&test, "/api/files").await;
    let names: Vec<&str> = answer.json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    // Listing it would advertise a file the jail then refuses to open, which
    // reads as a bug in the UI rather than as the refusal it is.
    assert_eq!(names, ["inside.txt"]);
}

#[tokio::test]
#[cfg(unix)]
async fn asking_for_a_symlink_that_points_out_of_the_workspace_is_a_refusal() {
    let test = server();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(outside.path().join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("elsewhere"),
        workspace_dir(&test, "default").join("escape"),
    )
    .unwrap();

    let answer = get(&test, "/api/files?path=escape").await;
    // 403, never 404: saying "not found" would let a caller map the filesystem
    // by probing for the difference between the two answers.
    assert_eq!(answer.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn asking_for_a_file_where_a_directory_belongs_is_a_bad_request() {
    let test = server();
    write_file(&test, "default", "notes.md", b"notes");
    let answer = get(&test, "/api/files?path=notes.md").await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn asking_for_a_directory_that_is_not_there_is_a_not_found() {
    let test = server();
    let answer = get(&test, "/api/files?path=nowhere").await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

// POST /api/files/upload

#[tokio::test]
async fn an_upload_writes_the_body_and_hands_back_a_url_for_it() {
    let test = server();
    let answer = raw(
        &test,
        "POST",
        "/api/files/upload?path=uploads/photo.png",
        Body::from(vec![1u8, 2, 3, 4]),
        true,
    )
    .await;

    assert_eq!(answer.status, StatusCode::CREATED);
    assert_eq!(answer.json()["path"], "uploads/photo.png");
    assert_eq!(answer.json()["sizeBytes"], 4);
    assert_eq!(answer.json()["mimeType"], "image/png");
    // Returned with the upload so a UI can render what it just sent without a
    // second round trip to ask permission to look at it.
    assert!(
        answer.json()["signedUrl"]["url"]
            .as_str()
            .unwrap()
            .starts_with("/api/media/")
    );
    assert_eq!(
        std::fs::read(workspace_dir(&test, "default").join("uploads/photo.png")).unwrap(),
        [1, 2, 3, 4]
    );
}

#[tokio::test]
async fn an_empty_upload_is_refused() {
    let test = server();
    let answer = raw(
        &test,
        "POST",
        "/api/files/upload?path=empty.bin",
        Body::empty(),
        true,
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_upload_that_tried_to_escape_is_clamped_and_says_where_it_landed() {
    let test = server();
    let answer = raw(
        &test,
        "POST",
        "/api/files/upload?path=../../escape.txt",
        Body::from("x"),
        true,
    )
    .await;

    assert_eq!(answer.status, StatusCode::CREATED);
    // The response carries the path it actually wrote, so a client is never
    // told it reached somewhere it did not.
    assert_eq!(answer.json()["path"], "escape.txt");
    assert!(workspace_dir(&test, "default").join("escape.txt").is_file());
    assert!(!test.home.path().join("escape.txt").exists());
}

#[tokio::test]
async fn an_upload_past_the_cap_is_refused_rather_than_buffered() {
    let test = server();
    let oversized = vec![0u8; darkwire_server::routes::files::MAX_UPLOAD_BYTES + 1];
    let answer = raw(
        &test,
        "POST",
        "/api/files/upload?path=big.bin",
        Body::from(oversized),
        true,
    )
    .await;
    assert_eq!(answer.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!workspace_dir(&test, "default").join("big.bin").exists());
}

#[tokio::test]
async fn an_upload_with_no_path_is_refused() {
    let test = server();
    let answer = raw(&test, "POST", "/api/files/upload", Body::from("x"), true).await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

// DELETE /api/files

#[tokio::test]
async fn deleting_removes_a_file() {
    let test = server();
    write_file(&test, "default", "notes.md", b"notes");
    let answer = delete(&test, "/api/files?path=notes.md").await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    assert!(!workspace_dir(&test, "default").join("notes.md").exists());
}

#[tokio::test]
async fn deleting_removes_an_empty_directory_without_ceremony() {
    let test = server();
    std::fs::create_dir_all(workspace_dir(&test, "default").join("empty")).unwrap();
    let answer = delete(&test, "/api/files?path=empty").await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    assert!(!workspace_dir(&test, "default").join("empty").exists());
}

#[tokio::test]
async fn deleting_a_directory_with_contents_is_refused_unless_the_caller_said_so() {
    let test = server();
    write_file(&test, "default", "notes/todo.md", b"todo");

    let answer = delete(&test, "/api/files?path=notes").await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(answer.json()["error"]["details"]["entryCount"], 1);
    // A mistyped path, a stale bookmark or a script looping over names cannot
    // recurse.
    assert!(
        workspace_dir(&test, "default")
            .join("notes/todo.md")
            .is_file()
    );
}

#[tokio::test]
async fn deleting_takes_the_contents_when_the_caller_does_say_so() {
    let test = server();
    write_file(&test, "default", "notes/todo.md", b"todo");
    let answer = delete(&test, "/api/files?path=notes&recursive=true").await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    assert!(!workspace_dir(&test, "default").join("notes").exists());
}

#[tokio::test]
async fn a_recursive_delete_is_clamped_to_the_workspace() {
    let test = server();
    write_file(&test, "default", "keep.txt", b"keep");
    std::fs::write(test.home.path().join("above.txt"), "above").unwrap();

    let answer = delete(&test, "/api/files?path=../above.txt&recursive=true").await;
    // The path clamps into the workspace, where nothing by that name exists.
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert!(test.home.path().join("above.txt").is_file());
}

#[tokio::test]
async fn deleting_something_that_is_not_there_is_a_not_found() {
    let test = server();
    assert_eq!(
        delete(&test, "/api/files?path=nowhere.txt").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn deleting_with_no_path_is_refused() {
    let test = server();
    assert_eq!(
        delete(&test, "/api/files").await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

// GET /api/files/text

#[tokio::test]
async fn reading_answers_with_the_file_and_the_timestamp_a_save_has_to_match() {
    let test = server();
    write_file(&test, "default", "notes.md", b"# Notes\n");

    let answer = get(&test, "/api/files/text?path=notes.md").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["path"], "notes.md");
    assert_eq!(answer.json()["content"], "# Notes\n");
    assert_eq!(answer.json()["sizeBytes"], 8);
    assert_eq!(answer.json()["truncated"], false);
    assert!(answer.json()["modifiedAtMs"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn reading_opens_a_source_file_the_media_table_has_never_heard_of() {
    let test = server();
    // Deciding from the bytes rather than the extension is what makes `.py`
    // and `.ts` — the files a person most wants to open — readable at all.
    write_file(&test, "default", "main.py", b"print('hi')\n");
    let answer = get(&test, "/api/files/text?path=main.py").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["content"], "print('hi')\n");
}

#[tokio::test]
async fn reading_refuses_a_binary_file_rather_than_answering_with_mojibake() {
    let test = server();
    write_file(&test, "default", "blob.txt", &[0x00, 0x01, 0x02]);
    let answer = get(&test, "/api/files/text?path=blob.txt").await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn reading_truncates_a_file_past_the_limit_and_says_that_it_did() {
    let test = server();
    let big = vec![b'a'; 600 * 1024];
    write_file(&test, "default", "big.txt", &big);

    let answer = get(&test, "/api/files/text?path=big.txt").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["truncated"], true);
    // The editor goes read-only on this, because a saved prefix would delete
    // the rest.
    assert_eq!(answer.json()["content"].as_str().unwrap().len(), 512 * 1024);
    assert_eq!(answer.json()["sizeBytes"], 600 * 1024);
}

#[tokio::test]
async fn reading_a_directory_is_a_bad_request() {
    let test = server();
    std::fs::create_dir_all(workspace_dir(&test, "default").join("notes")).unwrap();
    assert_eq!(
        get(&test, "/api/files/text?path=notes").await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn reading_a_path_that_tried_to_leave_is_clamped() {
    let test = server();
    std::fs::write(test.home.path().join("above.txt"), "above").unwrap();
    let answer = get(&test, "/api/files/text?path=../above.txt").await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

// PUT /api/files/text

#[tokio::test]
async fn writing_stores_the_content_and_answers_with_the_entry_it_produced() {
    let test = server();
    write_file(&test, "default", "notes.md", b"old");

    let answer = put(
        &test,
        "/api/files/text",
        json!({"path": "notes.md", "content": "new content"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["path"], "notes.md");
    assert_eq!(answer.json()["sizeBytes"], 11);
    assert_eq!(
        std::fs::read_to_string(workspace_dir(&test, "default").join("notes.md")).unwrap(),
        "new content"
    );
}

#[tokio::test]
async fn writing_creates_the_file_and_the_directory_over_it() {
    let test = server();
    let answer = put(
        &test,
        "/api/files/text",
        json!({"path": "deep/nested/notes.md", "content": "x"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        workspace_dir(&test, "default")
            .join("deep/nested/notes.md")
            .is_file()
    );
}

#[tokio::test]
async fn a_save_whose_file_moved_since_it_was_read_is_refused() {
    let test = server();
    write_file(&test, "default", "notes.md", b"old");
    let read = get(&test, "/api/files/text?path=notes.md").await;
    let stamp = read.json()["modifiedAtMs"].as_u64().unwrap();

    // The workspace is a tree a language model writes to while somebody is
    // looking at it: an editor that sat open through a turn and then saved
    // would silently delete whatever that turn wrote.
    let answer = put(
        &test,
        "/api/files/text",
        json!({
            "path": "notes.md",
            "content": "mine",
            "expectedModifiedAtMs": stamp - 1,
        }),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert!(answer.json()["error"]["details"]["modifiedAtMs"].is_number());
    assert_eq!(
        std::fs::read_to_string(workspace_dir(&test, "default").join("notes.md")).unwrap(),
        "old"
    );
}

#[tokio::test]
async fn a_save_whose_file_was_deleted_since_it_was_read_is_refused() {
    let test = server();
    let answer = put(
        &test,
        "/api/files/text",
        json!({
            "path": "gone.md",
            "content": "mine",
            "expectedModifiedAtMs": 1_700_000_000_000u64,
        }),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_save_that_names_no_timestamp_is_creating_a_file() {
    let test = server();
    // A caller that says nothing has nothing to conflict with.
    let answer = put(
        &test,
        "/api/files/text",
        json!({"path": "fresh.md", "content": "x"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
}

#[tokio::test]
async fn a_save_that_matches_the_timestamp_goes_through() {
    let test = server();
    write_file(&test, "default", "notes.md", b"old");
    let read = get(&test, "/api/files/text?path=notes.md").await;
    let stamp = read.json()["modifiedAtMs"].as_u64().unwrap();

    let answer = put(
        &test,
        "/api/files/text",
        json!({
            "path": "notes.md",
            "content": "mine",
            "expectedModifiedAtMs": stamp,
        }),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
}

#[tokio::test]
async fn writing_over_a_directory_is_a_bad_request() {
    let test = server();
    std::fs::create_dir_all(workspace_dir(&test, "default").join("notes")).unwrap();
    let answer = put(
        &test,
        "/api/files/text",
        json!({"path": "notes", "content": "x"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_write_whose_path_tried_to_leave_is_clamped_into_the_workspace() {
    let test = server();
    let answer = put(
        &test,
        "/api/files/text",
        json!({"path": "../../escaped.md", "content": "x"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["path"], "escaped.md");
    assert!(!test.home.path().join("escaped.md").exists());
}

#[tokio::test]
async fn a_write_body_that_is_not_json_is_a_bad_request() {
    let test = server();
    let answer = raw(
        &test,
        "PUT",
        "/api/files/text",
        Body::from("{not json"),
        true,
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_write_body_past_the_cap_is_refused() {
    let test = server();
    let body = json!({
        "path": "big.md",
        "content": "a".repeat(darkwire_server::routes::files::MAX_TEXT_BODY_BYTES),
    });
    let answer = raw(
        &test,
        "PUT",
        "/api/files/text",
        Body::from(body.to_string()),
        true,
    )
    .await;
    assert_eq!(answer.status, StatusCode::PAYLOAD_TOO_LARGE);
}

// POST /api/files/directory

#[tokio::test]
async fn mkdir_creates_a_directory_and_answers_with_its_entry() {
    let test = server();
    let answer = post(&test, "/api/files/directory", json!({"path": "notes"})).await;
    assert_eq!(answer.status, StatusCode::CREATED);
    assert_eq!(answer.json()["path"], "notes");
    assert_eq!(answer.json()["isDirectory"], true);
    assert_eq!(answer.json()["sizeBytes"], 0);
    assert!(workspace_dir(&test, "default").join("notes").is_dir());
}

#[tokio::test]
async fn mkdir_refuses_a_path_something_is_already_at() {
    let test = server();
    std::fs::create_dir_all(workspace_dir(&test, "default").join("notes")).unwrap();
    // "New folder" that quietly returns an existing one is how two things end
    // up sharing a directory nobody meant to share.
    let answer = post(&test, "/api/files/directory", json!({"path": "notes"})).await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn mkdir_clamps_a_directory_that_tried_to_be_created_outside() {
    let test = server();
    let answer = post(
        &test,
        "/api/files/directory",
        json!({"path": "../../outside"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CREATED);
    assert_eq!(answer.json()["path"], "outside");
    assert!(!test.home.path().join("outside").exists());
}

// POST /api/files/move

#[tokio::test]
async fn moving_renames_a_file_and_answers_with_where_it_landed() {
    let test = server();
    write_file(&test, "default", "old.md", b"content");

    let answer = post(
        &test,
        "/api/files/move",
        json!({"from": "old.md", "to": "new.md"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["path"], "new.md");
    assert!(!workspace_dir(&test, "default").join("old.md").exists());
    assert_eq!(
        std::fs::read_to_string(workspace_dir(&test, "default").join("new.md")).unwrap(),
        "content"
    );
}

#[tokio::test]
async fn moving_a_directory_takes_everything_inside_it() {
    let test = server();
    write_file(&test, "default", "notes/todo.md", b"todo");

    let answer = post(
        &test,
        "/api/files/move",
        json!({"from": "notes", "to": "archive"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["isDirectory"], true);
    assert!(
        workspace_dir(&test, "default")
            .join("archive/todo.md")
            .is_file()
    );
}

#[tokio::test]
async fn moving_into_another_directory_works_because_a_rename_is_a_move() {
    let test = server();
    write_file(&test, "default", "notes.md", b"x");
    std::fs::create_dir_all(workspace_dir(&test, "default").join("archive")).unwrap();

    let answer = post(
        &test,
        "/api/files/move",
        json!({"from": "notes.md", "to": "archive/notes.md"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["path"], "archive/notes.md");
}

#[tokio::test]
async fn moving_refuses_to_overwrite_whatever_is_already_at_the_target() {
    let test = server();
    write_file(&test, "default", "a.md", b"a");
    write_file(&test, "default", "b.md", b"b");

    // A rename will happily replace a file, and one that destroys whatever was
    // already there is a data loss the operator did not ask for.
    let answer = post(
        &test,
        "/api/files/move",
        json!({"from": "a.md", "to": "b.md"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(
        std::fs::read_to_string(workspace_dir(&test, "default").join("b.md")).unwrap(),
        "b"
    );
}

#[tokio::test]
async fn moving_a_source_that_is_not_there_is_a_not_found() {
    let test = server();
    let answer = post(
        &test,
        "/api/files/move",
        json!({"from": "nowhere.md", "to": "somewhere.md"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn moving_into_a_folder_that_is_not_there_names_the_folder() {
    let test = server();
    write_file(&test, "default", "notes.md", b"x");
    // The operating system reports this as a bare "no such file", which reads
    // as "the file is missing" rather than "the folder is".
    let answer = post(
        &test,
        "/api/files/move",
        json!({"from": "notes.md", "to": "missing/notes.md"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert!(
        answer.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("missing"),
        "{}",
        answer.text()
    );
}

#[tokio::test]
async fn moving_a_directory_inside_itself_is_refused() {
    let test = server();
    std::fs::create_dir_all(workspace_dir(&test, "default").join("notes")).unwrap();
    let answer = post(
        &test,
        "/api/files/move",
        json!({"from": "notes", "to": "notes/inner"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn moving_onto_itself_is_the_no_op_it_is() {
    let test = server();
    write_file(&test, "default", "notes.md", b"x");
    let answer = post(
        &test,
        "/api/files/move",
        json!({"from": "notes.md", "to": "./notes.md"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["path"], "notes.md");
    assert_eq!(
        std::fs::read_to_string(workspace_dir(&test, "default").join("notes.md")).unwrap(),
        "x"
    );
}

#[tokio::test]
async fn moving_clamps_both_ends_into_the_workspace() {
    let test = server();
    write_file(&test, "default", "notes.md", b"x");
    let answer = post(
        &test,
        "/api/files/move",
        json!({"from": "../../notes.md", "to": "../../moved.md"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["path"], "moved.md");
    assert!(!test.home.path().join("moved.md").exists());
}

// Signed media

#[tokio::test]
async fn a_signature_serves_the_file_and_nothing_else_does() {
    let test = server();
    write_file(&test, "default", "photo.png", &[0x89, b'P', b'N', b'G']);

    let minted = post(&test, "/api/files/signed-url", json!({"path": "photo.png"})).await;
    assert_eq!(minted.status, StatusCode::OK);
    let url = minted.json()["url"].as_str().unwrap().to_owned();

    // The signature is the credential, and it needs no session behind it.
    let served = raw(&test, "GET", &url, Body::empty(), false).await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(served.bytes, [0x89, b'P', b'N', b'G']);
    assert_eq!(served.header(header::CONTENT_TYPE), "image/png");
    // Without this a browser may sniff a text file into HTML and run it.
    assert_eq!(served.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert_eq!(served.header(header::CONTENT_DISPOSITION), "inline");
    assert_eq!(served.header(header::CONTENT_LENGTH), "4");
    // The URL is a bearer credential, so a shared cache must not hold it.
    assert!(served.header(header::CACHE_CONTROL).contains("private"));
}

#[tokio::test]
async fn a_token_whose_payload_was_edited_is_refused() {
    let test = server();
    write_file(&test, "default", "photo.png", b"png");
    write_file(&test, "default", "secret.txt", b"secret");

    let token = token_for(&test, "photo.png", "default").await;
    let (payload, signature) = token.rsplit_once('.').expect("a two-part token");
    let forged_payload = {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            json!({"p": "secret.txt", "w": "default", "e": test.clock.now_ms() + 60_000})
                .to_string(),
        )
    };
    let _ = payload;

    let answer = raw(
        &test,
        "GET",
        &format!("/api/media/{forged_payload}.{signature}"),
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_token_that_has_expired_is_refused() {
    let test = server();
    write_file(&test, "default", "photo.png", b"png");
    let token = token_for(&test, "photo.png", "default").await;

    // The alternative to moving the clock is sleeping through the TTL.
    test.clock.advance(Duration::from_mins(2));
    let answer = raw(
        &test,
        "GET",
        &format!("/api/media/{token}"),
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_signature_made_with_another_key_is_refused() {
    let test = server();
    write_file(&test, "default", "photo.png", b"png");
    let forged = sign_media_token(
        "a key this server has never held",
        &MediaClaim {
            path: "photo.png".to_owned(),
            workspace_id: "default".to_owned(),
            expires_at_ms: test.clock.now_ms() + 60_000,
        },
    );

    let answer = raw(
        &test,
        "GET",
        &format!("/api/media/{forged}"),
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_garbled_token_is_refused() {
    let test = server();
    for token in ["nope", "a.b", "....", "onlyonepart"] {
        let answer = raw(
            &test,
            "GET",
            &format!("/api/media/{token}"),
            Body::empty(),
            false,
        )
        .await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{token}");
    }
}

#[tokio::test]
async fn an_executable_type_is_never_served_inline() {
    let test = server();
    // An SVG can carry `<script>`, and served inline from this origin that
    // script runs with the session cookie attached.
    for name in ["page.html", "app.js", "diagram.svg"] {
        write_file(&test, "default", name, b"<script>alert(1)</script>");
        let token = token_for(&test, name, "default").await;
        let answer = raw(
            &test,
            "GET",
            &format!("/api/media/{token}"),
            Body::empty(),
            false,
        )
        .await;
        assert_eq!(answer.status, StatusCode::OK, "{name}");
        assert_eq!(
            answer.header(header::CONTENT_DISPOSITION),
            "attachment",
            "{name}"
        );
        assert_eq!(
            answer.header(header::CONTENT_TYPE),
            darkwire_server::workspace::DEFAULT_MIME_TYPE,
            "{name}"
        );
    }
}

#[tokio::test]
async fn signing_a_file_that_does_not_exist_is_refused() {
    let test = server();
    // A URL that 404s later is a worse answer than a 404 now, and the client is
    // holding the path.
    let answer = post(
        &test,
        "/api/files/signed-url",
        json!({"path": "nowhere.png"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_file_deleted_after_the_url_was_minted_is_a_not_found() {
    let test = server();
    write_file(&test, "default", "photo.png", b"png");
    let token = token_for(&test, "photo.png", "default").await;
    std::fs::remove_file(workspace_dir(&test, "default").join("photo.png")).unwrap();

    let answer = raw(
        &test,
        "GET",
        &format!("/api/media/{token}"),
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_signed_directory_is_a_not_found_rather_than_a_stream() {
    let test = server();
    std::fs::create_dir_all(workspace_dir(&test, "default").join("notes")).unwrap();
    let token = token_for(&test, "notes", "default").await;
    let answer = raw(
        &test,
        "GET",
        &format!("/api/media/{token}"),
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[cfg(unix)]
async fn a_signed_path_that_became_a_symlink_out_of_the_workspace_is_refused() {
    let test = server();
    write_file(&test, "default", "photo.png", b"png");
    let token = token_for(&test, "photo.png", "default").await;

    // A signature says who asked, not what the filesystem looks like now.
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("passwd"), "root:x:0:0").unwrap();
    let target = workspace_dir(&test, "default").join("photo.png");
    std::fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink(outside.path().join("passwd"), &target).unwrap();

    let answer = raw(
        &test,
        "GET",
        &format!("/api/media/{token}"),
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert!(!answer.text().contains("root:x"));
}

// Workspace scoping

#[tokio::test]
async fn the_listing_answers_only_the_workspace_that_was_asked_for() {
    let test = server();
    let id = make_workspace(&test, "Research");
    write_file(&test, "default", "root-only.txt", b"x");
    write_file(&test, &id, "research-only.txt", b"x");

    let answer = get(&test, &format!("/api/files?workspace={id}")).await;
    let names: Vec<&str> = answer.json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["research-only.txt"]);
}

#[tokio::test]
async fn a_traversal_cannot_reach_a_sibling_workspace() {
    let test = server();
    let one = make_workspace(&test, "One");
    let two = make_workspace(&test, "Two");
    write_file(&test, &two, "secret.txt", b"not yours");

    let answer = get(
        &test,
        &format!("/api/files/text?workspace={one}&path=../{two}/secret.txt"),
    )
    .await;
    // The traversal clamps inside `one`, where nothing by that name exists.
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_write_lands_in_the_workspace_the_request_named() {
    let test = server();
    let id = make_workspace(&test, "Research");

    let answer = put(
        &test,
        "/api/files/text",
        json!({"path": "notes.md", "content": "x", "workspaceId": id.clone()}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert!(workspace_dir(&test, &id).join("notes.md").is_file());
    assert!(!workspace_dir(&test, "default").join("notes.md").exists());
}

#[tokio::test]
async fn a_workspace_with_no_registry_row_is_refused_without_creating_it() {
    let test = server();
    // The path resolver would happily accept any legal slug — deliberately, so
    // a *detached* workspace's sessions keep working — so the boundary that
    // decides "a user can still see this one" is the registry lookup.
    let answer = get(&test, "/api/files?workspace=ghost").await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert!(!workspace_dir(&test, "ghost").exists());
}

#[tokio::test]
async fn no_workspace_named_means_the_default_one() {
    let test = server();
    write_file(&test, "default", "notes.md", b"x");
    let answer = get(&test, "/api/files/text?path=notes.md").await;
    assert_eq!(answer.status, StatusCode::OK);
}

#[tokio::test]
async fn an_empty_workspace_id_is_refused_by_the_schema_rather_than_defaulted() {
    let test = server();
    // The wire shape says a named workspace is a non-empty string, so the
    // empty case never reaches the handler's fallback — omitting the field is
    // how a caller asks for the default one.
    let answer = put(
        &test,
        "/api/files/text",
        json!({"path": "notes.md", "content": "x", "workspaceId": ""}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!workspace_dir(&test, "default").join("notes.md").exists());
}

#[tokio::test]
async fn a_url_is_signed_against_the_workspace_it_was_minted_for_and_no_other() {
    let test = server();
    let id = make_workspace(&test, "Research");
    write_file(&test, &id, "photo.png", b"research");
    write_file(&test, "default", "photo.png", b"default");

    let minted = post(
        &test,
        "/api/files/signed-url",
        json!({"path": "photo.png", "workspaceId": id}),
    )
    .await;
    assert_eq!(minted.status, StatusCode::OK);

    let served = raw(
        &test,
        "GET",
        minted.json()["url"].as_str().unwrap(),
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(served.text(), "research");
}
