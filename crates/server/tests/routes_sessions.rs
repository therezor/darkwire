//! The conversation routes, over real HTTP against a real store.
//!
//! Two properties carry most of the weight here and neither is visible from a
//! handler signature. The listings are cursor-paged because they move under a
//! reader — a turn landing anywhere bumps its session to the front — so the
//! tests that matter are the ones where something is appended *between* two
//! pages. And a binding is refused at the door: a conversation bound to an
//! agent or a workspace nobody can resolve is one the UI can never show, and
//! almost every dangling binding in the TypeScript came from a route that
//! checked one and not the other.
//!
//! The clock is moved by hand throughout. Four sessions created in a single
//! millisecond all tie on `updated_at_ms`, which is exactly the ordering a
//! cursor addresses — so a test that did not move it would be asserting on
//! whichever order SQLite happened to return.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture or a response that cannot be read is a failing test either way"
)]

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use darkwire_core::messages::{AssistantOptions, assistant_message, user_message};
use darkwire_core::session_store::{
    AppendOptions, CreateSession, SessionStore, TurnStatsRecord, UpdateSession,
};
use darkwire_core::workspace_store::CreateWorkspace;
use darkwire_protocol::config::{AgentEntry, Config};
use darkwire_protocol::messages::{ChatMessage, StopReason, Usage};
use darkwire_protocol::tasks::{TaskItem, TaskStatus};
use darkwire_server::cursor::{SessionListCursor, encode_session_cursor};
use darkwire_server::runtime::ServerRuntime as _;
use darkwire_server::testkit::{
    FakeRuntimeOptions, TestServer, TestServerOptions, start_test_server,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

// Harness

fn server() -> TestServer {
    start_test_server(TestServerOptions::default()).expect("a test server")
}

fn server_with(config: Config) -> TestServer {
    start_test_server(TestServerOptions {
        config: Some(config.clone()),
        runtime: FakeRuntimeOptions {
            config: Some(config),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    })
    .expect("a test server")
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
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
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

fn store(test: &TestServer) -> std::sync::Arc<SessionStore> {
    test.runtime.store()
}

/// A session with `n` user messages, each a millisecond apart so the listing
/// order is the one the test wrote rather than a coin toss.
fn seed_session(test: &TestServer, key: &str, title: &str, messages: usize) {
    let store = store(test);
    store
        .ensure_session(
            key,
            CreateSession {
                title: Some(title.to_owned()),
                ..CreateSession::default()
            },
        )
        .expect("the session was created");
    for index in 0..messages {
        test.clock.advance(Duration::from_millis(1));
        store
            .append(
                key,
                ChatMessage::User(user_message(format!("message {index}"))),
                &AppendOptions::default(),
            )
            .expect("the message was appended");
    }
    test.clock.advance(Duration::from_millis(1));
}

fn turn_stats(session_key: &str, turn_id: &str, started_at_ms: i64) -> TurnStatsRecord {
    TurnStatsRecord {
        turn_id: turn_id.to_owned(),
        session_key: session_key.to_owned(),
        agent_id: "default".to_owned(),
        workspace_id: "default".to_owned(),
        provider: "openai".to_owned(),
        model: "gpt-test".to_owned(),
        started_at_ms,
        ended_at_ms: started_at_ms + 500,
        iterations: 1,
        stop_reason: StopReason::Complete,
        usage: Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            ..Usage::default()
        },
        generation_ms: Some(400),
        generation_tokens: Some(5),
        first_token_ms: Some(100),
        error: None,
    }
}

/// A config carrying one extra agent, so the binding guards have something
/// legal to accept as well as something illegal to refuse.
fn config_with_agent(id: &str, enabled: bool) -> Config {
    let mut config = Config::default();
    let entry = AgentEntry {
        enabled,
        label: id.to_owned(),
        ..AgentEntry::default()
    };
    config.agents.list.insert(id.to_owned(), entry);
    config
}

// Creating, reading, renaming

#[tokio::test]
async fn creating_a_session_returns_it() {
    let test = server();
    let answer = post(
        &test,
        "/api/sessions",
        json!({"key": "s-1", "title": "Planning"}),
    )
    .await;

    assert_eq!(answer.status, StatusCode::CREATED);
    assert_eq!(answer.json()["key"], "s-1");
    assert_eq!(answer.json()["title"], "Planning");
    assert_eq!(answer.json()["messageCount"], 0);
    assert_eq!(answer.json()["origin"], "web");
    assert_eq!(answer.json()["workspaceId"], "default");
}

#[tokio::test]
async fn a_client_that_supplies_no_key_gets_one() {
    let test = server();
    let answer = post(&test, "/api/sessions", json!({"title": "Untitled"})).await;

    assert_eq!(answer.status, StatusCode::CREATED);
    let key = answer.json()["key"].as_str().expect("a minted key");
    // The layout is the protocol's UUIDv7, which is what keeps the browser's
    // spelling and the server's from drifting.
    assert_eq!(key.len(), 36);
    assert!(store(&test).get_session(key).unwrap().is_some());
}

#[tokio::test]
async fn creating_is_idempotent_on_a_repeated_key() {
    let test = server();
    let first = post(
        &test,
        "/api/sessions",
        json!({"key": "s-1", "title": "One"}),
    )
    .await;
    let second = post(
        &test,
        "/api/sessions",
        json!({"key": "s-1", "title": "Two"}),
    )
    .await;

    // A client that retries a create it never saw the response to gets its
    // session rather than a 409 about a session it already owns.
    assert_eq!(second.status, StatusCode::CREATED);
    assert_eq!(second.json()["key"], "s-1");
    assert_eq!(second.json()["title"], first.json()["title"]);
}

#[tokio::test]
async fn reading_one_session_reports_its_message_count() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 3);

    let answer = get(&test, "/api/sessions/s-1").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["messageCount"], 3);
    assert_eq!(answer.json()["title"], "Planning");
}

#[tokio::test]
async fn reading_a_session_that_is_not_there_is_a_not_found() {
    let test = server();
    let answer = get(&test, "/api/sessions/nope").await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.json()["error"]["code"], "not_found");
}

#[tokio::test]
async fn renaming_a_session_keeps_everything_else() {
    let test = server();
    seed_session(&test, "s-1", "Old", 2);

    let answer = patch(&test, "/api/sessions/s-1", json!({"title": "New"})).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["title"], "New");
    assert_eq!(answer.json()["messageCount"], 2);
}

#[tokio::test]
async fn renaming_a_session_that_is_not_there_is_a_not_found() {
    let test = server();
    let answer = patch(&test, "/api/sessions/nope", json!({"title": "New"})).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_body_that_is_not_json_at_all_is_a_bad_request() {
    let test = server();
    let request = Request::builder()
        .method("POST")
        .uri("/api/sessions")
        .header("authorization", format!("Bearer {}", test.token))
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    // Never reached a schema, so there is no field to name: a 400, not a 422.
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_body_of_the_wrong_shape_names_the_field() {
    let test = server();
    let answer = post(&test, "/api/sessions", json!({"title": 7})).await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(answer.json()["error"]["details"].is_object());
}

// Bindings: the workspace and the agent

#[tokio::test]
async fn a_session_moves_to_another_workspace() {
    let test = server();
    test.runtime
        .workspaces()
        .create(CreateWorkspace {
            name: "Research".to_owned(),
            ..CreateWorkspace::default()
        })
        .expect("the workspace was created");
    seed_session(&test, "s-1", "Planning", 1);

    let answer = patch(
        &test,
        "/api/sessions/s-1",
        json!({"workspaceId": "research"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["workspaceId"], "research");
    assert_eq!(
        store(&test)
            .get_session("s-1")
            .unwrap()
            .unwrap()
            .workspace_id,
        "research"
    );
}

#[tokio::test]
async fn a_session_cannot_be_moved_to_a_workspace_that_does_not_exist() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);

    let answer = patch(&test, "/api/sessions/s-1", json!({"workspaceId": "ghost"})).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    // The move is refused whole: nothing is half-applied.
    assert_eq!(
        store(&test)
            .get_session("s-1")
            .unwrap()
            .unwrap()
            .workspace_id,
        "default"
    );
}

#[tokio::test]
async fn a_new_session_cannot_be_bound_to_an_agent_that_does_not_exist() {
    let test = server();
    let answer = post(
        &test,
        "/api/sessions",
        json!({"key": "s-1", "agentId": "ghost"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert!(store(&test).get_session("s-1").unwrap().is_none());
}

#[tokio::test]
async fn a_disabled_agent_is_refused_too() {
    // It is absent from every listing an operator could have picked from, so
    // accepting it would bind a conversation to something the UI cannot show.
    let test = server_with(config_with_agent("reviewer", false));
    let answer = post(
        &test,
        "/api/sessions",
        json!({"key": "s-1", "agentId": "reviewer"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_new_session_binds_to_an_agent_that_does_exist() {
    let test = server_with(config_with_agent("reviewer", true));
    let answer = post(
        &test,
        "/api/sessions",
        json!({"key": "s-1", "agentId": "reviewer"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CREATED);
    assert_eq!(answer.json()["agentId"], "reviewer");
}

#[tokio::test]
async fn a_session_cannot_be_moved_onto_an_agent_that_does_not_exist() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    let answer = patch(&test, "/api/sessions/s-1", json!({"agentId": "ghost"})).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_session_moves_off_an_agent_that_has_been_deleted() {
    // The guard is about the *incoming* id and never the stored one: this
    // recovery is the whole reason the route exists.
    let test = server();
    let store = store(&test);
    store
        .ensure_session(
            "s-1",
            CreateSession {
                agent_id: Some("gone".to_owned()),
                ..CreateSession::default()
            },
        )
        .expect("the session was created");

    let answer = patch(&test, "/api/sessions/s-1", json!({"title": "Renamed"})).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["agentId"], "gone");
}

#[tokio::test]
async fn an_empty_agent_id_is_not_a_binding_to_check() {
    let test = server();
    let answer = post(&test, "/api/sessions", json!({"key": "s-1", "agentId": ""})).await;
    assert_eq!(answer.status, StatusCode::CREATED);
}

// Deleting and clearing

#[tokio::test]
async fn deleting_a_session_takes_its_messages_with_it() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 3);

    let answer = delete(&test, "/api/sessions/s-1").await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    assert!(store(&test).get_session("s-1").unwrap().is_none());
}

#[tokio::test]
async fn deleting_a_session_that_is_not_there_is_a_not_found() {
    let test = server();
    assert_eq!(
        delete(&test, "/api/sessions/nope").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn clearing_a_transcript_keeps_the_session() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 3);

    let answer = delete(&test, "/api/sessions/s-1/messages").await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);

    let session = store(&test).get_session("s-1").unwrap();
    assert!(session.is_some(), "the session survives its transcript");
    assert_eq!(store(&test).message_count("s-1").unwrap(), 0);
}

#[tokio::test]
async fn clearing_a_session_that_is_not_there_is_a_not_found() {
    let test = server();
    assert_eq!(
        delete(&test, "/api/sessions/nope/messages").await.status,
        StatusCode::NOT_FOUND
    );
}

// The listing

#[tokio::test]
async fn the_listing_orders_by_most_recent_activity() {
    let test = server();
    seed_session(&test, "old", "Old", 1);
    test.clock.advance(Duration::from_secs(1));
    seed_session(&test, "new", "New", 1);

    let answer = get(&test, "/api/sessions").await;
    assert_eq!(answer.status, StatusCode::OK);
    let keys: Vec<&str> = answer.json()["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["new", "old"]);
    assert_eq!(answer.json()["total"], 2);
}

#[tokio::test]
async fn the_listing_filters_by_origin() {
    let test = server();
    let store = store(&test);
    for (key, origin) in [("web-1", "web"), ("tg-1", "telegram")] {
        store
            .ensure_session(
                key,
                CreateSession {
                    origin: Some(origin.to_owned()),
                    ..CreateSession::default()
                },
            )
            .unwrap();
        test.clock.advance(Duration::from_millis(1));
    }

    let answer = get(&test, "/api/sessions?origin=telegram").await;
    let rows = answer.json()["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["key"], "tg-1");
    // The total describes the same set as the rows beneath it — a total over a
    // different predicate is a "Page 4 of 3" that only appears after a search.
    assert_eq!(answer.json()["total"], 1);
}

#[tokio::test]
async fn the_listing_excludes_one_origin_and_the_total_follows() {
    let test = server();
    let store = store(&test);
    for (key, origin) in [("web-1", "web"), ("sub-1", "subagent"), ("web-2", "web")] {
        store
            .ensure_session(
                key,
                CreateSession {
                    origin: Some(origin.to_owned()),
                    ..CreateSession::default()
                },
            )
            .unwrap();
        test.clock.advance(Duration::from_millis(1));
    }

    // The sidebar sends `subagent`: a shortlist of thirty is a list of
    // conversations, and a delegated run is a step inside one.
    let answer = get(&test, "/api/sessions?excludeOrigin=subagent").await;
    assert_eq!(answer.json()["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(answer.json()["total"], 2);
}

#[tokio::test]
async fn a_cursor_is_issued_only_when_there_is_another_row() {
    let test = server();
    seed_session(&test, "a", "A", 0);
    test.clock.advance(Duration::from_millis(10));
    seed_session(&test, "b", "B", 0);

    let full = get(&test, "/api/sessions?limit=2").await;
    assert!(
        full.json()["nextCursor"].is_null(),
        "a page that holds everything issues no cursor"
    );

    let partial = get(&test, "/api/sessions?limit=1").await;
    assert!(partial.json()["nextCursor"].is_string());
}

#[tokio::test]
async fn a_cursor_survives_an_append_landing_between_two_pages() {
    let test = server();
    for key in ["a", "b", "c"] {
        seed_session(&test, key, key, 0);
        test.clock.advance(Duration::from_millis(10));
    }

    let first = get(&test, "/api/sessions?limit=2").await;
    let seen: Vec<String> = first.json()["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["key"].as_str().unwrap().to_owned())
        .collect();
    let cursor = first.json()["nextCursor"].as_str().unwrap().to_owned();

    // A turn lands on a session already reported, which is what moves it to
    // the front and is precisely what an offset-paged reader gets wrong.
    test.clock.advance(Duration::from_millis(100));
    store(&test)
        .append(
            &seen[0],
            ChatMessage::User(user_message("a new turn")),
            &AppendOptions::default(),
        )
        .unwrap();

    let second = get(&test, &format!("/api/sessions?limit=2&cursor={cursor}")).await;
    let rest: Vec<&str> = second.json()["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["key"].as_str().unwrap())
        .collect();
    // No row appears twice across the two pages, which is the guarantee the
    // cursor exists to provide.
    for key in &rest {
        assert!(!seen.contains(&(*key).to_owned()), "{key} was sent twice");
    }
}

#[tokio::test]
async fn a_cursor_the_server_did_not_issue_is_refused() {
    let test = server();
    // Silently restarting from the top would page a client through the same
    // first page forever.
    let answer = get(&test, "/api/sessions?cursor=not-a-cursor").await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.json()["error"]["code"], "bad_request");
}

#[tokio::test]
async fn a_cursor_that_decodes_to_the_wrong_shape_is_refused() {
    let test = server();
    let bogus = base64_url(&json!({"nothing": true}).to_string());
    let answer = get(&test, &format!("/api/sessions?cursor={bogus}")).await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_cursor_this_server_issued_is_accepted() {
    let test = server();
    seed_session(&test, "a", "A", 0);
    test.clock.advance(Duration::from_millis(10));
    seed_session(&test, "b", "B", 0);

    // Hand-built rather than echoed, so the test pins the encoding itself: a
    // client round-trips `nextCursor` verbatim, and the only thing that makes
    // that safe is that the server can read back exactly what it wrote.
    let newest = store(&test).get_session("b").unwrap().unwrap();
    let cursor = encode_session_cursor(&SessionListCursor {
        updated_at_ms: newest.updated_at_ms,
        key: newest.key.clone(),
    });
    let answer = get(&test, &format!("/api/sessions?cursor={cursor}")).await;
    assert_eq!(answer.status, StatusCode::OK);
    let keys: Vec<&str> = answer.json()["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        ["a"],
        "the page resumes after the row the cursor names"
    );
}

#[tokio::test]
async fn a_cursor_with_an_empty_key_is_refused() {
    let test = server();
    // A cursor is `(updatedAtMs, key)`, and a key is what breaks the tie on the
    // timestamp: one without it addresses no position.
    let cursor = encode_session_cursor(&SessionListCursor {
        updated_at_ms: 1,
        key: String::new(),
    });
    let answer = get(&test, &format!("/api/sessions?cursor={cursor}")).await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_limit_past_the_cap_is_refused() {
    let test = server();
    let answer = get(&test, "/api/sessions?limit=500").await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_limit_that_is_not_a_number_is_refused() {
    let test = server();
    // A value that will not parse and a value that parses and breaks a bound
    // are the same mistake to whoever sent it — a parameter they have to fix.
    let answer = get(&test, "/api/sessions?limit=lots").await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn the_listing_pages_over_an_offset_for_a_reader_that_jumps() {
    let test = server();
    for key in ["a", "b", "c"] {
        seed_session(&test, key, key, 0);
        test.clock.advance(Duration::from_millis(10));
    }

    let answer = get(&test, "/api/sessions?limit=1&offset=1").await;
    assert_eq!(answer.status, StatusCode::OK);
    let rows = answer.json()["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    // Newest first, so offset 1 is the middle one.
    assert_eq!(rows[0]["key"], "b");
    // A pager cannot derive "of 3" from the row in front of it.
    assert_eq!(answer.json()["total"], 3);
}

#[tokio::test]
async fn a_request_naming_both_paging_modes_is_refused() {
    let test = server();
    // A page relative to a page has no reading more correct than the others,
    // and a precedence rule would silently ignore one of the two parameters.
    let answer = get(&test, "/api/sessions?cursor=abc&offset=1").await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_negative_offset_is_refused() {
    let test = server();
    let answer = get(&test, "/api/sessions?offset=-1").await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn the_listing_searches_titles_case_insensitively() {
    let test = server();
    seed_session(&test, "a", "Quarterly Planning", 0);
    test.clock.advance(Duration::from_millis(10));
    seed_session(&test, "b", "Bug triage", 0);

    let answer = get(&test, "/api/sessions?q=planning").await;
    let rows = answer.json()["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["key"], "a");
}

#[tokio::test]
async fn a_blank_search_is_the_same_as_no_search() {
    let test = server();
    seed_session(&test, "a", "A", 0);
    // Refusing it would make clearing the search field a 422.
    let answer = get(&test, "/api/sessions?q=").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["sessions"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn the_listing_orders_by_a_column_other_than_recency() {
    let test = server();
    seed_session(&test, "z", "Zebra", 0);
    test.clock.advance(Duration::from_millis(10));
    seed_session(&test, "a", "Aardvark", 0);

    let answer = get(&test, "/api/sessions?sort=title&desc=false").await;
    assert_eq!(answer.status, StatusCode::OK);
    let titles: Vec<&str> = answer.json()["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Aardvark", "Zebra"]);
}

#[tokio::test]
async fn no_cursor_is_issued_under_an_ordering_a_cursor_cannot_address() {
    let test = server();
    for key in ["a", "b", "c"] {
        seed_session(&test, key, key, 0);
        test.clock.advance(Duration::from_millis(10));
    }

    // A cursor encodes a position in `updatedAtMs DESC, key ASC` and in no
    // other, so handing one back under a title sort would be a cursor that
    // cannot be followed.
    let answer = get(&test, "/api/sessions?limit=1&sort=title").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert!(answer.json()["nextCursor"].is_null());
    assert_eq!(answer.json()["total"], 3);
}

#[tokio::test]
async fn an_explicit_ascending_recency_sort_also_issues_no_cursor() {
    let test = server();
    for key in ["a", "b"] {
        seed_session(&test, key, key, 0);
        test.clock.advance(Duration::from_millis(10));
    }
    let answer = get(&test, "/api/sessions?limit=1&sort=updated&desc=false").await;
    assert!(answer.json()["nextCursor"].is_null());
}

#[tokio::test]
async fn the_listing_reports_a_session_total_usage() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    store(&test)
        .record_turn_stats(&turn_stats("s-1", "turn-1", 1_700_000_000_000))
        .expect("the turn was recorded");

    let answer = get(&test, "/api/sessions").await;
    let rows = answer.json()["sessions"].as_array().unwrap();
    assert_eq!(rows[0]["totalUsage"]["totalTokens"], 15);
}

#[tokio::test]
async fn a_conversation_whose_turns_predate_the_table_reports_no_total() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    // Omitted rather than zeroed: reporting `0` would claim the conversation
    // cost nothing rather than that nobody counted.
    let answer = get(&test, "/api/sessions/s-1").await;
    assert!(answer.json().get("totalUsage").is_none_or(Value::is_null));
}

// The transcript

#[tokio::test]
async fn the_transcript_pages_in_order() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 3);

    let first = get(&test, "/api/sessions/s-1/messages?limit=2").await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.json()["sessionKey"], "s-1");
    let seqs: Vec<u64> = first.json()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, [1, 2]);

    let cursor = first.json()["nextCursor"].as_str().expect("another page");
    let second = get(
        &test,
        &format!("/api/sessions/s-1/messages?limit=2&cursor={cursor}"),
    )
    .await;
    let seqs: Vec<u64> = second.json()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, [3]);
    assert!(second.json()["nextCursor"].is_null());
}

#[tokio::test]
async fn a_session_with_no_messages_is_an_empty_page_rather_than_a_not_found() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 0);
    let answer = get(&test, "/api/sessions/s-1/messages").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["messages"].as_array().unwrap().len(), 0);
    assert!(answer.json()["nextCursor"].is_null());
}

#[tokio::test]
async fn the_transcript_of_a_session_that_is_not_there_is_a_not_found() {
    let test = server();
    assert_eq!(
        get(&test, "/api/sessions/nope/messages").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_malformed_transcript_cursor_is_refused() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    let answer = get(&test, "/api/sessions/s-1/messages?cursor=nope").await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_transcript_reports_why_a_turn_failed() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    let mut failed = turn_stats("s-1", "turn-1", 1_700_000_000_000);
    failed.stop_reason = StopReason::Error;
    failed.error = Some("the provider refused".to_owned());
    store(&test).record_turn_stats(&failed).unwrap();

    // A failed turn appends nothing, so without this a rebuilt transcript shows
    // the question, no answer, and no sign that anything went wrong.
    let answer = get(&test, "/api/sessions/s-1/messages").await;
    assert_eq!(answer.json()["failures"]["turn-1"], "the provider refused");
}

#[tokio::test]
async fn a_transcript_with_no_failures_carries_an_empty_map() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    store(&test)
        .record_turn_stats(&turn_stats("s-1", "turn-1", 1_700_000_000_000))
        .unwrap();
    let answer = get(&test, "/api/sessions/s-1/messages").await;
    assert_eq!(answer.json()["failures"], json!({}));
}

// The context inspector

#[tokio::test]
async fn the_context_route_reports_the_prompt_and_the_window() {
    let test = start_test_server(TestServerOptions {
        runtime: FakeRuntimeOptions {
            system_prompt: Some("# DarkWire\n\nSession: {session}".to_owned()),
            runtime_block: Some("## Live state".to_owned()),
            ..FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    })
    .expect("a test server");
    seed_session(&test, "s-1", "Planning", 2);

    let answer = get(&test, "/api/sessions/s-1/context").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["sessionKey"], "s-1");
    assert!(
        answer.json()["systemPrompt"]
            .as_str()
            .unwrap()
            .contains("Session: s-1"),
        "the prompt comes from the loop that would send it"
    );
    assert_eq!(answer.json()["runtimeBlock"], "## Live state");
    assert!(answer.json()["estimatedTokens"].as_u64().unwrap() > 0);
    assert!(answer.json()["contextWindowTokens"].as_u64().unwrap() > 0);
    // Section name to token cost, so an oversized block is visible.
    assert!(answer.json()["breakdown"]["systemPrompt"].is_number());
    assert!(answer.json()["breakdown"]["runtimeBlock"].is_number());
}

#[tokio::test]
async fn the_context_route_sends_no_reasoning_while_the_transcript_still_does() {
    let test = server();
    let store = store(&test);
    store
        .ensure_session("s-1", CreateSession::default())
        .unwrap();
    let assistant = assistant_message(
        "the answer",
        AssistantOptions {
            reasoning: Some("the working out".to_owned()),
            ..AssistantOptions::default()
        },
    );
    store
        .append(
            "s-1",
            ChatMessage::Assistant(assistant),
            &AppendOptions::default(),
        )
        .unwrap();

    let context = get(&test, "/api/sessions/s-1/context").await;
    let messages = context.json()["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .all(|row| row["message"].get("reasoning").is_none_or(Value::is_null)),
        "the wire has never carried reasoning into the window"
    );

    let transcript = get(&test, "/api/sessions/s-1/messages").await;
    let stored = transcript.json()["messages"].as_array().unwrap();
    assert_eq!(stored[0]["message"]["reasoning"], "the working out");
}

#[tokio::test]
async fn the_context_route_names_the_agent_it_measured() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    let answer = get(&test, "/api/sessions/s-1/context").await;
    assert_eq!(answer.json()["agentId"], "default");
    // Absent is the healthy state: a client treats its presence as the whole
    // signal rather than comparing two ids on every response.
    assert!(
        answer
            .json()
            .get("requestedAgentId")
            .is_none_or(Value::is_null)
    );
}

#[tokio::test]
async fn the_context_route_falls_back_rather_than_404ing_for_a_binding_that_is_gone() {
    let test = server();
    let store = store(&test);
    store
        .ensure_session("s-1", CreateSession::default())
        .unwrap();
    store
        .append(
            "s-1",
            ChatMessage::User(user_message("hello")),
            &AppendOptions::default(),
        )
        .unwrap();
    store
        .update_session(
            "s-1",
            UpdateSession {
                agent_id: Some(Some("deleted".to_owned())),
                ..UpdateSession::default()
            },
        )
        .unwrap();

    let answer = get(&test, "/api/sessions/s-1/context").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["agentId"], "default");
    // Reported, so a reader is told what they are looking at rather than
    // quietly shown something else.
    assert_eq!(answer.json()["requestedAgentId"], "deleted");
}

#[tokio::test]
async fn the_context_of_a_session_that_is_not_there_is_a_not_found() {
    let test = server();
    assert_eq!(
        get(&test, "/api/sessions/nope/context").await.status,
        StatusCode::NOT_FOUND
    );
}

// Branching

#[tokio::test]
async fn branching_forks_the_prefix_and_leaves_the_source_alone() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 4);

    let answer = post(&test, "/api/sessions/s-1/branch", json!({"seq": 2})).await;
    assert_eq!(answer.status, StatusCode::CREATED);
    let forked = answer.json()["key"].as_str().unwrap();
    assert_ne!(forked, "s-1");
    assert_eq!(answer.json()["messageCount"], 2);

    // The source keeps everything it had.
    assert_eq!(store(&test).message_count("s-1").unwrap(), 4);
}

#[tokio::test]
async fn branching_honours_a_title_and_a_key() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 3);

    let answer = post(
        &test,
        "/api/sessions/s-1/branch",
        json!({"seq": 1, "key": "fork-1", "title": "Another path"}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CREATED);
    assert_eq!(answer.json()["key"], "fork-1");
    assert_eq!(answer.json()["title"], "Another path");
}

#[tokio::test]
async fn branching_a_session_that_is_not_there_is_a_not_found() {
    let test = server();
    assert_eq!(
        post(&test, "/api/sessions/nope/branch", json!({"seq": 1}))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn branching_past_the_end_snaps_to_what_is_there() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 2);
    let answer = post(&test, "/api/sessions/s-1/branch", json!({"seq": 99})).await;
    assert_eq!(answer.status, StatusCode::CREATED);
    assert_eq!(answer.json()["messageCount"], 2);
}

// Turn stats

#[tokio::test]
async fn a_conversation_with_no_recorded_turns_answers_with_an_empty_list() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    let answer = get(&test, "/api/sessions/s-1/turns").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["sessionKey"], "s-1");
    assert_eq!(answer.json()["turns"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn the_recorded_turns_come_back_newest_first() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    let store = store(&test);
    store
        .record_turn_stats(&turn_stats("s-1", "turn-1", 1_700_000_000_000))
        .unwrap();
    store
        .record_turn_stats(&turn_stats("s-1", "turn-2", 1_700_000_100_000))
        .unwrap();

    let answer = get(&test, "/api/sessions/s-1/turns").await;
    let ids: Vec<&str> = answer.json()["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["turnId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["turn-2", "turn-1"]);
    assert_eq!(answer.json()["turns"][0]["usage"]["totalTokens"], 15);
    assert_eq!(answer.json()["turns"][0]["generationMs"], 400);
}

#[tokio::test]
async fn the_turn_listing_honours_its_limit() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    let store = store(&test);
    for index in 0..3 {
        store
            .record_turn_stats(&turn_stats(
                "s-1",
                &format!("turn-{index}"),
                1_700_000_000_000 + i64::from(index) * 1_000,
            ))
            .unwrap();
    }

    let answer = get(&test, "/api/sessions/s-1/turns?limit=2").await;
    assert_eq!(answer.json()["turns"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn the_turn_listing_takes_no_cursor() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    // Accepting a `cursor` that is then ignored would put a parameter in the
    // OpenAPI document the server does not honour, which is a lie the document
    // cannot recover from. An unknown parameter is simply not read.
    let answer = get(&test, "/api/sessions/s-1/turns?cursor=whatever").await;
    assert_eq!(answer.status, StatusCode::OK);
}

#[tokio::test]
async fn the_turns_of_a_session_that_is_not_there_are_a_not_found() {
    let test = server();
    assert_eq!(
        get(&test, "/api/sessions/nope/turns").await.status,
        StatusCode::NOT_FOUND
    );
}

// Tasks

#[tokio::test]
async fn a_session_with_no_plan_answers_with_an_empty_list() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    let answer = get(&test, "/api/sessions/s-1/tasks").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["tasks"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn the_plan_comes_back_in_the_order_it_was_written() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    store(&test)
        .set_tasks(
            "s-1",
            &[
                TaskItem {
                    text: "Inspect auth".to_owned(),
                    status: TaskStatus::Done,
                },
                TaskItem {
                    text: "Update sessions".to_owned(),
                    status: TaskStatus::Doing,
                },
            ],
        )
        .unwrap();

    let answer = get(&test, "/api/sessions/s-1/tasks").await;
    assert_eq!(answer.json()["tasks"][0]["text"], "Inspect auth");
    assert_eq!(answer.json()["tasks"][0]["status"], "done");
    assert_eq!(answer.json()["tasks"][1]["status"], "doing");
}

#[tokio::test]
async fn the_plan_can_be_emptied_by_hand() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 1);
    store(&test)
        .set_tasks(
            "s-1",
            &[TaskItem {
                text: "Add tests".to_owned(),
                status: TaskStatus::Todo,
            }],
        )
        .unwrap();

    let answer = delete(&test, "/api/sessions/s-1/tasks").await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    assert_eq!(store(&test).tasks("s-1").unwrap(), Vec::new());
}

/// Emptying the plan is not emptying the conversation.
#[tokio::test]
async fn emptying_the_plan_leaves_the_messages_alone() {
    let test = server();
    seed_session(&test, "s-1", "Planning", 2);

    delete(&test, "/api/sessions/s-1/tasks").await;

    assert_eq!(store(&test).message_count("s-1").unwrap(), 2);
}

/// The same answer `context` gives, and for the same reason: a conversation
/// nobody has started has no plan, and inventing an empty one would report a
/// list for a session that does not exist.
#[tokio::test]
async fn the_plan_of_a_session_that_is_not_there_is_a_not_found() {
    let test = server();
    assert_eq!(
        get(&test, "/api/sessions/nope/tasks").await.status,
        StatusCode::NOT_FOUND
    );
}

// Helpers

/// base64url without padding, which is how a cursor is spelled.
fn base64_url(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}
