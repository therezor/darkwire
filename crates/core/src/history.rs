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

use std::collections::HashSet;

use ghostai_protocol::ChatMessage;

use crate::errors::Result;

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
pub fn find_legal_start(messages: &[ChatMessage]) -> usize {
    let mut declared: HashSet<&str> = HashSet::new();
    let mut start = 0;

    for (index, message) in messages.iter().enumerate() {
        match message {
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

/// The default for [`HistoryOptions::max_messages`].
pub const DEFAULT_MAX_HISTORY_MESSAGES: usize = 500;

/// The cap the loop applies to a single tool result before it enters history.
///
/// 8k characters is roughly 2k tokens: large enough for a substantial file or
/// command output, small enough that three of them in one turn cannot crowd out
/// the conversation in a 64k window.
pub const DEFAULT_MAX_TOOL_RESULT_CHARS: usize = 8_000;

/// How [`history_for_llm`] windows a history. `0` disables a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryOptions {
    /// Most recent messages to keep.
    pub max_messages: usize,
    /// Cap on each `tool` result, in UTF-16 code units.
    pub max_tool_result_chars: usize,
}

impl Default for HistoryOptions {
    fn default() -> Self {
        Self {
            max_messages: DEFAULT_MAX_HISTORY_MESSAGES,
            max_tool_result_chars: DEFAULT_MAX_TOOL_RESULT_CHARS,
        }
    }
}

/// Builds the message list for a provider request.
///
/// Order matters and is not interchangeable:
///
///  1. Keep the most recent `max_messages`.
///  2. Start at the first `user` message, so the window opens on a complete
///     turn rather than mid-exchange. Skipped entirely when the window contains
///     no user message, since dropping everything would be worse than starting
///     mid-turn.
///  3. Align to a legal tool-call boundary, *after* step 2, because step 2 is
///     itself capable of stranding a `tool` result whose `assistant` it just
///     cut.
///  4. Truncate tool results.
///
/// The system prompt is not handled here. The loop owns `messages[0]` and
/// rewrites it each iteration to keep the static half cache-stable, so any
/// `system` message that reached storage is dropped by step 2 rather than
/// competing with it.
pub fn history_for_llm(messages: &[ChatMessage], options: &HistoryOptions) -> Vec<ChatMessage> {
    let mut window = messages;
    if options.max_messages > 0 && window.len() > options.max_messages {
        window = &window[window.len() - options.max_messages..];
    }

    if let Some(first_user) = window
        .iter()
        .position(|message| matches!(message, ChatMessage::User(_)))
        && first_user > 0
    {
        window = &window[first_user..];
    }

    let legal_start = find_legal_start(window);
    if legal_start > 0 {
        window = &window[legal_start..];
    }

    if options.max_tool_result_chars == 0 {
        return window.to_vec();
    }

    window
        .iter()
        .map(|message| match message {
            ChatMessage::Tool(tool) => {
                let result = truncate_head_tail(&tool.content, options.max_tool_result_chars);
                if !result.truncated {
                    return message.clone();
                }
                let mut truncated = tool.clone();
                truncated.content = result.text;
                truncated.truncated = true;
                ChatMessage::Tool(truncated)
            }
            other => other.clone(),
        })
        .collect()
}

/// Which stored rows [`session_history`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MessageWindow {
    /// Rows with a `seq` strictly greater than this. `0` reads from the start.
    pub after_seq: u64,
    /// At most this many rows; `None` for all of them.
    pub limit: Option<usize>,
    /// Take the *last* `limit` rows rather than the first, still in order.
    pub from_end: bool,
}

/// The narrow view of a session store that [`session_history`] needs.
///
/// A trait rather than the store itself, so the store can depend on this
/// module's boundary rules without this module depending on the store, and so
/// the windowing can be tested against a list.
pub trait SessionHistorySource {
    /// The messages of one session, in `seq` order. An unknown session is an
    /// empty list rather than an error: the window over nothing is empty.
    fn messages(&self, session_key: &str, window: &MessageWindow) -> Result<Vec<ChatMessage>>;
}

/// The message list to send to a provider, read out of a store.
///
/// Here rather than on the store because the decision it encodes is about what
/// a *model* should be sent, not about how rows are read; a persistence type
/// that knew a provider existed would be the wrong shape. The rows come from
/// the store; the window is this module's, beside [`history_for_llm`] and the
/// boundary rules it applies.
pub fn session_history(
    source: &impl SessionHistorySource,
    session_key: &str,
    options: &HistoryOptions,
) -> Result<Vec<ChatMessage>> {
    let window = if options.max_messages > 0 {
        MessageWindow {
            after_seq: 0,
            limit: Some(options.max_messages),
            from_end: true,
        }
    } else {
        MessageWindow::default()
    };
    let messages = source.messages(session_key, &window)?;
    Ok(history_for_llm(&messages, options))
}
