//! Reading the workspace over HTTP.
//!
//! Every path in this module has already been through [`WorkspaceJail`], and
//! none of it re-derives one: the jail returns the canonical absolute path it
//! actually verified, and using anything else — re-joining the input, resolving
//! it a second time — is how a check and the filesystem call it guards end up
//! looking at two different files.
//!
//! [`mime_type_for`] and `read_text` live in `ghostai-core` and are re-exported
//! here so this module stays the one place the HTTP layer reads from. They had
//! to move down: the agent loop turns an attached file into something a model
//! can read and has to reach the same verdict this route does, and
//! `ghostai-agent` cannot depend on a server.

use std::path::Path;

use ghostai_protocol::rest::FileEntry;
use ghostai_security::jail::WorkspaceJail;

pub use ghostai_core::workspace_files::{
    DEFAULT_MIME_TYPE, MAX_TEXT_BYTES, WorkspaceText, mime_type_for, read_text,
};

/// Types a browser executes in the origin that served them.
///
/// An SVG is a document: it can carry `<script>`, and served as `image/svg+xml`
/// from this origin that script runs with the session cookie attached. The
/// workspace is a tree a language model writes to, so "the agent wrote a file
/// the user then opened" is a realistic path to it rather than a contrived one.
/// These are served as a download instead — the file is still retrievable, it
/// simply does not execute.
const NEVER_INLINE: [&str; 8] = ["svg", "html", "htm", "xhtml", "xml", "js", "mjs", "wasm"];

/// Whether a browser may render this file in the page rather than download it.
pub fn inline_safe(path: &str) -> bool {
    let extension = Path::new(path)
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase());
    match extension {
        Some(extension) => !NEVER_INLINE.contains(&extension.as_str()),
        None => true,
    }
}

/// One entry, for a path a caller already has metadata for.
///
/// Returns `None` when the path is not inside the jail's root, which cannot
/// happen for a path the jail itself returned and is not worth a failure shape
/// of its own.
pub fn entry_at(
    jail: &WorkspaceJail,
    absolute: &Path,
    metadata: &std::fs::Metadata,
) -> Option<FileEntry> {
    let name = absolute
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    entry_for(jail, absolute, &name, metadata)
}

fn entry_for(
    jail: &WorkspaceJail,
    absolute: &Path,
    name: &str,
    metadata: &std::fs::Metadata,
) -> Option<FileEntry> {
    let is_directory = metadata.is_dir();
    Some(FileEntry {
        // Workspace-relative, always: an absolute path in a response tells a
        // client where the server keeps its files, and nothing above this layer
        // can use one.
        path: jail.relative(absolute).ok()?,
        name: name.to_owned(),
        is_directory,
        size_bytes: if is_directory { 0 } else { metadata.len() },
        modified_at_ms: modified_at_ms(metadata),
        mime_type: if is_directory {
            None
        } else {
            Some(mime_type_for(&absolute.to_string_lossy()).to_owned())
        },
    })
}

/// Modification time in epoch milliseconds, floored.
///
/// A file whose timestamp predates the epoch, or whose filesystem has none,
/// reports zero: the field exists so a client can sort and show "modified", and
/// no answer it could give instead is more useful than the oldest one.
fn modified_at_ms(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// One directory, directories first and then by name.
///
/// Not recursive, and not sorted by modification time: a file browser is
/// navigated, and the order a navigator needs is the one that does not move
/// when a file is written.
///
/// Every entry goes back through [`WorkspaceJail::check`], which is what drops
/// a symlink pointing out of the workspace. A prefix comparison would not: it
/// reads the path as written, and `<workspace>/escape -> /etc` has a perfect
/// prefix. Listing it would advertise a file the jail then refuses to open,
/// which reads as a bug in the UI rather than as the refusal it is.
pub fn list_directory(jail: &WorkspaceJail, directory: &Path) -> Vec<FileEntry> {
    let Ok(read) = std::fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut entries: Vec<FileEntry> = Vec::new();
    for dirent in read.flatten() {
        let name = dirent.file_name().to_string_lossy().into_owned();
        let Ok(relative) = jail.relative(&directory.join(&name)) else {
            continue;
        };
        let verdict = jail.check(&relative);
        let Some(accepted) = verdict.accepted() else {
            continue;
        };
        // The canonical path the jail verified, not one re-derived from the
        // name. A file deleted between the read and the stat — a turn cleaning
        // up while the panel refreshes — drops one row rather than the listing.
        let Ok(metadata) = std::fs::metadata(&accepted.path) else {
            continue;
        };
        if let Some(entry) = entry_for(jail, &accepted.path, &name, &metadata) {
            entries.push(entry);
        }
    }

    // Case-insensitively first and by bytes only to break a tie, which is what
    // a person reading a file list expects: `Readme` belongs beside `readme`,
    // not in a separate block of capitals ahead of every lowercase name.
    entries.sort_by(|a, b| {
        b.is_directory.cmp(&a.is_directory).then_with(|| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        })
    });
    entries
}
