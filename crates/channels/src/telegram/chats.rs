//! What the channel remembers about one Telegram chat.
//!
//! Deliberately small, and deliberately not persisted. The durable half of a
//! conversation is the session row in SQLite — the browser's sidebar lists the
//! same one — and everything here is either derivable from it or a rendering
//! preference that the terminal's own `/output` also loses on exit.
//!
//! The attachment is the interesting field. The manager derives a session from
//! whatever key the channel publishes, so **switching conversation is just
//! publishing a different key**: `/new` and `/session` change this map and the
//! next message lands on a different hub connection, with the manager needing no
//! notion of a switch at all. That is the same thing the loopback channel's
//! `conversation` option does, one level up.
//!
//! Keys are stored already namespaced (`telegram:4471`). The manager's own
//! namespacing is idempotent once prefixed, so one form travels everywhere —
//! publish, control, and the store the commands read. Two forms in flight would
//! be a bug factory.

use std::collections::HashMap;

/// Rendering preferences a chat owns, and the `/output` command toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderPrefs {
    /// Whether a turn is a message that fills in, or one final reply.
    ///
    /// Not the same field the terminal's `/output` carries. Reasoning cannot be
    /// expressed — the projection never emits a reasoning delta to any channel
    /// — and stats have nothing to read; these two are what a chat transport
    /// actually decides for itself.
    pub progress: bool,
    /// MarkdownV2, or plain text. The escape hatch when a message will not
    /// send.
    pub markdown: bool,
}

impl Default for RenderPrefs {
    fn default() -> RenderPrefs {
        RenderPrefs {
            progress: true,
            markdown: true,
        }
    }
}

/// One chat's mutable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatState {
    /// The conversation this chat is attached to, already namespaced.
    pub session_key: String,
    /// The message a running turn is being written into, if one is.
    pub live_message_id: Option<i64>,
    /// The turn that message belongs to, so a stale edit is not applied.
    pub live_turn_id: Option<String>,
    /// When that message was last edited, for the per-chat debounce.
    pub last_edit_ms: i64,
    /// How this chat wants answers rendered.
    pub prefs: RenderPrefs,
}

/// The default conversation for a chat: stable, so it survives a restart.
pub fn default_session_key(channel_id: &str, chat_id: i64) -> String {
    format!("{channel_id}:{chat_id}")
}

/// A fresh conversation in the same chat.
///
/// A suffix rather than a wholly new key, so a glance at the session list still
/// says which chat a conversation came from.
pub fn new_session_key(channel_id: &str, chat_id: i64, unique: &str) -> String {
    format!("{}:{unique}", default_session_key(channel_id, chat_id))
}

/// Whether a key names a conversation this channel owns.
///
/// `/session <key>` is the one place an operator types a key by hand, and the
/// manager would happily namespace `web-abc` into `telegram:web-abc` — a real
/// conversation, empty, that nothing explains. Refusing is the difference
/// between an error and a mystery.
pub fn owns_session_key(channel_id: &str, key: &str) -> bool {
    key.starts_with(&format!("{channel_id}:"))
}

/// Every chat this channel has heard from since it started.
#[derive(Debug)]
pub struct ChatBook {
    channel_id: String,
    states: HashMap<i64, ChatState>,
}

impl ChatBook {
    /// An empty book for one channel.
    pub fn new(channel_id: impl Into<String>) -> ChatBook {
        ChatBook {
            channel_id: channel_id.into(),
            states: HashMap::new(),
        }
    }

    /// The chat's state, created on first sight and attached to its default.
    pub fn for_chat(&mut self, chat_id: i64) -> &mut ChatState {
        let channel_id = &self.channel_id;
        self.states.entry(chat_id).or_insert_with(|| ChatState {
            session_key: default_session_key(channel_id, chat_id),
            live_message_id: None,
            live_turn_id: None,
            last_edit_ms: 0,
            prefs: RenderPrefs::default(),
        })
    }

    /// A copy of the chat's state, created on first sight.
    pub fn snapshot(&mut self, chat_id: i64) -> ChatState {
        self.for_chat(chat_id).clone()
    }

    /// Points a chat at another conversation.
    pub fn attach(&mut self, chat_id: i64, session_key: impl Into<String>) {
        let state = self.for_chat(chat_id);
        state.session_key = session_key.into();
        // The old turn's message belongs to the old conversation; editing it
        // after a switch would rewrite an answer the reader is still scrolled
        // to.
        state.live_message_id = None;
        state.live_turn_id = None;
    }
}
