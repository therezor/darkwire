//! `edit` against a real temporary workspace.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;

use darkwire_core::ErrorKind;
use darkwire_tools::edit_tool;
use darkwire_tools::testkit::TestWorkspace;
use serde_json::json;

use crate::common::{failure, run, text};

fn code(ws: &TestWorkspace, body: &str) {
    fs::write(ws.root().join("code.ts"), body).unwrap();
}

fn read_code(ws: &TestWorkspace) -> String {
    fs::read_to_string(ws.root().join("code.ts")).unwrap()
}

#[tokio::test]
async fn edit_replaces_a_unique_string() {
    let ws = TestWorkspace::new();
    code(&ws, "const a = 1;\nconst b = 2;\n");
    let result = text(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "const b = 2;", "newText": "const b = 3;"}),
        ws.context(),
    )
    .await;
    assert_eq!(read_code(&ws), "const a = 1;\nconst b = 3;\n");
    assert!(result.contains("Replaced 1 occurrence in code.ts"));
}

#[tokio::test]
async fn edit_refuses_an_ambiguous_edit_rather_than_guessing() {
    let ws = TestWorkspace::new();
    code(&ws, "x();\nx();\n");
    let error = failure(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "x();", "newText": "y();"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::Conflict));
    assert!(error.content.contains("occurs 2 times"));
    assert_eq!(read_code(&ws), "x();\nx();\n");
}

#[tokio::test]
async fn edit_replaces_every_occurrence_when_asked() {
    let ws = TestWorkspace::new();
    code(&ws, "x();\nx();\n");
    let result = text(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "x();", "newText": "y();", "replaceAll": true}),
        ws.context(),
    )
    .await;
    assert_eq!(read_code(&ws), "y();\ny();\n");
    assert!(result.contains("Replaced 2 occurrences"));
}

#[tokio::test]
async fn edit_refuses_the_string_false_for_replace_all_rather_than_reading_it_as_true() {
    let ws = TestWorkspace::new();
    code(&ws, "a");
    let error = failure(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "a", "newText": "b", "replaceAll": "false"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn edit_reports_text_that_is_not_there_with_advice() {
    let ws = TestWorkspace::new();
    code(&ws, "const a = 1;\n");
    let error = failure(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "const c = 3;", "newText": "x"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::NotFound));
    assert!(error.content.contains("exactly"));
}

#[tokio::test]
async fn edit_refuses_a_no_op_edit_and_a_missing_file() {
    let ws = TestWorkspace::new();
    code(&ws, "a");
    let noop = failure(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "a", "newText": "a"}),
        ws.context(),
    )
    .await;
    assert_eq!(noop.kind, Some(ErrorKind::InvalidInput));
    let missing = failure(
        &edit_tool(),
        json!({"path": "gone.ts", "oldText": "a", "newText": "b"}),
        ws.context(),
    )
    .await;
    assert_eq!(missing.kind, Some(ErrorKind::NotFound));
}

#[tokio::test]
async fn edit_writes_dollar_patterns_literally() {
    let ws = TestWorkspace::new();
    code(&ws, "PRICE");
    text(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "PRICE", "newText": "$& $1 $` costs $5"}),
        ws.context(),
    )
    .await;
    assert_eq!(read_code(&ws), "$& $1 $` costs $5");

    code(&ws, "A A");
    text(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "A", "newText": "$&", "replaceAll": true}),
        ws.context(),
    )
    .await;
    assert_eq!(read_code(&ws), "$& $&");
}

#[tokio::test]
async fn edit_reports_the_size_delta_for_the_audit_log() {
    let ws = TestWorkspace::new();
    code(&ws, "const a = 1;\nconst b = 2;\n");
    let result = run(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "const a = 1;", "newText": "let a=1;"}),
        ws.context(),
    )
    .await;
    assert_eq!(result.details.get("occurrences"), Some(&json!(1)));
    assert_eq!(result.details.get("delta"), Some(&json!(-4)));
    assert!(result.content.contains("(-4 characters)"));
}

#[tokio::test]
async fn edit_applies_several_blocks_in_one_write() {
    let ws = TestWorkspace::new();
    code(&ws, "const a = 1;\nconst b = 2;\nconst c = 3;\n");
    let out = text(
        &edit_tool(),
        json!({"path": "code.ts", "edits": [
            {"oldText": "const a = 1;", "newText": "const a = 10;"},
            {"oldText": "const c = 3;", "newText": "const c = 30;"}
        ]}),
        ws.context(),
    )
    .await;
    assert!(out.starts_with("Replaced 2 blocks in code.ts"), "{out}");
    assert_eq!(
        read_code(&ws),
        "const a = 10;\nconst b = 2;\nconst c = 30;\n"
    );
}

#[tokio::test]
async fn edit_shows_a_diff_of_what_it_changed() {
    let ws = TestWorkspace::new();
    code(&ws, "one\ntwo\nthree\n");
    let out = text(
        &edit_tool(),
        json!({"path": "code.ts", "edits": [{"oldText": "two", "newText": "TWO"}]}),
        ws.context(),
    )
    .await;
    assert!(out.contains("@@ -2,1 +2,1 @@"), "{out}");
    assert!(out.contains("-two"), "{out}");
    assert!(out.contains("+TWO"), "{out}");
    assert!(out.contains(" one"), "{out}");
}

#[tokio::test]
async fn edit_leaves_the_file_untouched_when_one_block_does_not_match() {
    let ws = TestWorkspace::new();
    code(&ws, "const a = 1;\nconst b = 2;\n");
    let failed = failure(
        &edit_tool(),
        json!({"path": "code.ts", "edits": [
            {"oldText": "const a = 1;", "newText": "const a = 10;"},
            {"oldText": "const z = 9;", "newText": "const z = 90;"}
        ]}),
        ws.context(),
    )
    .await;
    assert_eq!(failed.kind, Some(ErrorKind::NotFound));
    assert!(failed.content.contains("edit 2"), "{}", failed.content);
    assert_eq!(read_code(&ws), "const a = 1;\nconst b = 2;\n");
}

#[tokio::test]
async fn edit_refuses_a_block_that_is_ambiguous_on_its_own() {
    let ws = TestWorkspace::new();
    code(&ws, "call();\ncall();\n");
    let failed = failure(
        &edit_tool(),
        json!({"path": "code.ts", "edits": [{"oldText": "call();", "newText": "run();"}]}),
        ws.context(),
    )
    .await;
    assert_eq!(failed.kind, Some(ErrorKind::Conflict));
    assert!(
        failed.content.contains("occurs 2 times"),
        "{}",
        failed.content
    );
    assert_eq!(read_code(&ws), "call();\ncall();\n");
}

#[tokio::test]
async fn edit_refuses_two_blocks_that_overlap() {
    let ws = TestWorkspace::new();
    code(&ws, "alpha beta gamma\n");
    let failed = failure(
        &edit_tool(),
        json!({"path": "code.ts", "edits": [
            {"oldText": "alpha beta", "newText": "x"},
            {"oldText": "beta gamma", "newText": "y"}
        ]}),
        ws.context(),
    )
    .await;
    assert_eq!(failed.kind, Some(ErrorKind::Conflict));
    assert!(failed.content.contains("overlap"), "{}", failed.content);
    assert_eq!(read_code(&ws), "alpha beta gamma\n");
}

#[tokio::test]
async fn edit_refuses_both_forms_at_once_and_neither_form_at_all() {
    let ws = TestWorkspace::new();
    code(&ws, "a\n");
    let both = failure(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "a", "newText": "b",
               "edits": [{"oldText": "a", "newText": "c"}]}),
        ws.context(),
    )
    .await;
    assert_eq!(both.kind, Some(ErrorKind::InvalidInput));

    let neither = failure(&edit_tool(), json!({"path": "code.ts"}), ws.context()).await;
    assert_eq!(neither.kind, Some(ErrorKind::InvalidInput));

    let half = failure(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "a"}),
        ws.context(),
    )
    .await;
    assert_eq!(half.kind, Some(ErrorKind::InvalidInput));
    assert_eq!(read_code(&ws), "a\n");
}

#[tokio::test]
async fn edit_refuses_replace_all_together_with_a_batch() {
    let ws = TestWorkspace::new();
    code(&ws, "a\n");
    let failed = failure(
        &edit_tool(),
        json!({"path": "code.ts", "replaceAll": true,
               "edits": [{"oldText": "a", "newText": "b"}]}),
        ws.context(),
    )
    .await;
    assert_eq!(failed.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn edit_keeps_a_byte_order_mark_the_model_never_sent() {
    let ws = TestWorkspace::new();
    code(&ws, "\u{feff}const a = 1;\n");
    text(
        &edit_tool(),
        json!({"path": "code.ts", "edits": [{"oldText": "const a = 1;", "newText": "const a = 2;"}]}),
        ws.context(),
    )
    .await;
    assert_eq!(read_code(&ws), "\u{feff}const a = 2;\n");
}
