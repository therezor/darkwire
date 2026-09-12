//! `read_file`, `write_file`, `edit_file` and `list_dir` against a real
//! temporary workspace.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;
use std::os::unix::fs::symlink;

use ghostai_core::ErrorKind;
use ghostai_tools::testkit::TestWorkspace;
use ghostai_tools::{
    AnyTool, ToolContext, ToolExecution, edit_file_tool, list_dir_tool, read_file_tool,
    write_file_tool,
};
use serde_json::{Value, json};

async fn run(tool: &AnyTool, args: Value, ctx: &ToolContext) -> ToolExecution {
    tool.execute(args, ctx).await
}

async fn text(tool: &AnyTool, args: Value, ctx: &ToolContext) -> String {
    let execution = run(tool, args, ctx).await;
    assert!(!execution.is_error, "{}", execution.content);
    execution.content
}

async fn failure(tool: &AnyTool, args: Value, ctx: &ToolContext) -> ToolExecution {
    let execution = run(tool, args, ctx).await;
    assert!(
        execution.is_error,
        "expected a failure, got {}",
        execution.content
    );
    assert!(execution.kind.is_some());
    execution
}

#[tokio::test]
async fn read_file_reads_a_workspace_file() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("notes.md"), "hello\nworld\n").unwrap();
    assert_eq!(
        text(&read_file_tool(), json!({"path": "notes.md"}), ws.context()).await,
        "hello\nworld\n"
    );
}

#[tokio::test]
async fn read_file_reads_a_file_in_a_subdirectory() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("src")).unwrap();
    fs::write(ws.root().join("src/index.ts"), "export {};").unwrap();
    assert_eq!(
        text(
            &read_file_tool(),
            json!({"path": "src/index.ts"}),
            ws.context()
        )
        .await,
        "export {};"
    );
}

#[tokio::test]
async fn read_file_reports_a_missing_file_as_not_found_against_the_relative_path() {
    let ws = TestWorkspace::new();
    let error = failure(
        &read_file_tool(),
        json!({"path": "absent.md"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::NotFound));
    assert_eq!(error.content, "absent.md does not exist.");
    assert!(!error.content.contains(ws.root().to_str().unwrap()));
}

#[tokio::test]
async fn read_file_refuses_a_directory_and_points_at_the_tool_that_handles_it() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("src")).unwrap();
    let error = failure(&read_file_tool(), json!({"path": "src"}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::InvalidInput));
    assert!(error.content.contains("list_dir"));
}

#[tokio::test]
async fn read_file_refuses_to_dump_a_binary_file_into_the_context() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("logo.png"), [0x89, 0x50, 0x00, 0x01, 0x02]).unwrap();
    let error = failure(&read_file_tool(), json!({"path": "logo.png"}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::InvalidInput));
    assert!(error.content.contains("binary"));
}

#[tokio::test]
async fn read_file_says_so_rather_than_returning_nothing_for_an_empty_file() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("empty.txt"), "").unwrap();
    assert!(
        text(
            &read_file_tool(),
            json!({"path": "empty.txt"}),
            ws.context()
        )
        .await
        .contains("is empty")
    );
}

#[tokio::test]
async fn read_file_bounds_the_read_by_the_output_budget_rather_than_the_file_size() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("huge.txt"), "a".repeat(200_000)).unwrap();
    let result = text(
        &read_file_tool(),
        json!({"path": "huge.txt"}),
        &ws.with(|config| config.max_output_chars = 100),
    )
    .await;
    assert!(result.contains("showing the first 400 of 200000 bytes"));
    assert!(result.len() < 600);
}

#[tokio::test]
async fn read_file_returns_a_line_window() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("lines.txt"), "one\ntwo\nthree\nfour").unwrap();
    assert_eq!(
        text(
            &read_file_tool(),
            json!({"path": "lines.txt", "offset": 2, "limit": 2}),
            ws.context()
        )
        .await,
        "two\nthree"
    );
    // Models emit numbers as strings; the schema's numeric fields coerce them.
    assert_eq!(
        text(
            &read_file_tool(),
            json!({"path": "lines.txt", "offset": "2", "limit": "1"}),
            ws.context()
        )
        .await,
        "two"
    );
}

#[tokio::test]
async fn read_file_reads_to_the_end_when_only_an_offset_is_given() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("lines.txt"), "one\ntwo\nthree").unwrap();
    assert_eq!(
        text(
            &read_file_tool(),
            json!({"path": "lines.txt", "offset": 3}),
            ws.context()
        )
        .await,
        "three"
    );
}

#[tokio::test]
async fn read_file_reports_an_offset_past_the_end_instead_of_returning_nothing() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("lines.txt"), "one\ntwo").unwrap();
    assert!(
        text(
            &read_file_tool(),
            json!({"path": "lines.txt", "offset": 99}),
            ws.context()
        )
        .await
        .contains("past the end")
    );
}

#[tokio::test]
async fn read_file_clamps_escapes_into_the_workspace_and_says_where_it_looked() {
    let ws = TestWorkspace::new();
    for (path, landed) in [
        ("../secret", "secret"),
        ("/etc/passwd", "etc/passwd"),
        ("~/.ssh/id_rsa", ".ssh/id_rsa"),
    ] {
        // The workspace is a chroot, so none of these is a refusal — each names
        // a file inside the workspace that happens not to exist. What matters
        // is that the model is told so.
        let error = failure(&read_file_tool(), json!({"path": path}), ws.context()).await;
        assert_eq!(error.kind, Some(ErrorKind::NotFound), "{path}");
        assert!(error.content.contains(landed), "{path}: {}", error.content);
        assert!(
            error.content.contains("The workspace is the root"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn read_file_says_where_it_read_from_when_a_clamped_path_does_exist() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("passwd"), "not the real one").unwrap();
    let result = text(&read_file_tool(), json!({"path": "/passwd"}), ws.context()).await;
    assert!(result.contains("not the real one"));
    assert!(result.contains(r#""/passwd" was resolved to "passwd""#));
}

#[tokio::test]
async fn read_file_rejects_a_symlink_pointing_out_of_the_workspace() {
    let ws = TestWorkspace::new();
    let outside = ws.outside().join("outside.txt");
    fs::write(&outside, "stolen").unwrap();
    symlink(&outside, ws.root().join("link.txt")).unwrap();
    let error = failure(&read_file_tool(), json!({"path": "link.txt"}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::JailEscape));
}

#[tokio::test]
async fn write_file_writes_a_file_and_reports_its_size() {
    let ws = TestWorkspace::new();
    let result = text(
        &write_file_tool(),
        json!({"path": "out.txt", "content": "hello"}),
        ws.context(),
    )
    .await;
    assert_eq!(
        fs::read_to_string(ws.root().join("out.txt")).unwrap(),
        "hello"
    );
    assert_eq!(result, "Wrote 5 B to out.txt.");
}

#[tokio::test]
async fn write_file_creates_parent_directories() {
    let ws = TestWorkspace::new();
    text(
        &write_file_tool(),
        json!({"path": "a/b/c.txt", "content": "deep"}),
        ws.context(),
    )
    .await;
    assert_eq!(
        fs::read_to_string(ws.root().join("a/b/c.txt")).unwrap(),
        "deep"
    );
}

#[tokio::test]
async fn write_file_replaces_existing_contents() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("out.txt"), "old").unwrap();
    text(
        &write_file_tool(),
        json!({"path": "out.txt", "content": "new"}),
        ws.context(),
    )
    .await;
    assert_eq!(
        fs::read_to_string(ws.root().join("out.txt")).unwrap(),
        "new"
    );
}

#[tokio::test]
async fn write_file_reports_writing_over_a_directory_as_invalid_input() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("src")).unwrap();
    let error = failure(
        &write_file_tool(),
        json!({"path": "src", "content": "x"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn write_file_clamps_a_write_that_tried_to_escape() {
    let ws = TestWorkspace::new();
    let result = text(
        &write_file_tool(),
        json!({"path": "../escape.txt", "content": "x"}),
        ws.context(),
    )
    .await;
    assert_eq!(
        fs::read_to_string(ws.root().join("escape.txt")).unwrap(),
        "x"
    );
    assert!(!ws.outside().join("escape.txt").exists());
    assert!(result.contains(r#""../escape.txt" was resolved to "escape.txt""#));
}

#[tokio::test]
async fn write_file_still_refuses_a_symlink_that_leads_out_of_the_workspace() {
    let ws = TestWorkspace::new();
    let outside = ws.outside().join("target.txt");
    fs::write(&outside, "stolen").unwrap();
    symlink(&outside, ws.root().join("link.txt")).unwrap();
    let error = failure(
        &write_file_tool(),
        json!({"path": "link.txt", "content": "x"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::JailEscape));
    assert_eq!(fs::read_to_string(&outside).unwrap(), "stolen");
}

#[tokio::test]
async fn write_file_refuses_a_dangling_symlink_rather_than_creating_its_target_outside() {
    let ws = TestWorkspace::new();
    let outside = ws.outside().join("planted.txt");
    symlink(&outside, ws.root().join("decoy.txt")).unwrap();
    let error = failure(
        &write_file_tool(),
        json!({"path": "decoy.txt", "content": "x"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::JailEscape));
    assert!(!outside.exists());
}

#[tokio::test]
async fn write_file_carries_the_byte_count_for_the_audit_log() {
    let ws = TestWorkspace::new();
    let result = run(
        &write_file_tool(),
        json!({"path": "x.txt", "content": "héllo"}),
        ws.context(),
    )
    .await;
    assert_eq!(
        result.details,
        json!({"path": "x.txt", "bytes": 6})
            .as_object()
            .cloned()
            .unwrap()
    );
}

fn code(ws: &TestWorkspace, body: &str) {
    fs::write(ws.root().join("code.ts"), body).unwrap();
}

fn read_code(ws: &TestWorkspace) -> String {
    fs::read_to_string(ws.root().join("code.ts")).unwrap()
}

#[tokio::test]
async fn edit_file_replaces_a_unique_string() {
    let ws = TestWorkspace::new();
    code(&ws, "const a = 1;\nconst b = 2;\n");
    let result = text(
        &edit_file_tool(),
        json!({"path": "code.ts", "oldText": "const b = 2;", "newText": "const b = 3;"}),
        ws.context(),
    )
    .await;
    assert_eq!(read_code(&ws), "const a = 1;\nconst b = 3;\n");
    assert!(result.contains("Replaced 1 occurrence in code.ts"));
}

#[tokio::test]
async fn edit_file_refuses_an_ambiguous_edit_rather_than_guessing() {
    let ws = TestWorkspace::new();
    code(&ws, "x();\nx();\n");
    let error = failure(
        &edit_file_tool(),
        json!({"path": "code.ts", "oldText": "x();", "newText": "y();"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::Conflict));
    assert!(error.content.contains("occurs 2 times"));
    assert_eq!(read_code(&ws), "x();\nx();\n");
}

#[tokio::test]
async fn edit_file_replaces_every_occurrence_when_asked() {
    let ws = TestWorkspace::new();
    code(&ws, "x();\nx();\n");
    let result = text(
        &edit_file_tool(),
        json!({"path": "code.ts", "oldText": "x();", "newText": "y();", "replaceAll": true}),
        ws.context(),
    )
    .await;
    assert_eq!(read_code(&ws), "y();\ny();\n");
    assert!(result.contains("Replaced 2 occurrences"));
}

#[tokio::test]
async fn edit_file_refuses_the_string_false_for_replace_all_rather_than_reading_it_as_true() {
    let ws = TestWorkspace::new();
    code(&ws, "a");
    let error = failure(
        &edit_file_tool(),
        json!({"path": "code.ts", "oldText": "a", "newText": "b", "replaceAll": "false"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn edit_file_reports_text_that_is_not_there_with_advice() {
    let ws = TestWorkspace::new();
    code(&ws, "const a = 1;\n");
    let error = failure(
        &edit_file_tool(),
        json!({"path": "code.ts", "oldText": "const c = 3;", "newText": "x"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::NotFound));
    assert!(error.content.contains("exactly"));
}

#[tokio::test]
async fn edit_file_refuses_a_no_op_edit_and_a_missing_file() {
    let ws = TestWorkspace::new();
    code(&ws, "a");
    let noop = failure(
        &edit_file_tool(),
        json!({"path": "code.ts", "oldText": "a", "newText": "a"}),
        ws.context(),
    )
    .await;
    assert_eq!(noop.kind, Some(ErrorKind::InvalidInput));
    let missing = failure(
        &edit_file_tool(),
        json!({"path": "gone.ts", "oldText": "a", "newText": "b"}),
        ws.context(),
    )
    .await;
    assert_eq!(missing.kind, Some(ErrorKind::NotFound));
}

#[tokio::test]
async fn edit_file_writes_dollar_patterns_literally() {
    let ws = TestWorkspace::new();
    code(&ws, "PRICE");
    text(
        &edit_file_tool(),
        json!({"path": "code.ts", "oldText": "PRICE", "newText": "$& $1 $` costs $5"}),
        ws.context(),
    )
    .await;
    assert_eq!(read_code(&ws), "$& $1 $` costs $5");

    code(&ws, "A A");
    text(
        &edit_file_tool(),
        json!({"path": "code.ts", "oldText": "A", "newText": "$&", "replaceAll": true}),
        ws.context(),
    )
    .await;
    assert_eq!(read_code(&ws), "$& $&");
}

#[tokio::test]
async fn edit_file_reports_the_size_delta_for_the_audit_log() {
    let ws = TestWorkspace::new();
    code(&ws, "const a = 1;\nconst b = 2;\n");
    let result = run(
        &edit_file_tool(),
        json!({"path": "code.ts", "oldText": "const a = 1;", "newText": "let a=1;"}),
        ws.context(),
    )
    .await;
    assert_eq!(result.details.get("occurrences"), Some(&json!(1)));
    assert_eq!(result.details.get("delta"), Some(&json!(-4)));
    assert!(result.content.contains("(-4 characters)"));
}

#[tokio::test]
async fn list_dir_lists_directories_first_then_files_with_sizes() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("src")).unwrap();
    fs::write(ws.root().join("a.txt"), "aaa").unwrap();
    fs::write(ws.root().join("b.txt"), "bb").unwrap();
    let result = text(&list_dir_tool(), json!({}), ws.context()).await;
    assert_eq!(
        result.split('\n').collect::<Vec<_>>(),
        vec!["src/", "a.txt (3 B)", "b.txt (2 B)"]
    );
}

#[tokio::test]
async fn list_dir_defaults_to_the_workspace_root_and_hides_nothing() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("node_modules")).unwrap();
    fs::write(ws.root().join(".env"), "SECRET=1").unwrap();
    let result = text(&list_dir_tool(), json!({}), ws.context()).await;
    assert!(result.contains("node_modules/"));
    assert!(result.contains(".env"));
}

#[tokio::test]
async fn list_dir_walks_subdirectories_when_asked() {
    let ws = TestWorkspace::new();
    fs::create_dir_all(ws.root().join("src/deep")).unwrap();
    fs::write(ws.root().join("src/deep/x.ts"), "x").unwrap();
    let flat = text(&list_dir_tool(), json!({"path": "."}), ws.context()).await;
    assert!(!flat.contains("x.ts"));
    let result = text(
        &list_dir_tool(),
        json!({"path": ".", "recursive": true}),
        ws.context(),
    )
    .await;
    assert!(result.contains("src/deep/x.ts"));
    assert!(result.contains("src/deep/"));
}

#[tokio::test]
async fn list_dir_caps_the_listing_and_says_how_much_it_dropped() {
    let ws = TestWorkspace::new();
    for index in 0..10 {
        fs::write(ws.root().join(format!("f{index}.txt")), "x").unwrap();
    }
    let result = text(
        &list_dir_tool(),
        json!({"path": ".", "maxEntries": 3}),
        ws.context(),
    )
    .await;
    assert_eq!(result.split('\n').count(), 4);
    assert!(result.contains("7 more entries not shown"));
}

#[tokio::test]
async fn list_dir_says_a_directory_is_empty_and_reports_a_missing_one() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("empty")).unwrap();
    assert!(
        text(&list_dir_tool(), json!({"path": "empty"}), ws.context())
            .await
            .contains("is empty")
    );
    let error = failure(&list_dir_tool(), json!({"path": "nope"}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::NotFound));
}

#[tokio::test]
async fn list_dir_clamps_a_listing_that_tried_to_escape_back_to_the_workspace_root() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("inside.txt"), "x").unwrap();
    fs::write(ws.outside().join("outside-the-jail.txt"), "x").unwrap();
    let listing = text(&list_dir_tool(), json!({"path": ".."}), ws.context()).await;
    assert!(listing.contains("inside.txt"));
    assert!(!listing.contains("outside-the-jail.txt"));
    assert!(listing.contains("[list_dir:"));
}

#[tokio::test]
async fn list_dir_marks_an_unreadable_entry_rather_than_failing_the_whole_listing() {
    let ws = TestWorkspace::new();
    symlink(ws.root().join("nowhere"), ws.root().join("broken")).unwrap();
    fs::write(ws.root().join("fine.txt"), "x").unwrap();
    let result = text(&list_dir_tool(), json!({}), ws.context()).await;
    assert!(result.contains("broken (unreadable)"));
    assert!(result.contains("fine.txt (1 B)"));
}

#[tokio::test]
async fn every_filesystem_tool_honours_a_cancelled_token_before_touching_disk() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("a.txt"), "a").unwrap();
    ws.token().cancel();
    for (tool, args) in [
        (read_file_tool(), json!({"path": "a.txt"})),
        (write_file_tool(), json!({"path": "b.txt", "content": "b"})),
        (
            edit_file_tool(),
            json!({"path": "a.txt", "oldText": "a", "newText": "b"}),
        ),
        (list_dir_tool(), json!({})),
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
