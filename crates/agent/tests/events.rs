//! The property this crate exists to hold at its edge: **an `AgentEvent` plus
//! a `seq` is a `ServerMessage`**.
//!
//! There is no mapping table and no per-event translation function, so the only
//! way the two shapes can drift is if one of them stops parsing the other. The
//! samples are `fixtures/ws/frames/*.json`, which the TypeScript suite writes
//! from the schemas the browser actually parses — so this asserts agreement
//! across the language boundary rather than with itself.
//!
//! Two halves, and both matter:
//!
//!  - every frame whose type an [`AgentEvent`] can carry round-trips through
//!    one, stripped of its `seq` and stamped again;
//!  - every variant of the union is covered by one of those frames, so an event
//!    added without a fixture fails here rather than reaching a component three
//!    renders later.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use common::canonical_numbers;
use darkwire_agent::events::{AgentEvent, Stamped};
use darkwire_protocol::{
    AssistantDelta, ContextUsage, ErrorCode, ErrorEvent, NestedAgentEvent, NoticeKind,
    ServerMessage, StopReason, ToolRisk, TurnEnd,
};
use serde_json::Value;

fn frames_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ws/frames")
}

/// Every `{"direction": "server", "frame": …}` fixture, by file name.
fn server_frames() -> Vec<(String, Value)> {
    let mut files: Vec<PathBuf> = fs::read_dir(frames_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();

    files
        .into_iter()
        .filter_map(|path| {
            let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            if value.get("direction").and_then(Value::as_str) != Some("server") {
                return None;
            }
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            Some((name, value.get("frame").unwrap().clone()))
        })
        .collect()
}

/// The discriminators an [`AgentEvent`] can carry.
///
/// Spelled out rather than derived, so adding a variant without a fixture is a
/// failure here rather than a silent gap: the completeness assertion below
/// compares this list against what the fixtures actually exercised.
const AGENT_EVENT_TYPES: &[&str] = &[
    "turn.start",
    "assistant.delta",
    "reasoning.delta",
    "tool.call",
    "tool.progress",
    "tool.result",
    "tool.approvalRequest",
    "notice",
    "turn.end",
    "error",
    "subagent.event",
    "context.usage",
];

#[test]
fn every_frame_an_event_can_carry_round_trips_through_one() {
    let mut covered: BTreeSet<String> = BTreeSet::new();

    for (name, frame) in server_frames() {
        let tag = frame
            .get("type")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        if !AGENT_EVENT_TYPES.contains(&tag.as_str()) {
            // A connection-level or session-level frame the loop never emits.
            // It is still a `ServerMessage`, and the protocol crate holds it.
            assert!(
                serde_json::from_value::<ServerMessage>(frame).is_ok(),
                "{name}: not a ServerMessage"
            );
            continue;
        }

        // The transport owns the `seq`, so strip it and hand it back.
        let mut body = frame.clone();
        let seq = body
            .as_object_mut()
            .unwrap()
            .remove("seq")
            .and_then(|value| value.as_u64());

        let event: AgentEvent = serde_json::from_value(body)
            .unwrap_or_else(|error| panic!("{name}: not an AgentEvent: {error}"));
        assert_eq!(event.tag(), tag, "{name}");

        // `error` is the one member with no sequence number of its own: a
        // failure is not part of a session's replayable history, so the
        // protocol's own frame carries none and the conversion drops what it is
        // given.
        assert_eq!(seq.is_none(), tag == "error", "{name}");
        let message: ServerMessage = event.sequenced(seq.unwrap_or(7));
        assert_eq!(message.seq(), seq, "{name}");

        let encoded = canonical_numbers(serde_json::to_value(&message).unwrap());
        assert_eq!(encoded, canonical_numbers(frame), "{name}");

        covered.insert(tag);
    }

    let expected: BTreeSet<String> = AGENT_EVENT_TYPES.iter().map(|t| (*t).to_owned()).collect();
    assert_eq!(
        covered, expected,
        "every AgentEvent variant needs a frame fixture"
    );
}

#[test]
fn the_pair_conversion_and_the_method_are_the_same_step() {
    let event: AgentEvent = AssistantDelta {
        tag: darkwire_protocol::AssistantDeltaTag,
        turn_id: "t1".to_owned(),
        text: "hi".to_owned(),
    }
    .into();

    let by_method = event.clone().sequenced(9);
    let by_from = ServerMessage::from(Stamped::from((event, 9)));

    assert_eq!(by_method, by_from);
    assert_eq!(by_method.seq(), Some(9));
}

#[test]
fn a_subagent_cannot_wrap_a_context_report() {
    // The exclusion is the design: a subagent runs in a session of its own, so
    // its context report describes a conversation nobody is reading. Leaving it
    // out of `NestedAgentEvent` means a child that emitted one cannot be
    // wrapped — which is what this asserts at the type level, by showing the
    // nested union refuses the frame the outer one accepts.
    let frame = serde_json::json!({
        "type": "context.usage",
        "sessionKey": "web:1",
        "estimatedTokens": 10,
        "contextWindowTokens": 100,
        "breakdown": {},
    });

    assert!(serde_json::from_value::<AgentEvent>(frame.clone()).is_ok());
    assert!(serde_json::from_value::<NestedAgentEvent>(frame).is_err());
}

#[test]
fn an_event_with_no_type_is_refused_by_name() {
    let error = serde_json::from_value::<AgentEvent>(serde_json::json!({"text": "x"})).unwrap_err();
    assert!(error.to_string().contains("type"), "{error}");

    let error = serde_json::from_value::<AgentEvent>(serde_json::json!({"type": "session.reset"}))
        .unwrap_err();
    assert!(error.to_string().contains("session.reset"), "{error}");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one literal per union member is the point"
)]
fn every_body_converts_into_an_event_without_naming_the_union() {
    // The `From` impls exist so a call site names the event and nothing else.
    // One per body, because a missing one is a call site that has to spell out
    // two layers of enum.
    let events: Vec<AgentEvent> = vec![
        darkwire_protocol::TurnStart {
            tag: darkwire_protocol::TurnStartTag,
            session_key: "s".to_owned(),
            turn_id: "t".to_owned(),
            first_seq: None,
            agent_id: "default".to_owned(),
            model: "m".to_owned(),
            provider: "p".to_owned(),
        }
        .into(),
        AssistantDelta {
            tag: darkwire_protocol::AssistantDeltaTag,
            turn_id: "t".to_owned(),
            text: "a".to_owned(),
        }
        .into(),
        darkwire_protocol::ReasoningDelta {
            tag: darkwire_protocol::ReasoningDeltaTag,
            turn_id: "t".to_owned(),
            text: "r".to_owned(),
        }
        .into(),
        darkwire_protocol::ToolCallStarted {
            tag: darkwire_protocol::ToolCallTag,
            turn_id: "t".to_owned(),
            call_id: "c".to_owned(),
            name: "read".to_owned(),
            args: Value::Null,
            risk: ToolRisk::Safe,
        }
        .into(),
        darkwire_protocol::ToolProgress {
            tag: darkwire_protocol::ToolProgressTag,
            turn_id: "t".to_owned(),
            call_id: "c".to_owned(),
            elapsed_ms: 1,
            message: None,
        }
        .into(),
        darkwire_protocol::ToolResult {
            tag: darkwire_protocol::ToolResultTag,
            turn_id: "t".to_owned(),
            call_id: "c".to_owned(),
            ok: true,
            content: String::new(),
            truncated: false,
            duration_ms: 0,
        }
        .into(),
        darkwire_protocol::ToolApprovalRequest {
            tag: darkwire_protocol::ToolApprovalRequestTag,
            turn_id: "t".to_owned(),
            call_id: "c".to_owned(),
            name: "exec".to_owned(),
            args: Value::Null,
            risk: ToolRisk::Exec,
            expires_at_ms: 1,
            command: None,
        }
        .into(),
        darkwire_protocol::Notice {
            tag: darkwire_protocol::NoticeTag,
            kind: NoticeKind::Degraded,
            message: "m".to_owned(),
            turn_id: None,
            call_id: None,
        }
        .into(),
        TurnEnd {
            tag: darkwire_protocol::TurnEndTag,
            turn_id: "t".to_owned(),
            stop_reason: StopReason::Complete,
            usage: None,
            iterations: 1,
            elapsed_ms: None,
            generation_ms: None,
            generation_tokens: None,
            first_token_ms: None,
            first_seq: None,
            last_seq: None,
        }
        .into(),
        ErrorEvent {
            tag: darkwire_protocol::ErrorTag,
            code: ErrorCode::Internal,
            message: "m".to_owned(),
            retryable: false,
            turn_id: None,
            call_id: None,
        }
        .into(),
        darkwire_protocol::SubagentEventBody {
            tag: darkwire_protocol::SubagentEventTag,
            turn_id: "t".to_owned(),
            parent_session_key: "s".to_owned(),
            parent_call_id: "c".to_owned(),
            agent_id: "researcher".to_owned(),
            label: "Researcher".to_owned(),
            session_key: "sub".to_owned(),
            depth: 1,
            event: NestedAgentEvent::AssistantDelta(AssistantDelta {
                tag: darkwire_protocol::AssistantDeltaTag,
                turn_id: "t2".to_owned(),
                text: "x".to_owned(),
            }),
        }
        .into(),
        ContextUsage {
            tag: darkwire_protocol::ContextUsageTag,
            session_key: "s".to_owned(),
            estimated_tokens: 1,
            context_window_tokens: 2,
            breakdown: indexmap::IndexMap::new(),
        }
        .into(),
    ];

    let tags: Vec<&str> = events.iter().map(AgentEvent::tag).collect();
    assert_eq!(tags, AGENT_EVENT_TYPES);

    // And each survives the round trip through its own serialised form.
    for event in events {
        let encoded = serde_json::to_value(&event).unwrap();
        let decoded: AgentEvent = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, event);
    }
}
