//! `find` against a real temporary workspace.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;
use std::os::unix::fs::symlink;
use std::time::{Duration, SystemTime};

use darkwire_core::ErrorKind;
use darkwire_tools::testkit::TestWorkspace;
use darkwire_tools::{FindRequest, find_blocking, find_tool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{failure, text};

fn tree(ws: &TestWorkspace) {
    fs::create_dir_all(ws.root().join("src/deep")).unwrap();
    fs::write(ws.root().join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(ws.root().join("src/deep/lib.rs"), "pub fn a() {}\n").unwrap();
    fs::write(ws.root().join("README.md"), "# readme\n").unwrap();
}

/// Sets one file's modification time, so the ordering test is not a race.
///
/// Counted from the epoch rather than from now: the test only needs two
/// timestamps in a known order, and the clock is not one of the things under
/// test here.
fn touched(ws: &TestWorkspace, name: &str, epoch_seconds: u64) {
    let file = fs::File::options()
        .write(true)
        .open(ws.root().join(name))
        .unwrap();
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(epoch_seconds);
    file.set_modified(when).unwrap();
}

#[tokio::test]
async fn find_matches_a_recursive_glob() {
    let ws = TestWorkspace::new();
    tree(&ws);
    let out = text(&find_tool(), json!({"pattern": "**/*.rs"}), ws.context()).await;
    assert!(out.contains("src/main.rs"), "{out}");
    assert!(out.contains("src/deep/lib.rs"), "{out}");
    assert!(!out.contains("README.md"), "{out}");
}

#[tokio::test]
async fn find_matches_a_bare_name_glob_at_any_depth() {
    let ws = TestWorkspace::new();
    tree(&ws);
    let out = text(&find_tool(), json!({"pattern": "*.rs"}), ws.context()).await;
    assert!(out.contains("src/deep/lib.rs"), "{out}");
}

#[tokio::test]
async fn find_keeps_a_glob_with_a_slash_at_the_depth_it_names() {
    let ws = TestWorkspace::new();
    tree(&ws);
    let out = text(&find_tool(), json!({"pattern": "src/*.rs"}), ws.context()).await;
    assert!(out.contains("src/main.rs"), "{out}");
    assert!(!out.contains("deep/lib.rs"), "{out}");
}

#[tokio::test]
async fn find_returns_the_most_recently_modified_file_first() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("old.txt"), "a").unwrap();
    fs::write(ws.root().join("new.txt"), "b").unwrap();
    touched(&ws, "old.txt", 1_000_000);
    touched(&ws, "new.txt", 2_000_000);

    let out = text(&find_tool(), json!({"pattern": "*.txt"}), ws.context()).await;
    let order: Vec<&str> = out.lines().collect();
    assert_eq!(order, vec!["new.txt", "old.txt"], "{out}");
}

#[tokio::test]
async fn find_skips_what_gitignore_skips() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join(".gitignore"), "build/\n").unwrap();
    fs::create_dir_all(ws.root().join("build")).unwrap();
    fs::write(ws.root().join("build/out.rs"), "x").unwrap();
    fs::write(ws.root().join("keep.rs"), "x").unwrap();

    let out = text(&find_tool(), json!({"pattern": "*.rs"}), ws.context()).await;
    assert!(out.contains("keep.rs"), "{out}");
    assert!(!out.contains("build/"), "{out}");
}

#[tokio::test]
async fn find_excludes_directories_and_symlinks() {
    let ws = TestWorkspace::new();
    fs::create_dir_all(ws.root().join("thing.rs")).unwrap();
    fs::write(ws.root().join("real.rs"), "x").unwrap();
    symlink(ws.root().join("real.rs"), ws.root().join("link.rs")).unwrap();

    let out = text(&find_tool(), json!({"pattern": "*.rs"}), ws.context()).await;
    assert_eq!(out.lines().collect::<Vec<_>>(), vec!["real.rs"], "{out}");
}

#[tokio::test]
async fn find_stops_at_the_limit_and_says_how_many_it_held_back() {
    let ws = TestWorkspace::new();
    for index in 0..12 {
        fs::write(ws.root().join(format!("f{index:02}.txt")), "x").unwrap();
    }
    let out = text(
        &find_tool(),
        json!({"pattern": "*.txt", "limit": 4}),
        ws.context(),
    )
    .await;
    assert_eq!(
        out.lines().filter(|line| line.starts_with('f')).count(),
        4,
        "{out}"
    );
    assert!(
        out.contains("[find: 8 more files not shown (limit=4)"),
        "{out}"
    );
}

#[tokio::test]
async fn find_says_so_rather_than_returning_nothing_when_no_file_matches() {
    let ws = TestWorkspace::new();
    tree(&ws);
    let out = text(&find_tool(), json!({"pattern": "*.zzz"}), ws.context()).await;
    assert_eq!(out, "No files match \"*.zzz\" under the workspace.");
}

#[tokio::test]
async fn find_searches_only_under_the_path_it_was_given() {
    let ws = TestWorkspace::new();
    tree(&ws);
    let out = text(
        &find_tool(),
        json!({"pattern": "*.rs", "path": "src/deep"}),
        ws.context(),
    )
    .await;
    assert_eq!(out.lines().collect::<Vec<_>>(), vec!["lib.rs"], "{out}");
}

#[tokio::test]
async fn find_clamps_a_path_that_tried_to_escape_and_says_where_it_looked() {
    let ws = TestWorkspace::new();
    fs::create_dir_all(ws.root().join("etc")).unwrap();
    fs::write(ws.root().join("etc/hosts"), "x").unwrap();
    let out = text(
        &find_tool(),
        json!({"pattern": "hosts", "path": "/etc"}),
        ws.context(),
    )
    .await;
    assert!(out.contains("hosts"), "{out}");
    assert!(out.contains("was resolved to \"etc\" inside it"), "{out}");
}

#[tokio::test]
async fn find_refuses_a_pattern_that_is_not_a_glob() {
    let ws = TestWorkspace::new();
    let failed = failure(&find_tool(), json!({"pattern": "a["}), ws.context()).await;
    assert_eq!(failed.kind, Some(ErrorKind::InvalidInput));
    assert!(failed.content.contains("not a valid glob"));
}

#[tokio::test]
async fn find_stops_on_a_cancelled_token_part_way_through_a_tree() {
    let ws = TestWorkspace::new();
    for index in 0..50 {
        fs::write(ws.root().join(format!("f{index}.txt")), "x").unwrap();
    }
    let token = CancellationToken::new();
    token.cancel();
    let failed = find_blocking(
        &FindRequest {
            jail: std::sync::Arc::clone(ws.jail()),
            root: ws.root().to_path_buf(),
            pattern: "*.txt".to_owned(),
            limit: 1000,
        },
        &token,
    )
    .expect_err("a cancelled search reports it");
    assert_eq!(failed.kind, ErrorKind::Aborted);
}

#[test]
fn find_drops_a_hit_whose_directory_was_swapped_to_lead_out_after_the_check() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("sub")).unwrap();
    let accepted = ws.jail().accept("sub").unwrap();
    let elsewhere = ws.outside().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("secret.txt"), "needle\n").unwrap();
    fs::remove_dir(ws.root().join("sub")).unwrap();
    symlink(&elsewhere, ws.root().join("sub")).unwrap();
    let report = find_blocking(
        &FindRequest {
            jail: std::sync::Arc::clone(ws.jail()),
            root: accepted.path,
            pattern: "*.txt".to_owned(),
            limit: 1000,
        },
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(report.paths.is_empty(), "{:?}", report.paths);
    assert!(report.unreadable >= 1);
}
