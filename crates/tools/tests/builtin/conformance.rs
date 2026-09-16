//! Every built-in through the shared conformance suite.
//!
//! This is the file that makes "the built-ins behave the same at their edges"
//! a checked claim. A ninth tool added without a block here is a tool nobody
//! has proved rejects an unknown argument or notices a cancelled turn.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;

use darkwire_tools::testkit::{TestWorkspace, ToolConformance, tool_conformance};
use darkwire_tools::{
    AnyTool, ToolContext, edit_file_tool, exec_tool, list_dir_tool, read_file_tool, write_file_tool,
};
use serde_json::{Map, Value, json};

fn args(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

fn seeded(
    setup: fn(&TestWorkspace),
) -> Box<dyn Fn() -> (TestWorkspace, ToolContext) + Send + Sync> {
    Box::new(move || {
        let ws = TestWorkspace::new();
        setup(&ws);
        let ctx = ws.context().clone();
        (ws, ctx)
    })
}

fn with_notes(ws: &TestWorkspace) {
    fs::write(ws.root().join("notes.md"), "# Notes\nthe quick brown fox\n").unwrap();
    fs::write(
        ws.root().join("big.txt"),
        "lorem ipsum dolor sit amet\n".repeat(400),
    )
    .unwrap();
}

fn with_many_files(ws: &TestWorkspace) {
    fs::create_dir(ws.root().join("src")).unwrap();
    for index in 0..60 {
        fs::write(ws.root().join(format!("fixture-{index:03}.txt")), "x").unwrap();
    }
}

fn bare(_: &TestWorkspace) {}

async fn conform(tool: AnyTool, setup: fn(&TestWorkspace), valid: Value, large: Option<Value>) {
    tool_conformance(&ToolConformance {
        tool,
        context: seeded(setup),
        valid_args: args(&valid),
        large_output_args: large.as_ref().map(args),
    })
    .await;
}

#[tokio::test]
async fn read_file_conforms() {
    conform(
        read_file_tool(),
        with_notes,
        json!({"path": "notes.md"}),
        Some(json!({"path": "big.txt"})),
    )
    .await;
}

#[tokio::test]
async fn write_file_conforms() {
    conform(
        write_file_tool(),
        bare,
        json!({"path": "out/report.txt", "content": "hello"}),
        None,
    )
    .await;
}

#[tokio::test]
async fn edit_file_conforms() {
    conform(
        edit_file_tool(),
        with_notes,
        json!({"path": "notes.md", "oldText": "quick", "newText": "slow"}),
        None,
    )
    .await;
}

#[tokio::test]
async fn list_dir_conforms() {
    conform(
        list_dir_tool(),
        with_many_files,
        json!({"path": "."}),
        Some(json!({"path": "."})),
    )
    .await;
}

#[tokio::test]
async fn exec_conforms() {
    conform(
        exec_tool(),
        bare,
        json!({"argv": ["printf", "ok"]}),
        Some(json!({"argv": ["printf", "%05000d", "0"]})),
    )
    .await;
}
