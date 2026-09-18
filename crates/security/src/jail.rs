//! The workspace jail.
//!
//! Every path that reaches the filesystem from a model, a channel, an extension or
//! an MCP server passes through here first. It is the only thing in DarkWire
//! permitted to decide that an agent-supplied path is acceptable, which is why
//! `darkwire-core` documents its own path helpers as *not* being a safety check.
//!
//! The workspace is a **root**, in the `chroot` sense. `/etc/passwd` addresses
//! `<workspace>/etc/passwd`; `../../secrets` addresses `<workspace>/secrets`;
//! `~/.ssh/id_ed25519` addresses `<workspace>/.ssh/id_ed25519`. There is no
//! spelling of a path that means "outside", the same way there is none inside a
//! chroot. Three rules make that defensible:
//!
//!  1. **Lexical normalisation before resolution.** The string handed to the
//!     filesystem is *constructed*, not inspected: the input is split into
//!     segments, roots and drive letters and `~` prefixes are dropped, `.` is
//!     dropped and `..` pops the stack — popping an empty stack being a no-op,
//!     which is the clamp — and what survives is joined onto the canonical root.
//!     Containment therefore holds by construction rather than by comparison,
//!     and there is no input that can be *accepted* as escaping.
//!
//!  2. **Canonicalise, then containment.** The result is canonicalised through
//!     the filesystem, so a symlink inside the workspace pointing at `/etc`
//!     resolves to `/etc` and fails the check. This is the *only* thing that can
//!     refuse a well-formed path, which is exactly why it has to stay: a symlink
//!     is the one escape a lexical rule cannot see. Comparing before
//!     canonicalising is the classic bug — `workspace/link/passwd` has a perfect
//!     `workspace/` prefix and reads whatever `link` points at.
//!
//!  3. **Normalisation is total.** Every input maps either to a path inside the
//!     root or to a refusal *about the filesystem* — a NUL byte, a path the
//!     filesystem cannot answer for, a symlink leading out. No refusal is about
//!     the shape of the string any more; a shape is recorded as a [`PathShape`]
//!     rewrite and carried on the verdict for the audit log.
//!
//! **`exec` is the exception, and it is deliberate.** The exec guard refuses an
//! argument whose shape says "outside" rather than clamping it, because clamping
//! is a property of *this* module's resolution and a spawned child does not
//! honour it — `cat /etc/passwd` in a workspace-rooted child reads the real file.
//! [`path_shapes`] exists so the guard can classify without resolving. Which also
//! means: **a workspace is an organisational boundary, not a security boundary,
//! wherever `exec` is enabled.** A child process can walk out of it.
//!
//! The verdicts are platform-independent on purpose. `\` is treated as a
//! separator and drive letters are handled everywhere, so a config authored on
//! Windows and mounted into a Linux container cannot have a different idea of
//! what is legal than the machine it was written on.
//!
//! Paths that cannot be canonicalised for a reason *other* than "does not exist"
//! — a permission error, a symlink loop, a name too long for the filesystem —
//! are refused. A path whose containment cannot be established is not safe to
//! touch, and the alternative is trusting the string.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::path::{Component, MAIN_SEPARATOR_STR, Path, PathBuf};
use std::sync::Arc;

use darkwire_core::{ErrorKind, Result, WireError, ensure_dir};
use serde::{Deserialize, Serialize};

/// Why a path was refused. Carried in the error's `details` for the audit log.
///
/// Every member is a statement about the filesystem. Absolute, traversal,
/// home-prefix and UNC shapes are [`PathShape`] instead: they describe a path
/// rather than give a reason to refuse one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JailRejection {
    /// Nothing to resolve.
    Empty,
    /// A NUL byte: a truncation bypass, not a typo.
    NulByte,
    /// Resolved and canonicalised, and landed outside the root.
    OutsideRoot,
    /// Canonicalisation failed for a reason other than non-existence.
    Unverifiable,
}

impl JailRejection {
    /// The `snake_case` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            JailRejection::Empty => "empty",
            JailRejection::NulByte => "nul_byte",
            JailRejection::OutsideRoot => "outside_root",
            JailRejection::Unverifiable => "unverifiable",
        }
    }
}

impl fmt::Display for JailRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an input looked like before it was clamped.
///
/// Never a verdict — the record of a rewrite, for the audit log, for the message
/// a tool hands back to the model, and for the exec guard, which treats a
/// non-empty list as a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathShape {
    /// A leading `/` or `\`.
    Absolute,
    /// A leading `~`, which only a shell expands.
    HomePrefix,
    /// At least one `..` segment, whether or not it would have escaped.
    Traversal,
    /// A leading `\\` or `//`.
    Unc,
    /// A leading `C:`.
    Drive,
}

impl PathShape {
    /// The `snake_case` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            PathShape::Absolute => "absolute",
            PathShape::HomePrefix => "home_prefix",
            PathShape::Traversal => "traversal",
            PathShape::Unc => "unc",
            PathShape::Drive => "drive",
        }
    }
}

impl fmt::Display for PathShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An accepted path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JailAccept {
    /// The canonical, absolute path that was verified.
    ///
    /// Callers must use this for the filesystem call rather than re-deriving one
    /// from the input: it is the string that was actually checked.
    pub path: PathBuf,
    /// The workspace-relative form after clamping — what the caller addressed.
    pub relative: String,
    /// Empty when the input was already a plain relative path. In detection order.
    pub rewrites: Vec<PathShape>,
}

/// The verdict on one input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JailCheck {
    /// Inside the root.
    Accept(JailAccept),
    /// Refused, for a reason about the filesystem.
    Reject {
        /// Why.
        rejection: JailRejection,
        /// For humans.
        message: String,
    },
}

impl JailCheck {
    /// Whether the input was accepted.
    pub fn is_ok(&self) -> bool {
        matches!(self, JailCheck::Accept(_))
    }

    /// The acceptance, if any.
    pub fn accepted(&self) -> Option<&JailAccept> {
        match self {
            JailCheck::Accept(accept) => Some(accept),
            JailCheck::Reject { .. } => None,
        }
    }

    /// The rejection, if any.
    pub fn rejection(&self) -> Option<JailRejection> {
        match self {
            JailCheck::Accept(_) => None,
            JailCheck::Reject { rejection, .. } => Some(*rejection),
        }
    }
}

/// How to build a jail.
#[derive(Debug, Clone)]
pub struct JailOptions {
    /// The workspace root. Canonicalised on construction.
    pub root: PathBuf,
    /// Create the root if it is missing. Default `true`.
    pub create: bool,
    /// Compare paths case-insensitively. Defaults to `true` on Windows only.
    ///
    /// Not enabled on macOS despite APFS defaulting to case-insensitive: the
    /// volume *can* be case-sensitive, and folding case there would accept a
    /// prefix that is a genuinely different directory. It is not needed either —
    /// input paths are joined onto this object's own canonical root, so the
    /// compared prefix is byte-identical by construction.
    pub case_insensitive: Option<bool>,
}

impl JailOptions {
    /// Options for `root` with the defaults.
    pub fn new(root: impl Into<PathBuf>) -> JailOptions {
        JailOptions {
            root: root.into(),
            create: true,
            case_insensitive: None,
        }
    }
}

fn reject(rejection: JailRejection, message: impl Into<String>) -> JailCheck {
    JailCheck::Reject {
        rejection,
        message: message.into(),
    }
}

fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

fn has_drive_letter(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    )
}

fn is_unc(text: &str) -> bool {
    text.starts_with("\\\\") || text.starts_with("//")
}

fn is_rooted(text: &str) -> bool {
    text.starts_with('/') || text.starts_with('\\')
}

struct Normalised {
    segments: Vec<String>,
    rewrites: Vec<PathShape>,
}

/// Splits an input into the segments it addresses inside the root.
///
/// Two passes. The first folds the path: `.` and empty segments are dropped and
/// `..` pops the stack, popping an empty stack being the clamp. The second
/// strips root markers from the *head* of what survived — a leading `~` segment,
/// a leading drive letter — and it runs on the output rather than on the input
/// for one reason: **the result has to be a fixed point.** The REST layer echoes
/// `relative` back to clients that send it again, so a normalisation that moved
/// on the second pass would walk a path somewhere new on every round trip.
/// Stripping only at input position 0 fails that: `/~` would fold to a literal
/// `~` segment, which re-reads as a home prefix and resolves to the root instead.
///
/// The cost is that a file named `~something` directly in the workspace root is
/// not addressable. That was already true — the pre-chroot rule refused any
/// leading `~` outright — and it is confined to the root, so `a/~/b` still names
/// a directory called `~`.
fn normalise(input: &str) -> Normalised {
    let mut rewrites: Vec<PathShape> = Vec::new();
    let note = |shape: PathShape, rewrites: &mut Vec<PathShape>| {
        if !rewrites.contains(&shape) {
            rewrites.push(shape);
        }
    };

    if is_unc(input) {
        note(PathShape::Unc, &mut rewrites);
    } else if is_rooted(input) {
        note(PathShape::Absolute, &mut rewrites);
    }

    // Fold, strip one root marker off the head, fold again. Stripping has to be
    // followed by another fold rather than trusted: `./c:..` folds to `c:..`,
    // whose drive prefix comes off leaving a bare `..` — which, left in the
    // output, would join to the workspace's *parent* and reduce containment to
    // whatever canonicalisation happened to say. Each pass strictly shortens the
    // input, so this terminates; it exits when the head is no longer a root
    // marker, which is also what makes the result a fixed point.
    let mut parts: Vec<String> = input.split(is_separator).map(str::to_owned).collect();
    loop {
        let mut segments: Vec<String> = Vec::new();
        for segment in parts {
            if segment.is_empty() || segment == "." {
                continue;
            }
            if segment == ".." {
                note(PathShape::Traversal, &mut rewrites);
                segments.pop();
                continue;
            }
            segments.push(segment);
        }
        let Some(head) = segments.first().cloned() else {
            return Normalised { segments, rewrites };
        };
        if has_drive_letter(&head) {
            note(PathShape::Drive, &mut rewrites);
            let mut next = vec![head[2..].to_owned()];
            next.extend(segments.into_iter().skip(1));
            parts = next;
            continue;
        }
        if head.starts_with('~') {
            note(PathShape::HomePrefix, &mut rewrites);
            parts = segments.into_iter().skip(1).collect();
            continue;
        }
        return Normalised { segments, rewrites };
    }
}

/// The shapes that mean "this string addresses something outside the workspace".
///
/// Deliberately the **pre-chroot syntactic rule, verbatim**, and deliberately
/// not derived from `normalise`. It exists for the exec guard, which must refuse
/// what the file tools clamp: the guard hands its argument to a child process
/// unchanged, and a child does not honour this module's idea of a root, so
/// `cat /etc/passwd` with the working directory at the workspace root reads the
/// real file.
///
/// `normalise` answers "what did I rewrite"; this answers "should a child ever
/// see this". They coincide everywhere it matters and are allowed to differ on
/// exotica like `./~/x`, where the stricter of the two is the one the guard uses.
pub fn path_shapes(input: &str) -> Vec<PathShape> {
    let mut shapes = Vec::new();
    let raw: Vec<&str> = input.split(is_separator).collect();

    if raw.first().is_some_and(|head| head.starts_with('~')) {
        shapes.push(PathShape::HomePrefix);
    }
    if is_unc(input) {
        shapes.push(PathShape::Unc);
    } else if is_rooted(input) {
        shapes.push(PathShape::Absolute);
    }
    if has_drive_letter(input) {
        shapes.push(PathShape::Drive);
    }
    if raw.contains(&"..") {
        shapes.push(PathShape::Traversal);
    }
    shapes
}

/// Whether a name exists as a directory entry, even a broken one.
fn entry_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Only the two error kinds that mean "this path does not exist yet".
fn is_non_existent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// The errno name for a message, so a refusal says what the filesystem said.
fn errno_name(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::NotADirectory => "ENOTDIR",
        io::ErrorKind::InvalidFilename => "ENAMETOOLONG",
        io::ErrorKind::PermissionDenied => "EACCES",
        _ => "unknown",
    }
}

/// `canonicalize`, with the verbatim prefix Windows adds removed again, so a
/// canonical path compares and displays like the one the operator wrote.
fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    let real = std::fs::canonicalize(path)?;
    Ok(strip_verbatim(real))
}

#[cfg(windows)]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

#[cfg(not(windows))]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    path
}

/// Canonical form of the deepest ancestor that exists, with the missing tail
/// re-appended.
///
/// `write` legitimately targets a path that does not exist yet, and plain
/// canonicalisation fails on it — so a jail built on canonicalisation alone
/// could only ever validate reads. Re-appending is safe because the missing
/// segments cannot be symlinks: they do not exist. Every segment that *does*
/// exist has already been canonicalised by the successful call.
///
/// The one trap, and the reason for the `entry_exists` guard: canonicalisation
/// answers "not found" for a **dangling** symlink as well as for a name that is
/// not there at all, and the two must not be treated alike. A dangling link is
/// an existing directory entry whose target is missing, so popping it and
/// re-appending the name hands back a path inside the root that a write then
/// follows straight out of it — `<ws>/x → ../../vault.json` would pass
/// containment and then be created outside. `symlink_metadata` is what tells
/// the two apart: it succeeds on the link itself. Such a path is unverifiable,
/// not absent.
///
/// The walk stops at `floor`, which is always an ancestor of `target` because the
/// caller built the target by joining a `..`-free segment list onto it. A failure
/// at the floor itself means the workspace root has been deleted or unmounted
/// underneath the process, and there is nothing to verify against.
fn realpath_boundary(target: &Path, floor: &Path) -> io::Result<PathBuf> {
    let mut tail: VecDeque<std::ffi::OsString> = VecDeque::new();
    let mut current = target.to_path_buf();
    loop {
        match canonicalize(&current) {
            Ok(real) => {
                let mut resolved = real;
                for segment in &tail {
                    resolved.push(segment);
                }
                return Ok(resolved);
            }
            Err(error) => {
                if !is_non_existent(&error) || current == floor || entry_exists(&current) {
                    return Err(error);
                }
                let (Some(name), Some(parent)) = (current.file_name(), current.parent()) else {
                    return Err(error);
                };
                tail.push_front(name.to_owned());
                current = parent.to_path_buf();
            }
        }
    }
}

/// Resolves `.` and `..` lexically and makes the path absolute against the
/// working directory, without touching the filesystem.
fn lexical_absolute(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The workspace, as a boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceJail {
    root: PathBuf,
    case_insensitive: bool,
}

impl WorkspaceJail {
    /// Opens (and by default creates) the workspace root.
    pub fn new(options: JailOptions) -> Result<WorkspaceJail> {
        let JailOptions {
            root: requested_root,
            create,
            case_insensitive,
        } = options;
        let unusable = |requested: &Path, error: io::Error| {
            WireError::new(
                ErrorKind::Config,
                format!("Workspace root is unusable: {}", requested.display()),
            )
            .with_detail("root", requested.to_string_lossy().into_owned())
            .with_source(error)
        };
        let requested = std::path::absolute(&requested_root)
            .map_err(|error| unusable(&requested_root, error))?;
        if create {
            ensure_dir(&requested).map_err(|error| {
                WireError::new(
                    ErrorKind::Config,
                    format!("Workspace root is unusable: {}", requested.display()),
                )
                .with_detail("root", requested.to_string_lossy().into_owned())
                .with_source(error)
            })?;
        }
        let root = canonicalize(&requested).map_err(|error| unusable(&requested, error))?;
        Ok(WorkspaceJail {
            root,
            case_insensitive: case_insensitive.unwrap_or(cfg!(windows)),
        })
    }

    /// The canonical, absolute workspace root. Every legal path is under it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a path against the workspace root without erroring.
    ///
    /// Use this where a rejection is an expected outcome to be reported — the exec
    /// guard checking every argument, a batch of paths from one tool call — and
    /// [`accept`](Self::accept) / [`resolve`](Self::resolve) everywhere else.
    pub fn check(&self, input: &str) -> JailCheck {
        if input.is_empty() {
            return reject(JailRejection::Empty, "Path is empty");
        }
        if input.contains('\0') {
            return reject(JailRejection::NulByte, "Path contains a NUL byte");
        }

        let Normalised { segments, rewrites } = normalise(&self.without_root_prefix(input));
        let relative = segments.join(MAIN_SEPARATOR_STR);
        let mut target = self.root.clone();
        for segment in &segments {
            target.push(segment);
        }

        let real = match realpath_boundary(&target, &self.root) {
            Ok(real) => real,
            Err(error) => {
                return reject(
                    JailRejection::Unverifiable,
                    format!("Path cannot be verified: {input} ({})", errno_name(&error)),
                );
            }
        };

        if !self.contains(&real) {
            // The only way to arrive here after a lexical normalisation that cannot
            // produce an escaping string is a symlink inside the workspace pointing
            // out of it.
            return reject(
                JailRejection::OutsideRoot,
                format!("Path resolves outside the workspace: {input}"),
            );
        }
        JailCheck::Accept(JailAccept {
            path: real,
            relative,
            rewrites,
        })
    }

    /// Resolves a path against the workspace root and returns the whole verdict.
    ///
    /// `rewrites` is why this exists beside [`resolve`](Self::resolve): a caller
    /// that clamped a model's `/etc/hosts` into the workspace should say so in
    /// what it hands back, or the model believes it read the host's file.
    pub fn accept(&self, input: &str) -> Result<JailAccept> {
        match self.check(input) {
            JailCheck::Accept(accept) => Ok(accept),
            JailCheck::Reject { rejection, message } => {
                let kind = if rejection == JailRejection::Empty {
                    ErrorKind::InvalidInput
                } else {
                    ErrorKind::JailEscape
                };
                Err(WireError::new(kind, message)
                    .with_detail("path", input)
                    .with_detail("rejection", rejection.as_str()))
            }
        }
    }

    /// `accept(input).path`, for callers that need nothing else.
    pub fn resolve(&self, input: &str) -> Result<PathBuf> {
        Ok(self.accept(input)?.path)
    }

    /// Whether an already-absolute path lies inside the root.
    ///
    /// This is for paths DarkWire produced itself — the session database, a media
    /// file being served over HTTP. It does **not** canonicalise, so it is not a
    /// check for agent-supplied input; [`check`](Self::check) and
    /// [`accept`](Self::accept) are. Containment is component-wise, never a
    /// string prefix: `<root>-evil` is not under `<root>`.
    pub fn contains(&self, absolute: &Path) -> bool {
        let candidate = self.fold_path(&lexical_absolute(absolute));
        let root = self.fold_path(&self.root);
        candidate.starts_with(&root)
    }

    /// The workspace-relative form of a contained path, for display and logs.
    pub fn relative(&self, absolute: &Path) -> Result<String> {
        let resolved = lexical_absolute(absolute);
        if !self.contains(&resolved) {
            return Err(WireError::new(
                ErrorKind::JailEscape,
                format!("Path is outside the workspace: {}", absolute.display()),
            )
            .with_detail("path", absolute.to_string_lossy().into_owned())
            .with_detail("rejection", JailRejection::OutsideRoot.as_str()));
        }
        let relative: PathBuf = resolved
            .components()
            .skip(self.root.components().count())
            .collect();
        Ok(relative.to_string_lossy().into_owned())
    }

    /// The workspace root, removed from the front of a path that repeats it.
    ///
    /// Clamping treats a leading `/` as the root, which is right for `/notes/x` and
    /// silently wrong for the absolute path of the root itself: `<root>/notes/x`
    /// clamped segment by segment lands on `<root>/Users/you/project/notes/x` — a
    /// real directory tree of junk, created without an error by `write` and
    /// reported as "not found" by `read` for a file that exists.
    ///
    /// Nothing legitimate is lost. Addressing a directory *inside* the workspace
    /// whose path spells out the workspace's own absolute path is not a thing anyone
    /// does, and the alternative reading is never what was meant.
    ///
    /// The result stays a fixed point, which `normalise` explains is the property
    /// the REST layer depends on: what comes back no longer starts with the root, so
    /// a second pass finds nothing to remove. The leading separator is kept so the
    /// path is still recorded as an absolute rewrite rather than looking relative.
    fn without_root_prefix<'a>(&self, input: &'a str) -> std::borrow::Cow<'a, str> {
        let root = self.root.to_string_lossy();
        // A workspace at the filesystem root has no prefix to remove, and every
        // absolute path would match it.
        if root == "/" || root == MAIN_SEPARATOR_STR {
            return input.into();
        }

        // Compared with separators unified, because a model on a POSIX host can
        // still produce `\` and the two spell the same path.
        let candidate = self.fold(&input.replace('\\', "/"));
        let root_unified = self.fold(&root.replace('\\', "/"));

        if candidate == root_unified {
            return "/".into();
        }
        if candidate.starts_with(&format!("{root_unified}/"))
            && candidate.len() == input.len()
            && let Some(rest) = input.get(root.len()..)
        {
            return rest.into();
        }
        input.into()
    }

    fn fold(&self, value: &str) -> String {
        if self.case_insensitive {
            value.to_lowercase()
        } else {
            value.to_owned()
        }
    }

    fn fold_path(&self, path: &Path) -> PathBuf {
        if self.case_insensitive {
            PathBuf::from(path.to_string_lossy().to_lowercase())
        } else {
            path.to_path_buf()
        }
    }
}

/// Supplies the jail a turn runs inside, keyed by its session's workspace.
///
/// Declared here rather than in the agent crate because it is a statement about
/// jails: the tool registry, an MCP host and the file routes all need to reach
/// one workspace out of several, and none of them should have to depend on the
/// agent loop to say so.
pub trait JailResolver: Send + Sync {
    /// The jail for one workspace.
    fn for_workspace(&self, workspace_id: &str) -> Arc<WorkspaceJail>;
    /// For a session with no stored workspace, and for a preview with no session.
    fn default_jail(&self) -> Arc<WorkspaceJail>;
}

/// The single-workspace case, for tests and for embedders that have only one.
#[derive(Debug, Clone)]
pub struct SingleJail {
    jail: Arc<WorkspaceJail>,
}

/// A resolver that answers with the same jail for every workspace.
pub fn single_jail(jail: Arc<WorkspaceJail>) -> SingleJail {
    SingleJail { jail }
}

impl JailResolver for SingleJail {
    fn for_workspace(&self, _workspace_id: &str) -> Arc<WorkspaceJail> {
        Arc::clone(&self.jail)
    }

    fn default_jail(&self) -> Arc<WorkspaceJail> {
        Arc::clone(&self.jail)
    }
}
