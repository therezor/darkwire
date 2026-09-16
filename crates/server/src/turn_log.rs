//! The turn that is running, kept whole.
//!
//! The replay ring beside this one answers "what did I miss since `seq`", and it
//! answers it for the session — bounded by a frame count, because that is the
//! right shape for a question about arbitrary history. It is the wrong shape for
//! the other question a reload asks, which is "what has this turn done so far":
//! a turn spends one frame per token, and a delegation spends one per token of
//! its subagent too, so any real answer overruns a frame budget in seconds. Past
//! the ring the server could only offer the stored tail, and storage cannot hold
//! a turn that has not ended — so the answer on screen after a refresh was the
//! remainder of the turn and nothing before it.
//!
//! This holds the running turn instead, from its `turn.start`, and nothing else.
//! Three decisions make that affordable:
//!
//!  - **Adjacent deltas of the same part are merged.** A hundred thousand tokens
//!    of answer is one entry holding one string, so the log is the size of the
//!    turn's *output*, not of its frame count. This is the whole reason a
//!    complete in-flight turn can be retained at all, and it is exactly what the
//!    client does with the same frames when it renders them.
//!  - **The budget is bytes.** Frames are free after merging; tool output is not.
//!    A turn that reads fifty large files is the shape that reaches the cap, and
//!    a turn that writes for ten minutes is not, which no frame count can tell
//!    apart.
//!  - **Over budget it drops everything and says so.** A half-log is worse than
//!    none: it looks like a turn that began in the middle. `complete` is false
//!    from then on, the hub falls back to the stored tail, and the memory goes
//!    back immediately rather than being held for an answer nobody can use.
//!
//! Only a session with an open turn holds one, so the ceiling on all of this is
//! the number of turns running at once, not the number of sessions in memory.

use darkwire_protocol::ws::{NestedAgentEvent, ServerMessage};
use serde_json::Value;

/// Charged per entry on top of its payload, for the fields every frame carries.
///
/// A round number rather than a measurement: the budget exists to stop a turn
/// holding an unreasonable amount of memory, and being out by a few bytes per
/// entry cannot change whether it does.
const ENTRY_OVERHEAD: usize = 64;

/// The frames of one running turn, merged and byte-bounded.
#[derive(Debug)]
pub struct TurnLog {
    budget: usize,
    entries: Vec<ServerMessage>,
    turn: Option<String>,
    bytes: usize,
    /// Whether this still holds every frame since `turn.start`.
    ///
    /// Separate from a non-empty entry list, because an empty log is the honest
    /// state of a turn that has emitted only its start — and a *dropped* log is
    /// empty too. Only this tells the two apart.
    retaining: bool,
    last_key: Option<String>,
}

impl TurnLog {
    /// A log holding at most `max_bytes` of payload for the open turn.
    pub fn new(max_bytes: usize) -> TurnLog {
        TurnLog {
            budget: max_bytes,
            entries: Vec::new(),
            turn: None,
            bytes: 0,
            retaining: false,
            last_key: None,
        }
    }

    /// The turn this is holding, or `None` between turns.
    pub fn open_turn_id(&self) -> Option<&str> {
        self.turn.as_deref()
    }

    /// True when the frames below are the whole of the open turn.
    pub fn complete(&self) -> bool {
        self.turn.is_some() && self.retaining
    }

    /// How many frames it is holding, after merging.
    pub fn size(&self) -> usize {
        self.entries.len()
    }

    /// Approximately how much is being held, in bytes.
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }

    /// The open turn's frames, in emission order.
    pub fn frames(&self) -> &[ServerMessage] {
        &self.entries
    }

    /// Offers one emitted frame to the log.
    pub fn push(&mut self, message: &ServerMessage) {
        if let ServerMessage::TurnStart(start) = message {
            self.begin(start.event.turn_id.clone());
        }
        let Some(open) = self.turn.clone() else {
            return;
        };

        if let ServerMessage::TurnEnd(end) = message {
            // Only the open turn's ending closes it. A `turn.end` for another
            // turn is not something the hub emits, but reacting to one would
            // silently throw away the log of the turn that is still running.
            if end.event.turn_id == open {
                self.clear();
            }
            return;
        }

        if !self.retaining || !is_retained(message) {
            return;
        }
        if turn_id_of(message) != Some(open.as_str()) {
            return;
        }

        self.append(message);
    }

    /// Forgets the open turn — it ended, or the conversation moved under it.
    pub fn clear(&mut self) {
        self.turn = None;
        self.retaining = false;
        self.entries = Vec::new();
        self.bytes = 0;
        self.last_key = None;
    }

    fn begin(&mut self, turn_id: String) {
        self.clear();
        self.turn = Some(turn_id);
        self.retaining = self.budget > 0;
    }

    fn append(&mut self, message: &ServerMessage) {
        let key = mergeable_key(message);

        if key.is_some() && key == self.last_key {
            let merged = self
                .entries
                .last()
                .and_then(|previous| concat_delta(previous, message));
            if let Some(merged) = merged {
                if let Some(last) = self.entries.last_mut() {
                    *last = merged;
                }
                // The *later* seq, deliberately. A client's cursor is the
                // highest seq it has applied, so an entry that reported the
                // first seq of the run it merged would leave the cursor behind
                // the frames it had rendered — and the next resume would re-send
                // text already on screen.
                self.charge(payload_size(message));
                return;
            }
        }

        self.last_key = key;
        self.entries.push(message.clone());
        self.charge(payload_size(message) + ENTRY_OVERHEAD);
    }

    fn charge(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > self.budget {
            self.overflow();
        }
    }

    /// Past the budget: hold nothing, and stay honest about it for this turn.
    ///
    /// The entries go immediately rather than at the next `turn.start`, because
    /// the reason to stop retaining is that the memory is wanted back.
    fn overflow(&mut self) {
        self.retaining = false;
        self.entries = Vec::new();
        self.bytes = 0;
        self.last_key = None;
    }
}

/// The events that belong to a turn's own account of itself.
///
/// An allowlist rather than "everything carrying a `turn_id`", because two
/// events carry one without being part of the turn: `session.status` restates
/// the running turn on every attach, and a `notice` with no `turn_id` is about
/// the session. Replaying either would raise a second toast for something the
/// client has already seen. `turn.end` is absent for a different reason — it
/// closes the log, and by the time it is emitted storage holds the turn.
fn is_retained(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::TurnStart(_)
            | ServerMessage::AssistantDelta(_)
            | ServerMessage::ReasoningDelta(_)
            | ServerMessage::ToolCall(_)
            | ServerMessage::ToolProgress(_)
            | ServerMessage::ToolApprovalRequest(_)
            | ServerMessage::ToolResult(_)
            | ServerMessage::Notice(_)
            | ServerMessage::Subagent(_)
    )
}

/// The turn a frame belongs to, for the frames that name one.
fn turn_id_of(message: &ServerMessage) -> Option<&str> {
    match message {
        ServerMessage::TurnStart(e) => Some(&e.event.turn_id),
        ServerMessage::AssistantDelta(e) => Some(&e.event.turn_id),
        ServerMessage::ReasoningDelta(e) => Some(&e.event.turn_id),
        ServerMessage::ToolCall(e) => Some(&e.event.turn_id),
        ServerMessage::ToolProgress(e) => Some(&e.event.turn_id),
        ServerMessage::ToolResult(e) => Some(&e.event.turn_id),
        ServerMessage::ToolApprovalRequest(e) => Some(&e.event.turn_id),
        ServerMessage::TurnEnd(e) => Some(&e.event.turn_id),
        ServerMessage::Notice(e) => e.event.turn_id.as_deref(),
        ServerMessage::Subagent(e) => Some(&e.event.turn_id),
        _ => None,
    }
}

/// What a run of deltas has to agree on to be one entry.
///
/// The scope, not just the kind: a turn's own text and its subagent's are two
/// streams arriving interleaved, and merging across them would splice one into
/// the other. `parent_session_key` + `parent_call_id` names a delegation at any
/// depth, which is the same address the client uses to place the frames.
///
/// `None` for anything that is not a delta, which never merges.
fn mergeable_key(message: &ServerMessage) -> Option<String> {
    match message {
        ServerMessage::AssistantDelta(_) => Some("turn:assistant.delta".to_owned()),
        ServerMessage::ReasoningDelta(_) => Some("turn:reasoning.delta".to_owned()),
        ServerMessage::Subagent(outer) => {
            let inner = match &outer.event.event {
                NestedAgentEvent::AssistantDelta(_) => "assistant.delta",
                NestedAgentEvent::ReasoningDelta(_) => "reasoning.delta",
                _ => return None,
            };
            Some(format!(
                "sub:{}:{}:{inner}",
                outer.event.parent_session_key, outer.event.parent_call_id
            ))
        }
        _ => None,
    }
}

/// Two adjacent deltas as one frame, or `None` if they are not both.
fn concat_delta(previous: &ServerMessage, next: &ServerMessage) -> Option<ServerMessage> {
    match (previous, next) {
        (ServerMessage::AssistantDelta(before), ServerMessage::AssistantDelta(after)) => {
            let mut merged = before.clone();
            merged.seq = after.seq;
            merged.event.text.push_str(&after.event.text);
            Some(ServerMessage::AssistantDelta(merged))
        }
        (ServerMessage::ReasoningDelta(before), ServerMessage::ReasoningDelta(after)) => {
            let mut merged = before.clone();
            merged.seq = after.seq;
            merged.event.text.push_str(&after.event.text);
            Some(ServerMessage::ReasoningDelta(merged))
        }
        (ServerMessage::Subagent(before), ServerMessage::Subagent(after)) => {
            let tail = delta_text(&after.event.event)?;
            let mut merged = before.clone();
            merged.seq = after.seq;
            match &mut merged.event.event {
                NestedAgentEvent::AssistantDelta(body) => body.text.push_str(tail),
                NestedAgentEvent::ReasoningDelta(body) => body.text.push_str(tail),
                _ => return None,
            }
            Some(ServerMessage::Subagent(merged))
        }
        _ => None,
    }
}

/// The text of a nested delta, or `None` for anything else.
fn delta_text(event: &NestedAgentEvent) -> Option<&str> {
    match event {
        NestedAgentEvent::AssistantDelta(body) => Some(&body.text),
        NestedAgentEvent::ReasoningDelta(body) => Some(&body.text),
        _ => None,
    }
}

/// Roughly what one frame costs, without serialising it.
///
/// Every case measures the one field that can be large and ignores the ids and
/// enums around it, which [`ENTRY_OVERHEAD`] covers instead. A tool call's
/// arguments are the one thing serialised — once per call, never per token —
/// because a `write_file` carries its whole file there and nothing else on the
/// frame reveals the size.
fn payload_size(message: &ServerMessage) -> usize {
    match message {
        ServerMessage::AssistantDelta(e) => utf16_len(&e.event.text),
        ServerMessage::ReasoningDelta(e) => utf16_len(&e.event.text),
        ServerMessage::ToolResult(e) => utf16_len(&e.event.content),
        ServerMessage::ToolCall(e) => json_size(&e.event.args),
        ServerMessage::ToolApprovalRequest(e) => json_size(&e.event.args),
        ServerMessage::Notice(e) => utf16_len(&e.event.message),
        ServerMessage::Subagent(e) => nested_payload_size(&e.event.event),
        _ => 0,
    }
}

/// [`payload_size`] for a frame that arrived wrapped in a subagent event.
fn nested_payload_size(event: &NestedAgentEvent) -> usize {
    match event {
        NestedAgentEvent::AssistantDelta(body) => utf16_len(&body.text),
        NestedAgentEvent::ReasoningDelta(body) => utf16_len(&body.text),
        NestedAgentEvent::ToolResult(body) => utf16_len(&body.content),
        NestedAgentEvent::ToolCall(body) => json_size(&body.args),
        NestedAgentEvent::ToolApprovalRequest(body) => json_size(&body.args),
        NestedAgentEvent::Notice(body) => utf16_len(&body.message),
        _ => 0,
    }
}

/// The budget counts what the client counts, which is UTF-16 code units.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn json_size(value: &Value) -> usize {
    match value {
        Value::String(text) => utf16_len(text),
        Value::Null => 0,
        // A value that cannot be serialised is not one this log can measure, and
        // a guess is better than a failure on the emit path.
        other => serde_json::to_string(other).map_or(0, |json| utf16_len(&json)),
    }
}
