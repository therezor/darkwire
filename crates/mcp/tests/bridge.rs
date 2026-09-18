//! A remote descriptor as a `Tool`: names, bands, results and failures.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_mcp::testkit::echo_tool;
use darkwire_mcp::{
    BridgeOptions, BridgedTool, McpCallOptions, McpCallResult, McpCallTarget, McpContentPart,
    McpResourceContents, McpToolDescriptor, bridge_tool, flatten_content,
};
use darkwire_protocol::json::Object;
use darkwire_protocol::{ToolAnnotations, ToolRisk, ToolSource};
use darkwire_tools::ToolContext;
use darkwire_tools::testkit::TestWorkspace;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// The server, as a struct: records every call and answers with one result.
struct Recorder {
    calls: Mutex<Vec<(String, Object)>>,
    answer: Mutex<Option<Result<McpCallResult>>>,
    timeouts: Mutex<Vec<u64>>,
}

impl Recorder {
    fn answering(answer: Result<McpCallResult>) -> Arc<Recorder> {
        Arc::new(Recorder {
            calls: Mutex::new(Vec::new()),
            answer: Mutex::new(Some(answer)),
            timeouts: Mutex::new(Vec::new()),
        })
    }
}

impl McpCallTarget for Recorder {
    fn call(
        &self,
        upstream_name: &str,
        args: Object,
        options: McpCallOptions,
    ) -> BoxFuture<'_, Result<McpCallResult>> {
        self.calls.lock().push((upstream_name.to_owned(), args));
        self.timeouts.lock().push(options.timeout_ms);
        let answer = self
            .answer
            .lock()
            .take()
            .unwrap_or_else(|| Ok(McpCallResult::text("ok")));
        Box::pin(async move { answer })
    }
}

fn bridge(descriptor: &McpToolDescriptor, answer: McpCallResult) -> (BridgedTool, Arc<Recorder>) {
    let recorder = Recorder::answering(Ok(answer));
    let bridged = bridge_tool(
        descriptor,
        Arc::clone(&recorder) as Arc<dyn McpCallTarget>,
        BridgeOptions::new("mcp", "demo", descriptor),
    );
    (bridged, recorder)
}

fn ok() -> McpCallResult {
    McpCallResult::text("ok")
}

fn context() -> (TestWorkspace, ToolContext) {
    let workspace = TestWorkspace::new();
    let context = workspace.context().clone();
    (workspace, context)
}

#[tokio::test]
async fn advertises_the_flattened_name_and_calls_the_upstream_one() {
    let (bridged, recorder) = bridge(&echo_tool(), ok());
    let tool = bridged.tool.expect("a tool");
    assert_eq!(tool.definition().name, "mcp_demo_echo");
    assert_eq!(bridged.upstream_name, "echo");

    let (_workspace, ctx) = context();
    let execution = tool.execute(json!({ "text": "hi" }), &ctx).await;
    assert!(!execution.is_error, "{execution:?}");
    let calls = recorder.calls.lock();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "echo");
    assert_eq!(
        serde_json::to_value(&calls[0].1).unwrap(),
        json!({ "text": "hi" })
    );
}

#[test]
fn reports_its_source_as_mcp_and_honours_an_override() {
    let (bridged, _) = bridge(&echo_tool(), ok());
    assert_eq!(bridged.tool.unwrap().definition().source, ToolSource::Mcp);

    let descriptor = echo_tool();
    let bridged = bridge_tool(
        &descriptor,
        Recorder::answering(Ok(ok())),
        BridgeOptions::new("ext", "linear", &descriptor).source(ToolSource::Extension),
    );
    let tool = bridged.tool.unwrap();
    assert_eq!(tool.definition().source, ToolSource::Extension);
    assert_eq!(tool.definition().name, "ext_linear_echo");
}

#[test]
fn takes_a_read_only_claim_at_face_value_and_nothing_else() {
    let read = echo_tool();
    let silent = McpToolDescriptor {
        annotations: None,
        ..echo_tool()
    };
    let destructive = McpToolDescriptor {
        annotations: Some(ToolAnnotations {
            destructive_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        ..echo_tool()
    };
    let risk = |descriptor: &McpToolDescriptor| bridge(descriptor, ok()).0.tool.unwrap().risk();

    assert_eq!(risk(&read), ToolRisk::Safe);
    // Silence is not the same claim: this is third-party code over a socket.
    assert_eq!(risk(&silent), ToolRisk::Network);
    assert_eq!(risk(&destructive), ToolRisk::Exec);
}

#[test]
fn passes_annotations_through_because_the_vocabularies_are_the_same() {
    let descriptor = McpToolDescriptor {
        annotations: Some(ToolAnnotations {
            read_only_hint: Some(true),
            title: Some("Echo".to_owned()),
            ..ToolAnnotations::default()
        }),
        ..echo_tool()
    };
    let (bridged, _) = bridge(&descriptor, ok());
    let definition = bridged.tool.unwrap().definition().clone();
    assert_eq!(definition.annotations, descriptor.annotations);
    assert_eq!(definition.risk, ToolRisk::Safe);
}

#[test]
fn falls_back_through_title_to_a_sentence_never_to_nothing() {
    let titled = McpToolDescriptor {
        description: Some(String::new()),
        title: Some("Repeat a string".to_owned()),
        annotations: None,
        ..echo_tool()
    };
    let bare = McpToolDescriptor {
        description: None,
        title: None,
        ..echo_tool()
    };
    assert_eq!(
        bridge(&titled, ok())
            .0
            .tool
            .unwrap()
            .definition()
            .description,
        "Repeat a string"
    );
    let sentence = bridge(&bare, ok())
        .0
        .tool
        .unwrap()
        .definition()
        .description
        .clone();
    assert!(sentence.contains("demo"));
    assert!(sentence.contains("echo"));

    let extension = bridge_tool(
        &bare,
        Recorder::answering(Ok(ok())),
        BridgeOptions::new("ext", "linear", &bare).source(ToolSource::Extension),
    );
    assert!(
        extension
            .tool
            .unwrap()
            .definition()
            .description
            .contains("linear extension")
    );
}

#[test]
fn drops_one_unusable_tool_rather_than_the_whole_server() {
    let broken = McpToolDescriptor {
        name: "broken".to_owned(),
        title: None,
        description: None,
        input_schema: json!({ "type": "string" }),
        annotations: None,
    };
    let (bridged, _) = bridge(&broken, ok());
    assert!(bridged.tool.is_none());
    assert_eq!(bridged.upstream_name, "broken");
    assert!(bridged.issues[0].message.contains("must take an object"));
}

#[test]
fn carries_a_schema_warning_without_refusing_the_tool() {
    let sloppy = McpToolDescriptor {
        name: "sloppy".to_owned(),
        title: None,
        description: None,
        input_schema: json!({ "type": "object", "properties": { "a": { "type": "string" } } }),
        annotations: None,
    };
    let (bridged, _) = bridge(&sloppy, ok());
    assert!(bridged.tool.is_some());
    assert!(bridged.issues[0].message.contains("no description"));
}

#[tokio::test]
async fn reports_a_remote_failure_as_a_result_the_model_can_read() {
    let (bridged, _) = bridge(
        &echo_tool(),
        McpCallResult {
            content: vec![McpContentPart::text("no such repository")],
            is_error: Some(true),
            structured_content: None,
        },
    );
    let (_workspace, ctx) = context();
    let execution = bridged
        .tool
        .unwrap()
        .execute(json!({ "text": "hi" }), &ctx)
        .await;
    assert_eq!(execution.content, "no such repository");
    assert!(execution.is_error);
    // Flagged by the server, not failed by this client: no taxonomy kind.
    assert_eq!(execution.kind, None);
}

#[tokio::test]
async fn reports_a_transport_failure_as_a_result_with_a_kind() {
    let descriptor = echo_tool();
    let recorder = Recorder::answering(Err(WireError::new(ErrorKind::Network, "gone")));
    let bridged = bridge_tool(
        &descriptor,
        recorder,
        BridgeOptions::new("mcp", "demo", &descriptor),
    );
    let (_workspace, ctx) = context();
    let execution = bridged
        .tool
        .unwrap()
        .execute(json!({ "text": "hi" }), &ctx)
        .await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::Network));
    assert_eq!(execution.content, "gone");
}

#[tokio::test]
async fn refuses_arguments_that_do_not_validate_before_calling_anything() {
    let (bridged, recorder) = bridge(&echo_tool(), ok());
    let (_workspace, ctx) = context();
    let execution = bridged
        .tool
        .unwrap()
        .execute(json!({ "nope": true }), &ctx)
        .await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::InvalidInput));
    assert!(execution.content.contains("mcp_demo_echo"));
    assert_eq!(execution.details.get("tool"), Some(&json!("mcp_demo_echo")));
    assert!(recorder.calls.lock().is_empty());
}

#[tokio::test]
async fn honours_an_already_cancelled_token_before_it_calls_anything() {
    let (bridged, recorder) = bridge(&echo_tool(), ok());
    let (_workspace, ctx) = context();
    let token = CancellationToken::new();
    token.cancel();
    let ctx = ctx.with_token(token);
    let execution = bridged
        .tool
        .unwrap()
        .execute(json!({ "text": "hi" }), &ctx)
        .await;
    assert!(execution.is_aborted());
    assert!(recorder.calls.lock().is_empty());
}

#[tokio::test]
async fn keeps_structured_output_for_the_audit_log_out_of_the_prompt() {
    let (bridged, _) = bridge(
        &echo_tool(),
        McpCallResult {
            content: vec![McpContentPart::text("ok")],
            is_error: None,
            structured_content: Some(json!({ "count": 2 })),
        },
    );
    let (_workspace, ctx) = context();
    let execution = bridged
        .tool
        .unwrap()
        .execute(json!({ "text": "hi" }), &ctx)
        .await;
    assert_eq!(execution.content, "ok");
    assert_eq!(
        execution.details.get("structuredContent"),
        Some(&json!({ "count": 2 }))
    );
}

#[tokio::test]
async fn hands_the_per_call_cap_and_coerced_arguments_to_the_server() {
    let descriptor = echo_tool();
    let recorder = Recorder::answering(Ok(ok()));
    let bridged = bridge_tool(
        &descriptor,
        Arc::clone(&recorder) as Arc<dyn McpCallTarget>,
        BridgeOptions::new("mcp", "demo", &descriptor).timeout_ms(5_000),
    );
    let (_workspace, ctx) = context();
    bridged
        .tool
        .unwrap()
        .execute(json!({ "text": "hi", "times": "3" }), &ctx)
        .await;
    assert_eq!(*recorder.timeouts.lock(), [5_000]);
    assert_eq!(recorder.calls.lock()[0].1.get("times"), Some(&json!(3)));
}

#[test]
fn joins_text_parts() {
    let flattened = flatten_content(&McpCallResult {
        content: vec![McpContentPart::text("one"), McpContentPart::text("two")],
        ..McpCallResult::default()
    });
    assert_eq!(flattened, "one\ntwo");
}

#[test]
fn describes_a_binary_part_instead_of_pasting_its_base64() {
    // A model reads nothing from base64, and the bytes would consume the whole
    // output budget, evicting the text that does say something.
    let flattened = flatten_content(&McpCallResult {
        content: vec![McpContentPart {
            kind: "image".to_owned(),
            data: Some("AAAA".repeat(64)),
            mime_type: Some("image/png".to_owned()),
            ..McpContentPart::default()
        }],
        ..McpCallResult::default()
    });
    assert!(flattened.contains("image/png"));
    assert!(flattened.contains("192 bytes"));
    assert!(!flattened.contains("AAAA"));

    let untyped = flatten_content(&McpCallResult {
        content: vec![McpContentPart {
            kind: "audio".to_owned(),
            ..McpContentPart::default()
        }],
        ..McpCallResult::default()
    });
    assert_eq!(untyped, "[audio, 0 bytes, not shown]");
}

#[test]
fn prefers_a_resource_text_over_its_uri() {
    let with_text = McpCallResult {
        content: vec![McpContentPart {
            kind: "resource".to_owned(),
            resource: Some(McpResourceContents {
                uri: Some("file:///a".to_owned()),
                text: Some("body".to_owned()),
                mime_type: None,
            }),
            ..McpContentPart::default()
        }],
        ..McpCallResult::default()
    };
    assert_eq!(flatten_content(&with_text), "body");

    let uri_only = McpCallResult {
        content: vec![McpContentPart {
            kind: "resource".to_owned(),
            resource: Some(McpResourceContents {
                uri: Some("file:///a".to_owned()),
                ..McpResourceContents::default()
            }),
            ..McpContentPart::default()
        }],
        ..McpCallResult::default()
    };
    assert_eq!(flatten_content(&uri_only), "file:///a");

    let empty = McpCallResult {
        content: vec![McpContentPart {
            kind: "resource".to_owned(),
            ..McpContentPart::default()
        }],
        ..McpCallResult::default()
    };
    assert_eq!(flatten_content(&empty), "[resource, not shown]");
}

#[test]
fn names_a_link_and_a_part_it_does_not_understand_rather_than_dropping_them() {
    let link = McpCallResult {
        content: vec![
            McpContentPart {
                kind: "resource_link".to_owned(),
                uri: Some("https://x.test/doc".to_owned()),
                ..McpContentPart::default()
            },
            McpContentPart {
                kind: "resource_link".to_owned(),
                ..McpContentPart::default()
            },
            McpContentPart {
                kind: "invented".to_owned(),
                ..McpContentPart::default()
            },
        ],
        ..McpCallResult::default()
    };
    assert_eq!(
        flatten_content(&link),
        "https://x.test/doc\n[resource link]\n[invented, not shown]"
    );
}

#[test]
fn falls_back_to_structured_output_when_there_are_no_content_parts() {
    // Showing the model nothing would look like a tool that silently did
    // nothing, which is the one outcome it cannot recover from.
    let flattened = flatten_content(&McpCallResult {
        structured_content: Some(json!({ "ok": true })),
        ..McpCallResult::default()
    });
    assert!(flattened.contains("ok"));
}

#[test]
fn is_empty_for_a_result_that_genuinely_carried_nothing() {
    assert_eq!(flatten_content(&McpCallResult::default()), "");
}

#[tokio::test]
async fn a_session_is_a_call_target() {
    // What the extension host hands the bridge: the session itself, with no
    // connection in between.
    let fake = darkwire_mcp::testkit::FakeServer::default();
    let session = fake
        .connector()
        .connect(
            darkwire_mcp::resolve_spec(
                "demo",
                &serde_json::from_value(json!({ "command": "npx" })).unwrap(),
            )
            .unwrap(),
            darkwire_mcp::McpConnectContext::bare(CancellationToken::new()),
        )
        .await
        .unwrap();
    let descriptor = echo_tool();
    let bridged = bridge_tool(
        &descriptor,
        Arc::new(session) as Arc<dyn McpCallTarget>,
        BridgeOptions::new("ext", "demo", &descriptor),
    );
    let (_workspace, ctx) = context();
    let execution = bridged
        .tool
        .unwrap()
        .execute(json!({ "text": "hi" }), &ctx)
        .await;
    assert!(!execution.is_error);
    assert_eq!(fake.calls()[0].name, "echo");
    let value: Value = serde_json::from_str(&execution.content).unwrap();
    assert_eq!(value, json!({ "text": "hi" }));
}
