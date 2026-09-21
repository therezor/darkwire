//! Everything that was said, over the whole window.
//!
//! Ctrl-T. The screen holds a conversation with its reasoning folded to one
//! row a turn and its tool output folded to the row that says how the call
//! went, which is what makes the answer readable. This is the other view: the
//! same cells with nothing left out.
//!
//! It takes the alternate screen, and that is the one place in this program
//! where doing so is right. A pager over a document *is* a screen: it scrolls,
//! it has no prompt, and when it closes the conversation underneath comes back
//! exactly as it was. The prompt itself takes no second buffer, because a
//! prompt that did would own scrolling, selection and search, and would take
//! the session with it on exit.

use darkwire_tui::{HistoryCell, Key, KeyName, Renderable, Theme, wrap_lines};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;

/// What a key did to the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayOutcome {
    /// Still open.
    Open,
    /// The reader is done.
    Closed,
}

/// The whole conversation, scrollable.
pub struct TranscriptOverlay {
    /// Every row, already folded to the width it was built for.
    rows: Vec<Line<'static>>,
    /// The first row shown.
    top: usize,
    /// The width the rows were folded to, so a resize can notice.
    width: u16,
    footer: Line<'static>,
}

impl TranscriptOverlay {
    /// Every cell's rows, with nothing folded away.
    ///
    /// Opened at the end rather than the start: what somebody pressing Ctrl-T
    /// wants is usually the reasoning behind the answer they just read.
    #[must_use]
    pub fn new(cells: &[Box<dyn HistoryCell>], width: u16, theme: &Theme, footer: &str) -> Self {
        let rows: Vec<Line<'static>> = cells
            .iter()
            .flat_map(|cell| cell.transcript_lines(width))
            .collect();
        Self {
            rows: wrap_lines(&rows, width.max(1)),
            top: usize::MAX,
            width,
            footer: darkwire_tui::styled_line(&theme.dim.apply(footer)),
        }
    }

    /// How many rows there are in all.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether nothing has been said yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The first row shown, given how many fit.
    fn top_for(&self, rows: usize) -> usize {
        let last = self.rows.len().saturating_sub(rows);
        self.top.min(last)
    }

    /// One keystroke, against a window `height` rows tall.
    pub fn handle_key(&mut self, key: &Key, height: u16) -> OverlayOutcome {
        let page = usize::from(height).saturating_sub(1).max(1);
        let last = self.rows.len().saturating_sub(page);
        let top = self.top_for(page);
        match key.name {
            KeyName::Escape => return OverlayOutcome::Closed,
            KeyName::Char if key.character == "q" && !key.ctrl => {
                return OverlayOutcome::Closed;
            }
            KeyName::Char if key.character == "t" && key.ctrl => {
                return OverlayOutcome::Closed;
            }
            KeyName::Up => self.top = top.saturating_sub(1),
            KeyName::Down => self.top = (top + 1).min(last),
            KeyName::PageUp => self.top = top.saturating_sub(page),
            KeyName::PageDown => self.top = (top + page).min(last),
            KeyName::Home => self.top = 0,
            KeyName::End => self.top = usize::MAX,
            _ => {}
        }
        OverlayOutcome::Open
    }

    /// The width the rows were folded to.
    ///
    /// A window that changed size needs the overlay built again, which the
    /// loop does because only it holds the cells.
    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }
}

impl Renderable for TranscriptOverlay {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let body = area.height.saturating_sub(1);
        let top = self.top_for(usize::from(body));
        for offset in 0..body {
            let Some(line) = self.rows.get(top + usize::from(offset)) else {
                break;
            };
            Widget::render(
                line,
                Rect::new(area.x, area.y + offset, area.width, 1),
                buffer,
            );
        }
        Widget::render(
            &self.footer,
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
            buffer,
        );
    }

    fn desired_height(&self, _width: u16) -> u16 {
        u16::try_from(self.rows.len().saturating_add(1)).unwrap_or(u16::MAX)
    }
}
