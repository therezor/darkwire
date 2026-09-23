#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::io;

use darkwire_core::ErrorKind;
use darkwire_tools::builtin::shared::{fs_failure, in_root, open_options, root_failure};
use darkwire_tools::format_bytes;
use darkwire_tools::testkit::TestWorkspace;
use nix::errno::Errno;
use serde_json::json;

#[test]
fn maps_a_kind_to_a_taxonomy_kind_and_a_workspace_relative_message() {
    let error = fs_failure(
        &io::Error::from(io::ErrorKind::PermissionDenied),
        "notes.md",
        "",
    );
    assert_eq!(error.kind, ErrorKind::PermissionDenied);
    assert_eq!(
        error.message,
        "notes.md is not readable or writable by this process."
    );
    assert_eq!(error.details.get("path"), Some(&json!("notes.md")));
    assert_eq!(error.details.get("code"), Some(&json!("PermissionDenied")));
}

#[test]
fn maps_each_recognised_kind() {
    let cases = [
        (
            io::ErrorKind::NotFound,
            ErrorKind::NotFound,
            "does not exist",
        ),
        (
            io::ErrorKind::NotADirectory,
            ErrorKind::NotFound,
            "not a directory",
        ),
        (
            io::ErrorKind::IsADirectory,
            ErrorKind::InvalidInput,
            "is a directory",
        ),
        (
            io::ErrorKind::ReadOnlyFilesystem,
            ErrorKind::PermissionDenied,
            "read-only",
        ),
        (io::ErrorKind::StorageFull, ErrorKind::Storage, "full"),
        (
            io::ErrorKind::InvalidFilename,
            ErrorKind::InvalidInput,
            "too long",
        ),
    ];
    for (io_kind, kind, phrase) in cases {
        let error = fs_failure(&io::Error::from(io_kind), "x", "");
        assert_eq!(error.kind, kind, "{io_kind:?}");
        assert!(error.message.contains(phrase), "{}", error.message);
    }
}

#[test]
fn recognises_a_symlink_loop_by_errno() {
    let error = fs_failure(&io::Error::from_raw_os_error(Errno::ELOOP as i32), "x", "");
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("symlink loop"));
}

#[test]
fn appends_the_clamp_note_to_the_sentence() {
    let error = fs_failure(
        &io::Error::from(io::ErrorKind::NotFound),
        "etc/passwd",
        " The workspace is the root.",
    );
    assert_eq!(
        error.message,
        "etc/passwd does not exist. The workspace is the root."
    );
}

#[test]
fn falls_back_to_the_tool_kind_for_an_unrecognised_failure() {
    let error = fs_failure(&io::Error::from(io::ErrorKind::CrossesDevices), "x", "");
    assert_eq!(error.kind, ErrorKind::Tool);
    assert!(error.message.contains("CrossesDevices"));
}

#[test]
fn renders_bytes_readably() {
    let cases: [(u64, &str); 7] = [
        (0, "0 B"),
        (1023, "1023 B"),
        (1024, "1.0 KB"),
        (10 * 1024, "10 KB"),
        (1024 * 1024, "1.0 MB"),
        (3 * 1024_u64.pow(4), "3.0 TB"),
        (4096 * 1024_u64.pow(4), "4096 TB"),
    ];
    for (bytes, expected) in cases {
        assert_eq!(format_bytes(bytes), expected, "{bytes}");
    }
}

#[test]
fn root_failure_names_an_escape_as_the_jail_would() {
    let escape = io::Error::new(io::ErrorKind::PermissionDenied, "led outside");
    let error = root_failure(&escape, "/linked/x", "linked/x", "");
    assert_eq!(error.kind, ErrorKind::JailEscape);
    assert_eq!(
        error.message,
        "Path resolves outside the workspace: /linked/x"
    );
    assert_eq!(error.details.get("rejection"), Some(&json!("outside_root")));

    let denied = io::Error::from_raw_os_error(Errno::EACCES as i32);
    let error = root_failure(&denied, "x", "x", "");
    assert_eq!(error.kind, ErrorKind::PermissionDenied);
}

#[tokio::test]
async fn in_root_refuses_a_directory_swapped_for_a_symlink_after_the_jail_looked() {
    let ws = TestWorkspace::new();
    std::fs::create_dir(ws.root().join("sub")).unwrap();
    std::fs::write(ws.root().join("sub/notes.md"), "mine").unwrap();
    let accepted = ws.jail().accept("sub/notes.md").unwrap();

    let elsewhere = ws.outside().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("notes.md"), "stolen").unwrap();
    std::fs::remove_dir_all(ws.root().join("sub")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, ws.root().join("sub")).unwrap();

    let opened = in_root(ws.jail(), &accepted, "read", |root, inside| {
        let mut options = open_options();
        options.read(true);
        root.open_with(inside, &options).map(|_| ())
    })
    .await
    .unwrap();
    let error = opened.expect_err("the swap leads out of the workspace");
    let refusal = root_failure(&error, "sub/notes.md", "sub/notes.md", "");
    assert_eq!(refusal.kind, ErrorKind::JailEscape);
}
