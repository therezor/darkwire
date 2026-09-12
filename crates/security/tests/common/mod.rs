//! Shared by every test file: fixture loading, temp workspaces and the error
//! kind of a result.

#![allow(
    dead_code,
    reason = "each test file uses a different subset of these helpers"
)]

use std::path::{Path, PathBuf};

use ghostai_core::Result;
use serde_json::Value;

/// The repository's `fixtures/` directory.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// A fixture file, parsed.
pub fn read_fixture(relative: &str) -> Value {
    let path = fixtures_dir().join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{relative} is not JSON: {error}"))
}

/// The `cases` array of a fixture.
pub fn cases(fixture: &Value) -> Vec<Value> {
    fixture["cases"]
        .as_array()
        .expect("fixture has cases")
        .clone()
}

/// The error kind as the wire spells it, or a sentinel when the call succeeded.
pub fn kind_of<T>(result: &Result<T>) -> String {
    match result {
        Ok(_) => "did-not-fail".to_owned(),
        Err(error) => error.kind.as_str().to_owned(),
    }
}

/// The error message, or a sentinel when the call succeeded.
pub fn message_of<T>(result: &Result<T>) -> String {
    match result {
        Ok(_) => "did-not-fail".to_owned(),
        Err(error) => error.message.clone(),
    }
}

/// A fresh temp directory whose path is already canonical.
///
/// macOS hands out `/var/folders/...`, a symlink to `/private/var/folders/...`;
/// anything that compares canonical paths against the raw one would see two
/// different places.
pub fn temp_base() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("ghostai-security-")
        .tempdir()
        .expect("temp dir");
    let canonical = std::fs::canonicalize(dir.path()).expect("canonical temp dir");
    (dir, canonical)
}

/// Writes `contents` at `path`, creating parents.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent dir");
    }
    std::fs::write(path, contents).expect("write file");
}

/// A symlink at `link` pointing at `target`, as written.
#[cfg(unix)]
pub fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("symlink");
}

/// Replaces `root` with `<root>` and `base` with `<base>`.
pub fn scrub(text: &str, root: &Path, base: &Path) -> String {
    text.replace(&root.to_string_lossy().into_owned(), "<root>")
        .replace(&base.to_string_lossy().into_owned(), "<base>")
}

/// A path as the fixtures spell it: forward slashes.
pub fn slashes(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}
