//! A selection list, with no terminal attached.
//!
//! Everything a menu decides — which rows match, which one the cursor is on,
//! which slice of them is visible — happens here, in a struct with no I/O, no
//! timers and no streams. [`crate::select`] is then a key map that feeds it
//! keys and hands its lines to a frame, and it is small enough to read in one
//! sitting because this file holds all the arithmetic.
//!
//! That split is what makes the hard part testable. A test for "the window
//! follows the cursor past the bottom of the visible range" constructs a list,
//! calls `move_down` eleven times and reads `render`; it needs no fake tty, no
//! event loop and no timing, which is the difference between an assertion
//! that holds and one that holds on a fast machine.
//!
//! **Filtering is substring, not fuzzy.** A subsequence matcher needs a
//! scoring function, and a scoring function nobody can predict from reading it
//! produces tests that assert whatever the implementation happened to do. A
//! substring match is predictable, and it makes highlighting the match a slice
//! rather than a second search.
//!
//! **No prose.** Not a title, not a footer, not a "nothing matches" line —
//! those are language, they belong to the caller, and they are why this crate
//! has no dependency on the translation layer. The one thing rendered here
//! that is not an item is the `(3/12)` scroll counter, and digits are the same
//! in every locale this ships in.

use crate::text::{fit_to_width, truncate_to_width, visible_width};
use crate::theme::Theme;

/// One row of a menu. `label` is prose the caller has already translated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectItem<T> {
    /// What choosing this row answers with.
    pub value: T,
    /// The text drawn on the row.
    pub label: String,
    /// A dim right-hand column: a model id, a message count, "current".
    pub hint: Option<String>,
    /// Matched by the filter but never drawn.
    pub keywords: Option<String>,
    /// Drawn dim and skipped by the cursor.
    pub disabled: bool,
}

impl<T> SelectItem<T> {
    /// A plain, enabled row with no hint.
    pub fn new(value: T, label: &str) -> Self {
        Self {
            value,
            label: label.to_owned(),
            hint: None,
            keywords: None,
            disabled: false,
        }
    }

    /// Adds the dim right-hand column.
    #[must_use]
    pub fn with_hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.to_owned());
        self
    }

    /// Adds text the filter matches but nobody sees.
    #[must_use]
    pub fn with_keywords(mut self, keywords: &str) -> Self {
        self.keywords = Some(keywords.to_owned());
        self
    }

    /// Marks the row as one the cursor skips.
    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }
}

/// How many rows are visible when nobody says.
pub const DEFAULT_ROWS: usize = 10;
/// `❯ ` and `  ` — the same two columns either way, so nothing shifts.
const CURSOR_GLYPH: &str = "❯ ";
const CURSOR_BLANK: &str = "  ";
const GAP: usize = 2;
/// Below this, a hint column would leave too little room for the label.
const MIN_LABEL: usize = 8;
const MIN_HINT: usize = 6;

/// The cursor, the filter and the visible window over a list of items.
#[derive(Debug, Clone)]
pub struct SelectList<T> {
    all_items: Vec<SelectItem<T>>,
    /// Indices into `all_items`, in display order.
    filtered: Vec<usize>,
    filter_text: String,
    cursor: usize,
    /// The first visible row. Moves only as far as it must to keep up.
    start: usize,
    row_count: usize,
}

impl<T> SelectList<T> {
    /// A list showing `rows` at a time, with the cursor at `index` (clamped).
    pub fn new(items: Vec<SelectItem<T>>, rows: Option<usize>, index: Option<usize>) -> Self {
        let filtered = (0..items.len()).collect();
        let cursor = index.unwrap_or(0).min(items.len().saturating_sub(1));
        let mut list = Self {
            all_items: items,
            filtered,
            filter_text: String::new(),
            cursor,
            start: 0,
            row_count: rows.unwrap_or(DEFAULT_ROWS).max(1),
        };
        list.follow();
        list
    }

    /// The filter text as typed.
    pub fn filter(&self) -> &str {
        &self.filter_text
    }

    /// Where the cursor is, among the matches.
    pub fn index(&self) -> usize {
        self.cursor
    }

    /// How many rows are visible at once.
    pub fn rows(&self) -> usize {
        self.row_count
    }

    /// The items the filter kept, in display order.
    pub fn matches(&self) -> Vec<&SelectItem<T>> {
        self.filtered
            .iter()
            .map(|&index| &self.all_items[index])
            .collect()
    }

    /// The item under the cursor, if there is one.
    pub fn selected(&self) -> Option<&SelectItem<T>> {
        self.filtered
            .get(self.cursor)
            .map(|&index| &self.all_items[index])
    }

    /// Narrows the list, and puts the cursor back at the top.
    ///
    /// Keeping the cursor where it was would mean a keystroke that removes
    /// rows above it silently moves the selection to a different item — so the
    /// next Enter chooses something the operator did not look at.
    pub fn set_filter(&mut self, text: &str) {
        text.clone_into(&mut self.filter_text);
        self.filtered = self.matching(text);
        self.cursor = 0;
        self.start = 0;
        self.skip_disabled(1);
        self.follow();
    }

    /// Clamps the visible window to `rows`, never fewer than one.
    pub fn set_rows(&mut self, rows: usize) {
        self.row_count = rows.max(1);
        self.follow();
    }

    /// One row up, wrapping.
    pub fn move_up(&mut self) {
        self.move_by(-1);
    }

    /// One row down, wrapping.
    pub fn move_down(&mut self) {
        self.move_by(1);
    }

    /// Moves the cursor, wrapping at both ends.
    ///
    /// Wrapping rather than stopping because a list of four agents is faster
    /// to reach the last of by pressing up once than by pressing down three
    /// times, and because "the key did nothing" is the worst answer a menu can
    /// give.
    pub fn move_by(&mut self, delta: i64) {
        let total = self.filtered.len();
        if total == 0 {
            return;
        }
        let modulus = i64::try_from(total).unwrap_or(i64::MAX);
        let cursor = i64::try_from(self.cursor).unwrap_or(0);
        self.cursor = usize::try_from((cursor + delta).rem_euclid(modulus)).unwrap_or(0);
        self.skip_disabled(if delta < 0 { -1 } else { 1 });
        self.follow();
    }

    /// The first row that can be chosen.
    pub fn first(&mut self) {
        self.cursor = 0;
        self.skip_disabled(1);
        self.follow();
    }

    /// The last row that can be chosen.
    pub fn last(&mut self) {
        self.cursor = self.filtered.len().saturating_sub(1);
        self.skip_disabled(-1);
        self.follow();
    }

    /// Where the cursor is and how many rows there are, when some are off
    /// screen.
    ///
    /// `None` when the whole list is visible, because a count of the rows
    /// somebody can see is a count they did not need. Handed out rather than
    /// rendered: it is one short phrase, and whoever draws the footer has a
    /// better row for it than one of its own.
    #[must_use]
    pub fn counter(&self) -> Option<(usize, usize)> {
        let visible = self.row_count.min(self.filtered.len());
        if self.filtered.len() <= visible {
            return None;
        }
        Some((self.cursor + 1, self.filtered.len()))
    }

    /// The visible rows.
    ///
    /// Never returns a line wider than `width`: every cell is measured and cut
    /// before it is coloured, because a line that wraps is a row the
    /// renderer's erase will not reach.
    pub fn render(&self, width: usize, theme: &Theme) -> Vec<String> {
        if self.filtered.is_empty() {
            return Vec::new();
        }

        let visible = self.row_count.min(self.filtered.len());
        self.filtered
            .iter()
            .enumerate()
            .skip(self.start)
            .take(visible)
            .map(|(row, &index)| {
                self.render_row(&self.all_items[index], row == self.cursor, width, theme)
            })
            .collect()
    }

    fn render_row(
        &self,
        item: &SelectItem<T>,
        is_cursor: bool,
        width: usize,
        theme: &Theme,
    ) -> String {
        let glyph = if is_cursor {
            CURSOR_GLYPH
        } else {
            CURSOR_BLANK
        };
        let available = width.saturating_sub(visible_width(glyph)).max(1);
        let hint = item.hint.as_deref().unwrap_or("");

        // The hint is the first thing to go when the window is narrow: a label
        // the operator cannot read is a menu they cannot use, and a model id
        // they cannot read is only a menu that tells them less.
        let room = available.saturating_sub(GAP + MIN_HINT);
        if hint.is_empty() || room < MIN_LABEL {
            let label = truncate_to_width(&item.label, available, "…");
            return Self::paint(
                glyph,
                &self.highlight(&label, theme),
                "",
                is_cursor,
                item,
                theme,
            );
        }

        let widest = self
            .filtered
            .iter()
            .map(|&index| visible_width(&self.all_items[index].label))
            .max()
            .unwrap_or(0);
        let label_width = widest.clamp(MIN_LABEL, room);
        let hint_width = available - label_width - GAP;
        let label = self.highlight(&fit_to_width(&item.label, label_width), theme);

        Self::paint(
            glyph,
            &label,
            &truncate_to_width(hint, hint_width, "…"),
            is_cursor,
            item,
            theme,
        )
    }

    /// Colour, applied last.
    ///
    /// Every width above was measured on uncoloured text, and escape sequences
    /// cost no columns — so doing this at the end is what keeps the arithmetic
    /// and the output describing the same line.
    fn paint(
        glyph: &str,
        label: &str,
        hint: &str,
        is_cursor: bool,
        item: &SelectItem<T>,
        theme: &Theme,
    ) -> String {
        let body = if hint.is_empty() {
            label.to_owned()
        } else {
            format!("{label}{}{}", " ".repeat(GAP), theme.dim.apply(hint))
        };
        if item.disabled {
            return theme.dim.apply(&format!("{glyph}{body}"));
        }
        if is_cursor {
            return theme.cursor.apply(&format!("{glyph}{body}"));
        }
        format!("{glyph}{}", theme.text.apply(&body))
    }

    /// The matched span, marked. A no-op when nothing is being filtered.
    ///
    /// Lower-casing can change a string's byte length (`İ` becomes two
    /// characters), and a span found in the lower-cased label would then name
    /// the wrong bytes of the original. When the lengths differ the label is
    /// drawn unmarked rather than cut inside a character.
    fn highlight(&self, label: &str, theme: &Theme) -> String {
        if self.filter_text.is_empty() {
            return label.to_owned();
        }
        let lowered = label.to_lowercase();
        let needle = self.filter_text.to_lowercase();
        if lowered.len() != label.len() {
            return label.to_owned();
        }
        let Some(at) = lowered.find(&needle) else {
            return label.to_owned();
        };
        let end = at + needle.len();
        let (Some(head), Some(span), Some(tail)) =
            (label.get(..at), label.get(at..end), label.get(end..))
        else {
            return label.to_owned();
        };
        format!("{head}{}{tail}", theme.match_.apply(span))
    }

    /// Case-insensitive substring, over the label, the hint and the keywords.
    ///
    /// Ranked by where the match landed, then by the caller's own order.
    /// Because the haystack is built label-first, a hit in the label outranks
    /// a hit in the hint without that having to be a rule anywhere.
    fn matching(&self, text: &str) -> Vec<usize> {
        let needle = text.trim().to_lowercase();
        if needle.is_empty() {
            return (0..self.all_items.len()).collect();
        }

        let mut hits: Vec<(usize, usize)> = self
            .all_items
            .iter()
            .enumerate()
            .filter_map(|(order, item)| {
                let hay = format!(
                    "{} {} {}",
                    item.label,
                    item.hint.as_deref().unwrap_or(""),
                    item.keywords.as_deref().unwrap_or("")
                )
                .to_lowercase();
                hay.find(&needle).map(|at| (at, order))
            })
            .collect();

        hits.sort_unstable();
        hits.into_iter().map(|(_, order)| order).collect()
    }

    /// Steps off a disabled row in the given direction.
    ///
    /// Bounded by the list length so a list of nothing but disabled rows
    /// leaves the cursor where it was rather than spinning.
    fn skip_disabled(&mut self, step: i64) {
        let total = self.filtered.len();
        if total == 0 {
            return;
        }
        let modulus = i64::try_from(total).unwrap_or(i64::MAX);
        for _ in 0..total {
            let on_disabled = self.selected().is_some_and(|item| item.disabled);
            if !on_disabled {
                return;
            }
            let cursor = i64::try_from(self.cursor).unwrap_or(0);
            self.cursor = usize::try_from((cursor + step).rem_euclid(modulus)).unwrap_or(0);
        }
    }

    /// Moves the window the least amount that puts the cursor back inside it.
    fn follow(&mut self) {
        let visible = self.row_count.min(self.filtered.len());
        let highest = self.filtered.len().saturating_sub(visible);
        if self.cursor < self.start {
            self.start = self.cursor;
        } else if self.cursor >= self.start + visible {
            self.start = self.cursor + 1 - visible;
        }
        self.start = self.start.min(highest);
    }
}
