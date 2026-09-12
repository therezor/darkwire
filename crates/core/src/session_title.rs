//! Naming a conversation from the first thing said in it.
//!
//! A session row has always had a `title`, and nothing ever wrote one, so every
//! list of conversations was a list of opaque keys. The fix is deliberately not
//! a model call: a summariser would cost a request, a provider dependency and a
//! failure mode on the hot path of the very first turn, to name something the
//! user is about to read anyway. The first message is what they typed; the
//! first line of it is almost always what the conversation is about.
//!
//! This is a pure function, and it lives here rather than in the agent because
//! the *caller* is the agent loop, which is what makes the CLI, the web and any
//! future channel derive titles identically, without one of them being the
//! "real" implementation the others copy.
//!
//! What it strips is chosen by what a first message actually looks like. Fenced
//! code is the common case that ruins a title: someone pastes a stack trace
//! under one line of question, and a naive slice names the conversation after
//! the stack. Markdown furniture is stripped for the same reason a heading is
//! not part of its own text.

use std::sync::LazyLock;

use ghostai_protocol::json::js_trim;
use regex::Regex;

/// The character budget for a derived title, in UTF-16 code units.
///
/// Wide enough to hold a real sentence, narrow enough that the sidebar
/// truncates with CSS rather than the row growing: the column is
/// `--layout-sidebar` and the rows are single-line.
pub const MAX_TITLE_CHARS: usize = 60;

/// How far back from the budget a space still counts as a word boundary.
///
/// A third. Nearer than that and the title loses a visible amount of its last
/// word's worth of content to avoid a hyphen nobody would have noticed; further
/// and a message with one early space gets cut to almost nothing.
const BOUNDARY_FRACTION: f64 = 1.0 / 3.0;

static FENCED_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)```.*?```").unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// Leading list markers, headings and quotes, per line, not per string.
static LINE_FURNITURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*(?:#{1,6}[ \t]+|>[ \t]*|[-*][ \t]+|[0-9]+\.[ \t]+)")
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s+").unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// A conversation title derived from its first message under the default
/// budget, or `""` when there is nothing worth naming it after.
pub fn derive_session_title(text: &str) -> String {
    derive_session_title_within(text, MAX_TITLE_CHARS)
}

/// A conversation title derived from its first message, or `""` when there is
/// nothing worth naming it after.
///
/// The empty return is meaningful: the caller writes nothing, leaving the
/// stored title empty so that a later message, or a manual rename, can still
/// claim it. `max_chars` is in UTF-16 code units, the unit the sidebar's CSS
/// budget and the stored column both count in.
pub fn derive_session_title_within(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    // Code first: it can contain anything the later passes look for, including
    // `#` comments and lines that read as list items.
    let mut cleaned = FENCED_CODE.replace_all(text, " ").into_owned();

    // A message that was *only* code still deserves a name, and the code is the
    // one thing available to name it after. Backticks come off so the title is
    // the identifier rather than the markup around it.
    if js_trim(&cleaned).is_empty() {
        cleaned = text.replace('`', " ");
    }

    let cleaned = LINE_FURNITURE.replace_all(&cleaned, " ");
    let flat = WHITESPACE.replace_all(&cleaned, " ");
    let flat = js_trim(&flat);
    if flat.is_empty() {
        return String::new();
    }

    let units: Vec<u16> = flat.encode_utf16().collect();
    if units.len() <= max_chars {
        return flat.to_owned();
    }

    // The ellipsis is inside the budget, matching the CLI renderer's clip: a
    // "60 character" title that renders 61 is a column that overflows by one.
    let room = max_chars - 1;
    let boundary = units[..=room]
        .iter()
        .rposition(|&unit| unit == u16::from(b' '));
    let near_enough = nearest_boundary(room);
    let cut = match boundary {
        Some(index) if index >= near_enough => index,
        _ => room,
    };

    let head = String::from_utf16_lossy(&units[..cut]);
    let head = head.trim_end_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
    format!("{head}…")
}

/// The earliest index a word boundary may sit at and still be preferred to a
/// hard cut.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a title budget is a few dozen units; the floor of a fraction of it fits"
)]
fn nearest_boundary(room: usize) -> usize {
    (room as f64 * (1.0 - BOUNDARY_FRACTION)).floor() as usize
}
