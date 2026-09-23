//! Turning filesystem failures into something a model can act on.
//!
//! A raw `No such file or directory (os error 2)` for
//! `/Users/x/.darkwire/workspace/notes.md` is wrong for this surface twice over.
//! It reports an absolute path, when the tool contract the model was given is
//! workspace-relative — so the model's next call tends to copy the absolute
//! form straight back and be rejected by the jail. And it carries a `kind` of
//! `tool`, when `not_found` and `permission_denied` are distinctions the agent
//! loop and the audit log both care about.

use std::fs::Metadata;
use std::io;
use std::path::Path;
use std::sync::Arc;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt as _};
use darkwire_core::{ErrorKind, WireError};
use darkwire_security::{JailAccept, WorkspaceJail, escape_refusal, led_outside};
use nix::errno::Errno;
use nix::fcntl::OFlag;

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

/// [`fs_failure`], except that a path which led out of the workspace during a
/// call through [`in_root`] is refused the way the jail refuses it.
///
/// `requested` is what the model asked for, which is what the jail's own
/// refusal names.
pub fn root_failure(error: &io::Error, requested: &str, path: &str, note: &str) -> WireError {
    if led_outside(error) {
        return escape_refusal(requested);
    }
    fs_failure(error, path, note)
}

/// Runs blocking filesystem work against the workspace root, off the runtime.
///
/// `work` gets the root as a capability and the accepted path relative to it.
/// The jail looked at the path a moment ago; opening through the root is what
/// keeps a component swapped for a symlink since then from leading out.
pub async fn in_root<T, F>(
    jail: &Arc<WorkspaceJail>,
    accepted: &JailAccept,
    tool: &str,
    work: F,
) -> Result<io::Result<T>, WireError>
where
    T: Send + 'static,
    F: FnOnce(&Dir, &Path) -> io::Result<T> + Send + 'static,
{
    let jail = Arc::clone(jail);
    let inside = jail.beneath(accepted);
    tokio::task::spawn_blocking(move || work(&jail.open_root()?, &inside))
        .await
        .map_err(|error| WireError::new(ErrorKind::Internal, format!("{tool} failed: {error}")))
}

/// Open options for a path the jail has already accepted.
///
/// `O_NONBLOCK` so a FIFO cannot hang the open. The last component is not
/// followed, so one swapped for a symlink after the jail looked is refused;
/// the jail canonicalised the path, so a legitimate one never ends in a
/// symlink. Earlier components are the root's to refuse.
pub fn open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .follow(FollowSymlinks::No)
        .custom_flags(OFlag::O_NONBLOCK.bits());
    options
}

/// Refuses anything but a regular file, before a tool reads or writes it.
pub fn assert_regular(stats: &Metadata, path: &str, note: &str) -> Result<(), WireError> {
    if stats.is_dir() || stats.is_file() {
        return Ok(());
    }
    Err(not_regular(path, note))
}

/// The refusal for a FIFO, a socket or a device.
pub fn not_regular(path: &str, note: &str) -> WireError {
    WireError::new(
        ErrorKind::InvalidInput,
        format!("{path} is not a regular file.{note}"),
    )
    .with_detail("path", path)
}

/// Whether `inside` names something other than a regular file or a directory.
/// A path that does not exist yet is neither.
pub fn is_irregular(root: &Dir, inside: &Path) -> bool {
    root.symlink_metadata(inside)
        .is_ok_and(|stats| !stats.is_dir() && !stats.is_file())
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
