//! Pricing a request on the body rather than the record.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use darkwire_core::messages::{FileDetails, file_part, text_part};
use darkwire_protocol::{ToolAnnotations, ToolDefinition, ToolRisk, ToolSource};
use darkwire_providers::{estimate_message_tokens, estimate_tokens, estimate_tool_tokens};
use serde_json::json;

fn tool() -> ToolDefinition {
    // A bare definition encodes to about the length it stores at; what a real
    // tool carries beyond `risk` and `source` is where the two figures
    // separate.
    ToolDefinition {
        annotations: Some(ToolAnnotations {
            title: Some("Run a shell command in the workspace".into()),
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            open_world_hint: None,
        }),
        risk: ToolRisk::Exec,
        source: ToolSource::Mcp,
        ..common::tool_definition(
            "exec",
            "Runs a command.",
            json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
        )
    }
}

#[test]
fn ignores_the_reasoning_the_wire_never_carries() {
    let reasoning = "x".repeat(4000);
    let thought = common::assistant_with("hi", vec![], Some(&reasoning));
    assert_eq!(
        estimate_message_tokens(&thought),
        estimate_message_tokens(&common::assistant("hi"))
    );
    // The old measurement, of the stored record, is a thousand tokens larger.
    assert!(
        estimate_message_tokens(&thought)
            < estimate_tokens(&serde_json::to_string(&thought).unwrap())
    );
}

#[test]
fn counts_a_tool_call() {
    let called = common::assistant_with(
        "",
        vec![common::call("a", "exec", "{\"cmd\":\"ls\"}")],
        None,
    );
    assert!(estimate_message_tokens(&called) > estimate_message_tokens(&common::assistant("")));
}

#[test]
fn measures_the_collapsed_string_for_text_only_content() {
    let parts = common::user_parts(vec![text_part("first"), text_part("second")]);
    assert_eq!(
        estimate_message_tokens(&parts),
        estimate_tokens(&json!({"role": "user", "content": "first\nsecond"}).to_string())
    );
}

#[test]
fn measures_a_file_part_as_the_reference_and_a_tool_result_whole() {
    let attached = common::user_parts(vec![file_part(
        "notes.md",
        "text/plain",
        FileDetails::default(),
    )]);
    let tokens = estimate_message_tokens(&attached);
    assert!(tokens > 0 && tokens < 50, "{tokens}");
    assert!(estimate_message_tokens(&common::tool("a", "exec", &"y".repeat(400))) > 100);
}

#[test]
fn bills_the_three_fields_a_body_carries_and_prices_the_array() {
    let definition = tool();
    assert_eq!(
        estimate_tool_tokens(std::slice::from_ref(&definition)),
        estimate_tokens(
            &json!([{
                "type": "function",
                "function": {
                    "name": definition.name,
                    "description": definition.description,
                    "parameters": definition.parameters,
                }
            }])
            .to_string()
        )
    );
    assert!(
        estimate_tool_tokens(std::slice::from_ref(&definition))
            < estimate_tokens(&serde_json::to_string(std::slice::from_ref(&definition)).unwrap())
    );
    assert!(
        estimate_tool_tokens(&[definition.clone(), definition.clone()])
            > estimate_tool_tokens(&[definition])
    );
}
