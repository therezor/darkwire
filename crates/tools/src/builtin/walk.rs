//! The one directory walk `grep` and `find` share.
//!
//! Everything interesting about it is a refusal. The walker never follows a
//! symlink and never descends one, so a link pointing out of the workspace is
//! an entry the search can see and not a door it can walk through. It never
//! reads an ignore file above the search root, and never the host's global
//! gitignore, so what the search skips depends on the workspace alone and two
//! machines agree about it. It sorts, because a model comparing two runs of the
//! same search should not have to wonder whether the filesystem reordered them.
//!
//! `.gitignore` *is* respected, which is the one place these two tools differ
//! from `ls` on purpose. `ls` hides nothing because it answers "what is in this
//! directory", and a listing that omits things teaches the model the directory
//! is empty. A search answers "where is this code", and `target/` holding nine
//! thousand copies of the answer is noise that costs the budget the real hits
//! needed. Dotfiles are still searched: `.github` and `.env` are places code
//! lives, and only `.git` itself is skipped.

use std::path::{Path, PathBuf};

use darkwire_core::{Result, WireError};
use ignore::WalkBuilder;
use tokio_util::sync::CancellationToken;

/// Files above this are not searched.
///
/// A 16 MiB source file does not exist; a 16 MiB minified bundle, database dump
/// or vendored blob does, and reading one costs the whole walk's time for a
/// result no model can use.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// How a walk ended.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WalkTally {
    /// Entries that could not be read, counted rather than raised: one
    /// unreadable file in a tree is not a reason to answer nothing.
    pub unreadable: usize,
}

/// The walker both tools use, rooted at a path the jail has already accepted.
///
/// Takes the canonical path out of [`darkwire_security::JailAccept`], never a
/// model-supplied string.
pub fn walker(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(false)
        .hidden(false)
        .parents(false)
        .require_git(false)
        .git_global(false)
        .git_exclude(true)
        .git_ignore(true)
        .ignore(true)
        .same_file_system(true)
        .max_filesize(Some(MAX_FILE_BYTES))
        .filter_entry(|entry| entry.file_name() != ".git")
        .sort_by_file_path(Path::cmp);
    builder
}

/// Every regular file under `root`, in path order, cancellation honoured.
///
/// A symlink is skipped whatever it points at. The depth-0 entry is the root
/// itself and is kept only when the root is a file, which is how both tools
/// accept a single path as well as a directory.
pub fn files(
    root: &Path,
    token: &CancellationToken,
    name: &'static str,
) -> Result<(Vec<PathBuf>, WalkTally)> {
    let mut found = Vec::new();
    let mut tally = WalkTally::default();
    for entry in walker(root).build() {
        if token.is_cancelled() {
            return Err(WireError::aborted(name));
        }
        let Ok(entry) = entry else {
            tally.unreadable = tally.unreadable.saturating_add(1);
            continue;
        };
        // `file_type` is `None` only for stdin, which this walk never has.
        let Some(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() || !kind.is_file() {
            continue;
        }
        found.push(entry.into_path());
    }
    Ok((found, tally))
}
