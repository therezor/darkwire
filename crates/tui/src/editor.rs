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

/// Whether this key means "a new line in what I am writing".
///
/// Return ends a message, so something else has to add a line to one. Which
/// something depends on what the terminal can say: Shift-Return only reaches a
/// program on a terminal that has been asked to disambiguate its escape codes,
/// and Alt-Return and Ctrl-J reach one anywhere.
#[must_use]
pub fn is_newline(key: &Key) -> bool {
    match key.name {
        KeyName::Enter => key.shift || key.meta,
        KeyName::Char => is_ctrl(key, 'j'),
        _ => false,
    }
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

        // A newline in what is being written rather than the end of it.
        //
        // Three spellings because terminals disagree about what they can say.
        // Shift-Return is the one everybody reaches for and the one a terminal
        // can only send when it has been asked to disambiguate its escape
        // codes; Alt-Return and Ctrl-J need nothing and work everywhere, which
        // is what makes them the fallback rather than an alternative.
        if is_newline(key) {
            self.insert("\n");
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
            KeyName::Home => self.caret = self.line_start(),
            KeyName::End => self.caret = self.line_end(),
            // Between the lines of a message being written, and out of it into
            // the history at the ends. A message of several lines that lost
            // itself to the history on an Up would be a message nobody would
            // risk writing.
            KeyName::Up => {
                if !self.move_line(-1) {
                    self.recall_previous();
                }
            }
            KeyName::Down => {
                if !self.move_line(1) {
                    self.recall_next();
                }
            }
            KeyName::Char if !key.ctrl => self.insert(&key.character),
            KeyName::Char => self.handle_control(key),
            _ => {}
        }
        EditorOutcome::None
    }

    /// Puts text at the caret and leaves the caret after it.
    fn insert(&mut self, text: &str) {
        self.text.insert_str(self.caret, text);
        self.caret += text.len();
    }

    /// Where the line the caret is on begins.
    fn line_start(&self) -> usize {
        self.text[..self.caret].rfind('\n').map_or(0, |at| at + 1)
    }

    /// Where the line the caret is on ends.
    fn line_end(&self) -> usize {
        self.text[self.caret..]
            .find('\n')
            .map_or(self.text.len(), |at| self.caret + at)
    }

    /// Moves the caret a line up or down, keeping its column.
    ///
    /// `false` when there is no such line, which is what hands Up and Down
    /// back to the history at the top and the bottom of a message.
    fn move_line(&mut self, delta: i32) -> bool {
        let start = self.line_start();
        let column = self.text[start..self.caret].chars().count();
        let target = if delta < 0 {
            if start == 0 {
                return false;
            }
            self.text[..start - 1].rfind('\n').map_or(0, |at| at + 1)
        } else {
            let end = self.line_end();
            if end == self.text.len() {
                return false;
            }
            end + 1
        };
        let target_end = self.text[target..]
            .find('\n')
            .map_or(self.text.len(), |at| target + at);
        let mut caret = target;
        for _ in 0..column {
            if caret >= target_end {
                break;
            }
            caret = next_boundary(&self.text, caret);
        }
        self.caret = caret.min(target_end);
        true
    }

    /// The Ctrl-letter bindings a shell has.
    fn handle_control(&mut self, key: &Key) {
        if is_ctrl(key, 'a') {
            self.caret = self.line_start();
        } else if is_ctrl(key, 'e') {
            self.caret = self.line_end();
        } else if is_ctrl(key, 'b') {
            self.caret = previous_boundary(&self.text, self.caret);
        } else if is_ctrl(key, 'f') {
            self.caret = next_boundary(&self.text, self.caret);
        } else if is_ctrl(key, 'u') {
            let start = self.line_start();
            self.delete(start, self.caret);
        } else if is_ctrl(key, 'k') {
            let end = self.line_end();
            self.delete(self.caret, end);
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

        // Split on the newlines somebody put there, *then* wrapped: a newline
        // is a row break they asked for and a fold is one the window imposed,
        // and a folder that could not tell them apart would run two of their
        // lines together whenever the first one happened to be short.
        //
        // Wrapped rather than cut, because a message longer than the window is
        // ordinary and the caller needs every row it will occupy. The marker
        // rides along inside the text, so it lands on whichever row the fold
        // put it on with no second measurement to keep in step.
        let indent: String = " ".repeat(prompt_width);
        let mut rows = Vec::new();
        for line in body.split('\n') {
            for row in wrap_to_width(line, usable) {
                // The caret is the one mark on these rows that belongs to the
                // program rather than to whoever is typing, so it is the one
                // that carries the colour. Every row after the first is
                // aligned under the first row's text.
                if rows.is_empty() {
                    rows.push(format!("{}{row}", self.theme.accent.apply(&self.prompt)));
                } else {
                    rows.push(format!("{indent}{row}"));
                }
            }
        }
        rows
    }
}
