//! The line being typed, as a component rather than as a terminal.
//!
//! A line editor that drew itself — at a row it measured for itself, by moving
//! up over a row count it cached — would have every one of those numbers wrong
//! the instant the window changed width, and none of them would be the
//! renderer's to correct; a frame with such an editor inside it is a frame
//! nobody owns. Here the line is state, the caret is a byte index, and drawing
//! is the renderer's job like everything else. The editor survives a resize
//! because there is nothing to survive: the frame is asked for again at the new
//! width and the caret is still an index into a string.
//!
//! The bindings are the ones muscle memory expects from a shell. Movement and
//! deletion step by grapheme cluster, so a caret never lands inside an emoji.

use crate::component::{CURSOR_MARKER, Component};
use crate::keys::{Key, KeyName, is_ctrl};
use crate::text::{next_boundary, previous_boundary, visible_width, wrap_to_width};
use crate::theme::Theme;

/// What a keystroke asked the surrounding program to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorOutcome {
    /// The key was handled inside the editor.
    None,
    /// Return: the line, which the editor has already cleared.
    Submit(String),
    /// Ctrl-C. Means "stop what is running", or "leave" when nothing is.
    Interrupt,
    /// Ctrl-D on an empty line, which is end-of-input and not a character.
    Eof,
}

/// Where a word starts, walking left from `at`.
fn word_start(text: &str, at: usize) -> usize {
    let before: Vec<(usize, char)> = text[..at].char_indices().collect();
    let mut index = before.len();
    while index > 0 && before[index - 1].1.is_whitespace() {
        index -= 1;
    }
    while index > 0 && !before[index - 1].1.is_whitespace() {
        index -= 1;
    }
    before.get(index).map_or(at, |&(offset, _)| offset)
}

/// Where a word ends, walking right from `at`.
fn word_end(text: &str, at: usize) -> usize {
    let mut end = at;
    let mut rest = text[at..].chars();
    let mut pending = rest.next();
    while let Some(ch) = pending.filter(|ch| ch.is_whitespace()) {
        end += ch.len_utf8();
        pending = rest.next();
    }
    while let Some(ch) = pending.filter(|ch| !ch.is_whitespace()) {
        end += ch.len_utf8();
        pending = rest.next();
    }
    end
}

/// The line being typed, its caret and its history.
pub struct Editor {
    theme: Theme,
    prompt: String,
    placeholder: Option<String>,
    text: String,
    /// A byte index into `text`, always on a grapheme boundary.
    caret: usize,
    history: Vec<String>,
    /// Where in the history the line came from; `history.len()` means "not".
    recalled: usize,
    /// What was being typed before the first Up, so Down can put it back.
    draft: String,
}

impl Editor {
    /// An empty editor drawing with `theme` and the default `› ` prompt.
    pub fn new(theme: &Theme) -> Self {
        Self {
            theme: *theme,
            prompt: "› ".to_owned(),
            placeholder: None,
            text: String::new(),
            caret: 0,
            history: Vec::new(),
            recalled: 0,
            draft: String::new(),
        }
    }

    /// Drawn before the caret, in the accent colour.
    #[must_use]
    pub fn with_prompt(mut self, prompt: &str) -> Self {
        prompt.clone_into(&mut self.prompt);
        self
    }

    /// Shown, dim, in place of an empty line.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = Some(placeholder.to_owned());
        self
    }

    /// The line as typed so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces the line and puts the caret at its end.
    pub fn set_text(&mut self, text: &str) {
        text.clone_into(&mut self.text);
        self.caret = self.text.len();
    }

    /// Adds a line to the history without submitting it. An empty line, or a
    /// repeat of the last one, is not kept.
    pub fn remember(&mut self, line: &str) {
        if line.is_empty() || self.history.last().is_some_and(|last| last == line) {
            return;
        }
        self.history.push(line.to_owned());
        self.recalled = self.history.len();
    }

    /// Removes `range` of the text, leaving the caret at its start.
    fn delete(&mut self, start: usize, end: usize) {
        if start < end {
            self.text.replace_range(start..end, "");
            self.caret = start;
        }
    }

    /// Forward delete: the cluster after the caret.
    fn delete_forward(&mut self) {
        if self.caret < self.text.len() {
            let end = next_boundary(&self.text, self.caret);
            self.text.replace_range(self.caret..end, "");
        }
    }

    /// Up and Down walk the history rather than the wrapped rows. A line long
    /// enough to wrap is rare and a history nobody can reach is not.
    fn recall_previous(&mut self) {
        if self.recalled == self.history.len() {
            self.draft = self.text.clone();
        }
        if self.recalled > 0 {
            self.recalled -= 1;
            let line = self.history[self.recalled].clone();
            self.set_text(&line);
        }
    }

    fn recall_next(&mut self) {
        if self.recalled < self.history.len() {
            self.recalled += 1;
            let line = if self.recalled == self.history.len() {
                self.draft.clone()
            } else {
                self.history[self.recalled].clone()
            };
            self.set_text(&line);
        }
    }

    /// Applies a keystroke.
    pub fn handle_key(&mut self, key: &Key) -> EditorOutcome {
        if is_ctrl(key, 'c') {
            return EditorOutcome::Interrupt;
        }
        if is_ctrl(key, 'd') {
            if self.text.is_empty() {
                return EditorOutcome::Eof;
            }
            self.delete_forward();
            return EditorOutcome::None;
        }

        match key.name {
            KeyName::Enter => {
                let line = std::mem::take(&mut self.text);
                self.caret = 0;
                self.recalled = self.history.len();
                self.draft.clear();
                return EditorOutcome::Submit(line);
            }
            KeyName::Backspace => {
                let start = previous_boundary(&self.text, self.caret);
                self.delete(start, self.caret);
            }
            KeyName::Delete => self.delete_forward(),
            KeyName::Left => {
                self.caret = if key.meta {
                    word_start(&self.text, self.caret)
                } else {
                    previous_boundary(&self.text, self.caret)
                };
            }
            KeyName::Right => {
                self.caret = if key.meta {
                    word_end(&self.text, self.caret)
                } else {
                    next_boundary(&self.text, self.caret)
                };
            }
            KeyName::Home => self.caret = 0,
            KeyName::End => self.caret = self.text.len(),
            KeyName::Up => self.recall_previous(),
            KeyName::Down => self.recall_next(),
            KeyName::Char if !key.ctrl => {
                self.text.insert_str(self.caret, &key.character);
                self.caret += key.character.len();
            }
            KeyName::Char => self.handle_control(key),
            _ => {}
        }
        EditorOutcome::None
    }

    /// The Ctrl-letter bindings a shell has.
    fn handle_control(&mut self, key: &Key) {
        if is_ctrl(key, 'a') {
            self.caret = 0;
        } else if is_ctrl(key, 'e') {
            self.caret = self.text.len();
        } else if is_ctrl(key, 'b') {
            self.caret = previous_boundary(&self.text, self.caret);
        } else if is_ctrl(key, 'f') {
            self.caret = next_boundary(&self.text, self.caret);
        } else if is_ctrl(key, 'u') {
            self.delete(0, self.caret);
        } else if is_ctrl(key, 'k') {
            self.text.truncate(self.caret);
        } else if is_ctrl(key, 'w') {
            let start = word_start(&self.text, self.caret);
            self.delete(start, self.caret);
        }
    }
}

impl Component for Editor {
    fn render(&mut self, width: usize) -> Vec<String> {
        let prompt_width = visible_width(&self.prompt);
        let usable = width.saturating_sub(prompt_width).max(1);
        let body = match &self.placeholder {
            Some(placeholder) if self.text.is_empty() => {
                format!("{}{CURSOR_MARKER}", self.theme.dim.apply(placeholder))
            }
            _ => {
                format!(
                    "{}{CURSOR_MARKER}{}",
                    &self.text[..self.caret],
                    &self.text[self.caret..]
                )
            }
        };

        // Wrapped rather than cut: a message longer than the window is
        // ordinary, and the renderer needs every row it will occupy, not a
        // promise that it fits. The marker rides along inside the text, so it
        // lands on whichever row the fold put it on with no second measurement
        // to keep in step.
        let rows = wrap_to_width(&body, usable);
        let mut iter = rows.into_iter();
        let head = iter.next().unwrap_or_default();
        let indent: String = " ".repeat(prompt_width);
        // The caret is the one mark on this row that belongs to the program
        // rather than to whoever is typing, so it is the one that carries the
        // colour. Continuation rows are aligned under the first row's text,
        // which is where a wrapped line continues in every editor anyone has
        // used.
        std::iter::once(format!("{}{head}", self.theme.accent.apply(&self.prompt)))
            .chain(iter.map(|row| format!("{indent}{row}")))
            .collect()
    }
}
