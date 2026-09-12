//! `src/pickers/sessions.rs` — the rows, and which one the menu opens on.
//!
//! One property decides this file: a person recognises a conversation by its
//! title and needs the key only when two of them share a name. Everything
//! asserted here is that arrangement holding for the cases where it is hard —
//! an untitled session, and the one the prompt is already in.

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

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use ghostai::i18n::Translations;
use ghostai::pickers::sessions::{pick_session, session_items};
use ghostai::pickers::{MenuRequest, NoMenu, PickerMenu};
use ghostai_core::session_store::{SessionRecord, SessionSummaryRecord};

fn session(key: &str, title: &str, messages: usize) -> SessionSummaryRecord {
    SessionSummaryRecord {
        session: SessionRecord {
            key: key.to_owned(),
            title: title.to_owned(),
            origin: "cli".to_owned(),
            workspace_id: "default".to_owned(),
            agent_id: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            metadata: serde_json::Map::new(),
        },
        message_count: messages,
    }
}

fn sessions() -> Vec<SessionSummaryRecord> {
    vec![
        session("cli:default", "Porting the CLI", 12),
        session("cli-9f2ab1", "", 3),
        session("web:1", "Porting the CLI", 1),
    ]
}

/// Records what it was asked and answers with whatever it was told to.
struct Recording {
    answer: Option<usize>,
    asked: Mutex<Vec<MenuRequest>>,
}

impl Recording {
    fn answering(answer: Option<usize>) -> Recording {
        Recording {
            answer,
            asked: Mutex::new(Vec::new()),
        }
    }

    /// The index the one request opened on.
    fn opened_on(&self) -> Option<usize> {
        self.asked.lock().unwrap()[0].index
    }
}

impl PickerMenu for Recording {
    fn available(&self) -> bool {
        true
    }

    fn choose<'a>(
        &'a self,
        request: MenuRequest,
    ) -> Pin<Box<dyn Future<Output = Option<usize>> + Send + 'a>> {
        self.asked.lock().unwrap().push(request);
        let answer = self.answer;
        Box::pin(async move { answer })
    }
}

#[test]
fn shows_the_title_and_keeps_the_key_for_searching() {
    // A title is what a person recognises; the key is what tells two
    // conversations with the same title apart, so it stays reachable as a
    // keyword rather than crowding the label.
    let items = session_items(&sessions(), "cli:default", &Translations::default());

    assert_eq!(items[0].label, "Porting the CLI");
    assert_eq!(items[0].keywords.as_deref(), Some("cli:default"));
    assert_eq!(items[2].keywords.as_deref(), Some("web:1"));
}

#[test]
fn falls_back_to_the_key_for_a_conversation_nothing_has_named_yet() {
    // A blank row would be unpickable. The key is ugly and unambiguous, which
    // is the right trade for a row that has nothing else.
    let items = session_items(&sessions(), "cli:default", &Translations::default());

    assert_eq!(items[1].label, "cli-9f2ab1");
}

#[test]
fn counts_the_messages_in_the_hint() {
    let items = session_items(&sessions(), "web:1", &Translations::default());

    assert!(
        items[0]
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains('1'))
    );
    assert!(
        items[1]
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains('3')),
        "{:?}",
        items[1].hint
    );
}

#[test]
fn marks_the_conversation_the_prompt_is_already_in() {
    // Without it, picking the one you are in looks like it did nothing.
    let items = session_items(&sessions(), "cli-9f2ab1", &Translations::default());

    let marked: Vec<bool> = items
        .iter()
        .map(|item| {
            item.hint
                .as_deref()
                .is_some_and(|hint| hint.contains("current"))
        })
        .collect();
    assert_eq!(marked, [false, true, false]);
}

#[tokio::test]
async fn opens_the_menu_on_the_conversation_the_prompt_is_in() {
    // Not on the first row: the list is newest-first, so the row somebody wants
    // to move away from is rarely at the top.
    let menu = Recording::answering(Some(2));

    let chosen = pick_session(&menu, &sessions(), "cli-9f2ab1", &Translations::default()).await;

    assert_eq!(menu.opened_on(), Some(1));
    assert_eq!(chosen.as_deref(), Some("web:1"));
}

#[tokio::test]
async fn answers_nothing_when_the_menu_was_cancelled() {
    // Escape leaves the conversation where it was, which is the whole reason
    // the answer is an option rather than a default.
    let menu = Recording::answering(None);

    let chosen = pick_session(&menu, &sessions(), "cli:default", &Translations::default()).await;

    assert_eq!(chosen, None);
}

#[tokio::test]
async fn answers_nothing_where_there_is_no_menu_to_open() {
    // A pipe has no terminal to draw on, and the caller prints the listing
    // instead — the same rule `/agent` and `/workspace` follow.
    let chosen = pick_session(
        &NoMenu,
        &sessions(),
        "cli:default",
        &Translations::default(),
    )
    .await;

    assert_eq!(chosen, None);
}
