//! Lines from a child process's pipe, each held to a byte cap.
//!
//! `tokio`'s `lines()` buffers until it sees a newline. A child that prints a
//! `\r` spinner, or a single huge frame, never sends one, and the buffer grows
//! for as long as the child keeps writing. It also stops at the first byte
//! that is not UTF-8, and a pipe nobody drains stalls the child once it fills.
//!
//! This reader keeps the first `cap` bytes of a line and reads past the rest
//! without storing it. It decodes lossily, so a stray byte costs one
//! replacement character rather than the reader.

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// One line, without its `\n` or `\r\n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedLine {
    /// The line, or its first `cap` bytes when it was longer.
    pub text: String,
    /// Whether bytes past the cap were read and dropped.
    pub truncated: bool,
}

/// Reads lines of at most `cap` bytes from `reader`.
#[derive(Debug)]
pub struct CappedLines<R> {
    reader: R,
    cap: usize,
    line: Vec<u8>,
    truncated: bool,
}

impl<R: AsyncBufRead + Unpin> CappedLines<R> {
    /// A reader that keeps at most `cap` bytes of any one line.
    pub fn new(reader: R, cap: usize) -> CappedLines<R> {
        CappedLines {
            reader,
            cap,
            line: Vec::new(),
            truncated: false,
        }
    }

    /// The next line, or `None` at the end of the stream.
    ///
    /// Cancel safe: a partial line stays buffered for the next call.
    ///
    /// # Errors
    ///
    /// Whatever the underlying read raises.
    pub async fn next_line(&mut self) -> std::io::Result<Option<CappedLine>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.line.is_empty() && !self.truncated {
                    return Ok(None);
                }
                return Ok(Some(self.take()));
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let read = &available[..newline.unwrap_or(available.len())];
            // The `\r` of a CRLF ending is terminator, not content, so a line
            // of exactly the cap is not cut for it.
            let body = match newline {
                Some(_) => read.strip_suffix(b"\r").unwrap_or(read),
                None => read,
            };
            let room = self.cap.saturating_sub(self.line.len());
            if body.len() > room {
                self.truncated = true;
            }
            self.line.extend_from_slice(&body[..body.len().min(room)]);
            let used = read.len() + usize::from(newline.is_some());
            self.reader.consume(used);
            if newline.is_some() {
                return Ok(Some(self.take()));
            }
        }
    }

    fn take(&mut self) -> CappedLine {
        // Only a whole line ends in its terminator. A cut one ends wherever the
        // cap fell.
        if !self.truncated && self.line.last() == Some(&b'\r') {
            self.line.pop();
        }
        let text = String::from_utf8_lossy(&self.line).into_owned();
        self.line.clear();
        CappedLine {
            text,
            truncated: std::mem::take(&mut self.truncated),
        }
    }
}
