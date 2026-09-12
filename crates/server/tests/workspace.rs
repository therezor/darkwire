//! Reading the workspace over HTTP.
//!
//! Two properties are load-bearing here and neither is obvious from the
//! signatures. A listing goes back through the jail per entry, so a symlink
//! pointing out of the workspace never appears — advertising a file the jail
//! then refuses to open reads as a bug in the UI rather than as the refusal it
//! is. And paths in a response are workspace-relative, always: an absolute path
//! tells a client where the server keeps its files, and nothing above this
//! layer can use one.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;
use std::path::Path;

use ghostai_security::jail::{JailOptions, WorkspaceJail};
use ghostai_server::workspace::{inline_safe, list_directory, mime_type_for};

fn workspace() -> (tempfile::TempDir, WorkspaceJail) {
    let dir = tempfile::tempdir().unwrap();
    // The jail canonicalises its root, and a listing is handed a path the jail
    // returned; a root reached under an uncanonical name is a root the jail
    // does not recognise as its own.
    let jail = WorkspaceJail::new(JailOptions::new(dir.path())).unwrap();
    (dir, jail)
}

fn names(jail: &WorkspaceJail, directory: &Path) -> Vec<String> {
    list_directory(jail, directory)
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

#[test]
fn a_listing_puts_directories_first_then_sorts_by_name() {
    let (_dir, jail) = workspace();
    let root = jail.root().to_path_buf();
    fs::write(root.join("b.txt"), "b").unwrap();
    fs::write(root.join("a.txt"), "a").unwrap();
    fs::create_dir(root.join("z-dir")).unwrap();
    fs::create_dir(root.join("a-dir")).unwrap();

    assert_eq!(names(&jail, &root), ["a-dir", "z-dir", "a.txt", "b.txt"]);
}

#[test]
fn a_listing_sorts_case_insensitively() {
    let (_dir, jail) = workspace();
    let root = jail.root().to_path_buf();
    for name in ["Beta.txt", "alpha.txt", "Gamma.txt"] {
        fs::write(root.join(name), "x").unwrap();
    }

    // `Readme` belongs beside `readme`, not in a separate block of capitals
    // ahead of every lowercase name.
    assert_eq!(names(&jail, &root), ["alpha.txt", "Beta.txt", "Gamma.txt"]);
}

#[test]
fn a_path_in_a_listing_is_workspace_relative() {
    let (_dir, jail) = workspace();
    let root = jail.root().to_path_buf();
    fs::create_dir(root.join("notes")).unwrap();
    fs::write(root.join("notes/todo.md"), "todo").unwrap();

    let entries = list_directory(&jail, &root.join("notes"));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "notes/todo.md");
    assert!(!entries[0].path.starts_with('/'));
}

#[test]
#[cfg(unix)]
fn a_symlink_out_of_the_workspace_never_appears_in_a_listing() {
    let (_dir, jail) = workspace();
    let root = jail.root().to_path_buf();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "not yours").unwrap();
    fs::write(root.join("inside.txt"), "yours").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.join("escape.txt")).unwrap();

    // A prefix comparison would pass it: the path as written has a perfect
    // prefix. The jail resolves it through the filesystem, which does not.
    assert_eq!(names(&jail, &root), ["inside.txt"]);
}

#[test]
#[cfg(unix)]
fn an_entry_that_cannot_be_stated_is_dropped_rather_than_failing_the_listing() {
    let (_dir, jail) = workspace();
    let root = jail.root().to_path_buf();
    fs::write(root.join("real.txt"), "real").unwrap();
    // A dangling link is the same shape as a file a turn deleted between the
    // directory read and the stat: one missing row beats a failed listing.
    std::os::unix::fs::symlink(root.join("gone.txt"), root.join("dangling.txt")).unwrap();

    assert_eq!(names(&jail, &root), ["real.txt"]);
}

#[test]
fn a_directory_has_no_size_and_no_media_type() {
    let (_dir, jail) = workspace();
    let root = jail.root().to_path_buf();
    fs::create_dir(root.join("notes")).unwrap();

    let entries = list_directory(&jail, &root);
    assert!(entries[0].is_directory);
    assert_eq!(entries[0].size_bytes, 0);
    // No media type at all rather than a guess: a directory is not a document,
    // and the default type on one would invite a client to fetch it.
    assert_eq!(entries[0].mime_type, None);
}

#[test]
fn a_file_carries_its_size_and_media_type() {
    let (_dir, jail) = workspace();
    let root = jail.root().to_path_buf();
    fs::write(root.join("notes.md"), "hello").unwrap();

    let entries = list_directory(&jail, &root);
    assert!(!entries[0].is_directory);
    assert_eq!(entries[0].size_bytes, 5);
    assert_eq!(entries[0].mime_type.as_deref(), Some(mime_type_for("x.md")));
}

#[test]
fn a_directory_that_cannot_be_read_lists_as_empty() {
    let (_dir, jail) = workspace();
    let missing = jail.root().join("not-there");
    assert!(list_directory(&jail, &missing).is_empty());
}

#[test]
fn a_document_a_browser_would_execute_is_never_inline() {
    // An SVG can carry `<script>`, and served from this origin that script runs
    // with the session cookie attached. The workspace is a tree a language
    // model writes to, so "the agent wrote a file the user then opened" is a
    // realistic path rather than a contrived one.
    for path in [
        "a.svg", "a.html", "a.htm", "a.xhtml", "a.xml", "a.js", "a.mjs", "a.wasm",
    ] {
        assert!(!inline_safe(path), "{path}");
    }
}

#[test]
fn an_extension_is_matched_case_insensitively() {
    assert!(!inline_safe("DIAGRAM.SVG"));
    assert!(!inline_safe("page.HTML"));
}

#[test]
fn an_ordinary_document_is_inline() {
    for path in ["a.png", "a.txt", "a.md", "a.pdf", "a.jpg", "README"] {
        assert!(inline_safe(path), "{path}");
    }
}
