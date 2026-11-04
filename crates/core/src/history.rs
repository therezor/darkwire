//! Turning stored history into a legal provider request.
//!
//! Stored history is append-only, and no row is ever rewritten: that is what
//! keeps a provider's prompt cache warm across a turn. (A *suffix* can be
//! dropped, by regenerate and edit; see the session store's `truncate_after`
//! for why that leaves the cache intact.) But the slice of it that goes to the
//! model is a fixed-size window, and a naive window cuts through the middle of
//! a tool exchange: the `assistant` message that declared `tool_calls` falls
//! off the front while the `tool` results that answer it remain. Every major
//! provider rejects that with a 400, and it happens on exactly the long
//! conversations where losing the turn costs the most.
//!
//! [`find_legal_start`] is the fix, and it is the highest-value pure function
//! in the repository: a handful of lines standing between the agent and a class
//! of failure that is invisible until a session gets long enough.
//!
//! Every length here is counted in UTF-16 code units, because that is the unit
//! the stored data, the wire and the web client all measure text in; a cap that
//! counted bytes or scalar values would move on a rewrite of the client.

use std::borrow::Borrow;
use std::collections::HashSet;

use darkwire_protocol::ChatMessage;

/// The first index from which every `tool` message has a matching preceding
/// `assistant` message that declared its `tool_call_id`.
///
/// The `declared` set is cleared whenever the cut point moves, which is the
/// subtle part: ids declared *before* the new start are about to be discarded
/// along with the assistant message that declared them, so continuing to treat
/// them as declared would leave a genuine orphan behind the cut. Clearing makes
/// the scan conservative: it can return an index past a repairable boundary,
/// never one before a broken pair.
///
/// Runs in a single pass, and returns `messages.len()` when no legal window
/// exists (an empty history is always legal).
///
/// Generic over the element so a caller holding stored rows can pass
/// references rather than copying every message to ask.
pub fn find_legal_start<M: Borrow<ChatMessage>>(messages: &[M]) -> usize {
    let mut declared: HashSet<&str> = HashSet::new();
    let mut start = 0;

    for (index, message) in messages.iter().enumerate() {
        match message.borrow() {
            ChatMessage::Assistant(assistant) => {
                declared.extend(assistant.tool_calls.iter().map(|call| call.id.as_str()));
            }
            ChatMessage::Tool(tool) if !declared.contains(tool.tool_call_id.as_str()) => {
                start = index + 1;
                declared.clear();
            }
            ChatMessage::Tool(_) | ChatMessage::User(_) | ChatMessage::System(_) => {}
        }
    }

    start
}

/// Whether any `tool` message lacks a preceding `assistant` that declared it.
///
/// The invariant [`find_legal_start`] exists to establish, stated independently
/// so it can be asserted rather than assumed: the property tests check the two
/// against each other, and the agent loop can check a request it assembled by
/// some other path.
pub fn has_orphaned_tool_result(messages: &[ChatMessage]) -> bool {
    let mut declared: HashSet<&str> = HashSet::new();
    for message in messages {
        match message {
            ChatMessage::Assistant(assistant) => {
                declared.extend(assistant.tool_calls.iter().map(|call| call.id.as_str()));
            }
            ChatMessage::Tool(tool) if !declared.contains(tool.tool_call_id.as_str()) => {
                return true;
            }
            ChatMessage::Tool(_) | ChatMessage::User(_) | ChatMessage::System(_) => {}
        }
    }
    false
}

/// The number of leading messages that form a tool-complete prefix.
///
/// The mirror of [`find_legal_start`]: that one finds where a window may
/// *begin*, this one finds where it may *end*. They exist for opposite defects.
/// A window that opens too early strands a `tool` result whose `assistant` fell
/// off the front; a history truncated at an arbitrary point strands the other
/// half, an `assistant` still declaring `tool_calls` whose answers were just
/// deleted. Providers reject both, and until truncation existed only the first
/// could happen.
///
/// The `answered` set is cleared whenever the cut point moves, for the same
/// reason [`find_legal_start`] clears `declared`: the `tool` messages that
/// answered those calls sit *after* the new end and are about to be dropped
/// with it, so continuing to count them as answers would leave a genuine
/// orphan in front of the cut. Clearing makes the scan conservative: it can
/// return an index before a repairable boundary, never one that leaves a call
/// unanswered.
///
/// Runs in a single backward pass, and returns `messages.len()` when the whole
/// list is already complete.
pub fn find_legal_end(messages: &[ChatMessage]) -> usize {
    let mut answered: HashSet<&str> = HashSet::new();
    let mut end = messages.len();

    for (index, message) in messages.iter().enumerate().rev() {
        match message {
            ChatMessage::Tool(tool) => {
                answered.insert(tool.tool_call_id.as_str());
            }
            ChatMessage::Assistant(assistant)
                if assistant
                    .tool_calls
                    .iter()
                    .any(|call| !answered.contains(call.id.as_str())) =>
            {
                end = index;
                answered.clear();
            }
            ChatMessage::Assistant(_) | ChatMessage::User(_) | ChatMessage::System(_) => {}
        }
    }

    end
}

/// Whether any `assistant` message declares a tool call that no later `tool`
/// message answers.
///
/// The invariant [`find_legal_end`] exists to establish, stated independently
/// so it can be asserted rather than assumed: the counterpart to
/// [`has_orphaned_tool_result`], and checked against [`find_legal_end`] by
/// property test.
pub fn has_unanswered_tool_call(messages: &[ChatMessage]) -> bool {
    let mut pending: HashSet<&str> = HashSet::new();
    for message in messages {
        match message {
            ChatMessage::Assistant(assistant) => {
                pending.extend(assistant.tool_calls.iter().map(|call| call.id.as_str()));
            }
            ChatMessage::Tool(tool) => {
                pending.remove(tool.tool_call_id.as_str());
            }
            ChatMessage::User(_) | ChatMessage::System(_) => {}
        }
    }
    !pending.is_empty()
}

/// What [`truncate_head_tail`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Truncation {
    /// The text, with the middle replaced by a marker when truncated.
    pub text: String,
    /// Whether anything was dropped.
    pub truncated: bool,
    /// UTF-16 code units dropped from the middle. `0` when nothing was.
    pub omitted: usize,
}

/// Keeps the head and the tail, drops the middle.
///
/// Head+tail rather than a plain head because both ends carry signal and the
/// middle rarely does: a directory listing's first entries identify what was
/// listed, its last entries are what the model was probably looking for, and a
/// stack trace's head names the error while its tail names the caller. Cutting
/// only the head loses the error; cutting only the tail loses the answer.
///
/// `max_chars` budgets the *retained content*, in UTF-16 code units; `0` means
/// no limit. The marker is added on top, so a caller sizing a token budget can
/// treat this as an exact bound on the part that varies, rather than a bound
/// that shrinks by the marker's length. A cut that lands inside a surrogate
/// pair leaves a replacement character on each side of the marker.
pub fn truncate_head_tail(text: &str, max_chars: usize) -> Truncation {
    let units: Vec<u16> = text.encode_utf16().collect();
    if max_chars == 0 || units.len() <= max_chars {
        return Truncation {
            text: text.to_owned(),
            truncated: false,
            omitted: 0,
        };
    }

    let head_chars = max_chars.div_ceil(2);
    let tail_chars = max_chars - head_chars;
    let omitted = units.len() - max_chars;
    let marker = format!("\n\n… [{omitted} characters truncated] …\n\n");
    let head = String::from_utf16_lossy(&units[..head_chars]);
    let tail = if tail_chars == 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&units[units.len() - tail_chars..])
    };

    Truncation {
        text: format!("{head}{marker}{tail}"),
        truncated: true,
        omitted,
    }
}

/// How many stored messages a request window keeps at most.
pub const DEFAULT_MAX_HISTORY_MESSAGES: usize = 500;

/// Where a request's window of `messages` begins.
///
/// Order matters: keep the most recent `max_messages` (`0` keeps all), then
/// start at the first `user` message so the window opens on a whole turn
/// (skipped when there is none), then align to a legal tool-call boundary,
/// because the second step can strand a `tool` result whose call it cut.
pub fn history_window_start<M: Borrow<ChatMessage>>(messages: &[M], max_messages: usize) -> usize {
    let mut start = 0;
    if max_messages > 0 && messages.len() > max_messages {
        start = messages.len() - max_messages;
    }

    if let Some(first_user) = messages[start..]
        .iter()
        .position(|message| matches!(message.borrow(), ChatMessage::User(_)))
        && first_user > 0
    {
        start += first_user;
    }

    start + find_legal_start(&messages[start..])
}
