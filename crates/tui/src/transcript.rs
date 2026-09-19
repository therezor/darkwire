//! Everything already said, as a list of blocks rather than a list of lines.
//!
//! The renderer redraws its frame when the window changes width, so something
//! has to hold what is on it. This is that. Writes arrive the way a stream takes them, in fragments that
//! mostly do not end on a line break, and are kept as *logical* lines inside
//! [`Block`]s. The wrap happens at render time against the current width, so a
//! narrower window refolds the paragraph instead of clipping it.
//!
//! **Why blocks and not lines.** A terminal that can hide the model's reasoning
//! has to know where the reasoning ended, and a flat line buffer cannot say: by
//! the time the text is appended it looks like every other line. So the caller
//! marks a run with [`Transcript::open_block`] and closes it, and what happens
//! between those two calls is one foldable thing. Prose needs no marking and
//! accumulates in an ordinary block, which is what a transcript with no folds
//! at all is: exactly one of them.
//!
//! **What is on screen, and what is behind it.** Everything that can go, goes,
//! on every draw. [`Transcript::take_committable`] returns the logical lines to
//! print above the frame, and what is left is the run still open and the line
//! still being written. Printed text is the terminal's now. It reflows on a
//! resize for free, it can be selected and searched with the emulator's own
//! tools, and nothing here can rewrite it, which is why a block stops being
//! foldable once it has gone.
//!
//! That last part is the trade, and it is deliberate. A fold is decided before
//! a block is printed rather than after, so the keys that fold reach the run
//! still open and set the default for what follows. The alternative is holding
//! the session in a buffer this program redraws, which is what made a resize
//! something it could get wrong.
//!
//! One block is exempt and deliberately so: the one still being written to. Its
//! *finished* lines commit as they complete, so a long answer does not grow the
//! frame. A folding block that is still open never commits at all, because
//! its summary is still changing. An expanded run longer than the window is
//! therefore the one case where the live region can exceed its cap. Both kinds
//! of run fold by default, so in the usual configuration it does not arise.
//!
//! Two things are worth knowing before changing it:
//!
//!  - **The wrap is cached per block.** A keystroke re-renders the frame, and
//!    re-wrapping a long conversation on every keypress would make typing cost
//!    the size of the session. A streamed token is the same problem once a
//!    second: only the block being written to re-wraps, and only its open line.
//!  - **It is bounded.** A session that streams for hours would otherwise grow
//!    without limit, and a redraw prints every row it holds. The oldest lines go
//!    in batches rather than one at a time, because dropping one shifts every
//!    row and costs a full redraw.

use crate::block::Block;
use crate::component::Component;
use crate::text::wrap_to_width;

/// Past this many logical lines the oldest are dropped.
const LIMIT: usize = 10_000;
/// How many go at once when it does, so the redraw is paid rarely.
const DROP: usize = 2_000;

/// How many rows a logical line takes at `width`, as the terminal folds it.
fn wrapped_rows(line: &str, width: usize) -> usize {
    wrap_to_width(line, width).len().max(1)
}

/// How many finished lines of the open block go to the scrollback at once.
const COMMIT_BATCH: usize = 16;

/// The conversation so far.
#[derive(Debug)]
pub struct Transcript {
    /// Never empty: there is always a block to write into.
    blocks: Vec<Block>,
    /// How many logical lines the whole transcript holds, for the bound.
    total: usize,
    /// Whether anything has been printed above the frame yet.
    ///
    /// A flag rather than the two hundred lines this used to keep. Those were
    /// there to reprint after a repaint erased the screen, and nothing erases
    /// the screen any more: what has been printed belongs to the terminal, and
    /// printing it again is a second copy of the session in the history.
    committed: bool,
}

impl Default for Transcript {
    fn default() -> Self {
        Self {
            blocks: vec![Block::prose()],
            total: 0,
            committed: false,
        }
    }
}

impl Transcript {
    /// An empty transcript, ready to be written to.
    pub fn new() -> Self {
        Self::default()
    }

    /// The block everything is written to.
    fn tail(&mut self) -> &mut Block {
        // The invariant this rests on is set in `Default` and preserved by
        // every method here: the list is never emptied, only reset to one.
        if self.blocks.is_empty() {
            self.blocks.push(Block::prose());
        }
        let at = self.blocks.len() - 1;
        &mut self.blocks[at]
    }

    /// Whether the last thing written ended a line.
    pub fn at_line_start(&self) -> bool {
        self.blocks.last().is_none_or(Block::at_line_start)
    }

    /// Every logical line, in order, with no regard for where blocks divide.
    pub fn lines(&self) -> Vec<&str> {
        self.blocks
            .iter()
            .flat_map(|block| block.lines().iter().map(String::as_str))
            .collect()
    }

    /// Takes text the way a stream does, newlines and all.
    pub fn write(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let before = self.tail().len();
        self.tail().write(text);
        let after = self.blocks.last().map_or(0, Block::len);
        self.total += after.saturating_sub(before);
        self.enforce_limit();
    }

    /// Starts a foldable run, which everything written now belongs to.
    ///
    /// `tag` is the caller's word for what kind of run it is, and is what
    /// [`Transcript::set_collapsed`] addresses a whole class of them by. This
    /// crate compares it and nothing else.
    pub fn open_block(&mut self, tag: &'static str, summary: &str, collapsed: bool) {
        // The line the last write left open belongs to whatever comes next, and
        // what comes next is this block. Keeping it would draw a blank row
        // between the answer and the fold under it, on every fold.
        if let Some(tail) = self.blocks.last_mut() {
            tail.trim_open_line();
        }
        self.drop_empty_tail();
        self.blocks.push(Block::folding(tag, summary, collapsed));
    }

    /// Starts a run that shows nothing at all while it is folded.
    ///
    /// For the one row somebody asks for after the fact rather than during. It
    /// is a block and not prose for the same reason a summarised run is: a key
    /// that reveals it has to reach every one of them, and
    /// [`Transcript::set_collapsed`] finds them by tag.
    pub fn hide_block(&mut self, tag: &'static str, collapsed: bool) {
        // The same preamble `open_block` gives, and for the same reason.
        if let Some(tail) = self.blocks.last_mut() {
            tail.trim_open_line();
        }
        self.drop_empty_tail();
        self.blocks.push(Block::hiding(tag, collapsed));
    }

    /// Ends the open run. Prose written after this lands in a block of its own.
    ///
    /// A run that said nothing goes rather than closing: a provider that opens
    /// and closes its reasoning channel with no content in it is common, and a
    /// summary row for a body that was never written is a fold that opens onto
    /// nothing.
    pub fn close_block(&mut self) {
        if !self.blocks.last().is_some_and(Block::folds) {
            return;
        }
        if let Some(tail) = self.blocks.last_mut() {
            tail.trim_open_line();
        }
        if self.blocks.last().is_some_and(Block::is_empty) && self.blocks.len() > 1 {
            self.blocks.pop();
        }
        // Always, even after dropping the empty run above. Trimming took the
        // line the next write was going to land on, so the block underneath
        // must not become the tail again: a write would join the answer onto
        // the end of whatever was there before the run started.
        self.blocks.push(Block::prose());
    }

    /// Replaces the open run's summary, for one that counts up while it runs.
    pub fn set_summary(&mut self, summary: &str) {
        if let Some(block) = self.blocks.last_mut() {
            block.set_summary(summary);
        }
    }

    /// Folds or unfolds every block carrying `tag`.
    ///
    /// All of them rather than the open one, because the key that does this is
    /// pressed to answer "show me what it was thinking", and a session holds
    /// more than one run of it.
    pub fn set_collapsed(&mut self, tag: &'static str, collapsed: bool) {
        for block in &mut self.blocks {
            if block.tag() == Some(tag) {
                block.set_collapsed(collapsed);
            }
        }
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.blocks.push(Block::prose());
        self.total = 0;
        self.committed = false;
    }

    /// A tail block nothing was written to is not worth keeping.
    ///
    /// Opening a run right after closing one would otherwise leave an empty
    /// block between them, which renders as nothing and still costs a row of
    /// comparison on every frame.
    fn drop_empty_tail(&mut self) {
        if self
            .blocks
            .last()
            .is_some_and(|block| block.is_empty() && !block.folds())
        {
            self.blocks.pop();
        }
    }

    /// Drops the oldest lines once there are too many of them.
    ///
    /// Whole blocks go first, because a block that has lost every line is a
    /// summary row for text nobody can read.
    fn enforce_limit(&mut self) {
        if self.total <= LIMIT {
            return;
        }
        let mut owed = DROP;
        while owed > 0 {
            let Some(held) = self.blocks.first().map(Block::len) else {
                break;
            };
            // The last block is the one being written to. Its lines can go; it
            // cannot, because there has to be somewhere for the next write to
            // land.
            if held <= owed && self.blocks.len() > 1 {
                self.blocks.remove(0);
                owed -= held;
                self.total -= held;
                continue;
            }
            let take = owed.min(held);
            if take == 0 {
                break;
            }
            if let Some(oldest) = self.blocks.first_mut() {
                oldest.drain_front(take);
            }
            self.total -= take;
            owed -= take;
        }
    }
}

// ------------------------------------------------------- the scrollback line

impl Transcript {
    /// How many rows the live region draws at `width`.
    pub fn height(&mut self, width: usize) -> usize {
        self.blocks
            .iter_mut()
            .map(|block| block.height(width))
            .sum()
    }

    /// The lines to print above the frame so it fits in `cap` rows.
    ///
    /// Returned as *logical* lines rather than rows, unwrapped: they are going
    /// to the terminal's own buffer, and a terminal that wrapped them itself is
    /// a terminal that can reflow them when the window moves. Wrapping them here
    /// would freeze them at today's width.
    ///
    /// Empty when the frame already fits, which is every frame of a short
    /// session and most frames of a long one.
    pub fn take_committable(&mut self, cap: usize, width: usize) -> Vec<String> {
        let mut out = Vec::new();
        while self.height(width) > cap {
            if !self.commit_one(&mut out) {
                break;
            }
        }
        self.committed |= !out.is_empty();
        out
    }

    /// Commits a run that is still open, because it no longer fits.
    ///
    /// The one case where a fold is taken away rather than offered. A block is
    /// held while it is open so the reader can still change its mind about it,
    /// and a tool printing ten thousand lines would hold the whole screen on
    /// that promise. Past `cap` rows the promise is the thing that goes: the
    /// lines are printed, and what is printed is history.
    ///
    /// Returns nothing when the live region already fits, which is every frame
    /// that is not a tool being noisy.
    pub fn give_up_the_fold(&mut self, cap: usize, width: usize) -> Vec<String> {
        let mut out = Vec::new();
        while self.height(width) > cap {
            // Only the block being written to can still fold; anything else has
            // already been offered to `commit_one`, which took it.
            let Some(tail) = self.blocks.last_mut() else {
                break;
            };
            let finished = tail.len().saturating_sub(1);
            if finished == 0 {
                break;
            }
            let take = finished.min(COMMIT_BATCH);
            let lines = tail.take_front(take);
            self.total = self.total.saturating_sub(lines.len());
            out.extend(lines);
        }
        self.committed |= !out.is_empty();
        out
    }

    /// Drops the oldest `rows` rows without printing them.
    ///
    /// For a window that shrank. The terminal scrolled that many rows off the
    /// top to keep the cursor visible, so they are already in the history:
    /// committing them would put a second copy directly under the first.
    ///
    /// Counted in rows rather than lines, because that is what the window lost,
    /// and a line that wrapped to three of them accounts for three. Lines still
    /// go whole, so this can forget a row more than the window took. That
    /// direction is the safe one: the row is in the history either way, and the
    /// other direction prints it a second time.
    pub fn forget_front(&mut self, rows: usize, width: usize) {
        let mut owed = rows;
        while owed > 0 {
            // The block being written to keeps its open line; there has to be
            // somewhere for the next write to land.
            let only = self.blocks.len() == 1;
            let Some(first) = self.blocks.first_mut() else {
                break;
            };
            let available = if only {
                first.len().saturating_sub(1)
            } else {
                first.len()
            };
            if available == 0 {
                break;
            }
            let went = first.take_front(1);
            self.total = self.total.saturating_sub(went.len());
            let height: usize = went.iter().map(|line| wrapped_rows(line, width)).sum();
            owed = owed.saturating_sub(height.max(1));
            self.committed = true;
            self.drop_empty_front();
        }
    }

    /// A leading block that has lost every line is a summary over nothing.
    fn drop_empty_front(&mut self) {
        while self.blocks.len() > 1 && self.blocks[0].is_empty() {
            self.blocks.remove(0);
        }
    }

    /// Whether anything has gone to the terminal's own scrollback yet.
    ///
    /// The question a caller asks to know whether the screen above the frame is
    /// its own conversation or is still whatever the shell left there. While
    /// this is false the frame is the only thing the program has drawn, so it
    /// is free to pad itself to the window; once it is true the terminal has
    /// scrolled and padding would push real conversation off the top.
    pub fn committed_anything(&self) -> bool {
        self.committed
    }

    /// Hands over the next thing that may go, or says nothing may.
    fn commit_one(&mut self, out: &mut Vec<String>) -> bool {
        // Everything except the block being written to goes whole. Half a block
        // in the history and half on screen is a fold nobody can open and a
        // summary that describes rows in two places.
        if self.blocks.len() > 1 {
            let mut going = self.blocks.remove(0);
            let held = going.len();
            out.extend(going.take());
            self.total = self.total.saturating_sub(held);
            return true;
        }

        // The last one. Its finished lines may go; the line still open may not,
        // and neither may a summary that is still counting up.
        let tail = &mut self.blocks[0];
        if tail.folds() {
            return false;
        }
        let finished = tail.len().saturating_sub(1);
        if finished == 0 {
            return false;
        }
        // In batches, because a commit repaints the live region under it and
        // doing that once per line of a streamed answer is the cost this whole
        // arrangement exists to avoid. A batch is one write either way.
        let take = finished.min(COMMIT_BATCH);
        let lines = tail.take_front(take);
        self.total = self.total.saturating_sub(lines.len());
        out.extend(lines);
        true
    }
}

impl Component for Transcript {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut out = Vec::new();
        for block in &mut self.blocks {
            out.extend(block.render(width));
        }
        out
    }
}
