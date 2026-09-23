//! Event constructors shared by the projection and manager suites.
//!
//! The protocol types are tagged structs with a literal `type` field, so a test
//! that built one inline would be six lines of boilerplate per event. Each
//! suite uses a different subset of these.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "each suite uses a subset of these helpers, and a fixture that cannot be built is a failing test either way"
)]

use darkwire_protocol::{
    AssistantDelta, AssistantDeltaTag, CommandPolicy, ContextUsage, ContextUsageTag, ErrorCode,
    ErrorEvent, ErrorTag, MessageQueued, MessageQueuedTag, NestedAgentEvent, Notice, NoticeKind,
    NoticeTag, ReasoningDelta, ReasoningDeltaTag, Sequenced, ServerMessage, SessionStatus,
    SessionStatusTag, StopReason, SubagentEventBody, SubagentEventTag, ToolApprovalRequest,
    ToolApprovalRequestTag, ToolCallStarted, ToolCallTag, ToolResult, ToolResultTag, ToolRisk,
    TurnEnd, TurnEndTag, TurnStart, TurnStartTag,
};
use indexmap::IndexMap;
use serde_json::{Value, json};

/// The turn every fixture below belongs to.
pub const TURN: &str = "turn-1";
/// The conversation every fixture below belongs to.
pub const SESSION: &str = "loopback:default";

/// Wraps an event with the `seq` the transport owns.
///
/// A macro rather than a function: `Sequenced` bounds its parameter on the
/// schema and validation traits, and naming those here would mean this crate's
/// tests depending on two crates nothing else in them touches.
macro_rules! sequenced {
    ($event:expr) => {
        Sequenced {
            seq: 0,
            event: $event,
        }
    };
}

pub fn turn_start() -> ServerMessage {
    ServerMessage::TurnStart(sequenced!(TurnStart {
        tag: TurnStartTag,
        session_key: SESSION.to_owned(),
        turn_id: TURN.to_owned(),
        first_seq: None,
        agent_id: "default".to_owned(),
        model: "scripted".to_owned(),
        provider: "scripted".to_owned(),
    }))
}

pub fn delta(text: &str) -> ServerMessage {
    ServerMessage::AssistantDelta(sequenced!(AssistantDelta {
        tag: AssistantDeltaTag,
        turn_id: TURN.to_owned(),
        text: text.to_owned(),
    }))
}

pub fn reasoning(text: &str) -> ServerMessage {
    ServerMessage::ReasoningDelta(sequenced!(ReasoningDelta {
        tag: ReasoningDeltaTag,
        turn_id: TURN.to_owned(),
        text: text.to_owned(),
    }))
}

pub fn tool_call(call_id: &str, name: &str) -> ServerMessage {
    ServerMessage::ToolCall(sequenced!(ToolCallStarted {
        tag: ToolCallTag,
        turn_id: TURN.to_owned(),
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        args: json!({"path": "a.txt"}),
        risk: ToolRisk::Safe,
    }))
}

pub fn tool_result(call_id: &str, ok: bool) -> ServerMessage {
    ServerMessage::ToolResult(sequenced!(ToolResult {
        tag: ToolResultTag,
        turn_id: TURN.to_owned(),
        call_id: call_id.to_owned(),
        ok,
        content: "done".to_owned(),
        truncated: false,
        duration_ms: 5,
    }))
}

pub fn approval_request(call_id: &str, name: &str, expires_at_ms: u64) -> ServerMessage {
    ServerMessage::ToolApprovalRequest(sequenced!(ToolApprovalRequest {
        tag: ToolApprovalRequestTag,
        turn_id: TURN.to_owned(),
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        // Model-authored and unbounded: the projection must not carry it.
        args: json!({"command": "rm -rf /", "secret": "hunter2"}),
        risk: ToolRisk::Exec,
        expires_at_ms,
        command: None,
    }))
}

/// An `exec` approval carrying what the rules made of the command.
pub fn exec_approval_request(call_id: &str, argv: &[&str]) -> ServerMessage {
    ServerMessage::ToolApprovalRequest(sequenced!(exec_request(call_id, argv)))
}

fn exec_request(call_id: &str, argv: &[&str]) -> ToolApprovalRequest {
    let argv: Vec<String> = argv.iter().map(|&token| token.to_owned()).collect();
    ToolApprovalRequest {
        tag: ToolApprovalRequestTag,
        turn_id: TURN.to_owned(),
        call_id: call_id.to_owned(),
        name: "exec".to_owned(),
        args: json!({ "argv": argv }),
        risk: ToolRisk::Exec,
        expires_at_ms: 4_000_000_000_000,
        command: Some(CommandPolicy {
            shell: argv.first().is_some_and(|program| program == "sh"),
            argv,
            rule: None,
        }),
    }
}

/// The denial notice the loop emits for `call_id`.
pub fn denied(call_id: &str, message: &str) -> ServerMessage {
    ServerMessage::Notice(sequenced!(denied_notice(call_id, message)))
}

fn denied_notice(call_id: &str, message: &str) -> Notice {
    Notice {
        tag: NoticeTag,
        kind: NoticeKind::ApprovalDenied,
        message: message.to_owned(),
        turn_id: Some(TURN.to_owned()),
        call_id: Some(call_id.to_owned()),
    }
}

/// The hub refusing what a `tool.approve` for `call_id` carried.
pub fn approval_error(call_id: &str, message: &str) -> ServerMessage {
    ServerMessage::Error(ErrorEvent {
        tag: ErrorTag,
        code: ErrorCode::BadRequest,
        message: message.to_owned(),
        retryable: false,
        turn_id: None,
        call_id: Some(call_id.to_owned()),
    })
}

/// A subagent's own approval request.
pub fn nested_approval_request(call_id: &str, argv: &[&str]) -> NestedAgentEvent {
    NestedAgentEvent::ToolApprovalRequest(exec_request(call_id, argv))
}

/// A subagent's denial notice.
pub fn nested_denied(call_id: &str, message: &str) -> NestedAgentEvent {
    NestedAgentEvent::Notice(denied_notice(call_id, message))
}

/// A subagent's tool answering.
pub fn nested_tool_result(call_id: &str) -> NestedAgentEvent {
    NestedAgentEvent::ToolResult(ToolResult {
        tag: ToolResultTag,
        turn_id: TURN.to_owned(),
        call_id: call_id.to_owned(),
        ok: true,
        content: "done".to_owned(),
        truncated: false,
        duration_ms: 5,
    })
}

pub fn notice(message: &str) -> ServerMessage {
    ServerMessage::Notice(sequenced!(Notice {
        tag: NoticeTag,
        kind: NoticeKind::PromptInjection,
        message: message.to_owned(),
        turn_id: Some(TURN.to_owned()),
        call_id: None,
    }))
}

pub fn queued(depth: u64) -> ServerMessage {
    ServerMessage::MessageQueued(sequenced!(MessageQueued {
        tag: MessageQueuedTag,
        session_key: SESSION.to_owned(),
        queue_depth: depth,
    }))
}

pub fn error(message: &str, turn_id: Option<&str>) -> ServerMessage {
    ServerMessage::Error(ErrorEvent {
        tag: ErrorTag,
        code: ErrorCode::ProviderError,
        message: message.to_owned(),
        retryable: true,
        turn_id: turn_id.map(str::to_owned),
        call_id: None,
    })
}

pub fn turn_end(stop_reason: StopReason) -> ServerMessage {
    ServerMessage::TurnEnd(sequenced!(TurnEnd {
        tag: TurnEndTag,
        turn_id: TURN.to_owned(),
        stop_reason,
        usage: None,
        iterations: 1,
        elapsed_ms: None,
        generation_ms: None,
        generation_tokens: None,
        first_token_ms: None,
        first_seq: None,
        last_seq: None,
    }))
}

pub fn session_status(busy: bool) -> ServerMessage {
    ServerMessage::SessionStatus(sequenced!(SessionStatus {
        tag: SessionStatusTag,
        session_key: SESSION.to_owned(),
        busy,
        queue_depth: 0,
        workspace_id: "default".to_owned(),
        turn_id: busy.then(|| TURN.to_owned()),
    }))
}

pub fn context_usage() -> ServerMessage {
    ServerMessage::ContextUsage(sequenced!(ContextUsage {
        tag: ContextUsageTag,
        session_key: SESSION.to_owned(),
        estimated_tokens: 100,
        context_window_tokens: 1000,
        breakdown: IndexMap::new(),
    }))
}

/// A subagent's event, wrapped as the hub forwards one.
pub fn subagent(label: &str, inner: NestedAgentEvent) -> ServerMessage {
    ServerMessage::Subagent(sequenced!(SubagentEventBody {
        tag: SubagentEventTag,
        turn_id: TURN.to_owned(),
        parent_session_key: SESSION.to_owned(),
        parent_call_id: "call-1".to_owned(),
        agent_id: "researcher".to_owned(),
        label: label.to_owned(),
        session_key: "subagent:1".to_owned(),
        depth: 1,
        event: inner,
    }))
}

/// The inner `turn.start` a subagent emits.
pub fn nested_turn_start() -> NestedAgentEvent {
    NestedAgentEvent::TurnStart(TurnStart {
        tag: TurnStartTag,
        session_key: "subagent:1".to_owned(),
        turn_id: "inner-1".to_owned(),
        first_seq: None,
        agent_id: "researcher".to_owned(),
        model: "scripted".to_owned(),
        provider: "scripted".to_owned(),
    })
}

/// The inner `turn.end` a subagent emits.
pub fn nested_turn_end() -> NestedAgentEvent {
    NestedAgentEvent::TurnEnd(TurnEnd {
        tag: TurnEndTag,
        turn_id: "inner-1".to_owned(),
        stop_reason: StopReason::Complete,
        usage: None,
        iterations: 1,
        elapsed_ms: None,
        generation_ms: None,
        generation_tokens: None,
        first_token_ms: None,
        first_seq: None,
        last_seq: None,
    })
}

/// An inner event that is neither end of the subagent's turn.
pub fn nested_delta() -> NestedAgentEvent {
    NestedAgentEvent::AssistantDelta(AssistantDelta {
        tag: AssistantDeltaTag,
        turn_id: "inner-1".to_owned(),
        text: "thinking".to_owned(),
    })
}

/// The `approval` object off a draft's metadata.
pub fn approval_detail(metadata: &serde_json::Map<String, Value>) -> Option<&Value> {
    metadata.get("approval")
}
