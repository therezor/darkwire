//! `read` against a real temporary workspace.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;
use std::os::unix::fs::symlink;

use darkwire_core::ErrorKind;
use darkwire_tools::read_tool;
use darkwire_tools::testkit::TestWorkspace;
use serde_json::json;

use crate::common::{failure, fifo, link_in, link_out, run_on_fifo, text};

/// `count` lines, each naming its own number, so a window can be checked.
fn numbered(count: u32) -> String {
    use std::fmt::Write as _;
    (1..=count).fold(String::new(), |mut out, line| {
        let _ = writeln!(out, "line {line}");
        out
    })
}

#[tokio::test]
async fn read_reads_a_workspace_file() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("notes.md"), "hello\nworld\n").unwrap();
    assert_eq!(
        text(&read_tool(), json!({"path": "notes.md"}), ws.context()).await,
        "hello\nworld\n"
    );
}

#[tokio::test]
async fn read_reads_a_file_in_a_subdirectory() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("src")).unwrap();
    fs::write(ws.root().join("src/index.ts"), "export {};").unwrap();
    assert_eq!(
        text(&read_tool(), json!({"path": "src/index.ts"}), ws.context()).await,
        "export {};"
    );
}

#[tokio::test]
async fn read_reports_a_missing_file_as_not_found_against_the_relative_path() {
    let ws = TestWorkspace::new();
    let error = failure(&read_tool(), json!({"path": "absent.md"}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::NotFound));
    assert_eq!(error.content, "absent.md does not exist.");
    assert!(!error.content.contains(ws.root().to_str().unwrap()));
}

#[tokio::test]
async fn read_refuses_a_directory_and_points_at_the_tool_that_handles_it() {
    let ws = TestWorkspace::new();
    fs::create_dir(ws.root().join("src")).unwrap();
    let error = failure(&read_tool(), json!({"path": "src"}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::InvalidInput));
    assert!(error.content.contains("ls"));
}

#[tokio::test]
async fn read_refuses_to_dump_a_binary_file_into_the_context() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("logo.png"), [0x89, 0x50, 0x00, 0x01, 0x02]).unwrap();
    let error = failure(&read_tool(), json!({"path": "logo.png"}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::InvalidInput));
    assert!(error.content.contains("binary"));
}

#[tokio::test]
async fn read_says_so_rather_than_returning_nothing_for_an_empty_file() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("empty.txt"), "").unwrap();
    assert!(
        text(&read_tool(), json!({"path": "empty.txt"}), ws.context())
            .await
            .contains("is empty")
    );
}

#[tokio::test]
async fn read_bounds_the_read_by_the_output_budget_rather_than_the_file_size() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("huge.txt"), "a".repeat(200_000)).unwrap();
    let result = text(
        &read_tool(),
        json!({"path": "huge.txt"}),
        &ws.with(|config| config.max_output_chars = 100),
    )
    .await;
    assert!(result.contains("showing the first 400 of 200000 bytes"));
    assert!(result.len() < 600);
}

#[tokio::test]
async fn read_returns_a_line_window() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("lines.txt"), "one\ntwo\nthree\nfour").unwrap();
    assert_eq!(
        text(
            &read_tool(),
            json!({"path": "lines.txt", "offset": 2, "limit": 2}),
            ws.context()
        )
        .await,
        "two\nthree\n\n[read: showing lines 2-3 of 4. Use offset=4 to continue.]"
    );
    // Models emit numbers as strings; the schema's numeric fields coerce them.
    assert_eq!(
        text(
            &read_tool(),
            json!({"path": "lines.txt", "offset": "2", "limit": "1"}),
            ws.context()
        )
        .await,
        "two\n\n[read: showing lines 2-2 of 4. Use offset=3 to continue.]"
    );
}

#[tokio::test]
async fn read_says_nothing_extra_when_the_window_reaches_the_end() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("lines.txt"), "one\ntwo\n").unwrap();
    assert_eq!(
        text(
            &read_tool(),
            json!({"path": "lines.txt", "offset": 2}),
            ws.context()
        )
        .await,
        "two\n"
    );
}

#[tokio::test]
async fn read_stops_at_two_thousand_lines_and_says_how_many_are_left() {
    let ws = TestWorkspace::new();
    let body = numbered(2500);
    fs::write(ws.root().join("long.txt"), body).unwrap();
    let out = text(&read_tool(), json!({"path": "long.txt"}), ws.context()).await;
    assert!(out.contains("line 2000\n"), "tail of the window");
    assert!(!out.contains("line 2001"), "past the window");
    assert!(
        out.ends_with("[read: 500 more lines in file. Use offset=2001 to continue.]"),
        "{}",
        &out[out.len() - 120..]
    );
}

#[tokio::test]
async fn read_reaches_a_line_far_past_the_byte_budget() {
    let ws = TestWorkspace::new();
    let body = numbered(4000);
    fs::write(ws.root().join("long.txt"), body).unwrap();
    let out = text(
        &read_tool(),
        json!({"path": "long.txt", "offset": 3900, "limit": 2}),
        &ws.with(|config| config.max_output_chars = 100),
    )
    .await;
    assert!(out.starts_with("line 3900\nline 3901"), "{out}");
}

#[tokio::test]
async fn read_reads_to_the_end_when_only_an_offset_is_given() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("lines.txt"), "one\ntwo\nthree").unwrap();
    assert_eq!(
        text(
            &read_tool(),
            json!({"path": "lines.txt", "offset": 3}),
            ws.context()
        )
        .await,
        "three"
    );
}

#[tokio::test]
async fn read_reports_an_offset_past_the_end_instead_of_returning_nothing() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("lines.txt"), "one\ntwo").unwrap();
    assert!(
        text(
            &read_tool(),
            json!({"path": "lines.txt", "offset": 99}),
            ws.context()
        )
        .await
        .contains("past the end")
    );
}

#[tokio::test]
async fn read_clamps_escapes_into_the_workspace_and_says_where_it_looked() {
    let ws = TestWorkspace::new();
    for (path, landed) in [
        ("../secret", "secret"),
        ("/etc/passwd", "etc/passwd"),
        ("~/.ssh/id_rsa", ".ssh/id_rsa"),
    ] {
        // The workspace is a chroot, so none of these is a refusal — each names
        // a file inside the workspace that happens not to exist. What matters
        // is that the model is told so.
        let error = failure(&read_tool(), json!({"path": path}), ws.context()).await;
        assert_eq!(error.kind, Some(ErrorKind::NotFound), "{path}");
        assert!(error.content.contains(landed), "{path}: {}", error.content);
        assert!(
            error.content.contains("The workspace is the root"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn read_says_where_it_read_from_when_a_clamped_path_does_exist() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("passwd"), "not the real one").unwrap();
    let result = text(&read_tool(), json!({"path": "/passwd"}), ws.context()).await;
    assert!(result.contains("not the real one"));
    assert!(result.contains(r#""/passwd" was resolved to "passwd""#));
}

#[tokio::test]
async fn read_rejects_a_symlink_pointing_out_of_the_workspace() {
    let ws = TestWorkspace::new();
    let outside = ws.outside().join("outside.txt");
    fs::write(&outside, "stolen").unwrap();
    symlink(&outside, ws.root().join("link.txt")).unwrap();
    let error = failure(&read_tool(), json!({"path": "link.txt"}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::JailEscape));
}

#[tokio::test]
async fn read_refuses_a_fifo_rather_than_blocking_on_it() {
    let ws = TestWorkspace::new();
    let pipe = ws.root().join("pipe");
    fifo(&pipe);
    let result = run_on_fifo(&read_tool(), json!({"path": "pipe"}), ws.context(), &pipe).await;
    assert!(result.is_error);
    assert_eq!(result.kind, Some(ErrorKind::InvalidInput));
    assert!(
        result.content.contains("not a regular file"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn read_refuses_a_file_behind_a_symlinked_directory_that_leads_out() {
    let ws = TestWorkspace::new();
    link_out(&ws);
    let error = failure(
        &read_tool(),
        json!({"path": "linked/secret.txt"}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::JailEscape));
    assert!(!error.content.contains("stolen"));
}

#[tokio::test]
async fn read_follows_a_symlinked_directory_that_stays_inside() {
    let ws = TestWorkspace::new();
    link_in(&ws);
    let result = text(
        &read_tool(),
        json!({"path": "alias/notes.md"}),
        ws.context(),
    )
    .await;
    assert!(result.contains("inside"), "{result}");
}
