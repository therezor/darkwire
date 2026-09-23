//! `edit` against a real temporary workspace.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use darkwire_core::ErrorKind;
use darkwire_tools::edit_tool;
use darkwire_tools::testkit::TestWorkspace;
use serde_json::json;

use super::common::{failure, fifo, link_in, link_out, run, run_on_fifo, text};

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

#[tokio::test]
async fn edit_refuses_a_fifo_rather_than_blocking_on_it() {
    let ws = TestWorkspace::new();
    let pipe = ws.root().join("pipe");
    fifo(&pipe);
    let result = run_on_fifo(
        &edit_tool(),
        json!({"path": "pipe", "oldText": "a", "newText": "b"}),
        ws.context(),
        &pipe,
    )
    .await;
    assert!(result.is_error);
    assert_eq!(result.kind, Some(ErrorKind::InvalidInput));
    assert!(
        result.content.contains("not a regular file"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn edit_counts_overlapping_occurrences_as_ambiguous() {
    let ws = TestWorkspace::new();
    code(&ws, "}\n}\n}");
    let result = failure(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "}\n}", "newText": "}"}),
        ws.context(),
    )
    .await;
    assert_eq!(result.kind, Some(ErrorKind::Conflict));
    assert!(
        result.content.contains("occurs 2 times"),
        "{}",
        result.content
    );
    assert_eq!(read_code(&ws), "}\n}\n}");
}

#[tokio::test]
async fn edit_refuses_a_file_too_large_to_hold_in_memory() {
    let ws = TestWorkspace::new();
    // Sparse, so the test costs no real disk writes.
    let file = fs::File::create(ws.root().join("code.ts")).unwrap();
    file.set_len(16 * 1024 * 1024 + 1).unwrap();
    let result = failure(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "a", "newText": "b"}),
        ws.context(),
    )
    .await;
    assert_eq!(result.kind, Some(ErrorKind::InvalidInput));
    assert!(result.content.contains("Use write"), "{}", result.content);
}

#[tokio::test]
async fn edit_writes_a_new_file_and_renames_it_over_the_old_one() {
    let ws = TestWorkspace::new();
    code(&ws, "const a = 1;\n");
    let path = ws.root().join("code.ts");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    let before = fs::metadata(&path).unwrap().ino();
    text(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "1", "newText": "2"}),
        ws.context(),
    )
    .await;
    let after = fs::metadata(&path).unwrap();
    assert_ne!(after.ino(), before);
    assert_eq!(after.permissions().mode() & 0o777, 0o666);
    assert_eq!(read_code(&ws), "const a = 2;\n");
    let names: Vec<_> = fs::read_dir(ws.root())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["code.ts"]);
}

#[tokio::test]
async fn edit_refuses_a_file_it_could_not_have_written_in_place() {
    let ws = TestWorkspace::new();
    code(&ws, "const a = 1;\n");
    let path = ws.root().join("code.ts");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    if fs::OpenOptions::new().write(true).open(&path).is_ok() {
        // Root ignores the mode, so there is nothing to observe.
        return;
    }
    let result = failure(
        &edit_tool(),
        json!({"path": "code.ts", "oldText": "1", "newText": "2"}),
        ws.context(),
    )
    .await;
    assert_eq!(result.kind, Some(ErrorKind::PermissionDenied));
    assert_eq!(read_code(&ws), "const a = 1;\n");
}

#[tokio::test]
async fn edit_refuses_a_file_behind_a_symlinked_directory_that_leads_out() {
    let ws = TestWorkspace::new();
    let elsewhere = link_out(&ws);
    let error = failure(
        &edit_tool(),
        json!({"path": "linked/secret.txt", "oldText": "stolen", "newText": "edited"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::JailEscape));
    assert_eq!(
        fs::read_to_string(elsewhere.join("secret.txt")).unwrap(),
        "stolen"
    );
}

#[tokio::test]
async fn edit_goes_through_a_symlinked_directory_that_stays_inside() {
    let ws = TestWorkspace::new();
    link_in(&ws);
    text(
        &edit_tool(),
        json!({"path": "alias/notes.md", "oldText": "inside", "newText": "edited"}),
        ws.context(),
    )
    .await;
    assert_eq!(
        fs::read_to_string(ws.root().join("real/notes.md")).unwrap(),
        "edited\n"
    );
    assert!(
        fs::symlink_metadata(ws.root().join("alias"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
