//! Text arriving a piece at a time, and the rows it has finished becoming.
//!
//! A model streams. What arrives is not lines: it is chunks that start and end
//! wherever the tokeniser happened to break, so a chunk may be half a word and
//! a line may take ten of them. Nothing can be written to the terminal's
//! scrollback until it is *finished*, because a row that went out cannot be
//! taken back and a half-written line will change.
//!
//! So this holds two things. Lines that have ended are rows, settled, ready to
//! commit. Whatever came after the last newline is a tail, still live, redrawn
//! in the area at the bottom of the screen on every frame until a newline
//! settles it. The split is the whole idea: the reader sees text as it arrives
//! *and* the finished part reaches the scrollback while the turn is still
//! running, rather than in one lump when it ends.

use darkwire_tui::{styled_line, wrap_to_width};
use ratatui::text::Line;

use crate::render::indented;

/// Text on its way to becoming rows.
#[derive(Debug, Default)]
pub struct StreamController {
    /// Whatever followed the last newline, escapes and all.
    tail: String,
    /// Lines that have ended, not yet taken.
    lines: Vec<String>,
    /// Whether the next chunk starts a line.
    ///
    /// Measured on what a reader sees: dimmed text ends in a closing escape
    /// however its prose ended, so testing the raw string would report
    /// "mid-line" for a chunk that plainly finished one.
    at_line_start: bool,
}

impl StreamController {
    /// Nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tail: String::new(),
            lines: Vec::new(),
            at_line_start: true,
        }
    }

    /// Whether nothing has arrived, or everything has been taken.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.tail.is_empty()
    }

    /// Adds a chunk, indented for the turn it belongs to.
    ///
    /// The indent goes on here rather than at the source because a chunk may
    /// start mid-line: a subagent's answer indented on whichever line a chunk
    /// happened to begin, and flush left everywhere else, is what doing it any
    /// earlier produces.
    pub fn push(&mut self, text: &str, depth: usize) {
        if text.is_empty() {
            return;
        }
        let indent = "  ".repeat(depth);
        let body = if indent.is_empty() {
            text.to_owned()
        } else {
            indented(text, &indent, self.at_line_start)
        };
        self.at_line_start = darkwire_tui::strip_ansi(text).ends_with('\n');

        self.tail.push_str(&body);
        while let Some(at) = self.tail.find('\n') {
            let line: String = self.tail.drain(..=at).collect();
            self.lines.push(line.trim_end_matches('\n').to_owned());
        }
    }

    /// Settles whatever line is open, if one is.
    pub fn end_line(&mut self) {
        if self.at_line_start && self.tail.is_empty() {
            return;
        }
        let line = std::mem::take(&mut self.tail);
        self.lines.push(line);
        self.at_line_start = true;
    }

    /// Adds text that was already complete when it arrived.
    ///
    /// Complete is not the same as one row: a tool that printed a paragraph
    /// arrives as one event with newlines in it, and every one of those is a
    /// row of its own. A row holding a newline is a row the terminal breaks
    /// wherever it likes, ignoring the width everything else was folded for.
    pub fn push_line(&mut self, text: &str) {
        self.end_line();
        for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
            self.lines.push(line.to_owned());
        }
    }

    /// How many lines have settled.
    #[must_use]
    pub fn settled(&self) -> usize {
        self.lines.len()
    }

    /// Everything as rows for `width`: settled lines, then the live tail.
    ///
    /// This is what the live area draws, so the tail is included and folded
    /// the same way a settled line is. A reader watching an answer arrive is
    /// watching this.
    #[must_use]
    pub fn rows(&self, width: u16) -> Vec<Line<'static>> {
        let mut out = fold(&self.lines, width);
        if !self.tail.is_empty() {
            out.extend(fold(std::slice::from_ref(&self.tail), width));
        }
        out
    }

    /// Takes the settled lines, keeping the last `keep` of them.
    ///
    /// This is what lets a long answer reach the scrollback while it is still
    /// being written: once the live area is at its cap, the oldest settled
    /// lines are finished text that will not change, so they go. Keeping a few
    /// back means the rows on screen do not jump as the boundary moves.
    pub fn take_settled(&mut self, keep: usize) -> Vec<Line<'static>> {
        if self.lines.len() <= keep {
            return Vec::new();
        }
        let taken: Vec<String> = self.lines.drain(..self.lines.len() - keep).collect();
        taken.iter().map(|line| styled_line(line)).collect()
    }

    /// Takes everything, closing the tail first.
    ///
    /// What is left of a line when a turn ends is a line: an answer that was
    /// interrupted mid-sentence still said what it said.
    pub fn take_all(&mut self) -> Vec<Line<'static>> {
        self.end_line();
        self.at_line_start = true;
        std::mem::take(&mut self.lines)
            .iter()
            .map(|line| styled_line(line))
            .collect()
    }
}

/// Lines folded to `width`, as rows.
///
/// Folding happens here rather than at commit time because the live area and
/// the scrollback have to agree about how many rows a line is: the area is
/// sized from this count, and a line the terminal folded differently would
/// leave the area drawn over its own history.
fn fold(lines: &[String], width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    lines
        .iter()
        .flat_map(|line| {
            if width == 0 {
                vec![line.clone()]
            } else {
                wrap_to_width(line, width)
            }
        })
        .map(|row| styled_line(&row))
        .collect()
}
