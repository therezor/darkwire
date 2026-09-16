//! Turning attached files into something a model can actually read.
//!
//! A `FilePart` is a reference — a workspace path and a MIME type — because
//! that is the only form of an attachment that survives being stored. The two
//! alternatives both rot: a signed URL is dead ten minutes later, and inline
//! base64 puts a megabyte in the stored payload that every replay of that
//! conversation then carries forever. So the bytes are read here instead, once
//! per request, from the jail the turn is already holding.
//!
//! Three ideas are load-bearing.
//!
//!  - **The path goes to the model even when the bytes do too.** An image the
//!    model can see is still a file it may want to crop, convert or measure,
//!    and a 30 MB video is nothing *but* a path. One header line before every
//!    attachment means "use a tool on this" is always available, so the failure
//!    mode of every cap below is degraded rather than blank.
//!
//!  - **"Is it text" is answered by the bytes, never by the MIME type.**
//!    `mime_type_for` calls `.py`, `.ts` and `.yaml` `application/octet-stream`
//!    — its table is deliberately small — and those are exactly the files
//!    someone attaches to an agent. `read_text`'s NUL-byte check gets them
//!    right.
//!
//!  - **The contents are not wrapped in the tool-output nonce.** An attachment
//!    arrives with the message a person typed and carries that message's trust
//!    level; fencing it as untrusted would tell the model to discount the thing
//!    the user just handed it. That is a deliberate difference from tool
//!    output, which comes from the network and does get wrapped. See the path
//!    guard below for what stops it from being a way to read arbitrary files.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use darkwire_core::messages::{ImageSource, image_part, text_part};
use darkwire_core::workspace_files::{DEFAULT_MIME_TYPE, mime_type_for, read_text};
use darkwire_protocol::{ChatMessage, ContentPart, FilePart};
use darkwire_security::{JailCheck, WorkspaceJail};
use darkwire_tools::format_bytes;
use regex::Regex;

/// The largest image sent inline, as base64.
///
/// Below every mainstream provider's per-image limit, and the encoding costs a
/// third on top — so this is ~5.5 MB on the wire, per iteration, for as long as
/// the attachment stays in the context window. A photo from a phone fits; a
/// screen recording does not, and becomes a path.
const MAX_INLINE_IMAGE_BYTES: u64 = 4 * 1024 * 1024;

/// The most file text pasted into a request.
///
/// Not the 512 KiB a human scrolling an editor gets: that much prose is well
/// over 100k tokens, so one attachment could fill a context window on its own.
/// 32 KiB is a long source file or a few thousand CSV rows, and it keeps the
/// drift in the context meter — which sizes the *stored* reference, not this —
/// small enough to ignore.
pub const MAX_INLINE_TEXT_BYTES: u64 = 32 * 1024;

/// The most bytes one request will inline across *all* its attachments.
///
/// The per-file caps above bound one read; this bounds the request, and without
/// it they do not compose. History keeps hundreds of messages and each is
/// re-materialised on every iteration, so a frame carrying one small image a
/// few thousand times — the same path, well under the upload limit — expands to
/// thousands of base64 blocks in one body. Past this budget attachments still
/// appear, as their path: degraded, which is the whole failure model here,
/// rather than an out-of-memory kill.
const MAX_TOTAL_INLINE_BYTES: u64 = 16 * 1024 * 1024;

/// A control character would be interpolated straight into the prompt by
/// `header`, where a newline forges the boundary between one attachment's line
/// and the next. No real path has one.
static CONTROL_CHARS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\x00-\x1f\x7f]").unwrap_or_else(|_| unreachable!()));

/// What the caps are, and whether images may be inlined at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterialiseOptions {
    /// The largest image inlined as base64.
    pub max_image_bytes: u64,
    /// The most file text pasted in.
    pub max_text_bytes: u64,
    /// The budget for one whole request.
    pub max_total_bytes: u64,
    /// Whether images may be inlined at all.
    ///
    /// `false` is the agent's vision setting switched off, and it lands here
    /// rather than anywhere downstream because this is the only place an image
    /// part is produced for a request. It is not a cap — the size the
    /// attachment happens to be is irrelevant — so it is answered before the
    /// two byte checks and never touches the shared budget.
    pub images: bool,
}

impl Default for MaterialiseOptions {
    fn default() -> MaterialiseOptions {
        MaterialiseOptions {
            max_image_bytes: MAX_INLINE_IMAGE_BYTES,
            max_text_bytes: MAX_INLINE_TEXT_BYTES,
            max_total_bytes: MAX_TOTAL_INLINE_BYTES,
            images: true,
        }
    }
}

/// A cache of already-read attachments, scoped to one turn.
///
/// The loop rebuilds the request on every iteration, so a six-tool turn would
/// otherwise read and base64 the same image six times.
///
/// Keyed by the resolved path, size *and* mtime, so an attachment the agent
/// rewrote mid-turn is read again rather than served from before its own edit —
/// which would leave the model holding two versions of one file in a single
/// context window, with nothing to say which is current. The stat that produces
/// the key runs every iteration; it is the read and the base64 encode that this
/// saves, and those are the expensive half by orders of magnitude.
pub type AttachmentCache = HashMap<String, Vec<ContentPart>>;

/// How much of the request's inline budget is left.
#[derive(Debug, Clone, Copy)]
struct Budget {
    remaining: u64,
}

/// The one line that precedes every attachment, whatever else follows it.
///
/// `format_bytes` is the tools' own, not a local copy: the model reads this and
/// `list_dir`'s output in the same context window, and two spellings of
/// "4.2 KB" is a difference it would be entitled to read meaning into.
fn header(path: &str, mime_type: &str, size_bytes: u64) -> String {
    format!(
        "[attachment: {path} · {mime_type} · {}]",
        format_bytes(size_bytes)
    )
}

/// Where an attachment is, or nothing if it is not addressable.
struct Located {
    absolute: String,
    relative: String,
}

/// The absolute path of an attachment, or `None` if it is not addressable.
///
/// A refusal on any rewrite. The jail *clamps* traversal rather than refusing
/// it — `../../etc/passwd` becomes `etc/passwd` inside the workspace — which is
/// right for a model that guessed at a path and wrong here: this path came off
/// a client frame, and a clamp would silently read a different file than the one
/// named while looking like a success. A genuine upload path never contains
/// `..`, so refusing costs nothing.
fn locate(part: &FilePart, jail: &WorkspaceJail) -> Option<Located> {
    if CONTROL_CHARS.is_match(&part.path) {
        return None;
    }

    let JailCheck::Accept(accept) = jail.check(&part.path) else {
        return None;
    };
    if !accept.rewrites.is_empty() {
        return None;
    }
    // The jail's own relative form, not the requested string. They agree on any
    // path an upload produced, but `./uploads/x.png` normalises without being
    // recorded as a rewrite — and the path shown to the model has to be the one
    // `read_file` and `list_dir` will echo back, or it learns two names for one
    // file.
    Some(Located {
        absolute: accept.path.to_string_lossy().into_owned(),
        relative: accept.relative,
    })
}

/// One file reference, as the provider can read it. Never returns a file part.
///
/// Note what the failure branch does *not* say: it names the attachment by the
/// path that was asked for only when that path was legal. A rejected path is
/// reported without echoing it, for the same reason `read_file` reports where a
/// read landed rather than what was requested — repeating it back would teach
/// the model that the workspace has paths it does not have.
pub fn materialise_file_part(
    part: &FilePart,
    jail: &WorkspaceJail,
    options: MaterialiseOptions,
    cache: Option<&mut AttachmentCache>,
) -> Vec<ContentPart> {
    let mut budget = Budget {
        remaining: options.max_total_bytes,
    };
    materialise_one(part, jail, options, cache, &mut budget)
}

fn materialise_one(
    part: &FilePart,
    jail: &WorkspaceJail,
    options: MaterialiseOptions,
    cache: Option<&mut AttachmentCache>,
    budget: &mut Budget,
) -> Vec<ContentPart> {
    let Some(found) = locate(part, jail) else {
        return vec![text_part(
            "[attachment: unavailable — the path is not inside this workspace]",
        )];
    };
    let relative = &found.relative;

    // The size on disk, never the size the part claims: that arrived on a
    // client frame, and every cap below is a memory bound.
    let Ok(stats) = fs::metadata(&found.absolute) else {
        return vec![text_part(format!(
            "[attachment: {relative} — no longer in the workspace]"
        ))];
    };
    if stats.is_dir() {
        return vec![text_part(format!(
            "[attachment: {relative} — a directory, not a file]"
        ))];
    }
    let size_bytes = stats.len();
    let mtime_ms = modified_ms(&stats);

    // Derived from the path, not taken from the part. This one value decides
    // whether the file is read as an image or as text, and the part's own type
    // came off a client frame — a text file labelled `image/png` would be
    // base64'd and sent to a vision model as garbage. The table's answer is the
    // same one the upload route recorded, and it falls back to the part only
    // where the table has nothing, so a channel that knows better is not
    // overruled.
    let derived = mime_type_for(relative);
    let mime_type: &str = if derived == DEFAULT_MIME_TYPE {
        &part.mime_type
    } else {
        derived
    };
    let line = header(relative, mime_type, size_bytes);

    if size_bytes == 0 {
        return vec![text_part(format!("{line} — the file is empty"))];
    }

    // Identity is path, size and mtime together: a file the agent rewrote
    // mid-turn has to be read again, or the model holds the version from before
    // its own edit beside the one `read_file` just returned. Size and mtime
    // lead so the delimiter needs no NUL: both are digits, and the first `:`
    // after them ends the number. A raw NUL in a source file is a real cost —
    // it makes the whole file invisible to `grep`.
    let key = format!("{size_bytes}:{mtime_ms}:{}", found.absolute);
    if let Some(cache) = cache.as_ref()
        && let Some(hit) = cache.get(&key)
    {
        return hit.clone();
    }

    let parts = read_parts(&found, mime_type, &line, size_bytes, options, budget);
    if let Some(cache) = cache {
        cache.insert(key, parts.clone());
    }
    parts
}

fn read_parts(
    found: &Located,
    mime_type: &str,
    line: &str,
    size_bytes: u64,
    options: MaterialiseOptions,
    budget: &mut Budget,
) -> Vec<ContentPart> {
    // An image is inlined as an image or named as a path, and never falls
    // through to the text branch below. Nothing rules out a small uncompressed
    // image holding no NUL byte, and `read_text` would then happily fence a
    // screenful of binary as if it were the file's contents.
    if mime_type.starts_with("image/") {
        // Before the byte checks and before the budget, because this is not a
        // cap: the model cannot read an image of any size, so spending budget
        // on one would take room away from the text attachments it *can* read.
        if !options.images {
            return vec![text_part(format!(
                "{line} — this model cannot read images; use the file tools"
            ))];
        }
        if size_bytes > options.max_image_bytes {
            return vec![text_part(format!(
                "{line} — too large to show; use the file tools to read it"
            ))];
        }
        if size_bytes > budget.remaining {
            return vec![text_part(format!(
                "{line} — not shown inline; use the file tools to read it"
            ))];
        }
        budget.remaining -= size_bytes;
        // `data`, never `url`: it works offline, works on every provider, needs
        // no second round trip out of our own network, and cannot expire.
        return match fs::read(&found.absolute) {
            Ok(bytes) => vec![
                text_part(line),
                image_part(mime_type, ImageSource::Data(BASE64.encode(bytes))),
            ],
            Err(_) => vec![text_part(format!("{line} — could not be read"))],
        };
    }

    if size_bytes <= options.max_text_bytes && size_bytes <= budget.remaining {
        match read_text(Path::new(&found.absolute), size_bytes) {
            Err(_) => return vec![text_part(format!("{line} — could not be read"))],
            Ok(Some(text)) => {
                budget.remaining -= size_bytes;
                let note = if text.truncated {
                    "\n\n[…truncated — read the file for the rest]"
                } else {
                    ""
                };
                return vec![text_part(format!(
                    "{line}\n\n```\n{}\n```{note}",
                    text.content
                ))];
            }
            Ok(None) => {}
        }
    }

    vec![text_part(format!(
        "{line} — not shown inline; use the file tools to read it"
    ))]
}

/// Modification time in epoch milliseconds, or `0` where the platform has none.
///
/// Only ever a cache key, so a host that cannot report one degrades to
/// "path and size decide", not to a wrong answer.
fn modified_ms(stats: &fs::Metadata) -> u128 {
    stats
        .modified()
        .ok()
        .and_then(|when| when.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_millis())
}

/// A legacy image part that no provider can fetch.
///
/// Before attachments were workspace files, the web put a *relative* signed URL
/// here, which every provider silently failed on and which the degradation
/// ladder then stripped, so the turn appeared to succeed without the image.
/// Those rows are still in storage. Nothing rewrites history, but there is no
/// reason to keep paying a guaranteed 4xx and a retry for them on every
/// iteration of every turn.
fn is_unfetchable(part: &ContentPart) -> bool {
    let ContentPart::Image(image) = part else {
        return false;
    };
    if image.data.is_some() {
        return false;
    }
    image.url.as_ref().is_none_or(|url| !SCHEME.is_match(url))
}

/// An absolute URL's scheme, which a relative signed URL does not have.
static SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.\-]*:").unwrap_or_else(|_| unreachable!()));

fn materialise_content(
    content: &[ContentPart],
    jail: &WorkspaceJail,
    options: MaterialiseOptions,
    cache: Option<&mut AttachmentCache>,
    budget: &mut Budget,
) -> Option<Vec<ContentPart>> {
    if !content
        .iter()
        .any(|part| matches!(part, ContentPart::File(_)) || is_unfetchable(part))
    {
        return None;
    }

    let mut cache = cache;
    let mut parts: Vec<ContentPart> = Vec::with_capacity(content.len());
    for part in content {
        match part {
            ContentPart::File(file) => {
                parts.extend(materialise_one(
                    file,
                    jail,
                    options,
                    cache.as_deref_mut(),
                    budget,
                ));
            }
            other if is_unfetchable(other) => parts.push(text_part(
                "[image: unavailable — this attachment predates workspace attachments]",
            )),
            other => parts.push(other.clone()),
        }
    }
    Some(parts)
}

/// Every message's content, with file parts replaced by what a provider reads.
///
/// Returns the input unchanged when there is nothing to do, which is the common
/// case: most turns have no attachments anywhere in their history, and this
/// runs on every iteration of every one of them.
pub fn materialise_attachments(
    messages: Vec<ChatMessage>,
    jail: &WorkspaceJail,
    options: MaterialiseOptions,
    cache: Option<&mut AttachmentCache>,
) -> Vec<ChatMessage> {
    // One budget for the whole request. The per-file caps bound a single read;
    // only this bounds the request, and without it they do not compose.
    let mut budget = Budget {
        remaining: options.max_total_bytes,
    };
    let mut cache = cache;

    messages
        .into_iter()
        .map(|message| match message {
            ChatMessage::System(_) | ChatMessage::Tool(_) => message,
            ChatMessage::User(mut user) => {
                if let Some(content) = materialise_content(
                    &user.content,
                    jail,
                    options,
                    cache.as_deref_mut(),
                    &mut budget,
                ) {
                    user.content = content;
                }
                ChatMessage::User(user)
            }
            ChatMessage::Assistant(mut assistant) => {
                if let Some(content) = materialise_content(
                    &assistant.content,
                    jail,
                    options,
                    cache.as_deref_mut(),
                    &mut budget,
                ) {
                    assistant.content = content;
                }
                ChatMessage::Assistant(assistant)
            }
        })
        .collect()
}
