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

    /// How many rows [`HistoryCell::display_lines`] will return.
    fn desired_height(&self, width: u16) -> u16 {
        u16::try_from(self.display_lines(width).len()).unwrap_or(u16::MAX)
    }

    /// The rows with nothing folded away, for the full transcript.
    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.display_lines(width)
    }
}

impl<T: HistoryCell + ?Sized> HistoryCell for Box<T> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        (**self).display_lines(width)
    }

    fn desired_height(&self, width: u16) -> u16 {
        (**self).desired_height(width)
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        (**self).transcript_lines(width)
    }
}

/// Rows that are already what they will be, hiding nothing.
pub struct PlainCell {
    lines: Vec<Line<'static>>,
}

impl PlainCell {
    /// A cell of exactly these rows.
    #[must_use]
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines }
    }
}

impl HistoryCell for PlainCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }

    fn desired_height(&self, _width: u16) -> u16 {
        u16::try_from(self.lines.len()).unwrap_or(u16::MAX)
    }
}
