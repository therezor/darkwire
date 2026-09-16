//! Whether a directory of code may be loaded, and what question that decision
//! actually answers.
//!
//! The shape is [`crate::container`]'s — parse the manifest, refuse what the
//! machinery cannot honour, hash what the operator reviewed — with one deliberate
//! difference, and it is the difference that makes the analogy hold.
//!
//! **A container is authorised by its manifest alone because the manifest pins the
//! code.** `image` must be a digest, so approving those bytes approves an exact,
//! immutable container. An extension's manifest names `entry`, which is a path:
//! approving the manifest would approve a *pointer*, and the file behind it
//! could be swapped afterwards without moving a single approved byte. So the
//! digest here covers the whole install directory — every regular file under it,
//! each contributing its relative path and its own sha256 to one ordered hash.
//! Editing any of them, adding one, or removing one moves the digest and revokes
//! the approval, which is exactly what the container gate buys and what a
//! manifest-only hash would only appear to.
//!
//! **What this does not buy** is worth stating in the same breath, because
//! `docs/security.md` states it too: an extension that passes runs with the
//! host's own privileges. It can read the vault file, spawn a process and open
//! a socket, and nothing here stops it. The question this module answers is
//! "are these the exact bytes the operator reviewed?", not "is this code safe"
//! — which is the same question the container store answers, at the same trust
//! level as a container with host `exec`.

use std::path::{Path, PathBuf};

use crate::environment::{parse_manifest, sha256_hex};
use crate::exec_guard::{SHELL_BINARIES, binary_name};
use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ExtensionManifest, ExtensionSchemaVersion, is_extension_id};

/// The file every installed extension is found by.
pub const EXTENSION_MANIFEST_FILE: &str = "darkwire.extension.yaml";

/// How many files of an install directory the digest will walk.
///
/// Not a performance tuning knob — a statement of what an extension is. The
/// expectation is a manifest plus a bundled entry, which is single digits of
/// files, and a directory that blows through this is one carrying an unbundled
/// dependency tree. Refusing loudly at that point is the difference between
/// "this extension is not shaped the way extensions are shaped" and a boot that
/// takes ninety seconds for a reason nobody can see.
pub const MAX_EXTENSION_FILES: usize = 4096;
/// How many bytes of an install directory the digest will read.
pub const MAX_EXTENSION_BYTES: u64 = 64 * 1024 * 1024;

/// What a v1 `entry` may end in: an ES module, which is what a v1 host loaded.
const ENTRY_EXTENSIONS: &[&str] = &[".js", ".mjs"];

/// Parses manifest bytes, with the schema's own errors turned into a sentence.
pub fn parse_extension(bytes: &[u8]) -> Result<ExtensionManifest> {
    parse_manifest(bytes, "Extension manifest")
}

fn policy_error(id: &str, message: String) -> WireError {
    WireError::new(ErrorKind::Config, message).with_detail("id", id)
}

/// Whether `candidate` sits strictly under `root`.
///
/// Both are already canonical when this is called — the caller canonicalises
/// them — so this is the lexical half and nothing more. The candidate *being*
/// the root cannot happen for an entry file and is refused anyway rather than
/// special-cased into an acceptance.
fn contains(root: &Path, candidate: &Path) -> bool {
    candidate
        .strip_prefix(root)
        .is_ok_and(|rest| rest.components().next().is_some())
}

/// Refuses an extension the host cannot load safely.
///
/// Separate from parsing for the reason [`crate::assert_environment_policy`] is: a
/// manifest can be perfectly well-formed and still name something that would put
/// the code that runs outside the bytes that were approved.
///
/// `dir` is the *install* directory, and it is canonicalised before the
/// containment check. A lexical check alone is defeated by a symlink —
/// `entry: "lib/x.js"` where `lib` points at `/etc` reads as contained and is
/// not — which is the same reasoning, and the same order, the jail uses.
pub fn assert_extension_policy(manifest: &ExtensionManifest, dir: &Path) -> Result<()> {
    let id = manifest.id.as_str();
    if !is_extension_id(id) {
        return Err(policy_error(
            id,
            format!(
                "\"{id}\" is not a usable extension id.\n  1–40 characters of lowercase letters, digits and hyphens, not starting\n  or ending with a hyphen. The id names a directory and prefixes every\n  tool, channel, provider and command the extension contributes."
            ),
        ));
    }

    // The directory name wins nothing here — it is checked *against* the
    // manifest, and a disagreement is refused rather than resolved. Letting
    // either side win would mean the id an operator approved and the id the host
    // registers under could differ, and the approval row is keyed by id.
    let absolute = std::path::absolute(dir).map_err(|error| {
        WireError::new(
            ErrorKind::Config,
            format!("Extension directory cannot be resolved: {}", dir.display()),
        )
        .with_source(error)
    })?;
    let dir_name = absolute
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if dir_name != id {
        return Err(policy_error(
            id,
            format!(
                "Extension \"{id}\" is installed in a directory called \"{dir_name}\".\n  The two have to agree: the approval is recorded against the id, and the\n  directory is how the extension is found. Rename one to match the other."
            ),
        )
        .with_detail("dirName", dir_name));
    }

    let root = std::fs::canonicalize(&absolute)?;
    match manifest.schema {
        ExtensionSchemaVersion::V1 => assert_entry_policy(id, &manifest.entry, &root),
        ExtensionSchemaVersion::V2 => assert_command_policy(id, &manifest.command, &root),
    }
}

/// The `darkwire.extension/1` rule: `entry` is an ES module inside the install.
///
/// Kept whole. No host in this build loads a v1 bundle, but approving one is
/// still a thing an operator can ask for, and an approval that skipped its
/// checks would record a digest against a manifest nothing had validated.
fn assert_entry_policy(id: &str, entry: &str, root: &Path) -> Result<()> {
    if Path::new(entry).is_absolute() {
        return Err(policy_error(
            id,
            format!(
                "Extension \"{id}\" names an absolute entry: {entry}\n  The entry is relative to the extension directory, so that the digest\n  covering that directory also covers the code that runs."
            ),
        )
        .with_detail("entry", entry));
    }

    if !ENTRY_EXTENSIONS
        .iter()
        .any(|suffix| entry.ends_with(suffix))
    {
        return Err(policy_error(
            id,
            format!(
                "Extension \"{id}\" names an entry that is not an ES module: {entry}\n  It has to end in {}.",
                ENTRY_EXTENSIONS.join(" or ")
            ),
        )
        .with_detail("entry", entry));
    }

    let resolved = std::fs::canonicalize(root.join(entry))?;
    if !contains(root, &resolved) {
        return Err(policy_error(
            id,
            format!(
                "Extension \"{id}\" names an entry outside its own directory: {entry}\n  Resolved, that is a path the approval digest does not cover, so the code\n  that ran would not be the code that was reviewed."
            ),
        )
        .with_detail("entry", entry)
        .with_detail("resolved", resolved.to_string_lossy().into_owned()));
    }
    Ok(())
}

/// The `darkwire.extension/2` rule: `command` is an argv the host can spawn.
///
/// The same question the `entry` rule asks — "will the code that runs be the
/// code that was approved?" — put to an argv instead of a module path, plus one
/// the module path could not raise. An `entry` was always interpreted by Node;
/// an argv is interpreted by whatever `argv[0]` names, and a *shell* named there
/// turns the rest of the argv back into a program string. So a shell binary is
/// refused with the same list [`crate::SHELL_BINARIES`] gives the exec guard.
///
/// `argv[0]` has two legal shapes, and the split is on whether it contains a
/// separator rather than on whether it exists. A bare name (`node`, `python3`)
/// is resolved by the operating system on the host `PATH`: it names a program
/// the operator installed, not one the extension shipped, so the digest has
/// nothing to say about it. Anything with a separator is a path into the install
/// directory and is held to exactly the containment rule `entry` was — lexical
/// first, then `realpath`, because a symlink defeats the lexical half alone.
fn assert_command_policy(id: &str, command: &[String], root: &Path) -> Result<()> {
    let Some(program) = command
        .first()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Err(policy_error(
            id,
            format!(
                "Extension \"{id}\" names no command.\n  A \"darkwire.extension/2\" manifest runs as a child process, so it has to\n  say what to run: \"command\": [\"node\", \"index.mjs\"]."
            ),
        ));
    };

    if SHELL_BINARIES.contains(&binary_name(program).as_str()) {
        return Err(policy_error(
            id,
            format!(
                "Extension \"{id}\" names a shell as its command: {program}\n  The command is an argv, passed to the program as arguments — never a\n  line for a shell to re-parse. Name the interpreter directly, as in\n  \"command\": [\"node\", \"index.mjs\"]."
            ),
        )
        .with_detail("program", program));
    }

    if !program.contains(std::path::MAIN_SEPARATOR) && !program.contains('/') {
        // A bare name is resolved on the host `PATH` by the operating system.
        // It names something the operator installed rather than something the
        // extension shipped, so the approval digest has no claim on it.
        return Ok(());
    }

    if Path::new(program).is_absolute() {
        return Err(policy_error(
            id,
            format!(
                "Extension \"{id}\" names an absolute program: {program}\n  A command is either a bare name resolved on PATH or a path relative to\n  the extension directory, so that the digest covering that directory also\n  covers the code that runs."
            ),
        )
        .with_detail("program", program));
    }

    let resolved = std::fs::canonicalize(root.join(program))?;
    if !contains(root, &resolved) {
        return Err(policy_error(
            id,
            format!(
                "Extension \"{id}\" names a program outside its own directory: {program}\n  Resolved, that is a path the approval digest does not cover, so the code\n  that ran would not be the code that was reviewed."
            ),
        )
        .with_detail("program", program)
        .with_detail("resolved", resolved.to_string_lossy().into_owned()));
    }
    Ok(())
}

fn too_large(dir: &Path) -> WireError {
    WireError::new(
        ErrorKind::Config,
        format!(
            "The extension in {} is too large to authorise.\n  The limit is {MAX_EXTENSION_FILES} files and {} MB, and every byte under the\n  directory is hashed. An extension is expected to ship a bundled entry\n  rather than an installed dependency tree.",
            dir.display(),
            MAX_EXTENSION_BYTES / 1024 / 1024
        ),
    )
    .with_detail("dir", dir.to_string_lossy().into_owned())
}

fn unreadable(
    dir: &Path,
    what: &Path,
    error: impl std::error::Error + Send + Sync + 'static,
) -> WireError {
    WireError::new(
        ErrorKind::Config,
        format!(
            "The extension in {} could not be read: {}",
            dir.display(),
            what.display()
        ),
    )
    .with_detail("dir", dir.to_string_lossy().into_owned())
    .with_source(error)
}

/// The digest of everything installed under one extension directory.
///
/// Over each file's *bytes*, never over a re-serialisation of anything: the same
/// reason [`crate::manifest_hash`] gives, one directory wider. The relative path
/// goes into the hash beside the content so that renaming a file — which changes
/// what `entry` resolves to — moves the digest even when no byte of content did.
///
/// `\0` separates the two halves because it is the one byte a path cannot
/// contain, so no pair of (path, hash) can be re-cut into a different pair.
/// Sorted, because directory order is a property of the filesystem and an
/// approval that changed when an install was copied between machines would be
/// an approval nobody trusted. The sort key is UTF-16 code units, which is the
/// order the approvals already recorded were computed in.
///
/// Symlinks are not walked in either direction: a symlinked directory is a loop
/// waiting to happen and a way to pull an arbitrary subtree of the host into the
/// digest, and a symlinked file is content that lives outside the bytes being
/// approved. Existing approvals were computed this way, so following either
/// would move every digest.
pub fn extension_digest(dir: &Path) -> Result<String> {
    let root = std::fs::canonicalize(std::path::absolute(dir)?)?;
    let mut lines: Vec<String> = Vec::new();
    let mut bytes: u64 = 0;

    for entry in walkdir::WalkDir::new(&root).follow_links(false) {
        let entry = entry.map_err(|error| unreadable(dir, &root, error))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let content =
            std::fs::read(entry.path()).map_err(|error| unreadable(dir, entry.path(), error))?;
        bytes += u64::try_from(content.len()).unwrap_or(u64::MAX);
        if lines.len() >= MAX_EXTENSION_FILES || bytes > MAX_EXTENSION_BYTES {
            return Err(too_large(dir));
        }
        let relative = entry
            .path()
            .strip_prefix(&root)
            .unwrap_or(entry.path())
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        lines.push(format!("{relative}\0{}", sha256_hex(&content)));
    }

    lines.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    Ok(sha256_hex(lines.join("\n").as_bytes()))
}

/// Reads and parses the manifest an install directory holds.
pub fn read_extension_manifest(dir: &Path) -> Result<ExtensionManifest> {
    let file: PathBuf = dir.join(EXTENSION_MANIFEST_FILE);
    let bytes = std::fs::read(&file).map_err(|error| {
        WireError::new(
            ErrorKind::Config,
            format!("Extension manifest could not be read: {}", file.display()),
        )
        .with_detail("file", file.to_string_lossy().into_owned())
        .with_source(error)
    })?;
    parse_extension(&bytes)
}
