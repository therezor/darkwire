//! `write` against a real temporary workspace.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;
use std::os::unix::fs::symlink;

use darkwire_core::ErrorKind;
use darkwire_tools::testkit::TestWorkspace;
use darkwire_tools::write_tool;
use serde_json::json;

use crate::common::{failure, fifo, link_in, link_out, run, run_on_fifo, text};

#[tokio::test]
async fn write_writes_a_file_and_reports_its_size() {
    let ws = TestWorkspace::new();
    let result = text(
        &write_tool(),
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
async fn write_creates_parent_directories() {
    let ws = TestWorkspace::new();
    text(
        &write_tool(),
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
async fn write_replaces_existing_contents() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("out.txt"), "old").unwrap();
    text(
        &write_tool(),
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
async fn write_reports_writing_over_a_directory_as_invalid_input() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("src")).unwrap();
    let error = failure(
        &write_tool(),
        json!({"path": "src", "content": "x"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn write_clamps_a_write_that_tried_to_escape() {
    let ws = TestWorkspace::new();
    let result = text(
        &write_tool(),
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
async fn write_still_refuses_a_symlink_that_leads_out_of_the_workspace() {
    let ws = TestWorkspace::new();
    let outside = ws.outside().join("target.txt");
    fs::write(&outside, "stolen").unwrap();
    symlink(&outside, ws.root().join("link.txt")).unwrap();
    let error = failure(
        &write_tool(),
        json!({"path": "link.txt", "content": "x"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::JailEscape));
    assert_eq!(fs::read_to_string(&outside).unwrap(), "stolen");
}

#[tokio::test]
async fn write_refuses_a_dangling_symlink_rather_than_creating_its_target_outside() {
    let ws = TestWorkspace::new();
    let outside = ws.outside().join("planted.txt");
    symlink(&outside, ws.root().join("decoy.txt")).unwrap();
    let error = failure(
        &write_tool(),
        json!({"path": "decoy.txt", "content": "x"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::JailEscape));
    assert!(!outside.exists());
}

#[tokio::test]
async fn write_carries_the_byte_count_for_the_audit_log() {
    let ws = TestWorkspace::new();
    let result = run(
        &write_tool(),
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

#[tokio::test]
async fn write_refuses_a_fifo_rather_than_blocking_on_it() {
    let ws = TestWorkspace::new();
    let pipe = ws.root().join("pipe");
    fifo(&pipe);
    let result = run_on_fifo(
        &write_tool(),
        json!({"path": "pipe", "content": "x"}),
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
async fn write_refuses_a_file_behind_a_symlinked_directory_that_leads_out() {
    let ws = TestWorkspace::new();
    let elsewhere = link_out(&ws);
    for path in [
        "linked/secret.txt",
        "linked/new.txt",
        "linked/deeper/new.txt",
    ] {
        let error = failure(
            &write_tool(),
            json!({"path": path, "content": "x"}),
            ws.context(),
        )
        .await;
        assert_eq!(error.kind, Some(ErrorKind::JailEscape), "{path}");
    }
    assert_eq!(
        fs::read_to_string(elsewhere.join("secret.txt")).unwrap(),
        "stolen"
    );
    assert!(!elsewhere.join("new.txt").exists());
    assert!(!elsewhere.join("deeper").exists());
}

#[tokio::test]
async fn write_goes_through_a_symlinked_directory_that_stays_inside() {
    let ws = TestWorkspace::new();
    link_in(&ws);
    text(
        &write_tool(),
        json!({"path": "alias/fresh/new.txt", "content": "x"}),
        ws.context(),
    )
    .await;
    assert_eq!(
        fs::read_to_string(ws.root().join("real/fresh/new.txt")).unwrap(),
        "x"
    );
}
