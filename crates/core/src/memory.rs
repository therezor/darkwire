//! Memory: what an agent has learned about a workspace, kept in `memory/`.
//!
//! **One plain markdown file per memory**, named by its key:
//!
//! ```text
//! <workspace>/memory/
//! ├── auth-sessions.md
//! ├── package-manager.md
//! └── ui-stack.md
//! ```
//!
//! ```markdown
//! # PostgreSQL-backed sessions
//!
//! Sessions live in PostgreSQL and expire after 30 days. JWT-only auth was
//! rejected because revocation has to be immediate.
//! ```
//!
//! This module is the disk half and nothing else: bytes to [`Memory`] and back.
//! What reaches the prompt is the agent's memory contributor, which asks for
//! [`index_line`] and nothing more.
//!
//! ## Four decisions worth stating
//!
//! **The file is the memory, with no frontmatter.** What the index needs is a
//! key and a title, and the key is the filename while the title is the first
//! heading. Neither has to be asked for or stored twice, so the model writes
//! one field and the file stays something a person can read and edit.
//!
//! **The key is the identity**, so saving under a key that exists *replaces*
//! that memory. That is the whole of how a wrong memory is corrected rather
//! than contradicted by a second one beside it.
//!
//! **Reading never fails.** A memory file is workspace content, which means it
//! is whatever a person or a previous turn left there. An unreadable one costs
//! that memory, not every turn on the workspace, the same position the skill
//! loader takes on a broken skill.
//!
//! **Every write is atomic.** A temp file beside the target, then a rename, so
//! a crash midway cannot leave half a memory for the next turn to index.
//!
//! The folder lives in the workspace, inside the jail, so `write` and `exec`
//! can both edit it. The `memory` tool is the *intended* way to write these
//! files, not an enforced one.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use darkwire_protocol::json::js_trim;
use parking_lot::Mutex;
use regex::Regex;

use crate::errors::{ErrorKind, Result, WireError};

/// The folder inside the workspace that holds them.
pub const MEMORY_DIRNAME: &str = "memory";

/// The most of one memory that is read at all.
///
/// A different job from [`MAX_MEMORIES`], and the two are not interchangeable:
/// this one stops a runaway *file* from reaching the model, and that one bounds
/// how many index lines it is shown.
///
/// The same figure as the skill cap, and the argument transfers: this is what
/// reading one of these costs when the model opens it. Nothing here reaches the
/// prompt on its own but the title.
pub const MEMORY_MAX_BYTES: usize = 12 * 1024;

/// The most memories one workspace advertises.
///
/// A bound rather than a courtesy: the index costs a line per memory on every
/// request, and a folder that has accumulated a thousand should meet a wall and
/// a log line rather than a prompt nobody budgeted for.
pub const MAX_MEMORIES: usize = 200;

/// How much of a derived title an index line carries, in characters.
///
/// Small on purpose. Two hundred of these are in the prompt on every request,
/// so the line has to be a label rather than a sentence. Counted in `char`s,
/// which is what a person writing a title in any script would count.
pub const MAX_MEMORY_TITLE_CHARS: usize = 80;

/// The longest key a memory may be named. Comfortably a phrase.
pub const MAX_MEMORY_NAME_CHARS: usize = 64;

/// One memory, as it is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    /// The slug: the filename without `.md`, and what a `[[link]]` names.
    pub key: String,
    /// Derived from the content, never stored. See [`derive_title`].
    pub title: String,
    /// The whole file, already bounded by [`MEMORY_MAX_BYTES`].
    pub content: String,
}

/// What [`save_memory`] did, so a caller can say which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveMemoryResult {
    /// The key actually used, which may differ from the one asked for.
    pub key: String,
    /// True when a memory under that key already existed and was replaced.
    pub replaced: bool,
    /// How many memories the folder holds afterwards.
    pub total: usize,
}

/// What [`delete_memory`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteMemoryResult {
    /// The key actually used.
    pub key: String,
    /// False when there was nothing under that key, which is not an error.
    pub existed: bool,
    /// How many memories the folder holds afterwards.
    pub total: usize,
}

static NOT_SLUG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("[^a-z0-9]+").unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s+").unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// Heading hashes, block quotes and list markers at the head of a line.
///
/// A bullet has to be followed by whitespace to count as one. Without that,
/// `- **Bun**` loses the first star of the bold run to the bullet alternative
/// and the rest survives as literal text.
static LEADING_MARKERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\s*(?:#+\s*|>+\s*|[-*+]\s+|\d+[.)]\s+))+")
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// A link or image: the text is the title, the target is noise.
static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"!?\[([^\]]*)\]\([^)]*\)")
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// A paired emphasis run around at least one non-space character.
///
/// **Stars and tildes only, never underscores.** `_italic_` in a title is rare
/// and `snake_case_names` is not, so a rule that caught both would mangle the
/// commoner one. A title here is code as often as it is prose.
///
/// Spelled as alternatives rather than a backreference, because this regex
/// engine has none. Exactly one group matches, so the replacement concatenates
/// all three and the other two are empty.
static EMPHASIS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\*\*(\S|\S.*?\S)\*\*|~~(\S|\S.*?\S)~~|\*(\S|\S.*?\S)\*")
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// The one group of [`EMPHASIS`] that matched.
const EMPHASIS_INNER: &str = "${1}${2}${3}";

/// A model-chosen key as a safe file stem, or `None` when nothing survives.
///
/// **This is where the guarantee the `memory` tool gets for free is restored.**
/// That tool's justification over `write` is that it takes *no path*: nothing
/// for a model to get wrong and nothing for the jail to adjudicate. A key puts
/// a model-chosen string back into a filename, so the guarantee has to be
/// re-established somewhere, and it is here rather than in the tool's schema:
/// the result is `[a-z0-9-]` and nothing else, so it cannot contain `/`, `\`,
/// `.` or `..` and cannot leave `memory/` by construction rather than by a
/// check somebody could forget to call.
///
/// It **slugs rather than rejects**: `UI Stack Preferences` becomes
/// `ui-stack-preferences`, which is the useful answer. Refusing would cost a
/// round trip to teach a rule that could simply have been applied, and callers
/// report the key that was actually used, so nothing is silently renamed.
pub fn memory_slug(raw: &str) -> Option<String> {
    let lowered = js_trim(raw).to_lowercase();
    let dashed = NOT_SLUG.replace_all(&lowered, "-");
    let trimmed = dashed.trim_matches('-');
    // The slug is ASCII by now, so a byte cap is the character cap. The cap can
    // land on a separator, and a stem ending in one is untidy in a directory
    // listing for no reason.
    let capped = &trimmed[..trimmed.len().min(MAX_MEMORY_NAME_CHARS)];
    let slug = capped.trim_end_matches('-');

    if slug.is_empty() {
        return None;
    }
    Some(slug.to_owned())
}

/// The one line a memory contributes to the prompt.
///
/// `key: title`, and nothing else. The key is what the `memory` tool takes, so
/// it is the whole address; a path would be a second spelling of the same thing
/// that the model could reconstruct wrongly, and the kind labels this index
/// used to carry cost two hundred lines' worth of tokens to say what the title
/// already says.
pub fn index_line(memory: &Memory) -> String {
    format!("{}: {}", memory.key, memory.title)
}

/// A title for the index, from the content.
///
/// The first H1, else the first line with anything on it, else the key. Then
/// the markdown comes off, the whitespace collapses and the rest is cut to
/// [`MAX_MEMORY_TITLE_CHARS`].
///
/// Derived rather than asked for, because a description field is a second thing
/// to get wrong: a model that writes one that disagrees with the body leaves an
/// index line that is a lie, and every call pays for it in tokens whether or
/// not it is any good.
pub fn derive_title(content: &str, key: &str) -> String {
    // The whole content is scanned for an H1 before the first line is settled
    // for, so a memory that opens with a note or a `##` section still gets the
    // heading its author wrote rather than the line above it.
    let heading = content.lines().map(str::trim).find_map(|line| {
        line.strip_prefix('#')
            .filter(|rest| rest.starts_with(char::is_whitespace))
    });

    let raw = match heading {
        Some(text) => text,
        None => content
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or(""),
    };

    let stripped = LEADING_MARKERS.replace(raw, "");
    let unlinked = LINK.replace_all(&stripped, "$1");
    // Twice, so a nested run like `**bold _and_ italic**` comes out whole.
    let plain = EMPHASIS.replace_all(&unlinked, EMPHASIS_INNER);
    let plain = EMPHASIS.replace_all(&plain, EMPHASIS_INNER);
    let title = collapse(&plain.replace('`', ""));

    if title.is_empty() {
        return key.to_owned();
    }
    title.chars().take(MAX_MEMORY_TITLE_CHARS).collect()
}

/// One lock per workspace, so writers queue rather than interleave.
///
/// **Process-wide, not per-instance, and that is the point.** A save reports
/// how many memories the folder holds afterwards, so it reads the folder as
/// well as writing to it, and two saves landing together would each report a
/// count that does not know about the other's file. A lock owned by an instance
/// would not be seen by a second one.
///
/// In-process is enough: one process owns an install. Entries are dropped when
/// their last holder lets go, so this does not grow an entry per workspace
/// forever.
static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn with_memory_lock<T>(workspace_root: &Path, work: impl FnOnce() -> T) -> T {
    let lock = LOCKS
        .lock()
        .entry(workspace_root.to_path_buf())
        .or_default()
        .clone();
    let result = {
        let _held = lock.lock();
        work()
    };
    // Identity, not presence: if someone queued behind us they hold a clone,
    // and the entry stays for them.
    let mut locks = LOCKS.lock();
    if locks
        .get(workspace_root)
        .is_some_and(|entry| Arc::ptr_eq(entry, &lock) && Arc::strong_count(entry) == 2)
    {
        locks.remove(workspace_root);
    }
    result
}

/// Every memory key in a workspace, sorted.
///
/// Sorted because the result lands in the provider's cached prefix, and a
/// directory order that varies between hosts would move that prefix for no
/// reason anyone could see. The sort key is UTF-16 code units, the order the
/// web client agrees on.
///
/// **A stem that is not already a slug is skipped.** The key is the whole
/// address now: `read` and `delete` slug what they are handed, so a file called
/// `Auth Sessions.md` is reachable under no key at all. Advertising it would put
/// a line in every prompt naming a memory the tool answers "no memory" for, and
/// that line could never be removed from inside the session. A warning says
/// which file and what to do about it.
fn memory_keys(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut keys: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".md"))
                .map(str::to_owned)
        })
        .filter(|stem| {
            if memory_slug(stem).as_deref() == Some(stem.as_str()) {
                return true;
            }
            tracing::warn!(
                file = %dir.join(format!("{stem}.md")).display(),
                suggested = memory_slug(stem),
                "memory filename is not a usable key; rename it to letters, digits and hyphens"
            );
            false
        })
        .collect();
    keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    keys
}

/// Every loadable memory in a workspace, sorted by key.
///
/// A workspace with no `memory/` folder is the empty list. That is the ordinary
/// case rather than a misconfiguration, so it is not logged.
pub fn read_memories(workspace_root: &Path) -> Vec<Memory> {
    let dir = workspace_root.join(MEMORY_DIRNAME);
    let mut keys = memory_keys(&dir);

    if keys.len() > MAX_MEMORIES {
        tracing::warn!(
            dir = %dir.display(),
            found = keys.len(),
            max = MAX_MEMORIES,
            "more memory files than the cap; the rest are not advertised"
        );
        keys.truncate(MAX_MEMORIES);
    }

    keys.iter().filter_map(|key| read_one(&dir, key)).collect()
}

/// One memory by key, or `None` when there is nothing readable under it.
///
/// Opens the one file rather than scanning the folder, because the caller
/// already has the key: the index gave it one.
pub fn read_memory(workspace_root: &Path, key: &str) -> Option<Memory> {
    let key = memory_slug(key)?;
    read_one(&workspace_root.join(MEMORY_DIRNAME), &key)
}

/// Creates or replaces one memory.
pub fn save_memory(workspace_root: &Path, key: &str, content: &str) -> Result<SaveMemoryResult> {
    // Slugged here as well as by the caller, and deliberately: this is the
    // function that turns a string into a path, so it is the one that must not
    // be able to be handed a bad one.
    let key = slug_or_error(key)?;
    let dir = workspace_root.join(MEMORY_DIRNAME);

    with_memory_lock(workspace_root, || {
        let file = dir.join(format!("{key}.md"));
        let replaced = file.is_file();

        fs::create_dir_all(&dir)?;
        write_atomic(&file, &render_memory(content))?;

        Ok(SaveMemoryResult {
            key,
            replaced,
            total: memory_keys(&dir).len(),
        })
    })
}

/// Removes one memory. A key with nothing under it is not an error.
pub fn delete_memory(workspace_root: &Path, key: &str) -> Result<DeleteMemoryResult> {
    let key = slug_or_error(key)?;
    let dir = workspace_root.join(MEMORY_DIRNAME);

    with_memory_lock(workspace_root, || {
        let existed = match fs::remove_file(dir.join(format!("{key}.md"))) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };

        Ok(DeleteMemoryResult {
            key,
            existed,
            total: memory_keys(&dir).len(),
        })
    })
}

/// The bytes of one memory file: the content, trimmed, one trailing newline.
pub fn render_memory(content: &str) -> String {
    format!("{}\n", js_trim(content))
}

fn slug_or_error(key: &str) -> Result<String> {
    memory_slug(key).ok_or_else(|| {
        WireError::new(
            ErrorKind::InvalidInput,
            format!("memory key is not usable as a filename: {key}"),
        )
        .with_detail("key", key)
    })
}

fn read_one(dir: &Path, key: &str) -> Option<Memory> {
    let file = dir.join(format!("{key}.md"));

    let Ok(text) = fs::read_to_string(&file) else {
        tracing::warn!(memory = key, file = %file.display(), "memory could not be read");
        return None;
    };

    let content = truncate_bytes(&text).to_owned();
    Some(Memory {
        title: derive_title(&content, key),
        key: key.to_owned(),
        content,
    })
}

/// Writes beside the target and renames over it.
///
/// The temp file is in the same directory deliberately: a rename across
/// filesystems is a copy, and a copy is not atomic.
fn write_atomic(target: &Path, text: &str) -> Result<()> {
    let mut temp = target.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    fs::write(&temp, text)?;
    fs::rename(&temp, target)?;
    Ok(())
}

/// One line, whatever it arrived as. A title spanning two breaks an index.
fn collapse(text: &str) -> String {
    js_trim(&WHITESPACE.replace_all(text, " ")).to_owned()
}

/// At most [`MEMORY_MAX_BYTES`] of a file, cut on a character boundary.
fn truncate_bytes(text: &str) -> &str {
    if text.len() <= MEMORY_MAX_BYTES {
        return text;
    }
    // Back up to the start of the character the cap landed inside: the bound
    // holds and a multi-byte character is dropped whole rather than corrupting
    // what follows.
    let mut cut = MEMORY_MAX_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}
