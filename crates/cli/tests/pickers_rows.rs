//! `src/pickers/sessions.rs` and `src/pickers/workspaces.rs` — the two row
//! builders whose whole job is labelling.

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

use ghostai::i18n::Translations;
use ghostai::pickers::sessions::session_items;
use ghostai::pickers::workspaces::workspace_items;
use ghostai_core::WorkspaceRecord;
use ghostai_core::session_store::{SessionRecord, SessionSummaryRecord};
use serde_json::Map;

fn english() -> Translations {
    Translations::default()
}

/// Only the fields the row builder reads; a record carries a dozen more.
fn session(key: &str, title: &str, message_count: usize) -> SessionSummaryRecord {
    SessionSummaryRecord {
        session: SessionRecord {
            key: key.to_owned(),
            title: title.to_owned(),
            origin: "cli".to_owned(),
            workspace_id: "default".to_owned(),
            agent_id: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            metadata: Map::new(),
        },
        message_count,
    }
}

fn workspace(id: &str, name: &str) -> WorkspaceRecord {
    WorkspaceRecord {
        id: id.to_owned(),
        name: name.to_owned(),
        created_at_ms: 0,
        updated_at_ms: 0,
        is_default: id == "default",
        metadata: Map::new(),
    }
}

fn sessions() -> Vec<SessionSummaryRecord> {
    vec![
        session("cli-9f2ab1", "Refactor the parser", 12),
        session("cli-0000aa", "", 1),
    ]
}

#[test]
fn shows_the_title_because_that_is_what_a_person_recognises() {
    let t = english();
    assert_eq!(
        session_items(&sessions(), "cli-9f2ab1", &t)[0].label,
        "Refactor the parser"
    );
}

#[test]
fn falls_back_to_the_key_for_a_session_nobody_has_named() {
    let t = english();
    assert_eq!(
        session_items(&sessions(), "cli-9f2ab1", &t)[1].label,
        "cli-0000aa"
    );
}

#[test]
fn counts_the_messages_and_gets_the_singular_right() {
    let t = english();
    let items = session_items(&sessions(), "nothing", &t);
    assert_eq!(items[0].hint.as_deref(), Some("12 messages"));
    assert_eq!(items[1].hint.as_deref(), Some("1 message"));
}

#[test]
fn marks_the_session_the_prompt_is_on() {
    let t = english();
    let items = session_items(&sessions(), "cli-9f2ab1", &t);
    assert!(items[0].hint.as_deref().unwrap().contains("current"));
}

#[test]
fn keeps_the_key_searchable_for_when_two_conversations_share_a_name() {
    let t = english();
    assert_eq!(
        session_items(&sessions(), "nothing", &t)[0]
            .keywords
            .as_deref(),
        Some("cli-9f2ab1")
    );
}

#[test]
fn makes_no_rows_for_no_sessions() {
    let t = english();
    assert!(session_items(&[], "nothing", &t).is_empty());
}

fn workspaces() -> Vec<WorkspaceRecord> {
    vec![
        workspace("default", "Default"),
        workspace("research", "Research"),
    ]
}

#[test]
fn shows_the_workspace_name_with_the_id_beside_it() {
    // The id and not a session count: a workspace is where a conversation
    // *starts*, and one can be moved to another afterwards, so a count there
    // implies an ownership that does not hold. The id is what `/workspace <id>`
    // takes, which makes it the useful thing to show.
    let t = english();
    let items = workspace_items(&workspaces(), None, &t);
    assert_eq!(items[0].label, "Default");
    assert_eq!(items[0].hint.as_deref(), Some("default"));
    assert_eq!(items[1].hint.as_deref(), Some("research"));
}

#[test]
fn says_nothing_about_how_many_sessions_are_in_one() {
    let t = english();
    for item in workspace_items(&workspaces(), None, &t) {
        assert!(!item.hint.unwrap_or_default().contains("session"));
    }
}

#[test]
fn marks_the_workspace_new_sessions_land_in() {
    let t = english();
    let items = workspace_items(&workspaces(), Some("research"), &t);
    assert!(items[1].hint.as_deref().unwrap().contains("current"));
    assert!(!items[0].hint.as_deref().unwrap().contains("current"));
}

#[test]
fn makes_no_rows_for_no_workspaces() {
    let t = english();
    assert!(workspace_items(&[], None, &t).is_empty());
}
