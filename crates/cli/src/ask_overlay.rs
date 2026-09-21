//! A question with a line to answer it on, over the window.
//!
//! The third overlay, after the one that shows rows and the one that picks
//! between them. It exists because two of the things `/workspace` can do need a
//! *name*, and a list has nowhere to type one.
//!
//! The first shape of this put the command on the composer instead —
//! `/workspace rename research ` with the caret after it — and that worked, but
//! it made the window a place you leave in order to finish what you started
//! there. A manager you have to exit to manage with is not a manager.
//!
//! **What it is not is a second composer.** No history, no multi-line, no
//! completion: it is one line answering one question, and Return ends it. The
//! editor underneath is the same one the prompt uses, because the movement and
//! editing keys are worth having and are already written.

use darkwire_tui::{Component, Editor, EditorOutcome, Key, KeyName, Renderable, StyledRows, Theme};
use darkwire_tui::{cursor_in, is_ctrl};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// What a keystroke did to the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskOutcome {
    /// Still open; render again.
    Open,
    /// Answered. Empty is an answer the caller may still refuse.
    Answered(String),
    /// Left without answering.
    Cancelled,
}

/// A question, a line, and the keys under it.
pub struct AskOverlay {
    /// The question, already translated.
    title: String,
    /// The dim line at the foot, already translated.
    footer: String,
    editor: Editor,
    theme: Theme,
}

impl AskOverlay {
    /// A question over the window, with `initial` already typed.
    ///
    /// Pre-filled for a rename, because the name being changed is nearly always
    /// the start of the one replacing it, and empty for anything new.
    #[must_use]
    pub fn new(title: &str, initial: &str, footer: &str, theme: &Theme) -> Self {
        let mut editor = Editor::new(theme).with_prompt("› ");
        editor.set_text(initial);
        Self {
            title: title.to_owned(),
            footer: footer.to_owned(),
            editor,
            theme: *theme,
        }
    }

    /// Applies a keystroke.
    ///
    /// Escape is tested here rather than in the editor: the editor is the
    /// prompt's, where Escape means nothing, and giving it a meaning there
    /// would give it one at the prompt too.
    pub fn handle_key(&mut self, key: &Key) -> AskOutcome {
        if key.name == KeyName::Escape || is_ctrl(key, 'c') {
            return AskOutcome::Cancelled;
        }
        match self.editor.handle_key(key) {
            EditorOutcome::Submit(line) => AskOutcome::Answered(line),
            EditorOutcome::Interrupt | EditorOutcome::Eof => AskOutcome::Cancelled,
            EditorOutcome::None => AskOutcome::Open,
        }
    }

    /// Every row, top to bottom, at `width`.
    fn rows(&mut self, width: usize) -> Vec<String> {
        let mut rows = vec![self.theme.title.apply(&self.title), String::new()];
        rows.extend(Component::render(&mut self.editor, width));
        rows.push(String::new());
        rows.push(self.theme.dim.apply(&format!("  {}", self.footer)));
        rows
    }

    /// Draws it over the window.
    pub fn render(&mut self, area: Rect, buffer: &mut Buffer) {
        let rows = self.rows(usize::from(area.width));
        StyledRows::new(&rows).render(area, buffer);
    }

    /// Where the caret belongs, as a column and a row of `area`.
    ///
    /// Measured over every row rather than one of them, unlike the menu: the
    /// line being typed into wraps, so which row the caret is on is not known
    /// until the rows are built.
    pub fn cursor_pos(&mut self, area: Rect) -> Option<(u16, u16)> {
        let rows = self.rows(usize::from(area.width));
        let (row, column) = cursor_in(&rows)?;
        let x = area.x.saturating_add(u16::try_from(column).ok()?);
        let y = area.y.saturating_add(u16::try_from(row).ok()?);
        (x < area.right() && y < area.bottom()).then_some((x, y))
    }
}
