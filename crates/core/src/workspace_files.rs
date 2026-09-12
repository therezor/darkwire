//! Reading workspace files: what type a file is, and whether it is text.
//!
//! Here rather than in the server, because the agent loop needs both answers
//! too. An attached file has to be turned into something a model can read
//! (bytes for an image, characters for a `.csv`), and that decision has to
//! come out the same way whether it is being made to answer
//! `GET /api/files/text` or to build a provider request. Two copies of the MIME
//! table is two answers to "is this an image".
//!
//! The MIME table is small and deliberately so. It exists to make a browser
//! render an image inline, not to be a complete registry, and a type it does
//! not know becomes `application/octet-stream`, which downloads rather than
//! executes. The corollary matters more than the table does: **it answers
//! `application/octet-stream` for `.py`, `.ts` and `.yaml`**, so nothing may
//! use it to decide whether a file is text. That is [`read_text`]'s job, and it
//! reads the bytes.

use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use crate::errors::Result;

/// Extension to MIME type, for the types a chat UI actually renders.
const MIME_TYPES: &[(&str, &str)] = &[
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".gif", "image/gif"),
    (".webp", "image/webp"),
    (".avif", "image/avif"),
    (".bmp", "image/bmp"),
    (".ico", "image/x-icon"),
    (".pdf", "application/pdf"),
    (".txt", "text/plain; charset=utf-8"),
    (".md", "text/markdown; charset=utf-8"),
    (".csv", "text/csv; charset=utf-8"),
    (".json", "application/json; charset=utf-8"),
    (".log", "text/plain; charset=utf-8"),
    (".mp3", "audio/mpeg"),
    (".wav", "audio/wav"),
    (".ogg", "audio/ogg"),
    (".m4a", "audio/mp4"),
    (".mp4", "video/mp4"),
    (".webm", "video/webm"),
];

/// What a file of unknown type is served as. Downloads rather than executes.
pub const DEFAULT_MIME_TYPE: &str = "application/octet-stream";

/// The MIME type for a path, by extension, case-insensitively.
pub fn mime_type_for(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()));
    let Some(extension) = extension else {
        return DEFAULT_MIME_TYPE;
    };
    MIME_TYPES
        .iter()
        .find_map(|(known, mime)| (*known == extension).then_some(*mime))
        .unwrap_or(DEFAULT_MIME_TYPE)
}

/// The most bytes a text read returns.
///
/// A workspace holds whatever the agent wrote to it, and "open the 400 MB log
/// the last turn produced" must not be a way to make the server allocate 400 MB
/// or the tab freeze rendering it. Past this the read returns a prefix and says
/// so, and the editor goes read-only: a saved prefix would delete the rest.
pub const MAX_TEXT_BYTES: u64 = 512 * 1024;

/// One file's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceText {
    /// The text, lossily decoded.
    pub content: String,
    /// Whether only the first [`MAX_TEXT_BYTES`] were read.
    pub truncated: bool,
}

/// One file as text, or `None` when the bytes are not text.
///
/// "Not text" is a NUL byte in the prefix: the same heuristic `git` uses, and
/// for the same reason. It is the one signal that costs nothing and is almost
/// never wrong about a real file. The alternative, trusting the extension, is
/// wrong in both directions here, because the MIME table above is deliberately
/// small and answers `application/octet-stream` for `.py`, `.ts` and every
/// other source file a person would actually want to open.
///
/// Only the first [`MAX_TEXT_BYTES`] are read, not the whole file and then a
/// slice: the size is whatever the agent wrote, and reading it all is the
/// allocation this exists to avoid. `size_bytes` is what the caller already
/// knows from `stat`, so the cap can be decided before the file is opened.
pub fn read_text(absolute_path: &Path, size_bytes: u64) -> Result<Option<WorkspaceText>> {
    let cap = size_bytes.min(MAX_TEXT_BYTES);
    let mut bytes = Vec::with_capacity(usize::try_from(cap).unwrap_or(usize::MAX));
    File::open(absolute_path)?
        .take(cap)
        .read_to_end(&mut bytes)?;

    if bytes.contains(&0) {
        return Ok(None);
    }

    // Lossy on purpose. A cut at the cap can land mid-codepoint, which costs
    // one replacement character at the very end of content that is already
    // read-only for being truncated. A strict decoder would turn that into a
    // failure to open the file at all.
    Ok(Some(WorkspaceText {
        content: String::from_utf8_lossy(&bytes).into_owned(),
        truncated: size_bytes > cap,
    }))
}
