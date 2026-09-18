//! Helpers every built-in's tests use: run a tool, take its text, or assert
//! that it refused.

use darkwire_tools::{AnyTool, ToolContext, ToolExecution};
use serde_json::Value;

pub async fn run(tool: &AnyTool, args: Value, ctx: &ToolContext) -> ToolExecution {
    tool.execute(args, ctx).await
}

pub async fn text(tool: &AnyTool, args: Value, ctx: &ToolContext) -> String {
    let execution = run(tool, args, ctx).await;
    assert!(!execution.is_error, "{}", execution.content);
    execution.content
}

pub async fn failure(tool: &AnyTool, args: Value, ctx: &ToolContext) -> ToolExecution {
    let execution = run(tool, args, ctx).await;
    assert!(
        execution.is_error,
        "expected a failure, got {}",
        execution.content
    );
    assert!(execution.kind.is_some());
    execution
}
