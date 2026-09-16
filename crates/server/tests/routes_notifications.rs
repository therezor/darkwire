//! The notification inbox over HTTP.
//!
//! Nothing here creates one through the API: the route surface is deliberately
//! read-and-dismiss, so the rows are seeded through the store the way the
//! things that actually raise them do.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use darkwire_core::Clock;
use darkwire_protocol::ws::NotificationLevel;
use darkwire_server::notifications::{CreateNotificationInput, NotificationStore};
use darkwire_server::testkit::{TestServer, TestServerOptions, start_test_server};
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt as _;

fn server() -> TestServer {
    start_test_server(TestServerOptions::default()).expect("a test server")
}

/// A second handle on the same shared connection, so a test can seed the rows
/// the routes then read.
fn store(test: &TestServer) -> NotificationStore {
    let counter = Arc::new(Mutex::new(0u64));
    NotificationStore::new(
        test.database.clone(),
        Arc::clone(&test.clock) as Arc<dyn Clock>,
        Box::new(move || {
            let mut next = counter.lock();
            *next += 1;
            format!("seeded-{next}")
        }),
    )
    .expect("the store")
}

/// Rows a millisecond apart, so the keyset order is not a tie.
///
/// The listing orders by `created_at_ms DESC, id ASC`; four rows written inside
/// one millisecond — which is what a fast test does — would all tie and hide
/// the ordering the cursor depends on.
fn seed(test: &TestServer, store: &NotificationStore, titles: &[&str]) -> Vec<String> {
    titles
        .iter()
        .map(|title| {
            test.clock.advance(Duration::from_millis(1));
            store
                .create(CreateNotificationInput {
                    title: (*title).to_owned(),
                    body: String::new(),
                    level: NotificationLevel::Info,
                    session_key: None,
                    job_id: None,
                })
                .expect("a notification")
                .id
        })
        .collect()
}

struct Answer {
    status: StatusCode,
    body: Value,
}

async fn send(test: &TestServer, method: Method, uri: &str) -> Answer {
    let request = Request::builder()
        .method(method)
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

fn titles(answer: &Answer) -> Vec<String> {
    answer.body["notifications"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|row| row["title"].as_str().unwrap_or_default().to_owned())
        .collect()
}

// Listing

#[tokio::test]
async fn the_listing_is_newest_first() {
    let test = server();
    seed(&test, &store(&test), &["first", "second", "third"]);

    let answer = send(&test, Method::GET, "/api/notifications").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(titles(&answer), ["third", "second", "first"]);
}

#[tokio::test]
async fn the_listing_reports_the_whole_unread_count_not_the_page_count() {
    let test = server();
    seed(&test, &store(&test), &["a", "b", "c"]);

    // The badge counts what is waiting, not what is on screen.
    let answer = send(&test, Method::GET, "/api/notifications?limit=1").await;
    assert_eq!(
        answer.body["notifications"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(answer.body["unreadCount"], 3);
}

#[tokio::test]
async fn the_total_and_the_unread_count_are_two_different_questions() {
    let test = server();
    let store = store(&test);
    let ids = seed(&test, &store, &["a", "b", "c"]);
    store.mark_read(&ids[0]).expect("a row");

    let answer = send(&test, Method::GET, "/api/notifications").await;
    // Every row the filter matches, versus how many of them are still unread.
    assert_eq!(answer.body["total"], 3);
    assert_eq!(answer.body["unreadCount"], 2);
}

#[tokio::test]
async fn the_unread_filter_narrows_both_the_page_and_the_total() {
    let test = server();
    let store = store(&test);
    let ids = seed(&test, &store, &["a", "b", "c"]);
    store.mark_read(&ids[0]).expect("a row");
    store.mark_read(&ids[1]).expect("a row");

    let answer = send(&test, Method::GET, "/api/notifications?unread=true").await;
    assert_eq!(titles(&answer), ["c"]);
    assert_eq!(answer.body["total"], 1);
    assert_eq!(answer.body["unreadCount"], 1);
}

#[tokio::test]
async fn the_listing_pages_with_the_cursor_it_issued() {
    let test = server();
    seed(&test, &store(&test), &["a", "b", "c"]);

    let first = send(&test, Method::GET, "/api/notifications?limit=2").await;
    assert_eq!(titles(&first), ["c", "b"]);
    let cursor = first.body["nextCursor"]
        .as_str()
        .expect("a cursor for the next page")
        .to_owned();

    let second = send(
        &test,
        Method::GET,
        &format!("/api/notifications?limit=2&cursor={cursor}"),
    )
    .await;
    assert_eq!(titles(&second), ["a"]);
    // The last page issues none: there is nothing after it to address.
    assert!(second.body["nextCursor"].is_null());
}

#[tokio::test]
async fn the_listing_pages_over_an_offset_for_a_numbered_reader() {
    let test = server();
    seed(&test, &store(&test), &["a", "b", "c"]);

    let answer = send(&test, Method::GET, "/api/notifications?limit=1&offset=1").await;
    assert_eq!(titles(&answer), ["b"]);
    assert_eq!(answer.body["total"], 3);
}

#[tokio::test]
async fn a_request_naming_both_paging_modes_is_refused() {
    let test = server();
    let answer = send(&test, Method::GET, "/api/notifications?cursor=abc&offset=1").await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_cursor_this_server_did_not_issue_is_a_400() {
    let test = server();
    let answer = send(&test, Method::GET, "/api/notifications?cursor=nonsense").await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_limit_past_the_cap_is_a_422() {
    let test = server();
    let answer = send(&test, Method::GET, "/api/notifications?limit=500").await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_limit_that_is_not_a_number_is_a_422() {
    // The coercion these query shapes exist for: `limit=2` has to read as a
    // number, and `limit=lots` has to be refused rather than defaulted.
    let test = server();
    let answer = send(&test, Method::GET, "/api/notifications?limit=lots").await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn an_empty_inbox_lists_as_empty_rather_than_failing() {
    let test = server();
    let answer = send(&test, Method::GET, "/api/notifications").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(titles(&answer), Vec::<String>::new());
    assert_eq!(answer.body["total"], 0);
    assert_eq!(answer.body["unreadCount"], 0);
}

// Reading

#[tokio::test]
async fn marking_one_read_answers_with_the_updated_row() {
    let test = server();
    let ids = seed(&test, &store(&test), &["a"]);

    let answer = send(
        &test,
        Method::POST,
        &format!("/api/notifications/{}/read", ids[0]),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    // The row rather than a 204, so a client can reconcile one item instead of
    // refetching a list it is in the middle of scrolling.
    assert_eq!(answer.body["id"], ids[0]);
    assert!(answer.body["readAtMs"].is_u64());
}

#[tokio::test]
async fn marking_a_row_that_is_not_there_is_a_404() {
    let test = server();
    let answer = send(&test, Method::POST, "/api/notifications/ghost/read").await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn marking_everything_read_answers_204_and_empties_the_badge() {
    let test = server();
    seed(&test, &store(&test), &["a", "b"]);

    let answer = send(&test, Method::POST, "/api/notifications/read").await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);

    let after = send(&test, Method::GET, "/api/notifications").await;
    assert_eq!(after.body["unreadCount"], 0);
    assert_eq!(after.body["total"], 2);
}

#[tokio::test]
async fn mark_all_is_routed_ahead_of_the_id_parameter() {
    // `/api/notifications/read` and `/api/notifications/:id/read` differ in
    // segment count, so there is nothing ambiguous to resolve — this asserts
    // that the router agrees.
    let test = server();
    let answer = send(&test, Method::POST, "/api/notifications/read").await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
}

// Deleting

#[tokio::test]
async fn deleting_one_answers_204_and_it_is_gone() {
    let test = server();
    let ids = seed(&test, &store(&test), &["a", "b"]);

    let answer = send(
        &test,
        Method::DELETE,
        &format!("/api/notifications/{}", ids[0]),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);

    let after = send(&test, Method::GET, "/api/notifications").await;
    assert_eq!(titles(&after), ["b"]);
}

#[tokio::test]
async fn deleting_a_row_that_is_not_there_is_a_404() {
    let test = server();
    let answer = send(&test, Method::DELETE, "/api/notifications/ghost").await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn clearing_empties_the_list_read_and_unread_alike() {
    let test = server();
    let store = store(&test);
    let ids = seed(&test, &store, &["a", "b"]);
    store.mark_read(&ids[0]).expect("a row");

    let answer = send(&test, Method::DELETE, "/api/notifications").await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);

    let after = send(&test, Method::GET, "/api/notifications").await;
    assert_eq!(after.body["total"], 0);
}

#[tokio::test]
async fn clearing_an_empty_list_is_not_a_complaint() {
    let test = server();
    let answer = send(&test, Method::DELETE, "/api/notifications").await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
}
