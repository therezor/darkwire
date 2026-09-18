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
//! **What is on screen, and what is behind it.** A frame holding the whole
//! session costs the whole session on every draw, which at a token a frame is
//! the difference between a prompt that types and one that stutters. So the
//! transcript hands its oldest blocks to the terminal's own scrollback and stops
//! holding them: [`Transcript::take_committable`] returns the logical lines to
//! print above the frame, and what is left is a bounded live region. Committed
//! text is the terminal's now. It reflows on a resize for free, and nothing
//! here can rewrite it, which is why a block stops being foldable once it has
//! gone.
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

/// How many committed logical lines are kept for a repaint after a resize.
///
/// Enough to fill a tall window twice over. A resize erases the screen, and
/// what was on it but had not yet scrolled into the history is gone with it,
/// because most terminals do not push erased rows up. Reprinting this much puts the
/// screen back; anything older is already in the scrollback where it belongs.
const RING: usize = 200;

/// How many finished lines of the open block go to the scrollback at once.
const COMMIT_BATCH: usize = 16;

/// The conversation so far.
#[derive(Debug)]
pub struct Transcript {
    /// Never empty: there is always a block to write into.
    blocks: Vec<Block>,
    /// How many logical lines the whole transcript holds, for the bound.
    total: usize,
    /// Lines already printed above the frame, newest last. See [`RING`].
    ring: std::collections::VecDeque<String>,
}

impl Default for Transcript {
    fn default() -> Self {
        Self {
            blocks: vec![Block::prose()],
            total: 0,
            ring: std::collections::VecDeque::new(),
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
        self.ring.clear();
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
        for line in &out {
            self.ring.push_back(line.clone());
        }
        while self.ring.len() > RING {
            self.ring.pop_front();
        }
        out
    }

    /// The most recently committed lines that fill `rows`, oldest first.
    ///
    /// What a resize reprints above the frame. Erasing the screen takes with it
    /// the committed rows that were *on* it. Those had not yet scrolled into the
    /// history, and most terminals do not push erased rows up, so without this a
    /// resize would delete the last screenful of the conversation.
    ///
    /// Measured in rows rather than in lines, and that is the whole care in it.
    /// Reprinting more than the screen held would put a second copy of
    /// something that *is* already in the history right under the first, and a
    /// line that wrapped to three rows is three rows of that mistake.
    pub fn committed_tail(&self, rows: usize, width: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut used = 0;
        for line in self.ring.iter().rev() {
            let height = wrap_to_width(line, width).len().max(1);
            if used + height > rows {
                break;
            }
            used += height;
            out.push(line.clone());
        }
        out.reverse();
        out
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
