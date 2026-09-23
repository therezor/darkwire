//! Buttons, and the table their `callback_data` points into.
//!
//! Telegram caps `callback_data` at **64 bytes**, and the things a button has to
//! carry do not fit: a call id is model-authored — `toolu_01…`, and an MCP tool
//! call's id runs longer — and a session key is `telegram:<chatId>:<uuid>`.
//! Encoding the payload is a bug waiting for one long id, and the failure is a
//! button that silently stops working.
//!
//! So a button carries a **token**: a counter in base 36, four to six bytes
//! whatever it points at. That is not only a way round the limit; it is what
//! makes three other things possible at all.
//!
//!  - **A stale button can say so.** An entry has a deadline, so a menu from
//!    yesterday answers "That menu has expired" instead of switching to a
//!    session that has since been deleted.
//!  - **A button belongs to the chat it was posted in.** The entry records the
//!    chat, so a press relayed from somewhere else is refused. That matters
//!    because anybody in a group can tap a button the bot posted — see
//!    `access.rs` for the other half of that check.
//!  - **The table is bounded.** Menus are cheap to produce and a chat could open
//!    a hundred; the oldest entries are dropped rather than kept forever.

use std::sync::Arc;

use darkwire_core::clock::Clock;
use darkwire_protocol::{ApprovalScope, ExecRule};
use indexmap::IndexMap;
use parking_lot::Mutex;

use crate::telegram::api::{InlineKeyboardButton, InlineKeyboardMarkup};
use crate::telegram::approvals::rule_label;

/// Which listing a paging button belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    /// The conversations this channel owns.
    Sessions,
    /// The agents a conversation may be bound to.
    Agents,
    /// The models the configured endpoints offer.
    Models,
    /// The workspaces on this install.
    Workspaces,
    /// The plan a conversation is running on.
    Tasks,
}

/// What a button does when it is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackPayload {
    /// Answer an approval request.
    Approve {
        /// The call being decided.
        call_id: String,
        /// The conversation it belongs to.
        session_key: String,
        /// Yes or no.
        approved: bool,
        /// How long the decision holds.
        scope: ApprovalScope,
        /// The command rule to save first, for "Always".
        rule: Option<ExecRule>,
    },
    /// Attach this chat to a conversation.
    Session {
        /// The conversation.
        session_key: String,
    },
    /// Bind this conversation to an agent.
    Agent {
        /// The agent.
        agent_id: String,
    },
    /// Move this install onto a model.
    Model {
        /// The model.
        model_id: String,
    },
    /// Move this conversation into a workspace.
    Workspace {
        /// The workspace.
        workspace_id: String,
    },
    /// Ask for a name, and make a workspace with it.
    WorkspaceNew,
    /// Ask for another name for this one.
    WorkspaceRename {
        /// The workspace.
        workspace_id: String,
    },
    /// Ask whether to detach this one.
    WorkspaceRemoveAsk {
        /// The workspace.
        workspace_id: String,
    },
    /// Detach it, having asked.
    WorkspaceRemove {
        /// The workspace.
        workspace_id: String,
    },
    /// Offer somewhere to send this one's sessions.
    WorkspaceMoveAsk {
        /// The workspace they are leaving.
        workspace_id: String,
    },
    /// Send them there.
    WorkspaceMove {
        /// The workspace they are leaving.
        from: String,
        /// The one they are going to.
        to: String,
    },
    /// Delete a conversation, having confirmed.
    Delete {
        /// The conversation.
        session_key: String,
    },
    /// Ask whether to delete a conversation, from its row in `/session`.
    ///
    /// Two taps rather than one, because this is the one thing here that
    /// cannot be undone — and the row it is fired from is a list somebody is
    /// scrolling, where a fingertip lands on the wrong name easily.
    DeleteAsk {
        /// The conversation.
        session_key: String,
        /// What it is called, for the question.
        title: String,
    },
    /// Empty a conversation's plan.
    ///
    /// The whole list rather than one task: the `todo` tool replaces it
    /// wholesale on its next planning step, so a task dropped by hand comes
    /// straight back and the gesture taught nothing.
    TasksClear {
        /// The conversation whose plan it is.
        session_key: String,
    },
    /// Toggle one rendering preference.
    Output {
        /// `progress` or `markdown`.
        field: String,
    },
    /// Another screen of a listing.
    Page {
        /// Which listing.
        menu: MenuKind,
        /// Where that screen starts.
        offset: usize,
    },
}

impl CallbackPayload {
    /// Whether a hit is consumed rather than left live.
    ///
    /// An approval answers once, and so does a task delete: both name
    /// something that is gone after the press, and a second tap on the same
    /// button would drop whatever had taken its place. A menu's paging buttons
    /// stay live so the reader can go back a page.
    fn is_one_shot(&self) -> bool {
        matches!(
            self,
            CallbackPayload::Approve { .. } | CallbackPayload::TasksClear { .. }
        )
    }
}

struct CallbackEntry {
    chat_id: i64,
    expires_at_ms: i64,
    payload: CallbackPayload,
}

/// Why a press did nothing, when it did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackRefusal {
    /// Unknown, or past its deadline.
    Expired,
    /// Posted in another chat.
    WrongChat,
}

/// What a pressed button meant, or why it means nothing now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackLookup {
    /// It meant this.
    Found(CallbackPayload),
    /// It meant nothing, for this reason.
    Refused(CallbackRefusal),
}

/// How long a menu button stays live when nothing else says.
const DEFAULT_CALLBACK_TTL_MS: i64 = 30 * 60 * 1000;

/// How many live buttons the process will remember at once.
pub const MAX_CALLBACK_ENTRIES: usize = 500;

/// The tokens currently pointing at something.
///
/// One per channel, not per chat: the token is unique across the process, and
/// the chat it belongs to is checked on lookup rather than by partitioning.
pub struct CallbackStore {
    clock: Arc<dyn Clock>,
    ttl_ms: i64,
    state: Mutex<CallbackState>,
}

struct CallbackState {
    entries: IndexMap<String, CallbackEntry>,
    counter: u64,
}

impl std::fmt::Debug for CallbackStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackStore")
            .field("size", &self.len())
            .finish_non_exhaustive()
    }
}

/// A counter in base 36: four to six bytes, whatever it points at.
fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut out = Vec::new();
    while value > 0 {
        let index = usize::try_from(value % 36).unwrap_or(0);
        out.push(DIGITS[index]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

impl CallbackStore {
    /// An empty table with the default deadline.
    pub fn new(clock: Arc<dyn Clock>) -> CallbackStore {
        CallbackStore::with_ttl(clock, DEFAULT_CALLBACK_TTL_MS)
    }

    /// An empty table whose entries live `ttl_ms`.
    pub fn with_ttl(clock: Arc<dyn Clock>, ttl_ms: i64) -> CallbackStore {
        CallbackStore {
            clock,
            ttl_ms,
            state: Mutex::new(CallbackState {
                entries: IndexMap::new(),
                counter: 0,
            }),
        }
    }

    /// Live tokens. Exposed for the eviction test and for a debug log.
    pub fn len(&self) -> usize {
        self.state.lock().entries.len()
    }

    /// Whether nothing is filed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Files a payload and returns the token to put on the button.
    ///
    /// `expires_at_ms` overrides the default, which is how an approval button
    /// inherits the gate's own deadline rather than outliving the request it
    /// answers.
    pub fn put(
        &self,
        chat_id: i64,
        payload: CallbackPayload,
        expires_at_ms: Option<i64>,
    ) -> String {
        let now = self.clock.now_ms();
        let mut state = self.state.lock();
        state.counter += 1;
        let token = base36(state.counter);
        state.entries.insert(
            token.clone(),
            CallbackEntry {
                chat_id,
                expires_at_ms: expires_at_ms.unwrap_or(now + self.ttl_ms),
                payload,
            },
        );
        evict(&mut state.entries, now);
        token
    }

    /// What that button meant, or why it means nothing now.
    pub fn take(&self, token: &str, chat_id: i64) -> CallbackLookup {
        let now = self.clock.now_ms();
        let mut state = self.state.lock();
        let Some(entry) = state.entries.get(token) else {
            // An unknown token and an expired one are the same answer on
            // purpose: a reader pressing a button from yesterday should be told
            // the menu is gone, not that it never existed.
            return CallbackLookup::Refused(CallbackRefusal::Expired);
        };
        if entry.expires_at_ms <= now {
            state.entries.shift_remove(token);
            return CallbackLookup::Refused(CallbackRefusal::Expired);
        }
        if entry.chat_id != chat_id {
            return CallbackLookup::Refused(CallbackRefusal::WrongChat);
        }
        let payload = entry.payload.clone();
        if payload.is_one_shot() {
            state.entries.shift_remove(token);
        }
        CallbackLookup::Found(payload)
    }

    /// Drops the buttons answering one approval, once it is settled elsewhere.
    pub fn forget_call(&self, call_id: &str) {
        self.state.lock().entries.retain(|_, entry| {
            !matches!(&entry.payload, CallbackPayload::Approve { call_id: answers, .. } if answers == call_id)
        });
    }

    /// Drops everything for one chat. `/exit` detaching, or a menu superseded.
    pub fn forget(&self, chat_id: i64) {
        self.state
            .lock()
            .entries
            .retain(|_, entry| entry.chat_id != chat_id);
    }
}

/// Keeps the table bounded, oldest first.
///
/// Insertion order *is* age here, because a token is never re-inserted — so the
/// map's own iteration order is the eviction order and no second index is
/// needed. The same trick the manager uses for its LRU.
fn evict(entries: &mut IndexMap<String, CallbackEntry>, now: i64) {
    if entries.len() <= MAX_CALLBACK_ENTRIES {
        return;
    }
    // Expired entries first, then simply the oldest.
    entries.retain(|_, entry| entry.expires_at_ms > now);
    while entries.len() > MAX_CALLBACK_ENTRIES {
        entries.shift_remove_index(0);
    }
}

// Keyboards

/// Rows shown eight at a time, which is about a phone screen.
pub const DEFAULT_PAGE_SIZE: usize = 8;

/// One row of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    /// Button text. Plain — Telegram does not parse entities in a button.
    pub label: String,
    /// Marked, so a listing says where you already are.
    pub current: bool,
    /// What pressing it does.
    pub payload: CallbackPayload,
}

/// One page of a listing, with the arrows it needs and no more.
///
/// The arrows are themselves tokens, so paging costs a round trip and no state
/// on the message — which is what lets a menu survive a restart badly rather
/// than wrongly: the button is simply expired.
pub fn picker_keyboard(
    rows: &[PickerRow],
    menu: MenuKind,
    chat_id: i64,
    store: &CallbackStore,
    offset: usize,
    page_size: usize,
) -> InlineKeyboardMarkup {
    let page_size = page_size.max(1);
    let offset = offset.min(rows.len());
    let page = &rows[offset..(offset + page_size).min(rows.len())];

    let mut keyboard: Vec<Vec<InlineKeyboardButton>> = page
        .iter()
        .map(|row| {
            vec![InlineKeyboardButton {
                text: if row.current {
                    format!("• {}", row.label)
                } else {
                    row.label.clone()
                },
                callback_data: store.put(chat_id, row.payload.clone(), None),
            }]
        })
        .collect();

    let mut arrows = Vec::new();
    if offset > 0 {
        arrows.push(InlineKeyboardButton {
            text: "« Prev".to_owned(),
            callback_data: store.put(
                chat_id,
                CallbackPayload::Page {
                    menu,
                    offset: offset.saturating_sub(page_size),
                },
                None,
            ),
        });
    }
    if offset + page_size < rows.len() {
        arrows.push(InlineKeyboardButton {
            text: "Next »".to_owned(),
            callback_data: store.put(
                chat_id,
                CallbackPayload::Page {
                    menu,
                    offset: offset + page_size,
                },
                None,
            ),
        });
    }
    if !arrows.is_empty() {
        keyboard.push(arrows);
    }

    InlineKeyboardMarkup {
        inline_keyboard: keyboard,
    }
}

/// One page of a listing at the default size.
pub fn picker(
    rows: &[PickerRow],
    menu: MenuKind,
    chat_id: i64,
    store: &CallbackStore,
) -> InlineKeyboardMarkup {
    picker_keyboard(rows, menu, chat_id, store, 0, DEFAULT_PAGE_SIZE)
}

/// Three buttons: the two approval scopes, and a refusal. A fourth, "Always",
/// when `rule` names a command rule that may be saved from a prompt.
///
/// Denial is `once` on purpose, though the gate remembers a refusal just as it
/// remembers an approval. A "deny for the session" one tap away from "deny
/// once", on a phone, is a way to silently disable a tool and not find out for
/// an hour. An operator who means it can say so where there is room to explain
/// it.
pub fn approval_keyboard(
    call_id: &str,
    session_key: &str,
    chat_id: i64,
    store: &CallbackStore,
    expires_at_ms: i64,
    rule: Option<ExecRule>,
) -> InlineKeyboardMarkup {
    let button = |text: &str, approved: bool, scope: ApprovalScope, rule: Option<ExecRule>| {
        InlineKeyboardButton {
            text: text.to_owned(),
            callback_data: store.put(
                chat_id,
                CallbackPayload::Approve {
                    call_id: call_id.to_owned(),
                    session_key: session_key.to_owned(),
                    approved,
                    scope,
                    rule,
                },
                Some(expires_at_ms),
            ),
        }
    };

    let mut inline_keyboard = vec![vec![
        button("✅ Once", true, ApprovalScope::Once, None),
        button("✅ This session", true, ApprovalScope::Session, None),
    ]];
    // Session scope, as the web prompt sends it with a rule.
    if let Some(rule) = rule {
        let label = rule_label(&rule);
        inline_keyboard.push(vec![button(
            &label,
            true,
            ApprovalScope::Session,
            Some(rule),
        )]);
    }
    inline_keyboard.push(vec![button("⛔ Deny", false, ApprovalScope::Once, None)]);
    InlineKeyboardMarkup { inline_keyboard }
}

/// A yes/no pair for something that cannot be undone.
pub fn confirm_keyboard(
    chat_id: i64,
    store: &CallbackStore,
    confirm: CallbackPayload,
    label: Option<&str>,
) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: label.unwrap_or("Yes, delete it").to_owned(),
            callback_data: store.put(chat_id, confirm, None),
        }]],
    }
}
