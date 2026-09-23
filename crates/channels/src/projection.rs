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

use std::collections::{HashMap, HashSet};

use darkwire_core::message_bus::OutboundKind;
use darkwire_protocol::{
    CommandPolicy, NestedAgentEvent, Notice, NoticeKind, ServerMessage, StopReason,
    ToolApprovalRequest, ToolRisk,
};
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
/// wrote to the one person whose judgement is the last check on it. The
/// command is the exception, because nobody can judge `exec` without it. A
/// channel caps it before showing it.
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
    /// What the rules made of an `exec` call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandPolicy>,
}

/// The key [`ApprovalDraftDetail`] travels under on a draft's metadata.
pub const APPROVAL_METADATA_KEY: &str = "approval";

/// The key naming the approval a draft settles: `{ "callId": … }`.
///
/// On the denial notice, and on an `update` once the call has moved on
/// without one. A channel that posted a card edits it; one that did not
/// renders the notice as before and never accepts the update.
pub const APPROVAL_SETTLED_METADATA_KEY: &str = "approvalSettled";

/// The key naming the approval an error answers: `{ "callId": … }`.
///
/// The hub refused what was sent for that call, a rule most often, and the
/// prompt is still open.
pub const APPROVAL_ERROR_METADATA_KEY: &str = "approvalError";

/// What an approval card says once the call went ahead without a denial.
const NO_LONGER_WAITING: &str = "No longer waiting for an answer.";

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
    /// Approval requests announced and not yet settled, a subagent's included.
    open_approvals: HashSet<String>,
}

impl TurnProjection {
    /// A projection with nothing buffered.
    pub fn new(options: TurnProjectionOptions) -> TurnProjection {
        TurnProjection {
            options,
            answer: String::new(),
            turn_id: None,
            tools: HashMap::new(),
            open_approvals: HashSet::new(),
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
                self.open_approvals.clear();
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

            ServerMessage::ToolProgress(event) => self.moved_on(&event.event.call_id),

            ServerMessage::ToolResult(event) => {
                let result = &event.event;
                let settled = self.moved_on(&result.call_id);
                if !settled.is_empty() || !self.options.send_tool_hints || result.ok {
                    return settled;
                }
                let name = self
                    .tools
                    .get(&result.call_id)
                    .map_or("a tool", String::as_str)
                    .to_owned();
                vec![self.draft(OutboundKind::Notice, format!("{name} failed."))]
            }

            ServerMessage::ToolApprovalRequest(event) => vec![self.approval(&event.event)],

            ServerMessage::Notice(event) => vec![self.notice(&event.event)],

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
            //
            // An approval is the exception, and ungated for the same reason as
            // the parent's own: the subagent is stopped until somebody answers,
            // and a prompt nobody sees waits out its whole deadline.
            ServerMessage::Subagent(event) => {
                let body = &event.event;
                match &body.event {
                    NestedAgentEvent::ToolApprovalRequest(request) => {
                        return vec![self.approval(request)];
                    }
                    NestedAgentEvent::Notice(notice) if self.settles(notice).is_some() => {
                        return vec![self.notice(notice)];
                    }
                    NestedAgentEvent::ToolProgress(progress) => {
                        return self.moved_on(&progress.call_id);
                    }
                    NestedAgentEvent::ToolResult(result) => {
                        return self.moved_on(&result.call_id);
                    }
                    _ => {}
                }
                if !self.options.send_tool_hints {
                    return Vec::new();
                }
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
                metadata: event
                    .call_id
                    .as_deref()
                    .map(|call_id| naming(APPROVAL_ERROR_METADATA_KEY, call_id))
                    .unwrap_or_default(),
            }],

            ServerMessage::TurnEnd(event) => {
                let end = &event.event;
                let answer = std::mem::take(&mut self.answer);
                let mut open: Vec<String> = self.open_approvals.drain().collect();
                open.sort();
                let mut drafts: Vec<OutboundDraft> = open
                    .iter()
                    .map(|call_id| OutboundDraft {
                        kind: OutboundKind::Update,
                        text: "The turn ended before this was answered.".to_owned(),
                        turn_id: Some(end.turn_id.clone()),
                        metadata: naming(APPROVAL_SETTLED_METADATA_KEY, call_id),
                    })
                    .collect();
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

    /// The notice an approval request becomes, with the detail a channel
    /// needs to render it as a card.
    ///
    /// The text names no particular place to answer. "In the web UI" would be
    /// shown by every channel that ignores the detail, and is true only while
    /// nothing else can answer.
    fn approval(&mut self, request: &ToolApprovalRequest) -> OutboundDraft {
        self.open_approvals.insert(request.call_id.clone());
        let approval = ApprovalDraftDetail {
            call_id: request.call_id.clone(),
            name: request.name.clone(),
            risk: request.risk,
            expires_at_ms: request.expires_at_ms,
            command: request.command.clone(),
        };
        let mut metadata = Map::new();
        metadata.insert(
            APPROVAL_METADATA_KEY.to_owned(),
            serde_json::to_value(&approval).unwrap_or(Value::Null),
        );
        OutboundDraft {
            kind: OutboundKind::Notice,
            text: format!(
                "{} needs approval before it can run. \
                 It is denied automatically if nobody answers.",
                request.name
            ),
            turn_id: self.turn_of(Some(&request.turn_id)),
            metadata,
        }
    }

    /// A notice, marked as settling the approval it denies if it does.
    fn notice(&mut self, notice: &Notice) -> OutboundDraft {
        let mut draft = self.draft(OutboundKind::Notice, notice.message.clone());
        if let Some(call_id) = self.settles(notice).map(str::to_owned) {
            self.open_approvals.remove(&call_id);
            draft.metadata = naming(APPROVAL_SETTLED_METADATA_KEY, &call_id);
        }
        draft
    }

    /// The open approval a denial notice answers, if it answers one.
    fn settles<'a>(&self, notice: &'a Notice) -> Option<&'a str> {
        let call_id = notice.call_id.as_deref()?;
        (notice.kind == NoticeKind::ApprovalDenied && self.open_approvals.contains(call_id))
            .then_some(call_id)
    }

    /// Settles an open approval whose call went ahead: it ran, or the turn
    /// stopped under it.
    fn moved_on(&mut self, call_id: &str) -> Vec<OutboundDraft> {
        if !self.open_approvals.remove(call_id) {
            return Vec::new();
        }
        let mut draft = self.draft(OutboundKind::Update, NO_LONGER_WAITING.to_owned());
        draft.metadata = naming(APPROVAL_SETTLED_METADATA_KEY, call_id);
        vec![draft]
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

/// `{ key: { "callId": call_id } }`, the shape both approval markers take.
fn naming(key: &str, call_id: &str) -> Map<String, Value> {
    let mut detail = Map::new();
    detail.insert("callId".to_owned(), Value::String(call_id.to_owned()));
    let mut metadata = Map::new();
    metadata.insert(key.to_owned(), Value::Object(detail));
    metadata
}
