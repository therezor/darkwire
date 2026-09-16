//! A bridged tool, put through the same suite every built-in holds.
//!
//! The conformance contract covers a tool "built-in, extension-supplied, or
//! proxied from an MCP server", and this is the third of those being checked.
//!
//! **The fixture is a well-formed descriptor, deliberately.** The suite asserts
//! that every property carries a description and that unknown keys are refused,
//! and a real server is free to advertise neither. Those cases are covered in
//! `bridge.rs` and `schema.rs`, where the answer is "a warning, and the tool
//! still works" — which is a different claim from the one made here.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_core::Result;
use darkwire_mcp::{
    BridgeOptions, McpCallOptions, McpCallResult, McpCallTarget, McpToolDescriptor, bridge_tool,
};
use darkwire_protocol::ToolAnnotations;
use darkwire_protocol::json::Object;
use darkwire_tools::testkit::{TestWorkspace, ToolConformance, tool_conformance};
use futures::future::BoxFuture;
use serde_json::{Value, json};

/// The server, as a function. Nothing is spawned and nothing is dialled: the
/// suite is about the tool's edges, not about a transport.
struct Repeater;

impl McpCallTarget for Repeater {
    fn call(
        &self,
        _: &str,
        args: Object,
        _: McpCallOptions,
    ) -> BoxFuture<'_, Result<McpCallResult>> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let times = args
            .get("times")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(1);
        Box::pin(async move { Ok(McpCallResult::text(text.repeat(times))) })
    }
}

/// One string, one required, one integer with a minimum.
///
/// Each earns its place: the string exercises the wrong-type refusal, the
/// integer exercises the `"10"` coercion models actually emit, and `required`
/// exercises the missing-argument case. `times` is what the large-output case
/// drives.
fn descriptor() -> McpToolDescriptor {
    McpToolDescriptor {
        name: "repeat".to_owned(),
        title: None,
        description: Some("Repeats a string, for as long as it is asked to.".to_owned()),
        input_schema: json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "The string to repeat." },
                "times": { "type": "integer", "minimum": 1, "description": "How many times to repeat it." }
            },
            "required": ["text"],
            "additionalProperties": false
        }),
        annotations: Some(ToolAnnotations {
            read_only_hint: Some(true),
            ..ToolAnnotations::default()
        }),
    }
}

fn args(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

#[tokio::test]
async fn a_bridged_tool_conforms() {
    let descriptor = descriptor();
    let bridged = bridge_tool(
        &descriptor,
        Arc::new(Repeater),
        BridgeOptions::new("mcp", "demo", &descriptor),
    );
    let tool = bridged
        .tool
        .expect("the conformance fixture failed to bridge");
    tool_conformance(&ToolConformance {
        tool,
        context: Box::new(|| {
            let workspace = TestWorkspace::new();
            let context = workspace.context().clone();
            (workspace, context)
        }),
        valid_args: args(json!({ "text": "hi", "times": 2 })),
        large_output_args: Some(args(json!({ "text": "long-enough-line\n", "times": 500 }))),
    })
    .await;
}
