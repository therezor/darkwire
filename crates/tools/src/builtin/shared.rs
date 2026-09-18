//! Turning filesystem failures into something a model can act on.
//!
//! A raw `No such file or directory (os error 2)` for
//! `/Users/x/.darkwire/workspace/notes.md` is wrong for this surface twice over.
//! It reports an absolute path, when the tool contract the model was given is
//! workspace-relative — so the model's next call tends to copy the absolute
//! form straight back and be rejected by the jail. And it carries a `kind` of
//! `tool`, when `not_found` and `permission_denied` are distinctions the agent
//! loop and the audit log both care about.

use std::io;

use darkwire_core::{ErrorKind, WireError};
use darkwire_security::JailAccept;
use nix::errno::Errno;

/// The kind and the sentence for one class of failure.
fn describe(kind: io::ErrorKind) -> (ErrorKind, String) {
    match kind {
        io::ErrorKind::NotFound => (ErrorKind::NotFound, "does not exist".to_owned()),
        io::ErrorKind::NotADirectory => (
            ErrorKind::NotFound,
            "has a parent that is not a directory".to_owned(),
        ),
        io::ErrorKind::IsADirectory => (ErrorKind::InvalidInput, "is a directory".to_owned()),
        io::ErrorKind::PermissionDenied => (
            ErrorKind::PermissionDenied,
            "is not readable or writable by this process".to_owned(),
        ),
        io::ErrorKind::ReadOnlyFilesystem => (
            ErrorKind::PermissionDenied,
            "is on a read-only filesystem".to_owned(),
        ),
        io::ErrorKind::StorageFull => (
            ErrorKind::Storage,
            "could not be written: the filesystem is full".to_owned(),
        ),
        io::ErrorKind::InvalidFilename => (
            ErrorKind::InvalidInput,
            "has a name too long for the filesystem".to_owned(),
        ),
        other => (ErrorKind::Tool, format!("could not be used ({other:?})")),
    }
}

/// A symlink loop has no stable `io::ErrorKind`, so it is recognised by errno.
fn is_loop(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|code| Errno::from_raw(code) == Errno::ELOOP)
}

/// A sentence explaining where a path actually landed, or `""`.
///
/// The workspace is a chroot, so `/etc/hosts` addresses a file *inside* it.
/// That is the right behaviour and the wrong silence: without saying so, a
/// model that asked for `/etc/hosts` and got "does not exist" concludes the
/// host has no hosts file, and one that asked and got content concludes it
/// read the host's. Refusing got this for free — clamping has to pay for it
/// explicitly.
///
/// Costs a line of tokens only when something was actually rewritten, which is
/// rare once the model has read the tool description.
pub fn clamp_note(requested: &str, accepted: &JailAccept) -> String {
    if accepted.rewrites.is_empty() {
        return String::new();
    }
    let landed = if accepted.relative.is_empty() {
        "."
    } else {
        accepted.relative.as_str()
    };
    format!(" The workspace is the root: \"{requested}\" was resolved to \"{landed}\" inside it.")
}

/// Re-describes a filesystem error against the workspace-relative path.
///
/// `note` carries [`clamp_note`]'s sentence, so a miss on a path the model
/// wrote as absolute explains itself rather than reading as "that file is not
/// there".
pub fn fs_failure(error: &io::Error, path: &str, note: &str) -> WireError {
    let (kind, detail) = if is_loop(error) {
        (
            ErrorKind::InvalidInput,
            "is behind a symlink loop".to_owned(),
        )
    } else {
        describe(error.kind())
    };
    WireError::new(kind, format!("{path} {detail}.{note}"))
        .with_detail("path", path)
        .with_detail("code", format!("{:?}", error.kind()))
}

/// Bytes as something readable in a directory listing.
///
/// The agent loop formats attachment sizes with it as well, and the model
/// reads both those strings and `ls`'s in the same context window — two
/// spellings of "4.2 KB" is a difference it would be entitled to read meaning
/// into.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    // Exact below 2^53, and a size above that has no useful decimal anyway.
    #[allow(
        clippy::cast_precision_loss,
        reason = "a display value, not arithmetic"
    )]
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}
