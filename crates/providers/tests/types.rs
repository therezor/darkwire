//! The request and result shapes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use ghostai_providers::{
    ChatRequest, ChatStreamEvent, FinishReason, ToolChoice, WireAdapterOptions, empty_usage,
};
use serde_json::json;

#[test]
fn empty_usage_is_zeroed() {
    let usage = empty_usage();
    assert_eq!(usage.prompt_tokens, 0);
    assert_eq!(usage.completion_tokens, 0);
    assert_eq!(usage.total_tokens, 0);
    assert_eq!(usage.cached_tokens, None);
}

#[test]
fn enums_spell_themselves_the_way_the_wire_does() {
    assert_eq!(ToolChoice::Auto.as_str(), "auto");
    assert_eq!(ToolChoice::None.as_str(), "none");
    assert_eq!(ToolChoice::Required.as_str(), "required");
    assert_eq!(
        serde_json::to_value(FinishReason::ToolCalls).unwrap(),
        json!("tool_calls")
    );
    assert_eq!(
        serde_json::to_value(FinishReason::ContentFilter).unwrap(),
        json!("content_filter")
    );
    let request: ChatRequest = serde_json::from_value(json!({
        "model": "m",
        "messages": [],
        "toolChoice": "required",
        "reasoningEffort": "xhigh",
    }))
    .unwrap();
    assert_eq!(request.tool_choice, Some(ToolChoice::Required));
    assert!(request.tools.is_empty());
}

#[test]
fn adapter_options_redact_the_key_in_debug_output() {
    let options = WireAdapterOptions {
        api_key: Some("sk-secret".into()),
        ..WireAdapterOptions::new(common::spec_of("openai"))
    };
    let debug = format!("{options:?}");
    assert!(debug.contains("redacted"));
    assert!(!debug.contains("sk-secret"));
    assert!(matches!(
        ChatStreamEvent::Text("a".into()),
        ChatStreamEvent::Text(_)
    ));
}
