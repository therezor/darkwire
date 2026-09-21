//! The pieces of a conversation, each on its way to the terminal's scrollback.
//!
//! A cell is finished. It is built when the thing it describes has ended — an
//! answer that stopped streaming, a tool call that returned, a run of
//! reasoning that closed — and from then on it is rows, not state. The rows go
//! out once and belong to the emulator after that.
//!
//! That is why folding is a decision *before* a cell is written and not after.
//! A row in the scrollback cannot be taken back, so a reasoning run that
//! arrives while reasoning is collapsed commits its summary and keeps its body
//! for the transcript overlay, which is the one place the whole thing is shown.
//! The alternative — holding the exchange in the live area so a key can still
//! fold it — is what this replaced: it meant the live area grew with the
//! answer, nothing reached the scrollback until a turn ended, and the
//! terminal's own scrolling was useless for the thing being read.

use darkwire_tui::{HistoryCell, Theme, styled_line};
use ratatui::text::Line;

/// The words a folded cell says about itself, already translated.
#[derive(Clone, Debug, Default)]
pub struct FoldLabels {
    /// While a run of reasoning is open.
    pub thinking: String,
    /// Once it has closed.
    pub reasoning: String,
    /// When the window is too small to draw a prompt in.
    pub too_small: String,
}

/// One row, from a string that may carry SGR escapes.
fn row(text: &str) -> Line<'static> {
    styled_line(text)
}

/// Rows from a block of text, splitting on the newlines in it.
fn rows(text: &str) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .map(row)
        .collect()
}

/// What the operator asked, as the exchange it opens.
///
/// The blank row in front of it is the one thing that makes a long session
/// readable when scrolled back through: it is what the eye counts exchanges by.
pub struct UserCell {
    lines: Vec<Line<'static>>,
}

impl UserCell {
    /// The message, with the caret the composer drew in front of it.
    ///
    /// A message of several lines keeps them, indented under the first so the
    /// whole of it reads as one thing the way it did while it was being
    /// written. Only the first row carries the caret, because only one row of
    /// it was the start.
    #[must_use]
    pub fn new(content: &str, theme: &Theme) -> Self {
        let caret = theme.accent.apply("›");
        let mut lines = Vec::new();
        for line in content.split('\n') {
            if lines.is_empty() {
                lines.push(row(&format!("{caret} {line}")));
            } else {
                lines.push(row(&format!("  {line}")));
            }
        }
        Self { lines }
    }
}

impl HistoryCell for UserCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut out = vec![Line::default()];
        out.extend(self.lines.iter().cloned());
        out
    }
}

/// An answer that has finished arriving.
pub struct AssistantCell {
    lines: Vec<Line<'static>>,
}

impl AssistantCell {
    /// The answer's rows, already styled and indented.
    #[must_use]
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines }
    }

    /// Whether there is anything to write.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

impl HistoryCell for AssistantCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }
}

/// A cell that shows one row and keeps the rest for the transcript.
///
/// Reasoning and tool output are the same shape: a summary that says what
/// happened and a body that says how. Which one is written is settled when the
/// cell is built, from the setting in force at the time.
pub struct FoldedCell {
    summary: Line<'static>,
    body: Vec<Line<'static>>,
    expanded: bool,
}

impl FoldedCell {
    /// A summary row over a body, shown or not.
    #[must_use]
    pub fn new(summary: Line<'static>, body: Vec<Line<'static>>, expanded: bool) -> Self {
        Self {
            summary,
            body,
            expanded,
        }
    }

    /// Whether this cell is showing its body.
    #[must_use]
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }
}

impl HistoryCell for FoldedCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![self.summary.clone()];
        if self.expanded {
            lines.extend(self.body.iter().cloned());
        }
        lines
    }

    fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![self.summary.clone()];
        lines.extend(self.body.iter().cloned());
        lines
    }
}

/// What the renderer said: a notice, a warning, a tool call.
///
/// One event, but not always one row. A warning that explains itself arrives
/// as a paragraph with newlines in it, and a `Line` holding a newline is a
/// row the terminal breaks wherever it likes, at column zero, ignoring
/// whatever width everything else was folded for.
pub struct NoticeCell {
    lines: Vec<Line<'static>>,
}

impl NoticeCell {
    /// The rows, already styled.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self { lines: rows(text) }
    }
}

impl HistoryCell for NoticeCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }
}

/// What the turn cost, shown or not.
///
/// Hidden means nothing goes to the scrollback at all, which is different from
/// a folded cell: there is no summary worth a row. The transcript still has it,
/// because "what did that cost" is a question asked after the fact.
pub struct TurnStatsCell {
    line: Line<'static>,
    shown: bool,
}

impl TurnStatsCell {
    /// The row, and whether it goes out.
    #[must_use]
    pub fn new(text: &str, shown: bool) -> Self {
        Self {
            line: row(text),
            shown,
        }
    }
}

impl HistoryCell for TurnStatsCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        if self.shown {
            vec![self.line.clone()]
        } else {
            Vec::new()
        }
    }

    fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![self.line.clone()]
    }
}

/// The rows a session opens with.
pub struct SessionHeaderCell {
    lines: Vec<Line<'static>>,
}

impl SessionHeaderCell {
    /// The header, which arrives as one string with newlines in it.
    #[must_use]
    pub fn new(header: &str) -> Self {
        Self {
            lines: rows(header),
        }
    }
}

impl HistoryCell for SessionHeaderCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }
}

/// The rule that says the prompt has moved to another conversation.
///
/// What is above it is the session that was left. Nothing can erase those rows
/// — the terminal owns them — so the honest thing is to say where one ended.
pub struct DividerCell {
    line: Line<'static>,
}

impl DividerCell {
    /// A rule the width of the window, drawn the way the composer's is.
    #[must_use]
    pub fn new(width: usize, theme: &Theme) -> Self {
        Self {
            line: row(&crate::header::input_rule(width, theme)),
        }
    }
}

impl HistoryCell for DividerCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![Line::default(), self.line.clone()]
    }
}
