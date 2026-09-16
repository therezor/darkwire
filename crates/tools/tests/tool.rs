//! `TypedTool`: the schema it advertises, the validation it applies, and the
//! contract that `execute` never fails.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_core::{ErrorKind, Result};
use darkwire_protocol::{ToolRisk, ToolSource};
use darkwire_tools::testkit::TestWorkspace;
use darkwire_tools::{
    BoxFuture, Tool, ToolContext, ToolExecution, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted, is_tool_name,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn one() -> u64 {
    1
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    #[schemars(length(min = 1), description = "What to echo.")]
    text: String,
    #[serde(default = "one")]
    #[schemars(range(min = 1), description = "How often.")]
    times: u64,
}

struct Echo;

impl ToolHandler for Echo {
    type Args = EchoArgs;

    fn execute<'a>(
        &'a self,
        args: EchoArgs,
        _: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            Ok(ToolOutput::text(
                args.text.repeat(usize::try_from(args.times).unwrap_or(1)),
            ))
        })
    }
}

fn echo() -> TypedTool<Echo> {
    TypedTool::new(ToolSpec::new("echo", "Echo a value back."), Echo).unwrap()
}

/// A handler whose argument type is not an object at all.
struct Scalar;

impl ToolHandler for Scalar {
    type Args = String;

    fn execute<'a>(
        &'a self,
        args: String,
        _: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move { Ok(ToolOutput::text(args)) })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LooseArgs {
    #[schemars(description = "A field.")]
    a: String,
}

/// A handler whose argument struct would strip unknown keys.
struct Loose;

impl ToolHandler for Loose {
    type Args = LooseArgs;

    fn execute<'a>(
        &'a self,
        args: LooseArgs,
        _: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move { Ok(ToolOutput::text(args.a)) })
    }
}

#[test]
fn derives_a_strict_object_schema_without_document_annotations() {
    let tool = echo();
    let parameters = tool.parameters();
    assert_eq!(parameters.get("type"), Some(&json!("object")));
    assert_eq!(parameters.get("additionalProperties"), Some(&json!(false)));
    // The output view would list `times` as required — it is always present
    // after parsing — and the model would dutifully invent a value for it.
    assert_eq!(parameters.get("required"), Some(&json!(["text"])));
    assert!(parameters.get("$schema").is_none());
    assert!(parameters.get("title").is_none());
    assert_eq!(&tool.definition().parameters, parameters);
}

#[test]
fn defaults_risk_to_safe_and_source_to_builtin() {
    let tool = echo();
    assert_eq!(tool.risk(), ToolRisk::Safe);
    assert_eq!(tool.definition().source, ToolSource::Builtin);
    assert!(tool.definition().annotations.is_none());
}

#[test]
fn rejects_a_name_a_provider_would_refuse() {
    let error = TypedTool::new(ToolSpec::new("web search", "x"), Echo)
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("must match"));
    assert!(!is_tool_name("web search"));
    assert!(is_tool_name("web_search-2"));
    assert!(!is_tool_name(&"x".repeat(65)));
}

#[test]
fn rejects_an_empty_description() {
    let error = TypedTool::new(ToolSpec::new("nameless", "   "), Echo)
        .err()
        .unwrap();
    assert!(error.message.contains("no description"));
}

#[test]
fn rejects_a_non_object_schema() {
    let error = TypedTool::new(ToolSpec::new("scalar", "x"), Scalar)
        .err()
        .unwrap();
    assert!(
        error.message.contains("must take an object"),
        "{}",
        error.message
    );
}

#[test]
fn rejects_a_schema_that_would_strip_unknown_arguments() {
    // Stripping would run a different call from the one the model made without
    // telling anyone.
    let error = TypedTool::new(ToolSpec::new("loose", "x"), Loose)
        .err()
        .unwrap();
    assert!(
        error.message.contains("deny_unknown_fields"),
        "{}",
        error.message
    );
}

#[test]
fn parse_args_accepts_valid_arguments_and_applies_defaults() {
    let parsed = echo().parse_args(json!({"text": "hi"})).unwrap();
    assert_eq!(parsed.text, "hi");
    assert_eq!(parsed.times, 1);
}

#[test]
fn parse_args_coerces_the_string_numbers_models_emit() {
    let parsed = echo()
        .parse_args(json!({"text": "hi", "times": "3"}))
        .unwrap();
    assert_eq!(parsed.times, 3);
    // But never a boolean's worth of a string, and never a non-number.
    assert!(
        echo()
            .parse_args(json!({"text": "hi", "times": "many"}))
            .is_err()
    );
    assert!(
        echo()
            .parse_args(json!({"text": "hi", "times": "0"}))
            .is_err()
    );
}

#[test]
fn parse_args_treats_a_missing_argument_object_as_empty() {
    // Providers hand through nothing for a no-argument call; the schema's own
    // `required` list is what should decide, not the absence itself.
    let error = echo().parse_args(Value::Null).err().unwrap();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    let issues = error
        .details
        .get("issues")
        .and_then(Value::as_array)
        .unwrap();
    assert!(
        issues
            .iter()
            .any(|issue| issue["message"].as_str().unwrap().contains("text"))
    );
}

#[test]
fn parse_args_reports_the_offending_path_in_the_message() {
    let error = echo().parse_args(json!({"text": 42})).err().unwrap();
    assert!(error.message.contains("echo"));
    assert!(error.message.contains("text"));
    assert_eq!(error.details.get("tool"), Some(&json!("echo")));
    let issues = error
        .details
        .get("issues")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["path"], json!("text"));
}

#[test]
fn parse_args_rejects_an_unknown_key_instead_of_stripping_it() {
    assert!(
        echo()
            .parse_args(json!({"text": "hi", "extra": true}))
            .is_err()
    );
}

#[tokio::test]
async fn execute_validates_then_executes() {
    let ws = TestWorkspace::new();
    let execution = echo()
        .execute(json!({"text": "ab", "times": 2}), ws.context())
        .await;
    assert_eq!(execution, ToolExecution::ok("abab"));
}

#[tokio::test]
async fn execute_reports_invalid_input_carrying_the_tool() {
    let ws = TestWorkspace::new();
    let execution = echo().execute(json!({}), ws.context()).await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::InvalidInput));
    assert_eq!(execution.details.get("tool"), Some(&json!("echo")));
}

#[tokio::test]
async fn execute_erases_to_a_shared_tool() {
    let ws = TestWorkspace::new();
    let tool: Arc<dyn Tool> = Arc::new(echo());
    let execution = tool.execute(json!({"text": "x"}), ws.context()).await;
    assert_eq!(execution.content, "x");
    assert_eq!(tool.definition().name, "echo");
}

#[test]
fn outputs_convert_into_executions() {
    let flagged: ToolExecution = ToolOutput::error("Error: not found")
        .with_detail("code", 7)
        .into();
    // The content starts with "Error", and that is not what made it a failure.
    assert!(flagged.is_error);
    assert_eq!(flagged.kind, None);
    assert_eq!(flagged.details.get("code"), Some(&json!(7)));

    let ok: ToolExecution = ToolOutput::from("text").into();
    assert!(!ok.is_error);
    assert_eq!(ok.content, "text");

    let failed: ToolExecution = Err(darkwire_core::WireError::new(ErrorKind::Tool, "boom")).into();
    assert_eq!(failed.kind, Some(ErrorKind::Tool));
    assert!(!failed.is_aborted());
    assert!(ToolExecution::error(ErrorKind::Aborted, "x").is_aborted());
    assert_eq!(ToolOutput::from(String::from("s")).content, "s");
}

#[test]
fn assert_not_aborted_reports_the_taxonomy_abort() {
    let token = CancellationToken::new();
    assert!(assert_not_aborted(&token, "thing").is_ok());
    token.cancel();
    let error = assert_not_aborted(&token, "thing").err().unwrap();
    assert_eq!(error.kind, ErrorKind::Aborted);
}

#[test]
fn a_context_describes_itself_without_leaking_its_seams() {
    let ws = TestWorkspace::new();
    let text = format!("{:?}", ws.context());
    assert!(text.contains("ToolContext"));
    assert!(text.contains("sandboxed: false"));
    assert!(format!("{:?}", echo()).contains("echo"));
}
