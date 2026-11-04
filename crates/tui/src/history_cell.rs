//! A finished piece of the conversation, on its way to the scrollback.
//!
//! Everything the terminal ends up owning passes through this trait. A cell is
//! asked for its rows at a width, they are written above the live area once,
//! and from then on they belong to the emulator: its selection, its search,
//! its wheel. Nothing here can address them again, which is the point.
//!
//! Two spellings, because a cell shows less than it holds. [`display_lines`]
//! is what goes to the scrollback — a collapsed run of reasoning is one
//! summary row there. [`transcript_lines`] is the same cell with nothing left
//! out, which is what the full transcript shows. A cell that hides nothing
//! implements the first and inherits the second.
//!
//! [`display_lines`]: HistoryCell::display_lines
//! [`transcript_lines`]: HistoryCell::transcript_lines

use ratatui::text::Line;

/// A finished cell that can render itself for a width.
pub trait HistoryCell {
    /// The rows that go to the scrollback.
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;

    /// The rows with nothing folded away, for the full transcript.
    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.display_lines(width)
    }
}

impl<T: HistoryCell + ?Sized> HistoryCell for Box<T> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        (**self).display_lines(width)
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        (**self).transcript_lines(width)
    }
}
