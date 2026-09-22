//! How much of a page the model actually gets, and where the cut lands.
//!
//! The registry truncates a result to `max_output_chars` and then fences it, and
//! its truncation keeps the **head and the tail** with a marker between them
//! (`truncate_head_tail` in `darkwire-core`). For a search result that means
//! losing the middle: the first extract's opening and the last one's closing
//! paragraph, joined by a marker, which reads as a document with a hole in it.
//! So the tool stays under the budget itself and does its own cut, at a line
//! boundary, naming what was dropped and how to get it.
//!
//! Lengths are UTF-16 code units, because that is the unit the registry counts
//! in. Counting bytes here would let a CJK page overshoot by a factor of three.

/// Room left for the scaffolding written after the share is computed: the
/// `===== [1] url =====` separators, the `# Title` lines, the `> note:` lines
/// and the trailing markers.
pub const RESERVE: usize = 400;

/// No extract is worth printing below this, however many were asked for. Better
/// to read fewer pages properly than to hand back six openings of six
/// paragraphs.
pub const MIN_EXTRACT: usize = 900;

/// The length the registry will measure.
pub fn width(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The head of `text`, cut at a line boundary. Returns it and what was dropped.
///
/// A cut mid-sentence reads as corruption; a cut at a newline reads as an
/// ending. The boundary is only honoured past halfway, so a page that is one
/// enormous line does not come back empty. `0` means no limit.
pub fn cut_at_line(text: &str, limit: usize) -> (String, usize) {
    let total = width(text);
    if limit == 0 || total <= limit {
        return (text.to_owned(), 0);
    }
    let units: Vec<u16> = text.encode_utf16().take(limit).collect();
    let mut head = String::from_utf16_lossy(&units);
    if let Some(at) = head.rfind('\n')
        && width(&head[..at]) > limit / 2
    {
        head.truncate(at);
    }
    let head = head.trim_end().to_owned();
    (head.clone(), total.saturating_sub(width(&head)))
}

/// What one search result's extracts may each spend, and how many fit.
///
/// Returns the per-extract share and the number of extracts it supports. When
/// the budget cannot carry the reads that were asked for, **fewer reads** is the
/// answer rather than a smaller share: overflowing hands the middle of the
/// result to the registry's head-and-tail cut, which is the failure this exists
/// to prevent.
pub fn share(budget: usize, listing: usize, asked: usize) -> (usize, usize) {
    let inline = budget.saturating_sub(RESERVE).saturating_sub(listing);
    if asked == 0 || inline < MIN_EXTRACT {
        return (0, 0);
    }
    let mut fits = asked;
    while fits > 1 && inline / fits < MIN_EXTRACT {
        fits -= 1;
    }
    (inline / fits, fits)
}
