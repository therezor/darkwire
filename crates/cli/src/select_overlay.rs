//! A menu that takes the window, for a list nobody reads top to bottom.
//!
//! Five rows under the composer is right for a vocabulary: the model names, the
//! agents, the efforts. It is wrong for the sessions, where the list is as long
//! as the install is old and the only way through it is to type. A filter over
//! five rows is a filter applied blind, so that one list gets the screen the
//! same way `/help` and the transcript do.
//!
//! What this adds over [`Select`] is placement and a caret. The menu draws its
//! rows and its footer; the question and the filter are one row it hands out,
//! because where that row goes is the host's to decide, and here it goes at the
//! top with the conversation's own prompt out of sight.

use darkwire_tui::{Component, Key, Renderable, Select, SelectOutcome, StyledRows, cursor_in};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// How many rows are not the list: the prompt, the blank under it, the footer.
const CHROME_ROWS: usize = 3;

/// A menu over the whole window, waiting for a row to be chosen.
pub struct SelectOverlay {
    select: Select<usize>,
}

impl SelectOverlay {
    /// A menu drawn over `height` rows.
    #[must_use]
    pub fn new(mut select: Select<usize>, height: u16) -> Self {
        select.set_rows(usize::from(height).saturating_sub(CHROME_ROWS).max(1));
        Self { select }
    }

    /// Tells it the window changed size.
    pub fn resize(&mut self, height: u16) {
        self.select
            .set_rows(usize::from(height).saturating_sub(CHROME_ROWS).max(1));
    }

    /// Applies a keystroke.
    pub fn handle_key(&mut self, key: &Key) -> SelectOutcome<usize> {
        self.select.handle_key(key)
    }

    /// Text the terminal pasted, into the filter.
    pub fn paste(&mut self, text: &str) {
        self.select.paste(text);
    }

    /// Every row, top to bottom, at `width`.
    fn rows(&mut self, width: usize) -> Vec<String> {
        let mut rows = vec![self.select.prompt(), String::new()];
        rows.extend(Component::render(&mut self.select, width));
        rows
    }

    /// Draws it over the window.
    pub fn render(&mut self, area: Rect, buffer: &mut Buffer) {
        let rows = self.rows(usize::from(area.width));
        StyledRows::new(&rows).render(area, buffer);
    }

    /// Where the caret belongs, as a column and a row of `area`.
    ///
    /// A menu is typed into, unlike the two overlays that came before it, so
    /// this is the one that has an answer. Measured from the prompt alone
    /// rather than from every row: the caret is always on the first of them,
    /// and drawing four hundred sessions again to find a column somebody
    /// already knows is four hundred rows of work for nothing.
    ///
    /// The marker is inline in the prompt and never reaches a terminal. The
    /// row is measured for it here and the buffer drops it.
    pub fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let (_, column) = cursor_in(&[self.select.prompt()])?;
        let x = area.x.saturating_add(u16::try_from(column).ok()?);
        (x < area.right() && area.height > 0).then_some((x, area.y))
    }
}
