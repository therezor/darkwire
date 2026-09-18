//! Addressing a message from a prompt.
//!
//! The negative-offset rule is the part worth testing directly: `-1` counts
//! back over the messages *you* wrote, not over rows. A conversation with a
//! tool call in it has assistant turns and tool results between two questions,
//! and an offset that counted rows would resolve `-2` to a tool result nobody
//! can edit.

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

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use darkwire::messages::{recent_messages, resolve_seq};
use darkwire_core::messages::{AssistantOptions, ToolOptions};
use darkwire_core::session_store::{AppendOptions, CreateSession};
use darkwire_core::{
    Database, ErrorKind, SessionStore, SystemClock, assistant_message, tool_message, user_message,
};
use darkwire_protocol::ToolCall;

const SESSION: &str = "cli:default";

/// An in-memory store with counted ids, so nothing here touches a disk.
fn empty_store() -> SessionStore {
    let counter = Arc::new(AtomicU64::new(0));
    SessionStore::new(
        Database::in_memory().unwrap(),
        Arc::new(SystemClock),
        Box::new(move || format!("id-{}", counter.fetch_add(1, Ordering::Relaxed))),
    )
    .unwrap()
}

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "read".to_owned(),
        arguments_json: "{}".to_owned(),
    }
}

/// Six rows: two questions with a tool call and two answers between them.
fn store() -> SessionStore {
    let created = empty_store();
    let options = AppendOptions::default();
    created
        .append(SESSION, user_message("the first question").into(), &options)
        .unwrap();
    created
        .append(
            SESSION,
            assistant_message(
                "",
                AssistantOptions {
                    tool_calls: vec![call("a")],
                    reasoning: None,
                },
            )
            .into(),
            &options,
        )
        .unwrap();
    created
        .append(
            SESSION,
            tool_message("a", "read", "contents", ToolOptions::default()).into(),
            &options,
        )
        .unwrap();
    created
        .append(
            SESSION,
            assistant_message("the first answer", AssistantOptions::default()).into(),
            &options,
        )
        .unwrap();
    created
        .append(
            SESSION,
            user_message("the second question").into(),
            &options,
        )
        .unwrap();
    created
        .append(
            SESSION,
            assistant_message("the second answer", AssistantOptions::default()).into(),
            &options,
        )
        .unwrap();
    created
}

#[test]
fn defaults_to_the_last_thing_you_said() {
    assert_eq!(resolve_seq(&store(), SESSION, None).unwrap(), 5);
}

#[test]
fn counts_back_over_your_messages_not_over_rows() {
    // Rows 2, 3 and 4 sit between the two questions. `-2` is the earlier
    // question, not a tool result.
    assert_eq!(resolve_seq(&store(), SESSION, Some("-2")).unwrap(), 1);
}

#[test]
fn takes_a_seq_from_the_listing_verbatim() {
    assert_eq!(resolve_seq(&store(), SESSION, Some("3")).unwrap(), 3);
}

#[test]
fn refuses_a_seq_that_names_no_row() {
    let error = resolve_seq(&store(), SESSION, Some("99")).unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
}

#[test]
fn refuses_an_offset_past_what_you_have_said() {
    let error = resolve_seq(&store(), SESSION, Some("-9")).unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
}

#[test]
fn refuses_anything_that_is_not_an_integer_including_zero() {
    let db = store();
    for reference in ["0", "x", "1.5", "--1"] {
        let error = resolve_seq(&db, SESSION, Some(reference)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{reference}");
    }
}

#[test]
fn says_so_plainly_when_you_have_said_nothing_yet() {
    let db = empty_store();
    db.ensure_session(SESSION, CreateSession::default())
        .unwrap();
    let error = resolve_seq(&db, SESSION, None).unwrap_err();
    assert!(
        error.message.contains("not said anything"),
        "{}",
        error.message
    );
}

#[test]
fn recent_messages_returns_the_tail_oldest_first_with_the_seqs_the_listing_prints() {
    let rows = recent_messages(&store(), SESSION, 3).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![4, 5, 6]
    );
}

#[test]
fn recent_messages_names_each_row_by_role_so_a_seq_can_be_judged_before_it_is_used() {
    let rows = recent_messages(&store(), SESSION, 2).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.role).collect::<Vec<_>>(),
        vec!["user", "assistant"]
    );
}

#[test]
fn recent_messages_carries_the_text_the_listing_shows() {
    let rows = recent_messages(&store(), SESSION, 2).unwrap();
    assert_eq!(rows[0].text, "the second question");
}
