//! What the agent wrote, as Telegram will accept it.
//!
//! Two problems, and they pull in opposite directions. Telegram's MarkdownV2 is
//! not Markdown: eighteen characters are reserved *everywhere*, so an unescaped
//! `.` or `-` — a full stop, a bullet — is a `can't parse entities` 400 that
//! loses the whole message. And a message is capped at 4096 characters, which a
//! `read` answer passes without trying.
//!
//! The shape here is a deliberate middle. A full Markdown→MarkdownV2
//! translation would be a parser, and every gap in it is a lost message;
//! escaping everything is safe but turns the model's code blocks into a wall of
//! backslashes, and code is most of what an agent says worth reading. So the
//! text is split into three kinds of segment — fenced block, inline code, prose
//! — and each is treated as its own thing. Bold and italic survive because they
//! are cheap to recognise; the rest of Markdown renders literally, which is
//! honest rather than broken.
//!
//! The safety net is in the channel, not here: a send that Telegram rejects is
//! retried once with no `parse_mode` at all. That is what makes a bug in this
//! file cost formatting rather than the message, and it is why this can stay
//! small instead of growing a case for every construct.
//!
//! ## Lengths are counted in UTF-16 code units
//!
//! Telegram's 4096 is a count of UTF-16 code units, not of bytes and not of
//! scalar values, so every length and every cut here is measured that way. The
//! cuts themselves still land on character boundaries: an emoji is two units
//! and splitting it would send half a surrogate pair, which is a worse message
//! than a chunk one unit under the limit.

use std::sync::LazyLock;

use regex::Regex;

/// Telegram's own ceiling for one message's text, in UTF-16 code units.
pub const MAX_MESSAGE_CHARS: usize = 4096;

/// Reserved in MarkdownV2 outside code, all eighteen of them.
///
/// From the Bot API docs verbatim, plus `\` itself, which has to be escaped
/// because it is what does the escaping.
const RESERVED: &[char] = &[
    '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!', '\\',
];

/// Inside a code span or block, only these two carry meaning.
const RESERVED_IN_CODE: &[char] = &['`', '\\'];

/// Bold, which MarkdownV2 spells with one asterisk rather than two.
static BOLD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\*\*([^\n*]+)\*\*").unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// Fenced blocks and inline code, in one pass.
static SEGMENTS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)```([^\n`]*)\n?(.*?)```|`([^`\n]+)`")
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// Placeholders, written as escapes rather than as literal bytes.
///
/// Control characters, because they are the one thing a Telegram message
/// cannot contain, so no input can collide with them. Written as `\u{0}` rather
/// than typed, because a raw control byte in a source file is invisible to
/// `grep` and to most editors that will ever open this.
const BOLD_MARK: char = '\u{0}';
const ITALIC_MARK: char = '\u{1}';

/// How many UTF-16 code units this text occupies.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The byte offset at most `units` UTF-16 code units into `text`.
///
/// Rounds down to a character boundary, so a cut that would land inside a
/// surrogate pair takes the shorter piece instead of an unpaired half.
fn byte_index_at_utf16(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (index, character) in text.char_indices() {
        let width = character.len_utf16();
        if seen + width > units {
            return index;
        }
        seen += width;
    }
    text.len()
}

/// Whether a character is one JavaScript's `\w` matches: the class the italic
/// rule below is stated in.
fn is_word(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// Escapes every reserved character. Safe for anything, ugly for code.
pub fn escape_markdown_v2(text: &str) -> String {
    escape(text, RESERVED)
}

fn escape(text: &str, reserved: &[char]) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if reserved.contains(&character) {
            out.push('\\');
        }
        out.push(character);
    }
    out
}

/// One piece of the message, and how it wants to be treated.
#[derive(Debug, PartialEq, Eq)]
enum Segment<'a> {
    /// A fenced block and its info string — `ts` in ```` ```ts ````.
    Fence { body: &'a str, language: &'a str },
    /// An inline code span.
    Code(&'a str),
    /// Everything else.
    Prose(&'a str),
}

/// Splits into fenced blocks, inline code and everything else.
///
/// One pass, and unterminated markers are prose: a model that opens a fence and
/// stops mid-sentence — which a truncated turn does — must still produce a
/// message rather than a parse error.
fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut found = Vec::new();
    let mut index = 0;

    for captures in SEGMENTS.captures_iter(text) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        if whole.start() > index {
            found.push(Segment::Prose(&text[index..whole.start()]));
        }
        if let Some(inline) = captures.get(3) {
            found.push(Segment::Code(inline.as_str()));
        } else {
            found.push(Segment::Fence {
                body: captures.get(2).map_or("", |group| group.as_str()),
                language: captures.get(1).map_or("", |group| group.as_str()).trim(),
            });
        }
        index = whole.end();
    }

    if index < text.len() {
        found.push(Segment::Prose(&text[index..]));
    }
    found
}

/// Wraps every italic run in [`ITALIC_MARK`], leaving everything else alone.
///
/// Hand-written rather than a pattern, because the rule is stated with
/// lookaround — a `_` only opens an italic when the character before it is
/// neither a word character nor a backslash, and only closes when the character
/// after it is not a word character — and `regex` has no lookaround by design.
///
/// Deliberately not a parser, exactly like the bold pattern beside it: a `_`
/// that finds no partner is left alone and escaped like any other underscore,
/// which is what makes this safe on a half-finished sentence.
fn mark_italic(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;

    while at < characters.len() {
        let character = characters[at];
        let opens = character == '_'
            && (at == 0 || !(is_word(characters[at - 1]) || characters[at - 1] == '\\'));
        if opens && let Some(close) = italic_close(&characters, at) {
            out.push(ITALIC_MARK);
            out.extend(&characters[at + 1..close]);
            out.push(ITALIC_MARK);
            at = close + 1;
            continue;
        }
        out.push(character);
        at += 1;
    }
    out
}

/// Where the italic run opened at `open` closes, if it closes at all.
fn italic_close(characters: &[char], open: usize) -> Option<usize> {
    let mut at = open + 1;
    while at < characters.len() && characters[at] != '\n' && characters[at] != '_' {
        at += 1;
    }
    // The body is `[^\n_]+`: at least one character, and terminated by the
    // underscore rather than by a newline or the end of the text.
    if at == open + 1 || at >= characters.len() || characters[at] != '_' {
        return None;
    }
    let closes = characters.get(at + 1).is_none_or(|next| !is_word(*next));
    closes.then_some(at)
}

/// Bold and italic, and nothing else.
///
/// MarkdownV2 spells bold `*x*` and italic `_x_`, so `**x**` has to be rewritten
/// rather than passed through. Both run *before* escaping and put their markers
/// back afterwards through a placeholder no input can contain — doing it the
/// other way round would escape the markers we are trying to keep.
fn prose(text: &str) -> String {
    let bolded = BOLD.replace_all(text, format!("{BOLD_MARK}${{1}}{BOLD_MARK}").as_str());
    let marked = mark_italic(&bolded);
    escape_markdown_v2(&marked)
        .replace(BOLD_MARK, "*")
        .replace(ITALIC_MARK, "_")
}

/// The whole message, ready for `parse_mode: MarkdownV2`.
pub fn to_markdown_v2(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for segment in segments(text) {
        match segment {
            Segment::Prose(body) => out.push_str(&prose(body)),
            Segment::Code(body) => {
                out.push('`');
                out.push_str(&escape(body, RESERVED_IN_CODE));
                out.push('`');
            }
            Segment::Fence { body, language } => {
                out.push_str("```");
                out.push_str(&escape_markdown_v2(language));
                out.push('\n');
                out.push_str(&escape(body, RESERVED_IN_CODE));
                out.push_str("```");
            }
        }
    }
    out
}

/// Whether a line opens or closes a fenced block.
fn fence_delta(line: &str) -> usize {
    line.matches("```").count()
}

/// Hard-splits one over-long line without cutting an escape in half.
///
/// A `\` is only ever the first half of a two-character escape here, so a chunk
/// that ended on one would send a dangling backslash and leave the character it
/// was protecting unescaped at the head of the next.
fn split_line(line: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    let mut parts = Vec::new();
    let mut rest = line;

    while utf16_len(rest) > limit {
        let mut cut = byte_index_at_utf16(rest, limit);
        if rest[..cut].ends_with('\\') {
            cut -= 1;
        }
        if cut == 0 {
            // A limit of one against a leading backslash. Emitting the
            // backslash alone is wrong-looking; emitting nothing is a loop that
            // never ends, and this function has to terminate on every input.
            cut = rest.chars().next().map_or(rest.len(), char::len_utf8);
        }
        parts.push(rest[..cut].to_owned());
        rest = &rest[cut..];
    }

    if !rest.is_empty() {
        parts.push(rest.to_owned());
    }
    parts
}

/// One formatted message, cut into sendable pieces.
///
/// Cuts on line boundaries, because that is the one place a MarkdownV2 escape
/// cannot straddle. A fenced block that spans a cut is **closed and reopened**,
/// so each piece is valid on its own — without that, piece one is an unclosed
/// fence Telegram rejects and piece two is code rendered as prose.
///
/// Returns one empty string for empty input rather than nothing: the caller is
/// sending a message, and an empty list would silently send none.
pub fn chunk_message(text: &str, limit: usize) -> Vec<String> {
    if utf16_len(text) <= limit {
        return vec![text.to_owned()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut open_fence = false;

    // Four characters are held back from *every* chunk, not only from one being
    // built inside a fence. A line can open a fence after it has already been
    // appended, so the chunk holding it is decided to need a closing marker
    // only once it is too late to have made room — reserving unconditionally is
    // what makes "no chunk exceeds the limit" true rather than usually true.
    let room = limit.saturating_sub(4);

    for line in text.split('\n') {
        // A piece that lands in a reopened fence has to leave room for the
        // ```` ```\n ```` in front of it as well as for the closing marker the
        // room already reserves.
        let piece_limit = if open_fence {
            room.saturating_sub(4)
        } else {
            room.saturating_sub(1)
        };
        for piece in split_line(line, piece_limit) {
            let separator = usize::from(!current.is_empty());
            if utf16_len(&current) + separator + utf16_len(&piece) > room {
                let was_open = open_fence;
                flush(&mut chunks, &mut current, was_open);
                if was_open {
                    current.push_str("```");
                }
            }
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(&piece);
        }
        // After the line lands, not before: a chunk that ends *on* the opening
        // fence must still be closed, and one that ends on the closing fence
        // must not be reopened.
        if fence_delta(line) % 2 == 1 {
            open_fence = !open_fence;
        }
    }

    flush(&mut chunks, &mut current, open_fence);
    if chunks.is_empty() {
        vec![String::new()]
    } else {
        chunks
    }
}

fn flush(chunks: &mut Vec<String>, current: &mut String, open_fence: bool) {
    if current.is_empty() {
        return;
    }
    if open_fence {
        current.push_str("\n```");
    }
    chunks.push(std::mem::take(current));
}

/// Undoes MarkdownV2 escaping, for the plain-text retry.
///
/// The text reaching a retry has already been through [`to_markdown_v2`], so it
/// is full of backslashes that would otherwise be shown literally — a
/// worse-looking message than the one that failed.
pub fn strip_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        // Both characters of the pair are consumed, so `\\\\` unescapes to one
        // backslash rather than to none.
        if character == '\\'
            && let Some(escaped) = characters.peek().copied()
            && RESERVED.contains(&escaped)
        {
            out.push(escaped);
            characters.next();
            continue;
        }
        out.push(character);
    }
    out
}
