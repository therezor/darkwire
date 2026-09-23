//! `ls` against a real temporary workspace.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use darkwire_core::ErrorKind;
use darkwire_tools::ls_tool;
use darkwire_tools::testkit::TestWorkspace;
use serde_json::json;

use crate::common::{failure, run, text};

#[tokio::test]
async fn ls_lists_directories_first_then_files_with_sizes() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("src")).unwrap();
    fs::write(ws.root().join("a.txt"), "aaa").unwrap();
    fs::write(ws.root().join("b.txt"), "bb").unwrap();
    let result = text(&ls_tool(), json!({}), ws.context()).await;
    assert_eq!(
        result.split('\n').collect::<Vec<_>>(),
        vec!["src/", "a.txt (3 B)", "b.txt (2 B)"]
    );
}

#[tokio::test]
async fn ls_defaults_to_the_workspace_root_and_hides_nothing() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("node_modules")).unwrap();
    fs::write(ws.root().join(".env"), "SECRET=1").unwrap();
    let result = text(&ls_tool(), json!({}), ws.context()).await;
    assert!(result.contains("node_modules/"));
    assert!(result.contains(".env"));
}

#[tokio::test]
async fn ls_walks_subdirectories_when_asked() {
    let ws = TestWorkspace::new();
    fs::create_dir_all(ws.root().join("src/deep")).unwrap();
    fs::write(ws.root().join("src/deep/x.ts"), "x").unwrap();
    let flat = text(&ls_tool(), json!({"path": "."}), ws.context()).await;
    assert!(!flat.contains("x.ts"));
    let result = text(
        &ls_tool(),
        json!({"path": ".", "recursive": true}),
        ws.context(),
    )
    .await;
    assert!(result.contains("src/deep/x.ts"));
    assert!(result.contains("src/deep/"));
}

#[tokio::test]
async fn ls_caps_the_listing_and_says_how_much_it_dropped() {
    let ws = TestWorkspace::new();
    for index in 0..10 {
        fs::write(ws.root().join(format!("f{index}.txt")), "x").unwrap();
    }
    let result = text(
        &ls_tool(),
        json!({"path": ".", "maxEntries": 3}),
        ws.context(),
    )
    .await;
    assert_eq!(result.split('\n').count(), 4);
    assert!(result.contains("7 more entries not shown"));
}

#[tokio::test]
async fn ls_says_a_directory_is_empty_and_reports_a_missing_one() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("empty")).unwrap();
    assert!(
        text(&ls_tool(), json!({"path": "empty"}), ws.context())
            .await
            .contains("is empty")
    );
    let error = failure(&ls_tool(), json!({"path": "nope"}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::NotFound));
}

#[tokio::test]
async fn ls_clamps_a_listing_that_tried_to_escape_back_to_the_workspace_root() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("inside.txt"), "x").unwrap();
    fs::write(ws.outside().join("outside-the-jail.txt"), "x").unwrap();
    let listing = text(&ls_tool(), json!({"path": ".."}), ws.context()).await;
    assert!(listing.contains("inside.txt"));
    assert!(!listing.contains("outside-the-jail.txt"));
    assert!(listing.contains("[ls:"));
}

#[tokio::test]
async fn ls_marks_an_unreadable_entry_rather_than_failing_the_whole_listing() {
    let ws = TestWorkspace::new();
    symlink(ws.root().join("nowhere"), ws.root().join("broken")).unwrap();
    fs::write(ws.root().join("fine.txt"), "x").unwrap();
    let result = text(&ls_tool(), json!({}), ws.context()).await;
    assert!(result.contains("broken (unreadable)"));
    assert!(result.contains("fine.txt (1 B)"));
}

#[tokio::test]
async fn ls_skips_an_unreadable_directory_and_lists_the_rest() {
    let ws = TestWorkspace::new();
    let locked = ws.root().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("inside.txt"), "x").unwrap();
    fs::write(ws.root().join("fine.txt"), "x").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read_dir(&locked).is_ok();
    let result = run(
        &ls_tool(),
        json!({"path": ".", "recursive": true}),
        ws.context(),
    )
    .await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        // Root reads through the mode, so there is nothing to observe.
        return;
    }
    assert!(!result.is_error, "{}", result.content);
    assert!(
        result.content.contains("fine.txt (1 B)"),
        "{}",
        result.content
    );
    assert!(result.content.contains("locked/"), "{}", result.content);
    assert!(
        result.content.contains("skipped 1 unreadable entries"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn ls_counts_what_a_recursive_walk_left_past_the_cap() {
    let ws = TestWorkspace::new();
    for folder in ["a", "b"] {
        fs::create_dir(ws.root().join(folder)).unwrap();
        for index in 0..3 {
            fs::write(ws.root().join(folder).join(format!("f{index}")), "x").unwrap();
        }
    }
    let result = text(
        &ls_tool(),
        json!({"path": ".", "recursive": true, "maxEntries": 3}),
        ws.context(),
    )
    .await;
    assert_eq!(
        result.split('\n').collect::<Vec<_>>(),
        vec![
            "a/",
            "a/f0 (1 B)",
            "a/f1 (1 B)",
            "… 5 more entries not shown (maxEntries=3)."
        ]
    );
}
