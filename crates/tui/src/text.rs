//! How wide a string is on a terminal, and how to cut one to fit.
//!
//! `str::len` is bytes, which is wrong three separate ways for anything a menu
//! draws: an ANSI escape occupies several bytes and no columns, a CJK label
//! occupies three bytes per two columns, and an emoji occupies four bytes for
//! two columns. Prose about to be printed on a line of its own can get away
//! with a byte count. A frame cannot.
//!
//! Every row the renderer addresses is one entry of a `Vec`, and a line that
//! *wraps* because it was one column too wide occupies two rows instead of one
//! — so every later row's address is out by one, and what that looks like is
//! an erase taking a line of the conversation with it. Measuring, cutting and
//! folding to a real width is the invariant the renderer rests on, which is why
//! this module exists at all and why it is worth more than a slice.
//!
//! The measurement is `unicode-width` over grapheme clusters from
//! `unicode-segmentation`. Per cluster that decides: East Asian Wide and
//! Fullwidth are two columns, an emoji-presentation cluster (U+FE0F or a
//! default-emoji code point) is two, a ZWJ sequence is one cluster of two, a
//! combining mark or format character is zero, and everything ambiguous is
//! one — the same answers a terminal gives.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The escape byte. Never spelled as a raw `0x1b` character in a source file,
/// which is invisible in an editor and unsearchable with the tools everyone
/// reaches for first.
const ESC: char = '\x1b';
const BEL: char = '\x07';

/// `\x1b[0m` — closes every SGR attribute at once.
const RESET: &str = "\x1b[0m";

/// The SGR reset, for callers that end a row while a style is still open.
pub const STYLE_RESET: &str = RESET;

/// One piece of a string: an escape sequence, or a run of visible text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Segment<'a> {
    /// An escape sequence, which costs no columns and must survive a cut.
    pub(crate) ansi: bool,
    pub(crate) text: &'a str,
}

/// How many bytes of escape sequence start at `text[at..]`, if any.
///
/// The grammar is the one every terminal shares, tried in this order:
///
///  - CSI, `ESC [` then parameter bytes (`0-9 ; : ?`), then intermediate
///    bytes (`space` through `/`), then one final byte (`@` through `~`) —
///    colour, cursor movement, erase.
///  - The string introducers `ESC ]` (OSC: a window title, a hyperlink),
///    `ESC X`, `ESC ^` and `ESC _` (SOS, PM, APC), each terminated by BEL or by
///    ST (`ESC \`). APC has to be here rather than falling through to the
///    two-byte rule below, which would match only the introducer and leave the
///    payload behind as visible text. That is not hypothetical — the cursor
///    marker is an APC string, and without this it measured as fifteen columns
///    rather than none, so the editor folded its line fifteen columns early.
///  - Fe, the two-byte forms `ESC @` through `ESC _`.
///  - Fp, the private two-byte forms `ESC 0` through `ESC ?`, which is where
///    DECSC and DECRC live — `ESC 7` and `ESC 8`, the save and restore a
///    status bar is built on. Without this they measure as one visible column
///    each, and every width computed over a line carrying them comes out two
///    too wide.
///
/// An unterminated string introducer falls through to the two-byte rule, so
/// the payload stays visible rather than swallowing the rest of the line.
fn escape_len(text: &str, at: usize) -> Option<usize> {
    let rest = text.get(at..)?;
    let mut chars = rest.char_indices();
    if chars.next()?.1 != ESC {
        return None;
    }
    let (_, introducer) = chars.next()?;

    match introducer {
        '[' => {
            let mut end = None;
            for (index, ch) in chars {
                match ch {
                    '0'..='9' | ';' | ':' | '?' | ' '..='/' => {}
                    '@'..='~' => {
                        end = Some(index + ch.len_utf8());
                        break;
                    }
                    _ => break,
                }
            }
            end
        }
        ']' | 'X' | '^' | '_' => {
            let mut previous_esc = false;
            let mut terminated = None;
            for (index, ch) in chars {
                if ch == BEL {
                    terminated = Some(index + ch.len_utf8());
                    break;
                }
                if previous_esc {
                    if ch == '\\' {
                        terminated = Some(index + ch.len_utf8());
                    }
                    break;
                }
                previous_esc = ch == ESC;
            }
            // Two bytes: the introducer alone, when the string never ends.
            Some(terminated.unwrap_or(2))
        }
        '@'..='Z' | '\\'..='_' | '0'..='?' => Some(1 + introducer.len_utf8()),
        _ => None,
    }
}

/// The string split into escape sequences and the runs of text between them.
///
/// Segmenting the raw string into graphemes would be wrong: `\x1b[31m` is not
/// five characters the user can see, and the segmenter has no way to know that.
pub(crate) fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut plain_start = 0;
    let mut at = 0;

    while at < text.len() {
        if let Some(len) = escape_len(text, at) {
            if plain_start < at {
                out.push(Segment {
                    ansi: false,
                    text: &text[plain_start..at],
                });
            }
            out.push(Segment {
                ansi: true,
                text: &text[at..at + len],
            });
            at += len;
            plain_start = at;
        } else {
            at += text[at..].chars().next().map_or(1, char::len_utf8);
        }
    }
    if plain_start < text.len() {
        out.push(Segment {
            ansi: false,
            text: &text[plain_start..],
        });
    }

    out
}

/// The text with every escape sequence removed.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for segment in segments(text) {
        if !segment.ansi {
            out.push_str(segment.text);
        }
    }
    out
}

/// One grapheme cluster's columns: 0, 1 or 2.
fn cluster_width(cluster: &str) -> usize {
    cluster.width()
}

/// How many terminal columns the string occupies.
pub fn visible_width(text: &str) -> usize {
    segments(text)
        .into_iter()
        .filter(|segment| !segment.ansi)
        .flat_map(|segment| segment.text.graphemes(true))
        .map(cluster_width)
        .sum()
}

/// Whether a sequence turns styling on rather than off.
fn opens_style(sequence: &str) -> bool {
    let Some(parameters) = sequence
        .strip_prefix("\x1b[")
        .and_then(|body| body.strip_suffix('m'))
    else {
        return false;
    };
    !parameters.is_empty() && parameters != "0"
}

/// The style sequences still open at the end of `text`, given `open` before it.
///
/// A style that spans a line break has to be re-opened on the next line,
/// because every row is drawn on its own: a run of dim prose whose `\x1b[2m`
/// sits on the line above arrives at the terminal with nothing turning it on.
/// That is not a corner case — a streamed chunk of reasoning is routinely
/// `"\n\nLet me think"`, wrapped whole, so the opener lands on one line and the
/// first words of the paragraph on another. They rendered in plain white
/// against dim grey.
pub fn carry_styles(open: &str, text: &str) -> String {
    let mut carried = open.to_owned();
    for segment in segments(text).into_iter().filter(|segment| segment.ansi) {
        if opens_style(segment.text) {
            carried.push_str(segment.text);
        } else {
            carried.clear();
        }
    }
    carried
}

/// The ellipsis to use when a cut has to fit inside `max`, and the budget left.
fn ellipsis_budget(ellipsis: &str, max: usize) -> (&str, usize) {
    let mark = if visible_width(ellipsis) <= max {
        ellipsis
    } else {
        ""
    };
    (mark, max - visible_width(mark))
}

/// The string cut to at most `max` columns, ellipsis included in the budget.
///
/// Two things this does that a slice cannot. Escape sequences are copied
/// through at no cost, so a cut never lands in the middle of one and prints
/// `[31` as text. And if the cut happens while an SGR attribute is open, a
/// reset is appended — otherwise the colour of the truncated row bleeds down
/// the rest of the menu and out into the transcript below it.
///
/// The ellipsis has to fit as well, or the result is one column over budget —
/// which is exactly the wrap that breaks the erase arithmetic. An ellipsis too
/// wide for `max` is dropped.
pub fn truncate_to_width(text: &str, max: usize, ellipsis: &str) -> String {
    if max == 0 {
        return String::new();
    }
    if visible_width(text) <= max {
        return text.to_owned();
    }

    let (mark, budget) = ellipsis_budget(ellipsis, max);
    let mut out = String::new();
    let mut width = 0;
    let mut styled = false;

    for segment in segments(text) {
        if segment.ansi {
            out.push_str(segment.text);
            styled = opens_style(segment.text);
            continue;
        }
        for cluster in segment.text.graphemes(true) {
            let cost = cluster_width(cluster);
            if width + cost > budget {
                return finish_cut(out, mark, styled);
            }
            out.push_str(cluster);
            width += cost;
        }
    }

    finish_cut(out, mark, styled)
}

fn finish_cut(mut out: String, mark: &str, styled: bool) -> String {
    out.push_str(mark);
    if styled {
        out.push_str(RESET);
    }
    out
}

/// The string padded with spaces to `width` columns. Never truncates.
pub fn pad_to_width(text: &str, width: usize) -> String {
    let short = width.saturating_sub(visible_width(text));
    if short == 0 {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + short);
    out.push_str(text);
    out.extend(std::iter::repeat_n(' ', short));
    out
}

/// Exactly `width` columns: truncated if long, padded if short.
pub fn fit_to_width(text: &str, width: usize) -> String {
    pad_to_width(&truncate_to_width(text, width, "…"), width)
}

/// Two strings on one line, pushed to opposite ends.
///
/// The layout a status row wants, and the reason it lives here rather than in
/// a caller: the gap has to be measured in *columns*, so a right-hand side
/// containing colour, a CJK label or an ellipsis lands in the right place.
///
/// When the two cannot both fit, the right-hand side wins whole and the left
/// is truncated to what is left. A status bar's right end is the model and the
/// context budget; those are the fields that change, and a bar that dropped
/// them to keep a workspace name would be showing the part nobody is watching.
pub fn justify(left: &str, right: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let right_width = visible_width(right);
    if right_width >= width {
        return truncate_to_width(right, width, "…");
    }

    // One column of breathing space, so the two never touch.
    let room = width - right_width - 1;
    let cut = truncate_to_width(left, room, "…");
    let gap = width
        .saturating_sub(visible_width(&cut) + right_width)
        .max(1);
    let mut out = cut;
    out.extend(std::iter::repeat_n(' ', gap));
    out.push_str(right);
    out
}

/// A horizontal rule exactly `width` columns wide, drawn with `glyph`.
pub fn rule(width: usize, glyph: &str) -> String {
    glyph.repeat(width)
}

/// The text with its last grapheme cluster removed. What Backspace should do.
///
/// Popping a `char` takes a code point, which strips one member of a family
/// emoji and leaves the rest. A person pressing Backspace means "the thing I
/// can see", and that is a cluster — the same unit `visible_width` measures in.
pub fn drop_last_grapheme(text: &str) -> String {
    let last = text
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(index, _)| index);
    text.get(..last).unwrap_or("").to_owned()
}

/// The byte index one grapheme cluster before `at`.
///
/// A caret moves by what a person can see, which is a cluster and not a code
/// point: stepping over a family emoji by code point lands between two of its
/// members and the next keystroke splits the family. The same unit
/// `visible_width` measures in and `drop_last_grapheme` deletes.
pub fn previous_boundary(text: &str, at: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|&index| index < at)
        .last()
        .unwrap_or(0)
}

/// The byte index one grapheme cluster after `at`.
pub fn next_boundary(text: &str, at: usize) -> usize {
    text.grapheme_indices(true)
        .find(|&(index, _)| index >= at)
        .map_or(text.len(), |(index, cluster)| index + cluster.len())
}

/// The folding state of one logical line being wrapped.
struct Wrapper {
    rows: Vec<String>,
    row: String,
    used: usize,
    /// Where in `row` the last space sits, when the row has one.
    break_at: Option<usize>,
    /// Re-opened at the head of every row after the first.
    open: String,
}

impl Wrapper {
    fn flush(&mut self, up_to: usize, resume: &str) {
        let finished = self.row.get(..up_to).unwrap_or(&self.row).to_owned();
        self.rows.push(finished);
        self.row = format!("{}{resume}", self.open);
        self.used = visible_width(resume);
        self.break_at = None;
    }

    fn place(&mut self, cluster: &str, width: usize) {
        let cost = cluster_width(cluster);
        // `used > 0` keeps a cluster wider than the whole window from folding
        // forever onto empty rows: it goes on the row and overhangs by one.
        if self.used + cost > width && self.used > 0 {
            match self.break_at {
                // The space itself is dropped: it is what the fold replaces.
                Some(space) => {
                    let resume = self.row.get(space + 1..).unwrap_or("").to_owned();
                    self.flush(space, &resume);
                }
                None => self.flush(self.row.len(), ""),
            }
        }
        if cluster == " " && self.used > 0 {
            self.break_at = Some(self.row.len());
        }
        self.row.push_str(cluster);
        self.used += cost;
    }
}

/// One logical line broken into as many drawn rows as `width` needs.
///
/// One row per entry is what the frame counts in, so nothing may be left for
/// the terminal to fold. Wrapping here is also what makes a resize a re-render:
/// the same logical line is asked for again at the new width and comes back as
/// however many rows it now needs.
///
/// Breaks at a space when there is one, and mid-cluster when there is not: a
/// URL or a hash longer than the window still has to be shown. Styling carries
/// across a break, because a colour that stopped at the fold would be a colour
/// that changed with the window size.
pub fn wrap_to_width(text: &str, width: usize) -> Vec<String> {
    if width == 0 || visible_width(text) <= width {
        return vec![text.to_owned()];
    }

    let mut wrapper = Wrapper {
        rows: Vec::new(),
        row: String::new(),
        used: 0,
        break_at: None,
        open: String::new(),
    };

    for segment in segments(text) {
        if segment.ansi {
            wrapper.row.push_str(segment.text);
            if opens_style(segment.text) {
                wrapper.open.push_str(segment.text);
            } else {
                wrapper.open.clear();
            }
            continue;
        }
        for cluster in segment.text.graphemes(true) {
            wrapper.place(cluster, width);
        }
    }

    wrapper.rows.push(wrapper.row);
    wrapper.rows
}

/// The *end* of the string, cut to at most `max` columns.
///
/// `truncate_to_width` keeps the head, which is right for a label and wrong
/// for something being typed: the interesting end of a message in progress is
/// the end, and a field that froze after its first fifty characters would be a
/// field nobody could use. Escape sequences are stripped rather than carried.
pub fn truncate_start_to_width(text: &str, max: usize, ellipsis: &str) -> String {
    if max == 0 {
        return String::new();
    }
    if visible_width(text) <= max {
        return text.to_owned();
    }

    let (mark, budget) = ellipsis_budget(ellipsis, max);

    // Backwards, cluster by cluster, until the tail fills the budget.
    let stripped = strip_ansi(text);
    let mut kept: Vec<&str> = Vec::new();
    let mut width = 0;
    for cluster in stripped.graphemes(true).rev() {
        let cost = cluster_width(cluster);
        if width + cost > budget {
            break;
        }
        kept.push(cluster);
        width += cost;
    }
    kept.reverse();

    let mut out = String::from(mark);
    out.extend(kept);
    out
}
