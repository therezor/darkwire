//! Every built-in through the shared conformance suite.
//!
//! This is the file that makes "the built-ins behave the same at their edges"
//! a checked claim. A tool added without a block here is a tool nobody has
//! proved rejects an unknown argument or notices a cancelled turn.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;

use darkwire_core::ErrorKind;
use darkwire_tools::testkit::{TestWorkspace, ToolConformance, tool_conformance};
use darkwire_tools::{
    AnyTool, ToolContext, edit_tool, exec_tool, find_tool, grep_tool, ls_tool, read_tool,
    write_tool,
};
use serde_json::{Map, Value, json};

use crate::common::run;

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

/// A tree wide enough that a search overruns the conformance budget.
fn with_a_haystack(ws: &TestWorkspace) {
    for index in 0..60 {
        fs::write(
            ws.root().join(format!("page-{index:03}.md")),
            "lorem ipsum dolor sit amet\n".repeat(20),
        )
        .unwrap();
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
async fn read_conforms() {
    conform(
        read_tool(),
        with_notes,
        json!({"path": "notes.md"}),
        Some(json!({"path": "big.txt"})),
    )
    .await;
}

#[tokio::test]
async fn write_conforms() {
    conform(
        write_tool(),
        bare,
        json!({"path": "out/report.txt", "content": "hello"}),
        None,
    )
    .await;
}

#[tokio::test]
async fn edit_conforms() {
    conform(
        edit_tool(),
        with_notes,
        json!({"path": "notes.md", "oldText": "quick", "newText": "slow"}),
        None,
    )
    .await;
}

#[tokio::test]
async fn ls_conforms() {
    conform(
        ls_tool(),
        with_many_files,
        json!({"path": "."}),
        Some(json!({"path": "."})),
    )
    .await;
}

#[tokio::test]
async fn grep_conforms() {
    conform(
        grep_tool(),
        with_a_haystack,
        json!({"pattern": "lorem"}),
        Some(json!({"pattern": "lorem", "limit": 1000})),
    )
    .await;
}

#[tokio::test]
async fn find_conforms() {
    conform(
        find_tool(),
        with_many_files,
        json!({"pattern": "*.txt"}),
        Some(json!({"pattern": "*.txt"})),
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

#[tokio::test]
async fn every_filesystem_tool_honours_a_cancelled_token_before_touching_disk() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("a.txt"), "a").unwrap();
    ws.token().cancel();
    for (tool, args) in [
        (read_tool(), json!({"path": "a.txt"})),
        (write_tool(), json!({"path": "b.txt", "content": "b"})),
        (
            edit_tool(),
            json!({"path": "a.txt", "oldText": "a", "newText": "b"}),
        ),
        (ls_tool(), json!({})),
    ] {
        let execution = run(&tool, args, ws.context()).await;
        assert_eq!(
            execution.kind,
            Some(ErrorKind::Aborted),
            "{}",
            tool.definition().name
        );
    }
    assert!(!ws.root().join("b.txt").exists());
    assert_eq!(fs::read_to_string(ws.root().join("a.txt")).unwrap(), "a");
}
