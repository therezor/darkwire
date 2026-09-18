//! One run of text with a shape: a paragraph, or something that folds.
//!
//! The transcript is a list of these rather than a list of lines, because a
//! terminal that can hide the model's reasoning has to know where the reasoning
//! *ended*. A flat line buffer cannot answer that: by the time the text has been
//! appended it is indistinguishable from the answer around it, and a fold drawn
//! over a guess at where a run started is a fold that eats the sentence above
//! it.
//!
//! What a block is, deliberately, is structure and nothing else. It has a `tag`
//! the caller chooses and this crate never reads, whatever stands in for the
//! body while it is folded, a body, and a flag saying which is showing. It does
//! not know what reasoning is, or a tool, or an answer. Those are the caller's
//! words, and keeping them out is what lets this crate stay free of anything
//! domain-shaped.
//!
//! Three things worth knowing before changing it:
//!
//!  - **The wrap is cached per block, and grows with the text.** Re-wrapping a
//!    conversation on every streamed token costs the size of the session per
//!    word. A block that has not changed hands back the rows it handed back
//!    last time; the one being written to re-wraps only the line that is open.
//!  - **Styles are carried per block.** A dim run that spans a line break has to
//!    re-open on the next row, because the row above has already been drawn. The
//!    state that tracks it belongs to the block, so a reasoning run's dim cannot
//!    leak into whatever is written after it closes.
//!  - **What a collapsed block shows is in the type**, and there are three
//!    answers: prose shows its body and cannot collapse at all, a folding block
//!    shows a summary row, and a hiding block shows nothing. The last is for a
//!    run with nothing worth promising, one row that is either on screen or is
//!    not.

use crate::text::{STYLE_RESET, carry_styles, wrap_to_width};

/// What stands in for a block's body while it is folded.
///
/// Three kinds and not two. A summary is a promise that there is something
/// under the row, and some runs have nothing to promise: a line that is either
/// on screen or is not, with no row left behind to say it exists. A summary of
/// `""` would have said the same thing and said it by accident, and the rule
/// would then have to be repeated as an implicit string check everywhere the
/// summary is read.
#[derive(Debug, Default)]
enum Folded {
    /// Nothing stands in for it, so it cannot fold at all. Prose.
    #[default]
    Never,
    /// One row, shown in place of the body while it is hidden.
    Summary(String),
    /// Nothing at all. Folded, the block is off the screen entirely.
    Away,
}

/// A run of text, with the rows it wraps to at the width last asked for.
#[derive(Debug, Default)]
pub struct Block {
    /// The caller's word for what this is. Compared, never interpreted.
    tag: Option<&'static str>,
    /// What is shown in place of the body when this is folded.
    folded: Folded,
    /// Logical lines, unwrapped. The wrap is layout and happens at render.
    lines: Vec<String>,
    collapsed: bool,
    /// Style sequences still open at the end of the line being written to.
    open: String,
    /// The width `rows` was built for, if it is valid.
    cached_width: Option<usize>,
    rows: Vec<String>,
    /// How many logical lines `rows` covers, and where a re-wrap resumes.
    cached_lines: usize,
    /// How many of `rows` belong to logical lines before the last one.
    ///
    /// The point an append rewinds to: only the last line is still open for
    /// writing, so only its rows can have changed.
    cached_settled: usize,
    /// Whether the open line has been written to since the last render.
    ///
    /// A write carrying no newline lengthens the last line without adding one,
    /// so counting lines does not notice it. This is what does.
    tail_dirty: bool,
}

impl Block {
    /// A block of prose: no summary, and nothing to fold.
    pub fn prose() -> Self {
        Self::default()
    }

    /// A block that shows `summary` when it is folded.
    pub fn folding(tag: &'static str, summary: &str, collapsed: bool) -> Self {
        Self {
            tag: Some(tag),
            folded: Folded::Summary(summary.to_owned()),
            collapsed,
            ..Self::default()
        }
    }

    /// A block that shows nothing at all when it is folded.
    ///
    /// For a run with no summary worth writing: one row that is either on
    /// screen or is not. It is still a block rather than prose because a key
    /// that reveals it has to find every one of them, and that is done by tag.
    pub fn hiding(tag: &'static str, collapsed: bool) -> Self {
        Self {
            tag: Some(tag),
            folded: Folded::Away,
            collapsed,
            ..Self::default()
        }
    }

    /// What the caller called it, if anything.
    pub fn tag(&self) -> Option<&'static str> {
        self.tag
    }

    /// Whether the body is hidden behind the summary.
    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// Whether this block can fold at all.
    pub fn folds(&self) -> bool {
        !matches!(self.folded, Folded::Never)
    }

    /// Shows or hides the body. Prose ignores it: there would be nothing left
    /// on screen to say the text was ever there.
    pub fn set_collapsed(&mut self, collapsed: bool) {
        if self.folds() {
            self.collapsed = collapsed;
        }
    }

    /// Replaces the row shown when folded, for a summary that counts up.
    ///
    /// Ignored by prose and by a block that folds away to nothing, both of
    /// which have no row for it to replace.
    pub fn set_summary(&mut self, summary: &str) {
        if matches!(self.folded, Folded::Summary(_)) {
            self.folded = Folded::Summary(summary.to_owned());
        }
    }

    /// The logical lines, unwrapped.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// How many logical lines it holds.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether anything has been written to it.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Whether the last thing written ended a line.
    pub fn at_line_start(&self) -> bool {
        self.lines.last().is_none_or(|last| *last == self.open)
    }

    /// Takes text the way a stream does, newlines and all.
    pub fn write(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.tail_dirty = true;
        let mut parts = text.split('\n');
        // The first part continues whatever line was open; every later one
        // starts its own. A trailing newline leaves an empty open line, which
        // is exactly what `at_line_start` reports.
        let first = parts.next().unwrap_or("");
        match self.lines.last_mut() {
            Some(held) => held.push_str(first),
            None => self.lines.push(first.to_owned()),
        }
        self.open = carry_styles(&self.open, first);

        // Each new line re-opens whatever was still on, and the line it left
        // closes what it had. A style that spans a break would otherwise arrive
        // with its `\x1b[2m` on the row above, which is a row the terminal has
        // already drawn, so the text under it renders plain. A streamed chunk
        // of reasoning is routinely `"\n\nLet me think"`, dimmed whole, which
        // is exactly that shape.
        for part in parts {
            if !self.open.is_empty()
                && let Some(closing) = self.lines.last_mut()
            {
                closing.push_str(STYLE_RESET);
            }
            self.lines.push(format!("{}{part}", self.open));
            self.open = carry_styles(&self.open, part);
        }
    }

    /// Drops a trailing line that holds nothing but the styles still open.
    ///
    /// That line is not text. It is where the next write would land, and it
    /// exists because the last write ended on a newline. While the block is
    /// being written to it costs nothing, because the next chunk fills it. At a
    /// boundary it is a blank row between two blocks, and the block below is
    /// about to carry the same "start of a line" state anyway.
    pub fn trim_open_line(&mut self) {
        if self.at_line_start() && !self.lines.is_empty() {
            self.lines.pop();
            self.open.clear();
            self.invalidate();
        }
    }

    /// Drops the oldest `count` logical lines. For a transcript staying bounded.
    pub fn drain_front(&mut self, count: usize) {
        let count = count.min(self.lines.len());
        self.lines.drain(..count);
        self.invalidate();
    }

    /// One when the last line is only waiting for the next write, else zero.
    ///
    /// That line is not text. It holds the styles still open and nothing else,
    /// it exists because the last write ended on a newline, and it is where the
    /// next chunk lands. Its visible width is zero, so it wraps to exactly one
    /// row, and drawing it puts a blank row under every message that whatever
    /// sits below the transcript then has to reason about.
    ///
    /// [`Block::trim_open_line`] drops it for good at a block boundary. This
    /// keeps it out of the picture while the block is still being written to,
    /// and both [`Block::height`] and [`Block::render`] go through here so the
    /// two cannot disagree about how tall the block is.
    fn open_row(&self) -> usize {
        usize::from(self.at_line_start() && !self.lines.is_empty())
    }

    /// The row standing in for the body, if there is one, at `width`.
    fn head(&self, width: usize) -> usize {
        match &self.folded {
            Folded::Summary(summary) => wrap_to_width(summary, width).len(),
            Folded::Never | Folded::Away => 0,
        }
    }

    /// How many rows it draws at `width`, without building them a second time.
    pub fn height(&mut self, width: usize) -> usize {
        if self.collapsed && self.folds() {
            return self.head(width);
        }
        let open = self.open_row();
        let head = self.head(width);
        head + self.body(width).len().saturating_sub(open)
    }

    /// Everything this block would draw, as logical lines, leaving it empty.
    ///
    /// What a block hands over when it goes to the scrollback. A folded block
    /// hands over its summary and not the body it is hiding: the row on screen
    /// is what the reader chose to see, and the history is what was on screen.
    pub fn take(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let folded = std::mem::take(&mut self.folded);
        let folds = !matches!(folded, Folded::Never);
        if let Folded::Summary(summary) = folded {
            out.push(summary);
        }
        // Folded, so what was on screen is the summary alone, and for a block
        // that folds away to nothing it is nothing at all. Handing the body
        // over would put in the history precisely what the reader asked not to
        // see; handing a blank row over would put in the history a row the
        // screen never drew, and neither can be rewritten once it is there.
        if self.collapsed && folds {
            self.lines.clear();
            self.invalidate();
            return out;
        }
        out.append(&mut self.lines);
        self.invalidate();
        out
    }

    /// The oldest `count` logical lines, removed.
    pub fn take_front(&mut self, count: usize) -> Vec<String> {
        let count = count.min(self.lines.len());
        let out: Vec<String> = self.lines.drain(..count).collect();
        self.invalidate();
        out
    }

    /// The rows this block draws at `width`.
    ///
    /// Cloned rather than borrowed because the frame it goes into is owned, and
    /// a block that has not changed clones the same `Vec` it cloned last time,
    /// which is a memcpy of a screenful and not a re-wrap of a session.
    pub fn render(&mut self, width: usize) -> Vec<String> {
        if self.collapsed && self.folds() {
            return match &self.folded {
                Folded::Summary(summary) => wrap_to_width(summary, width),
                Folded::Never | Folded::Away => Vec::new(),
            };
        }
        // Before `body`, which borrows mutably.
        let open = self.open_row();
        let mut out = match &self.folded {
            Folded::Summary(summary) => wrap_to_width(summary, width),
            Folded::Never | Folded::Away => Vec::new(),
        };
        let body = self.body(width);
        out.extend(body[..body.len().saturating_sub(open)].iter().cloned());
        out
    }

    /// The wrapped body, rebuilt only as far as it has to be.
    fn body(&mut self, width: usize) -> &[String] {
        // A different width refolds every paragraph, and there is no shortcut:
        // a line that was one row at 120 columns is three at 40, so every row
        // after it moves.
        if self.cached_width != Some(width) {
            self.invalidate();
            self.cached_width = Some(width);
        }

        if self.tail_dirty || self.cached_lines != self.lines.len() {
            self.tail_dirty = false;
            self.rows.truncate(self.cached_settled);
            let resume = self.cached_lines.saturating_sub(1);
            for line in self.lines.iter().skip(resume) {
                self.rows.extend(wrap_to_width(line, width));
            }
            self.cached_lines = self.lines.len();
            // Everything except the rows of the line still open for writing.
            let open_rows = self
                .lines
                .last()
                .map_or(0, |line| wrap_to_width(line, width).len());
            self.cached_settled = self.rows.len().saturating_sub(open_rows);
        }

        &self.rows
    }

    /// Drops the cache whole, so the next render rebuilds it.
    fn invalidate(&mut self) {
        self.cached_width = None;
        self.rows.clear();
        self.cached_lines = 0;
        self.cached_settled = 0;
        self.tail_dirty = false;
    }
}
