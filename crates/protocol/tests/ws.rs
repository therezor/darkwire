//! Frames the fixtures cannot express: the ones that must be refused.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_protocol::{
    ClientMessage, PROTOCOL_VERSION, ProtocolVersion, ServerMessage, UNSEQUENCED_SERVER_EVENTS,
};
use serde_json::json;

#[test]
fn the_version_literal_is_pinned() {
    assert_eq!(PROTOCOL_VERSION, 2);
    assert!(serde_json::from_value::<ProtocolVersion>(json!(2)).is_ok());
    assert!(serde_json::from_value::<ProtocolVersion>(json!(1)).is_err());
    assert_eq!(serde_json::to_value(ProtocolVersion).unwrap(), json!(2));
    let base = json!({"type": "connected", "sessionKey": "s", "serverTimeMs": 0, "lastSeq": 0});
    let mut ok = base.clone();
    ok["protocolVersion"] = json!(2);
    assert!(serde_json::from_value::<ServerMessage>(ok).is_ok());
    let mut old = base;
    old["protocolVersion"] = json!(1);
    assert!(serde_json::from_value::<ServerMessage>(old).is_err());
}

#[test]
fn an_unknown_client_type_is_named_in_the_error() {
    let error = serde_json::from_value::<ClientMessage>(json!({"type": "nope"})).unwrap_err();
    assert!(error.to_string().contains("nope"), "{error}");
    assert!(serde_json::from_value::<ClientMessage>(json!({"type": "turn.stop"})).is_err());
}

#[test]
fn defaults_fill_what_the_browser_omits() {
    let parsed: ClientMessage = serde_json::from_value(
        json!({"type": "user.message", "sessionKey": "web:abc", "content": "hello"}),
    )
    .unwrap();
    let ClientMessage::UserMessage(message) = &parsed else {
        panic!("wrong variant")
    };
    assert!(message.attachments.is_empty());
    assert_eq!(parsed.tag(), "user.message");

    let parsed: ClientMessage =
        serde_json::from_value(json!({"type": "tool.approve", "callId": "c1", "approved": true}))
            .unwrap();
    let ClientMessage::ToolApprove(approve) = parsed else {
        panic!("wrong variant")
    };
    assert_eq!(approve.scope, ghostai_protocol::ApprovalScope::Once);
}

#[test]
fn a_negative_sequence_number_is_refused() {
    let frame = json!({"type": "assistant.delta", "seq": -1, "turnId": "t", "text": ""});
    assert!(serde_json::from_value::<ServerMessage>(frame).is_err());
}

#[test]
fn the_unsequenced_set_is_exactly_the_connection_level_events() {
    assert_eq!(UNSEQUENCED_SERVER_EVENTS, &["connected", "pong", "error"]);
    for value in ServerMessage::VALUES {
        assert!(!value.is_empty());
    }
    assert_eq!(ServerMessage::VALUES.len(), 23);
    assert_eq!(ClientMessage::VALUES.len(), 10);
}
