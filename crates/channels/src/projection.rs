//! A turn, as a chat app has to render it.
//!
//! The hub speaks the same event stream to every consumer: deltas, tool calls,
//! notices, a `turn.end`. A browser renders all of it. A chat channel cannot —
//! it has one message shape, no card layout, and a rate limit — so something has
//! to decide which events become text and which are dropped. That decision is
//! here, once, rather than in each channel, because a channel that reimplements
//! it gets the two dangerous cases wrong: an approval request that nobody
//! renders makes the turn hang until it expires, and a stop reason that nobody
//! renders makes a truncated answer look like a complete one.
//!
//! The rules:
//!
//!  - **The server never keeps the answer for the client's benefit — except
//!    here.** A channel has no accumulator: it posts a message. So this holds
//!    the deltas for the length of a turn and emits one `reply` at `turn.end`.
//!    That is a per-session buffer bounded by one turn's output, which is the
//!    one place the protocol's "the client accumulates" rule cannot apply.
//!  - **`progress` carries the answer so far; `reply` carries the whole
//!    answer.** They overlap on purpose — a transport that edits in place wants
//!    exactly that — which is why a channel opts into `progress` by declaring it
//!    in `accepts`, and why the default is not to send it.
//!  - **Reasoning deltas are never projected.** A model's scratchpad arriving
//!    unasked in someone's chat app is a different product decision than showing
//!    it in a collapsible block a browser can collapse.
//!  - **A non-`complete` turn always says so.** A partial answer that reads as a
//!    final one is the failure this exists to prevent; `StopReason::Error` is
//!    the single exception, because the hub already broadcast the `error` event
//!    that explains it and saying it twice is not saying it better.

use std::collections::HashMap;

use ghostai_core::message_bus::OutboundKind;
use ghostai_protocol::{NestedAgentEvent, ServerMessage, StopReason, ToolRisk};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One message the projection wants sent, before the manager addresses it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundDraft {
    /// Why it is being sent.
    pub kind: OutboundKind,
    /// The whole message.
    pub text: String,
    /// Present for everything scoped to a turn, so a channel can group edits.
    pub turn_id: Option<String>,
    /// Structured detail for a transport that can render more than a line of
    /// text.
    ///
    /// The text is always the whole message: a channel that ignores this
    /// renders exactly what it rendered before, which is what lets an approval
    /// grow buttons on Telegram without changing what the loopback channel
    /// says. The manager copies it onto `OutboundMessage.metadata`.
    pub metadata: Map<String, Value>,
}

/// `metadata.approval` on the draft an approval request produces.
///
/// `args` is deliberately absent. It is model-authored and unbounded, and a
/// channel that pasted it into a chat would be forwarding whatever the model
/// wrote to the one person whose judgement is the last check on it. What a
/// decision actually needs is the tool and its risk band; a channel that wants
/// a preview reads `tool.call` and caps it itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDraftDetail {
    /// The call being decided.
    pub call_id: String,
    /// The tool.
    pub name: String,
    /// The tool's declared risk band.
    pub risk: ToolRisk,
    /// When the prompt closes as denied.
    pub expires_at_ms: u64,
}

/// The key [`ApprovalDraftDetail`] travels under on a draft's metadata.
pub const APPROVAL_METADATA_KEY: &str = "approval";

/// What a channel's projection is allowed to say beyond the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnProjectionOptions {
    /// `channels.sendProgress`: the answer so far, at each tool boundary.
    pub send_progress: bool,
    /// `channels.sendToolHints`: which tool is running, and which one failed.
    pub send_tool_hints: bool,
}

impl Default for TurnProjectionOptions {
    fn default() -> TurnProjectionOptions {
        TurnProjectionOptions {
            send_progress: true,
            send_tool_hints: false,
        }
    }
}

/// What to say about a turn that did not simply finish.
fn stop_notice(stop_reason: StopReason, has_answer: bool) -> Option<&'static str> {
    match stop_reason {
        // Rare, and worth a word: a turn whose whole output was tool calls
        // looks from a chat app exactly like a bot that ignored the message.
        StopReason::Complete => (!has_answer).then_some("The turn finished without an answer."),
        StopReason::Aborted => Some("Stopped."),
        StopReason::MaxIterations => Some("Stopped: the turn reached its tool-iteration limit."),
        StopReason::WallTimeout => Some("Stopped: the turn reached its time limit."),
        // The hub broadcast an `error` event before this; that one carries what
        // actually went wrong, and this one would only carry that it did.
        StopReason::Error => None,
    }
}

/// One session's live turn.
///
/// Held by the manager per hub connection, which is per `(channel, session)`.
/// Nothing here is shared between sessions, so a slow channel cannot make one
/// session's accumulator show up in another's reply.
#[derive(Debug)]
pub struct TurnProjection {
    options: TurnProjectionOptions,
    /// The answer as it stands. Reset at each `turn.start`.
    answer: String,
    turn_id: Option<String>,
    /// `call_id` → tool name, so a failed result can be named.
    tools: HashMap<String, String>,
}

impl TurnProjection {
    /// A projection with nothing buffered.
    pub fn new(options: TurnProjectionOptions) -> TurnProjection {
        TurnProjection {
            options,
            answer: String::new(),
            turn_id: None,
            tools: HashMap::new(),
        }
    }

    /// The text accumulated for the running turn. Exposed for assertions.
    pub fn answer(&self) -> &str {
        &self.answer
    }

    /// What this event should say on a chat transport. Usually nothing.
    ///
    /// Exhaustive over [`ServerMessage`] on purpose: a protocol event added
    /// without a decision here is a compile error rather than an event a
    /// channel silently never sees.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per protocol event is the point: the match is the decision table"
    )]
    pub fn project(&mut self, message: &ServerMessage) -> Vec<OutboundDraft> {
        match message {
            ServerMessage::TurnStart(event) => {
                self.answer.clear();
                self.turn_id = Some(event.event.turn_id.clone());
                self.tools.clear();
                Vec::new()
            }

            ServerMessage::AssistantDelta(event) => {
                self.answer.push_str(&event.event.text);
                Vec::new()
            }

            ServerMessage::ToolCall(event) => {
                let call = &event.event;
                self.tools.insert(call.call_id.clone(), call.name.clone());
                let mut drafts = Vec::new();
                if self.options.send_tool_hints {
                    drafts
                        .push(self.draft(OutboundKind::Notice, format!("Running {}…", call.name)));
                }
                // At a tool boundary rather than per delta: the bus queue is
                // bounded, and a message per token would fill it with a copy of
                // the answer for every token in it.
                if self.options.send_progress && !self.answer.is_empty() {
                    drafts.push(self.draft(OutboundKind::Progress, self.answer.clone()));
                }
                drafts
            }

            ServerMessage::ToolResult(event) => {
                let result = &event.event;
                if !self.options.send_tool_hints || result.ok {
                    return Vec::new();
                }
                let name = self
                    .tools
                    .get(&result.call_id)
                    .map_or("a tool", String::as_str)
                    .to_owned();
                vec![self.draft(OutboundKind::Notice, format!("{name} failed."))]
            }

            ServerMessage::ToolApprovalRequest(event) => {
                // Ungated, unlike the hints. Whether this can be answered where
                // it lands depends on the transport, and either way the turn is
                // now stopped until somebody says yes — saying so is the
                // difference between a wait and a mystery.
                //
                // The text names no particular place to answer. "In the web
                // UI" would be shown by every channel that ignores the detail
                // below, and is true only while nothing else can answer.
                let request = &event.event;
                let approval = ApprovalDraftDetail {
                    call_id: request.call_id.clone(),
                    name: request.name.clone(),
                    risk: request.risk,
                    expires_at_ms: request.expires_at_ms,
                };
                let mut metadata = Map::new();
                metadata.insert(
                    APPROVAL_METADATA_KEY.to_owned(),
                    serde_json::to_value(&approval).unwrap_or(Value::Null),
                );
                vec![OutboundDraft {
                    kind: OutboundKind::Notice,
                    text: format!(
                        "{} needs approval before it can run. \
                         It is denied automatically if nobody answers.",
                        request.name
                    ),
                    turn_id: self.turn_of(Some(&request.turn_id)),
                    metadata,
                }]
            }

            ServerMessage::Notice(event) => {
                vec![self.draft(OutboundKind::Notice, event.event.message.clone())]
            }

            // A subagent's turn, reduced to its two ends.
            //
            // The nested stream is deliberately *not* projected. A chat
            // transport has one channel for everything, so forwarding a
            // subagent's deltas would interleave its working-out with the
            // answer the caller is composing — and the answer is a single
            // accumulator, so they would literally be concatenated. Saying
            // nothing is no better: a delegation can run for a minute, and a
            // silent minute reads as a hung bot.
            //
            // So: one line when it starts, one when it ends, and nothing in
            // between. Both are gated on `send_tool_hints`, because that is
            // already the flag for "tell me what the agent is doing, not only
            // what it concluded".
            ServerMessage::Subagent(event) => {
                if !self.options.send_tool_hints {
                    return Vec::new();
                }
                let body = &event.event;
                let who = if body.label.is_empty() {
                    body.agent_id.clone()
                } else {
                    body.label.clone()
                };
                match body.event {
                    NestedAgentEvent::TurnStart(_) => {
                        vec![self.draft(OutboundKind::Notice, format!("Asking {who}…"))]
                    }
                    NestedAgentEvent::TurnEnd(_) => {
                        vec![self.draft(OutboundKind::Notice, format!("{who} finished."))]
                    }
                    _ => Vec::new(),
                }
            }

            ServerMessage::MessageQueued(event) => {
                let depth = event.event.queue_depth;
                let plural = if depth == 1 { "" } else { "s" };
                vec![self.draft(
                    OutboundKind::Notice,
                    format!("Queued behind {depth} message{plural}."),
                )]
            }

            ServerMessage::Error(event) => vec![OutboundDraft {
                kind: OutboundKind::Error,
                text: event.message.clone(),
                turn_id: self.turn_of(event.turn_id.as_deref()),
                metadata: Map::new(),
            }],

            ServerMessage::TurnEnd(event) => {
                let end = &event.event;
                let answer = std::mem::take(&mut self.answer);
                let mut drafts = Vec::new();
                if !answer.is_empty() {
                    drafts.push(OutboundDraft {
                        kind: OutboundKind::Reply,
                        text: answer.clone(),
                        turn_id: Some(end.turn_id.clone()),
                        metadata: Map::new(),
                    });
                }
                if let Some(notice) = stop_notice(end.stop_reason, !answer.is_empty()) {
                    drafts.push(OutboundDraft {
                        kind: OutboundKind::Notice,
                        text: notice.to_owned(),
                        turn_id: Some(end.turn_id.clone()),
                        metadata: Map::new(),
                    });
                }
                self.turn_id = None;
                self.tools.clear();
                drafts
            }

            // Everything a chat transport has no way to render, and nothing it
            // loses by not seeing: the reasoning stream, the connection
            // handshake, and the bookkeeping a browser uses to reconcile its
            // own optimistic state.
            //
            // `session.truncated` belongs here for a reason worth stating: a
            // regenerate or edit driven from another client rewrites a
            // transcript, and a chat transport has none — the messages it
            // already delivered are in someone's message history, where nothing
            // can recall them. The retry simply arrives as another answer.
            // `context.usage` joins them: it exists to move a bar, and a chat
            // app has no bar. Posting the figure as a line instead would be a
            // token count arriving in someone's chat between two answers,
            // unasked.
            ServerMessage::ReasoningDelta(_)
            | ServerMessage::ToolProgress(_)
            | ServerMessage::ContextUsage(_)
            | ServerMessage::Connected(_)
            | ServerMessage::Pong(_)
            | ServerMessage::MessageAck(_)
            | ServerMessage::SessionStatus(_)
            | ServerMessage::SessionReset(_)
            | ServerMessage::SessionReplay(_)
            | ServerMessage::SessionTruncated(_)
            | ServerMessage::Notification(_)
            | ServerMessage::ToolsChanged(_)
            | ServerMessage::Steer(_) => Vec::new(),
        }
    }

    fn draft(&self, kind: OutboundKind, text: String) -> OutboundDraft {
        OutboundDraft {
            kind,
            text,
            turn_id: self.turn_id.clone(),
            metadata: Map::new(),
        }
    }

    /// The turn id from the event when it carries one, else the running turn's.
    fn turn_of(&self, turn_id: Option<&str>) -> Option<String> {
        turn_id.map(str::to_owned).or_else(|| self.turn_id.clone())
    }
}
