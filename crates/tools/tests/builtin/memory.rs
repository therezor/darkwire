//! `memory` and `skill` against a real temporary workspace.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;

use darkwire_core::ErrorKind;
use darkwire_tools::testkit::TestWorkspace;
use darkwire_tools::{AnyTool, ToolContext, ToolExecution, memory_tool, skill_tool};
use serde_json::{Value, json};

async fn run(tool: &AnyTool, args: Value, ctx: &ToolContext) -> ToolExecution {
    tool.execute(args, ctx).await
}

async fn text(tool: &AnyTool, args: Value, ctx: &ToolContext) -> String {
    let execution = run(tool, args, ctx).await;
    assert!(!execution.is_error, "{}", execution.content);
    execution.content
}

fn note() -> Value {
    json!({
        "name": "ui-stack-preferences",
        "description": "no shadcn/ui; Tailwind in rem, not px",
        "type": "user",
        "body": "The user wants an explicit design token layer.",
    })
}

fn with(mut base: Value, key: &str, value: Value) -> Value {
    base[key] = value;
    base
}

fn stored(ws: &TestWorkspace, name: &str) -> String {
    fs::read_to_string(ws.root().join("memory").join(format!("{name}.md"))).unwrap()
}

fn names(ws: &TestWorkspace) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(ws.root().join("memory"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn writes_one_file_per_fact_with_its_frontmatter() {
    let ws = TestWorkspace::new();
    text(&memory_tool(), note(), ws.context()).await;
    assert_eq!(
        stored(&ws, "ui-stack-preferences"),
        [
            "---",
            "name: ui-stack-preferences",
            "description: no shadcn/ui; Tailwind in rem, not px",
            "metadata:",
            "  type: user",
            "---",
            "",
            "The user wants an explicit design token layer.",
            "",
        ]
        .join("\n")
    );
}

#[tokio::test]
async fn says_where_it_went_and_indexes_it() {
    let ws = TestWorkspace::new();
    let result = text(&memory_tool(), note(), ws.context()).await;
    assert!(result.contains("memory/ui-stack-preferences.md"));
    assert!(result.contains("Recorded"));
    let index = fs::read_to_string(ws.root().join("memory/MEMORY.md")).unwrap();
    assert!(index.contains("(ui-stack-preferences.md)"));
}

#[tokio::test]
async fn keeps_two_differently_named_facts_apart() {
    let ws = TestWorkspace::new();
    text(&memory_tool(), note(), ws.context()).await;
    text(
        &memory_tool(),
        with(note(), "name", json!("run-full-ci-gate")),
        ws.context(),
    )
    .await;
    assert_eq!(
        names(&ws),
        vec![
            "MEMORY.md",
            "run-full-ci-gate.md",
            "ui-stack-preferences.md"
        ]
    );
}

#[tokio::test]
async fn replaces_a_fact_written_under_a_name_it_already_used() {
    let ws = TestWorkspace::new();
    text(
        &memory_tool(),
        with(note(), "body", json!("The old answer.")),
        ws.context(),
    )
    .await;
    let second = text(
        &memory_tool(),
        with(note(), "body", json!("The new answer.")),
        ws.context(),
    )
    .await;
    assert!(second.contains("Replaced"));
    assert!(stored(&ws, "ui-stack-preferences").contains("The new answer."));
    assert!(!stored(&ws, "ui-stack-preferences").contains("The old answer."));
}

#[tokio::test]
async fn slugs_a_name_a_model_typed_as_prose_and_reports_the_one_it_used() {
    let ws = TestWorkspace::new();
    let result = text(
        &memory_tool(),
        with(note(), "name", json!("Build Conventions")),
        ws.context(),
    )
    .await;
    assert!(result.contains("memory/build-conventions.md"));
    assert!(result.contains("named `build-conventions`"));
}

#[tokio::test]
async fn cannot_be_pointed_outside_the_workspace_by_its_name() {
    let ws = TestWorkspace::new();
    text(
        &memory_tool(),
        with(note(), "name", json!("../../etc/passwd")),
        ws.context(),
    )
    .await;
    assert_eq!(names(&ws), vec!["MEMORY.md", "etc-passwd.md"]);
}

#[tokio::test]
async fn reports_a_name_with_nothing_usable_in_it_rather_than_failing() {
    let ws = TestWorkspace::new();
    let result = run(
        &memory_tool(),
        with(note(), "name", json!("???")),
        ws.context(),
    )
    .await;
    assert!(result.is_error);
    assert_eq!(result.kind, None);
    assert!(
        result
            .content
            .contains("Names are letters, digits and hyphens")
    );
}

#[tokio::test]
async fn takes_no_path_so_there_is_none_to_point_outside_the_workspace() {
    let ws = TestWorkspace::new();
    let error = run(
        &memory_tool(),
        with(note(), "path", json!("../../etc/passwd")),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn refuses_a_kind_outside_the_four_and_a_body_over_the_cap() {
    let ws = TestWorkspace::new();
    let kind = run(
        &memory_tool(),
        with(note(), "type", json!("whatever")),
        ws.context(),
    )
    .await;
    assert_eq!(kind.kind, Some(ErrorKind::InvalidInput));
    let body = run(
        &memory_tool(),
        with(note(), "body", json!("x".repeat(2001))),
        ws.context(),
    )
    .await;
    assert_eq!(body.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn carries_the_saved_name_for_the_audit_log() {
    let ws = TestWorkspace::new();
    let result = run(&memory_tool(), note(), ws.context()).await;
    assert_eq!(
        result.details.get("name"),
        Some(&json!("ui-stack-preferences"))
    );
    assert_eq!(result.details.get("replaced"), Some(&json!(false)));
    assert_eq!(result.details.get("total"), Some(&json!(1)));
}

fn install(ws: &TestWorkspace, name: &str, body: &str) {
    let dir = ws.root().join("skills").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), body).unwrap();
}

#[tokio::test]
async fn skill_returns_the_sheet() {
    let ws = TestWorkspace::new();
    install(
        &ws,
        "deploy",
        "---\ndescription: Ship it.\n---\n\nRun make release.\n",
    );
    let result = run(&skill_tool(), json!({"name": "deploy"}), ws.context()).await;
    assert!(result.content.contains("Run make release."));
    assert_eq!(result.details.get("skill"), Some(&json!("deploy")));
    assert_eq!(
        result.details.get("path"),
        Some(&json!("skills/deploy/SKILL.md"))
    );
}

#[tokio::test]
async fn skill_reports_a_skill_that_is_not_there_rather_than_something_opaque() {
    let ws = TestWorkspace::new();
    let error = run(&skill_tool(), json!({"name": "nope"}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::NotFound));
    assert!(error.content.contains("skills/nope/SKILL.md"));
}

#[tokio::test]
async fn skill_cannot_be_pointed_outside_the_workspace_by_its_name() {
    let ws = TestWorkspace::new();
    fs::write(ws.outside().join("secret.md"), "not yours").unwrap();
    let error = run(&skill_tool(), json!({"name": "../../.."}), ws.context()).await;
    assert!(error.is_error);
    // Clamped to `SKILL.md` at the workspace root: every `..` was absorbed, so
    // nothing above the jail is addressable through the skill name.
    assert!(!error.content.contains(".."));
    assert!(!error.content.contains("not yours"));
}
