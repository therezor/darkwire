//! The two helpers every sandbox suite needs and neither owns.

#![allow(
    dead_code,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a shared harness is used by a subset of its consumers, and a fixture that will not load is a failing test either way"
)]

use std::path::Path;

use ghostai_core::{GhostError, Result};

/// The frozen wall clock every fixture starts at.
pub const NOW: i64 = 1_700_000_000_000;

/// Writes a file and everything above it.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// One error out of a result, or a failure naming what came instead.
pub fn err<T: std::fmt::Debug>(result: Result<T>) -> GhostError {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error,
    }
}

/// Polls `ready` until it holds or the deadline passes.
///
/// A condition rather than a fixed number of yields: a count passes on an idle
/// machine and fails on a loaded one, which is the flake this repo has already
/// paid for once.
pub async fn eventually(within: std::time::Duration, ready: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        if ready() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    ready()
}
