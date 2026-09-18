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

const CONTENT: &str = "# PostgreSQL-backed sessions\n\nSessions expire after 30 days.";

async fn run(tool: &AnyTool, args: Value, ctx: &ToolContext) -> ToolExecution {
    tool.execute(args, ctx).await
}

fn save(key: &str, content: &str) -> Value {
    json!({"action": "save", "key": key, "content": content})
}

fn read(key: &str) -> Value {
    json!({"action": "read", "key": key})
}

fn delete(key: &str) -> Value {
    json!({"action": "delete", "key": key})
}

fn stored(ws: &TestWorkspace, key: &str) -> String {
    fs::read_to_string(ws.root().join("memory").join(format!("{key}.md"))).unwrap()
}

fn keys(ws: &TestWorkspace) -> Vec<String> {
    let mut keys: Vec<String> = fs::read_dir(ws.root().join("memory"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    keys.sort();
    keys
}

#[tokio::test]
async fn saves_the_content_verbatim_under_the_key() {
    let ws = TestWorkspace::new();
    let result = run(&memory_tool(), save("auth-sessions", CONTENT), ws.context()).await;
    assert!(!result.is_error, "{}", result.content);
    // No frontmatter and no header: what the model wrote is what the file holds.
    assert_eq!(stored(&ws, "auth-sessions"), format!("{CONTENT}\n"));
}

#[tokio::test]
async fn says_it_saved_without_spending_a_line_on_the_path() {
    let ws = TestWorkspace::new();
    let result = run(&memory_tool(), save("auth-sessions", CONTENT), ws.context()).await;
    assert_eq!(result.content, "Saved auth-sessions");
}

#[tokio::test]
async fn keeps_two_differently_keyed_memories_apart() {
    let ws = TestWorkspace::new();
    run(&memory_tool(), save("alpha", "# A"), ws.context()).await;
    let second = run(&memory_tool(), save("zeta", "# Z"), ws.context()).await;
    assert_eq!(second.details.get("replaced"), Some(&json!(false)));
    assert_eq!(keys(&ws), ["alpha.md", "zeta.md"]);
}

#[tokio::test]
async fn saving_a_key_it_already_used_replaces_that_memory_whole() {
    let ws = TestWorkspace::new();
    run(
        &memory_tool(),
        save("auth-sessions", "# Postgres\n\nOld detail."),
        ws.context(),
    )
    .await;
    let again = run(
        &memory_tool(),
        save("auth-sessions", "# Redis"),
        ws.context(),
    )
    .await;
    assert_eq!(again.content, "Replaced auth-sessions");
    assert_eq!(again.details.get("replaced"), Some(&json!(true)));
    assert_eq!(stored(&ws, "auth-sessions"), "# Redis\n");
    assert_eq!(keys(&ws), ["auth-sessions.md"]);
}

#[tokio::test]
async fn asks_for_content_rather_than_writing_an_empty_memory() {
    let ws = TestWorkspace::new();
    let missing = run(
        &memory_tool(),
        json!({"action": "save", "key": "auth-sessions"}),
        ws.context(),
    )
    .await;
    assert!(missing.is_error);
    assert_eq!(missing.content, "Give content to save.");

    let blank = run(&memory_tool(), save("auth-sessions", "  \n "), ws.context()).await;
    assert!(blank.is_error);
    assert!(!ws.root().join("memory").exists());
}

#[tokio::test]
async fn refuses_content_over_the_cap() {
    let ws = TestWorkspace::new();
    let result = run(
        &memory_tool(),
        save("auth-sessions", &"x".repeat(2001)),
        ws.context(),
    )
    .await;
    assert!(result.is_error);
    assert!(result.content.contains("2000 characters at most"));
    assert!(!ws.root().join("memory").exists());
}

#[tokio::test]
async fn reads_back_the_content_and_nothing_around_it() {
    let ws = TestWorkspace::new();
    run(&memory_tool(), save("auth-sessions", CONTENT), ws.context()).await;
    let result = run(&memory_tool(), read("auth-sessions"), ws.context()).await;
    assert!(!result.is_error, "{}", result.content);
    // The trailing newline the file carries, and no title, key or path line:
    // the model asked for one memory and pays for that memory only.
    assert_eq!(result.content, format!("{CONTENT}\n"));
}

#[tokio::test]
async fn says_which_keys_to_look_at_when_there_is_nothing_under_one() {
    let ws = TestWorkspace::new();
    let result = run(&memory_tool(), read("never-written"), ws.context()).await;
    assert!(result.is_error);
    assert!(result.content.contains("No memory `never-written`"));
    assert!(result.content.contains("under Memory in your prompt"));
}

#[tokio::test]
async fn deletes_the_memory_the_key_names() {
    let ws = TestWorkspace::new();
    run(&memory_tool(), save("alpha", "# A"), ws.context()).await;
    run(&memory_tool(), save("zeta", "# Z"), ws.context()).await;
    let result = run(&memory_tool(), delete("alpha"), ws.context()).await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(result.content, "Deleted alpha");
    assert_eq!(result.details.get("existed"), Some(&json!(true)));
    assert_eq!(result.details.get("total"), Some(&json!(1)));
    assert_eq!(keys(&ws), ["zeta.md"]);
}

#[tokio::test]
async fn deleting_a_key_with_nothing_under_it_is_not_a_failure() {
    let ws = TestWorkspace::new();
    run(&memory_tool(), save("alpha", "# A"), ws.context()).await;
    let result = run(&memory_tool(), delete("never-written"), ws.context()).await;
    assert!(!result.is_error);
    assert_eq!(result.content, "No memory `never-written`");
    assert_eq!(result.details.get("existed"), Some(&json!(false)));
}

#[tokio::test]
async fn slugs_a_key_a_model_typed_as_prose_and_reports_the_one_it_used() {
    let ws = TestWorkspace::new();
    let saved = run(&memory_tool(), save("Auth Sessions", CONTENT), ws.context()).await;
    assert_eq!(saved.content, "Saved auth-sessions");
    assert_eq!(keys(&ws), ["auth-sessions.md"]);
    // And the same key reaches the same memory on every action.
    assert!(
        !run(&memory_tool(), read("Auth Sessions"), ws.context())
            .await
            .is_error
    );
    assert_eq!(
        run(&memory_tool(), delete("Auth Sessions"), ws.context())
            .await
            .details
            .get("existed"),
        Some(&json!(true))
    );
}

#[tokio::test]
async fn cannot_be_pointed_outside_the_workspace_by_its_key() {
    let ws = TestWorkspace::new();
    fs::write(ws.outside().join("secret.md"), "not yours").unwrap();
    run(
        &memory_tool(),
        save("../../secret", "# Mine now"),
        ws.context(),
    )
    .await;
    // Every separator was absorbed by the slug, so the write landed inside.
    assert_eq!(keys(&ws), ["secret.md"]);
    assert_eq!(
        fs::read_to_string(ws.outside().join("secret.md")).unwrap(),
        "not yours"
    );
}

#[tokio::test]
async fn reports_a_key_with_nothing_usable_in_it_rather_than_failing() {
    let ws = TestWorkspace::new();
    for args in [save("???", CONTENT), read("???"), delete("???")] {
        let result = run(&memory_tool(), args, ws.context()).await;
        assert!(result.is_error);
        assert!(result.content.contains("No usable key"));
        // A sentence the model can act on, not an error from two layers down.
        assert_eq!(result.kind, None);
    }
}

#[tokio::test]
async fn takes_no_path_so_there_is_none_to_point_outside_the_workspace() {
    let ws = TestWorkspace::new();
    let rejected = run(
        &memory_tool(),
        json!({"action": "save", "key": "a", "content": "# A", "path": "../escape.md"}),
        ws.context(),
    )
    .await;
    assert_eq!(rejected.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn refuses_an_action_outside_the_three() {
    let ws = TestWorkspace::new();
    let rejected = run(
        &memory_tool(),
        json!({"action": "forget", "key": "auth-sessions"}),
        ws.context(),
    )
    .await;
    assert_eq!(rejected.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn carries_the_key_for_the_audit_log_on_every_action() {
    let ws = TestWorkspace::new();
    let expected = Some(&json!("auth-sessions"));
    let saved = run(&memory_tool(), save("Auth Sessions", CONTENT), ws.context()).await;
    assert_eq!(saved.details.get("key"), expected);
    assert_eq!(saved.details.get("total"), Some(&json!(1)));

    let read = run(&memory_tool(), read("auth-sessions"), ws.context()).await;
    assert_eq!(read.details.get("key"), expected);

    let deleted = run(&memory_tool(), delete("auth-sessions"), ws.context()).await;
    assert_eq!(deleted.details.get("key"), expected);
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
