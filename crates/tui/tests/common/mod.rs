//! A terminal that exists entirely in memory.
//!
//! Everything the renderer and the keyboard need from a real one, and nothing
//! else: an input half to push key bytes into, an output half that accumulates
//! what was drawn, a size, and a `set_raw_mode` that records whether it was
//! called. No pty, no child process, no timing.
//!
//! This is what makes the escape-sequence assertions durable rather than
//! transient. A test does not look at a screen and hope the repaint has landed
//! — it reads back the bytes a completed paint emitted, which are the same
//! bytes whether the machine is fast or slow.

// Not every test file uses every helper here; the module is shared.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::io;

use ghostai_tui::{TerminalInput, TerminalOutput};

/// An input half with a script of chunks to hand over.
#[derive(Debug, Default)]
pub struct FakeInput {
    pub is_tty: bool,
    pub is_raw: bool,
    /// Every `set_raw_mode` call, in order.
    pub raw_mode_calls: Vec<bool>,
    chunks: VecDeque<String>,
}

impl FakeInput {
    /// A terminal input, not yet in raw mode.
    pub fn tty() -> Self {
        Self {
            is_tty: true,
            ..Self::default()
        }
    }

    /// A pipe: not a terminal, and no mode to set.
    pub fn pipe() -> Self {
        Self::default()
    }

    /// Queues bytes as if the user had typed them, one chunk per call.
    pub fn type_text(&mut self, data: &str) {
        self.chunks.push_back(data.to_owned());
    }
}

impl TerminalInput for FakeInput {
    fn is_tty(&self) -> bool {
        self.is_tty
    }

    fn is_raw(&self) -> bool {
        self.is_raw
    }

    fn supports_raw_mode(&self) -> bool {
        self.is_tty
    }

    fn set_raw_mode(&mut self, raw: bool) -> io::Result<()> {
        self.raw_mode_calls.push(raw);
        // Mirrors the real device, so the keyboard's "only turn off a mode I
        // turned on" check is exercised rather than assumed.
        self.is_raw = raw;
        Ok(())
    }

    fn read_chunk(&mut self) -> io::Result<Option<String>> {
        Ok(self.chunks.pop_front())
    }
}

/// An output half that keeps what was written.
#[derive(Debug)]
pub struct FakeOutput {
    pub columns: u16,
    pub rows: u16,
    pub is_tty: bool,
    text: String,
}

impl FakeOutput {
    /// A window of the given size.
    pub fn new(columns: u16, rows: u16) -> Self {
        Self {
            columns,
            rows,
            is_tty: true,
            text: String::new(),
        }
    }

    /// Everything written so far, concatenated.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Forgets what was written, so an assertion can name one repaint.
    pub fn reset(&mut self) {
        self.text.clear();
    }

    /// Changes the size, the way a real terminal does before anyone is told.
    pub fn resize_to(&mut self, columns: u16, rows: u16) {
        self.columns = columns;
        self.rows = rows;
    }
}

impl TerminalOutput for FakeOutput {
    fn write_str(&mut self, text: &str) {
        self.text.push_str(text);
    }

    fn columns(&self) -> Option<u16> {
        Some(self.columns)
    }

    fn rows(&self) -> Option<u16> {
        Some(self.rows)
    }

    fn is_tty(&self) -> bool {
        self.is_tty
    }
}
