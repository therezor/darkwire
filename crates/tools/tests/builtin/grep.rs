//! `grep` against a real temporary workspace.

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
use darkwire_tools::{GrepMode, GrepRequest, grep_blocking, grep_tool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{failure, text};

/// Two source files and a nested one, enough for path, glob and mode tests.
fn tree(ws: &TestWorkspace) {
    fs::create_dir_all(ws.root().join("src/deep")).unwrap();
    fs::write(
        ws.root().join("src/main.rs"),
        "fn main() {\n    let total = 1;\n    println!(\"{total}\");\n}\n",
    )
    .unwrap();
    fs::write(
        ws.root().join("src/deep/lib.rs"),
        "pub fn total() -> u8 {\n    2\n}\n",
    )
    .unwrap();
    fs::write(ws.root().join("notes.md"), "the total is written here\n").unwrap();
}

#[tokio::test]
async fn grep_returns_each_matching_line_with_its_path_and_number() {
    let ws = TestWorkspace::new();
    tree(&ws);
    let out = text(&grep_tool(), json!({"pattern": "total"}), ws.context()).await;
    assert!(out.contains("src/main.rs:2: "), "{out}");
    assert!(out.contains("src/deep/lib.rs:1: "), "{out}");
    assert!(out.contains("notes.md:1: "), "{out}");
}

#[tokio::test]
async fn grep_returns_only_paths_in_files_mode_and_counts_in_count_mode() {
    let ws = TestWorkspace::new();
    tree(&ws);
    let tool = grep_tool();
    let files = text(
        &tool,
        json!({"pattern": "total", "mode": "files"}),
        ws.context(),
    )
    .await;
    assert!(files.lines().any(|line| line == "notes.md"), "{files}");
    assert!(!files.contains(": "), "{files}");

    let counts = text(
        &tool,
        json!({"pattern": "total", "mode": "count"}),
        ws.context(),
    )
    .await;
    assert!(counts.contains("src/main.rs: 2"), "{counts}");
    assert!(counts.contains("notes.md: 1"), "{counts}");
}

#[tokio::test]
async fn grep_treats_the_pattern_as_a_regex_unless_literal_is_set() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("a.txt"), "abc\na.c\n").unwrap();
    let tool = grep_tool();
    let regex = text(&tool, json!({"pattern": "a.c"}), ws.context()).await;
    assert!(regex.contains("a.txt:1: abc"), "{regex}");

    let literal = text(
        &tool,
        json!({"pattern": "a.c", "literal": true}),
        ws.context(),
    )
    .await;
    assert!(literal.contains("a.txt:2: a.c"), "{literal}");
    assert!(!literal.contains("abc"), "{literal}");
}

#[tokio::test]
async fn grep_matches_regardless_of_case_when_asked() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("a.txt"), "Total\n").unwrap();
    let tool = grep_tool();
    let sensitive = text(&tool, json!({"pattern": "total"}), ws.context()).await;
    assert!(sensitive.starts_with("No matches"), "{sensitive}");

    let insensitive = text(
        &tool,
        json!({"pattern": "total", "ignoreCase": true}),
        ws.context(),
    )
    .await;
    assert!(insensitive.contains("a.txt:1: Total"), "{insensitive}");
}

#[tokio::test]
async fn grep_marks_context_lines_differently_from_matches() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let out = text(
        &grep_tool(),
        json!({"pattern": "two", "context": 1}),
        ws.context(),
    )
    .await;
    assert!(out.contains("a.txt-1- one"), "{out}");
    assert!(out.contains("a.txt:2: two"), "{out}");
    assert!(out.contains("a.txt-3- three"), "{out}");
}

#[tokio::test]
async fn grep_limits_the_search_to_a_glob_when_one_is_given() {
    let ws = TestWorkspace::new();
    tree(&ws);
    let out = text(
        &grep_tool(),
        json!({"pattern": "total", "glob": "*.rs"}),
        ws.context(),
    )
    .await;
    assert!(out.contains("src/main.rs"), "{out}");
    assert!(!out.contains("notes.md"), "{out}");
}

#[tokio::test]
async fn grep_searches_one_file_when_the_path_is_a_file() {
    let ws = TestWorkspace::new();
    tree(&ws);
    let out = text(
        &grep_tool(),
        json!({"pattern": "total", "path": "notes.md"}),
        ws.context(),
    )
    .await;
    assert!(out.contains("notes.md:1: "), "{out}");
    assert!(!out.contains("src/"), "{out}");
}

#[tokio::test]
async fn grep_skips_what_gitignore_skips_and_skips_dot_git_itself() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir_all(ws.root().join("ignored")).unwrap();
    fs::create_dir_all(ws.root().join(".git")).unwrap();
    fs::write(ws.root().join("ignored/a.txt"), "needle\n").unwrap();
    fs::write(ws.root().join(".git/config"), "needle\n").unwrap();
    fs::write(ws.root().join(".env"), "needle\n").unwrap();

    let out = text(&grep_tool(), json!({"pattern": "needle"}), ws.context()).await;
    assert!(out.contains(".env:1: needle"), "{out}");
    assert!(!out.contains("ignored/"), "{out}");
    assert!(!out.contains(".git/"), "{out}");
}

#[tokio::test]
async fn grep_reports_a_binary_file_as_skipped_rather_than_searching_it() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("a.bin"), b"needle\0more needle\n").unwrap();
    fs::write(ws.root().join("a.txt"), "needle\n").unwrap();
    let out = text(&grep_tool(), json!({"pattern": "needle"}), ws.context()).await;
    assert!(out.contains("a.txt:1: needle"), "{out}");
    assert!(out.contains("[grep: skipped 1 binary"), "{out}");
}

#[tokio::test]
async fn grep_says_so_rather_than_returning_nothing_when_there_are_no_matches() {
    let ws = TestWorkspace::new();
    tree(&ws);
    let out = text(&grep_tool(), json!({"pattern": "zzz"}), ws.context()).await;
    assert_eq!(out, "No matches for \"zzz\" in the workspace.");
}

#[tokio::test]
async fn grep_stops_at_the_limit_and_says_how_to_get_past_it() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("a.txt"), "needle\n".repeat(20)).unwrap();
    let out = text(
        &grep_tool(),
        json!({"pattern": "needle", "limit": 5}),
        ws.context(),
    )
    .await;
    assert_eq!(out.lines().filter(|line| line.contains(":1")).count(), 1);
    assert_eq!(
        out.lines()
            .filter(|line| line.starts_with("a.txt:"))
            .count(),
        5,
        "{out}"
    );
    assert!(out.contains("[grep: limit of 5 matches reached"), "{out}");
}

#[tokio::test]
async fn grep_clips_a_very_long_line_and_says_how_much_it_cut() {
    let ws = TestWorkspace::new();
    let long = format!("needle{}\n", "x".repeat(900));
    fs::write(ws.root().join("a.txt"), long).unwrap();
    let out = text(&grep_tool(), json!({"pattern": "needle"}), ws.context()).await;
    assert!(out.contains("[+406 chars]"), "{out}");
}

#[tokio::test]
async fn grep_does_not_follow_a_symlink_that_leads_out_of_the_workspace() {
    let ws = TestWorkspace::new();
    let outside = ws.outside().join("secret.txt");
    fs::write(&outside, "needle\n").unwrap();
    symlink(&outside, ws.root().join("link.txt")).unwrap();
    fs::write(ws.root().join("inside.txt"), "needle\n").unwrap();

    let out = text(&grep_tool(), json!({"pattern": "needle"}), ws.context()).await;
    assert!(out.contains("inside.txt:1: needle"), "{out}");
    assert!(!out.contains("link.txt"), "{out}");
}

#[tokio::test]
async fn grep_clamps_a_path_that_tried_to_escape_and_says_where_it_looked() {
    let ws = TestWorkspace::new();
    fs::create_dir_all(ws.root().join("etc")).unwrap();
    fs::write(ws.root().join("etc/hosts"), "needle\n").unwrap();
    let out = text(
        &grep_tool(),
        json!({"pattern": "needle", "path": "/etc"}),
        ws.context(),
    )
    .await;
    assert!(out.contains("hosts:1: needle"), "{out}");
    assert!(out.contains("was resolved to \"etc\" inside it"), "{out}");
}

#[tokio::test]
async fn grep_refuses_a_pattern_that_is_not_a_regex() {
    let ws = TestWorkspace::new();
    let failed = failure(&grep_tool(), json!({"pattern": "("}), ws.context()).await;
    assert_eq!(failed.kind, Some(ErrorKind::InvalidInput));
    assert!(failed.content.contains("not a valid regular expression"));
}

#[tokio::test]
async fn grep_refuses_a_glob_that_is_not_a_glob() {
    let ws = TestWorkspace::new();
    let failed = failure(
        &grep_tool(),
        json!({"pattern": "a", "glob": "["}),
        ws.context(),
    )
    .await;
    assert_eq!(failed.kind, Some(ErrorKind::InvalidInput));
    assert!(failed.content.contains("not a valid pattern"));
}

#[tokio::test]
async fn grep_refuses_more_context_than_it_will_render() {
    let ws = TestWorkspace::new();
    let failed = failure(
        &grep_tool(),
        json!({"pattern": "a", "context": 99}),
        ws.context(),
    )
    .await;
    assert_eq!(failed.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn grep_stops_on_a_cancelled_token_part_way_through_a_tree() {
    let ws = TestWorkspace::new();
    for index in 0..50 {
        fs::write(ws.root().join(format!("f{index}.txt")), "needle\n").unwrap();
    }
    let token = CancellationToken::new();
    token.cancel();
    let failed = grep_blocking(
        &GrepRequest {
            jail: std::sync::Arc::clone(ws.jail()),
            root: ws.root().to_path_buf(),
            pattern: "needle".to_owned(),
            glob: None,
            ignore_case: false,
            literal: false,
            context: 0,
            mode: GrepMode::Matches,
            limit: 100,
        },
        &token,
    )
    .expect_err("a cancelled search reports it");
    assert_eq!(failed.kind, ErrorKind::Aborted);
}

#[test]
fn grep_reads_nothing_behind_a_directory_swapped_to_lead_out_after_the_check() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("sub")).unwrap();
    let accepted = ws.jail().accept("sub").unwrap();
    let elsewhere = ws.outside().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("secret.txt"), "needle\n").unwrap();
    fs::remove_dir(ws.root().join("sub")).unwrap();
    symlink(&elsewhere, ws.root().join("sub")).unwrap();
    let report = grep_blocking(
        &GrepRequest {
            jail: std::sync::Arc::clone(ws.jail()),
            root: accepted.path,
            pattern: "needle".to_owned(),
            glob: None,
            ignore_case: false,
            literal: false,
            context: 0,
            mode: GrepMode::Matches,
            limit: 100,
        },
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(report.lines.is_empty(), "{:?}", report.lines);
    assert!(report.unreadable >= 1);
}
