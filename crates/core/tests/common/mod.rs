//! Shared by the store suites: a frozen clock, counter ids and message
//! constructors that build the protocol types directly.
//!
//! Each suite is its own binary and uses a different subset of these helpers.
#![allow(
    dead_code,
    reason = "each test binary uses a subset of the shared helpers"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use darkwire_core::session_store::{IdSource, SessionStore};
use darkwire_core::testkit::ManualClock;
use darkwire_core::{Database, Result};
use darkwire_protocol::messages::{
    AssistantMessage, AssistantRole, ChatMessage, ContentPart, TextPart, TextTag, ToolCall,
    ToolMessage, ToolRole, UserMessage, UserRole,
};

/// The frozen wall clock every fixture starts at.
pub const NOW: i64 = 1_700_000_000_000;

/// Deterministic ids. The prefix distinguishes two stores over the same file
/// — production uses UUIDv7, so only a fixture can collide this way.
pub fn counter_ids(prefix: &'static str) -> IdSource {
    let n = AtomicU64::new(0);
    Box::new(move || format!("{prefix}{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

/// A store over an in-memory database with the frozen clock and `m1`, `m2`…
/// ids.
pub fn make_store() -> (SessionStore, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::at(NOW));
    let store = make_store_on(Database::in_memory().unwrap(), Arc::clone(&clock)).unwrap();
    (store, clock)
}

/// A store over `db` with the given clock.
pub fn make_store_on(db: Database, clock: Arc<ManualClock>) -> Result<SessionStore> {
    SessionStore::new(db, clock, counter_ids("m"))
}

fn text_part(text: &str) -> ContentPart {
    ContentPart::Text(TextPart {
        tag: TextTag,
        text: text.to_owned(),
    })
}

/// A user message with one text part.
pub fn user_message(text: &str) -> ChatMessage {
    ChatMessage::User(UserMessage {
        role: UserRole,
        content: vec![text_part(text)],
    })
}

/// An assistant message with one text part (none when empty) and the calls.
pub fn assistant_message(text: &str, tool_calls: Vec<ToolCall>) -> ChatMessage {
    ChatMessage::Assistant(AssistantMessage {
        role: AssistantRole,
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![text_part(text)]
        },
        tool_calls,
        reasoning: None,
    })
}

/// A tool result answering `tool_call_id`.
pub fn tool_message(tool_call_id: &str, name: &str, content: &str) -> ChatMessage {
    ChatMessage::Tool(ToolMessage {
        role: ToolRole,
        tool_call_id: tool_call_id.to_owned(),
        name: name.to_owned(),
        content: content.to_owned(),
        is_error: false,
        truncated: false,
    })
}

/// A `read_file` call with the given id.
pub fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "read_file".to_owned(),
        arguments_json: "{\"path\":\"a.txt\"}".to_owned(),
    }
}

/// The text of a message's text parts, joined by newlines.
pub fn text_of(message: &ChatMessage) -> String {
    match message {
        ChatMessage::System(m) => m.content.clone(),
        ChatMessage::Tool(m) => m.content.clone(),
        ChatMessage::User(UserMessage { content, .. })
        | ChatMessage::Assistant(AssistantMessage { content, .. }) => content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(part) => Some(part.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
