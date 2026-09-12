//! Everything already said, kept as text rather than as pixels on a screen.
//!
//! The renderer redraws the whole frame when the window changes width, so the
//! frame has to include the conversation — which means something has to hold
//! it. This is that: writes arrive the way a stream takes them, in fragments
//! that mostly do not end on a line break, and are kept as *logical* lines.
//! The wrap happens at render time against the current width, so a narrower
//! window refolds the paragraph instead of clipping it.
//!
//! Two things are worth knowing before changing it:
//!
//!  - **The wrap is cached per width.** A keystroke re-renders the frame, and
//!    re-wrapping a long conversation on every keypress would make typing cost
//!    the size of the session. Between writes the same rows come back, so the
//!    renderer's diff finds them equal and stops at the editor.
//!  - **It is bounded.** A session that streams for hours would otherwise grow
//!    without limit, and a redraw prints every row it holds. The oldest lines
//!    are dropped in blocks rather than one at a time, because dropping one
//!    shifts every row and costs a full redraw.

use crate::component::Component;
use crate::text::{STYLE_RESET, carry_styles, wrap_to_width};

/// Past this many logical lines the oldest are dropped.
const LIMIT: usize = 10_000;
/// How many go at once when it does, so the redraw is paid rarely.
const DROP: usize = 2_000;

/// The conversation so far, as logical lines.
#[derive(Debug, Default)]
pub struct Transcript {
    logical: Vec<String>,
    /// The width the cache was built for, if it is valid.
    cached_width: Option<usize>,
    cached: Vec<String>,
    /// Style sequences still open at the end of the line being written to.
    open: String,
}

impl Transcript {
    /// An empty transcript.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the last thing written ended a line.
    pub fn at_line_start(&self) -> bool {
        self.logical.last().is_none_or(|last| *last == self.open)
    }

    /// The logical lines, unwrapped.
    pub fn lines(&self) -> &[String] {
        &self.logical
    }

    /// Takes text the way a stream does, newlines and all.
    pub fn write(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut parts = text.split('\n');
        // The first part continues whatever line was open; every later one
        // starts its own. A trailing newline leaves an empty open line, which
        // is exactly what `at_line_start` reports.
        let first = parts.next().unwrap_or("");
        match self.logical.last_mut() {
            Some(held) => held.push_str(first),
            None => self.logical.push(first.to_owned()),
        }
        self.open = carry_styles(&self.open, first);

        // Each new line re-opens whatever was still on, and the line it left
        // closes what it had. A style that spans a break would otherwise arrive
        // with its `\x1b[2m` on the row above — which is a row the terminal has
        // already drawn — so the text under it renders plain. A streamed chunk
        // of reasoning is routinely `"\n\nLet me think"`, dimmed whole, which
        // is exactly that shape.
        for part in parts {
            if !self.open.is_empty()
                && let Some(closing) = self.logical.last_mut()
            {
                closing.push_str(STYLE_RESET);
            }
            self.logical.push(format!("{}{part}", self.open));
            self.open = carry_styles(&self.open, part);
        }

        if self.logical.len() > LIMIT {
            self.logical.drain(..DROP);
        }
        self.cached_width = None;
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.logical.clear();
        self.open.clear();
        self.cached_width = None;
    }
}

impl Component for Transcript {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.cached_width != Some(width) {
            self.cached = self
                .logical
                .iter()
                .flat_map(|line| wrap_to_width(line, width))
                .collect();
            self.cached_width = Some(width);
        }
        self.cached.clone()
    }
}
