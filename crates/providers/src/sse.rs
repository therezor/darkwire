//! A server-sent-events reader.
//!
//! Written rather than depended upon because the event stream is where a
//! provider's response is least trustworthy. A proxy in front of a model
//! server can terminate the connection mid-event, a local server can emit a
//! plain JSON error body with a `text/event-stream` content type, and either
//! produces a partial frame that a permissive parser silently turns into a
//! truncated answer. Here that is a typed `stream_parse` error, which
//! `with_resilience` knows to retry as a non-streaming request.
//!
//! Three details of the spec are load-bearing:
//!
//! - **Multiple `data:` lines in one event join with `\n`.** Dropping all but
//!   the last is the classic bug; it silently deletes content in any provider
//!   that emits multi-line frames.
//! - **A single leading space after the colon is part of the syntax**, not the
//!   payload. Stripping the whole leading run corrupts indented JSON.
//! - **`\r\n`, `\n` and a bare `\r` are all line terminators.** Providers
//!   behind a Windows proxy do send `\r\n`, and a parser that splits on `\n`
//!   alone leaves a `\r` at the end of every JSON payload.

use darkwire_core::Result;
use futures::stream::{BoxStream, Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::errors::{ProviderError, ProviderErrorReason};

/// One event off the stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SseEvent {
    /// The `event:` field, or `message` when the stream omitted one.
    pub event: String,
    /// All `data:` lines of this frame, joined with newlines.
    pub data: String,
}

/// The cap on one unterminated frame, in UTF-16 code units.
///
/// A response that never sends a line terminator would otherwise grow the
/// buffer until the process dies. 1 MiB is far past any legitimate SSE frame:
/// a full non-streaming completion is smaller than that. Measured in UTF-16
/// units so the same bytes trip the cap here and in the browser.
pub const MAX_SSE_FRAME_CHARS: usize = 1_048_576;

/// How a parser is configured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseOptions {
    /// Named in the error when a frame is refused.
    pub provider_id: Option<String>,
    /// Overrides [`MAX_SSE_FRAME_CHARS`].
    pub max_frame_chars: Option<usize>,
}

/// An incremental SSE parser: feed bytes in, take frames out.
///
/// Push-based rather than a stream adaptor so the adapter can drive it inside
/// its own read loop and a test can drive it with a list of chunks.
#[derive(Debug)]
pub struct SseParser {
    provider_id: Option<String>,
    max_frame_chars: usize,
    /// Bytes that did not yet complete a UTF-8 sequence.
    pending_bytes: Vec<u8>,
    /// Decoded text not yet terminated by a line break.
    buffer: String,
    /// `buffer`'s length in UTF-16 units, kept incrementally so the cap check
    /// is not a rescan per chunk.
    buffer_units: usize,
    data_lines: Vec<String>,
    event_name: String,
}

impl SseParser {
    /// A parser with `options`.
    pub fn new(options: SseOptions) -> SseParser {
        SseParser {
            provider_id: options.provider_id,
            max_frame_chars: options.max_frame_chars.unwrap_or(MAX_SSE_FRAME_CHARS),
            pending_bytes: Vec::new(),
            buffer: String::new(),
            buffer_units: 0,
            data_lines: Vec::new(),
            event_name: String::new(),
        }
    }

    /// Feeds one chunk and returns every frame it completed.
    ///
    /// An unterminated frame past the cap is a `stream_parse` error; the
    /// parser is unusable after one.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>> {
        let text = self.decode(chunk);
        self.buffer_units += text.encode_utf16().count();
        self.buffer.push_str(&text);

        let mut events = Vec::new();
        while let Some((line_end, terminator_len)) = find_terminator(&self.buffer) {
            let line: String = self.buffer.drain(..line_end + terminator_len).collect();
            self.buffer_units -= line.encode_utf16().count();
            let line = &line[..line_end];
            if line.is_empty() {
                events.extend(self.frame());
            } else {
                self.consume(line);
            }
        }

        if self.buffer_units > self.max_frame_chars {
            let mut error = ProviderError::new(
                ProviderErrorReason::StreamParse,
                format!(
                    "Event stream frame exceeded {} characters",
                    self.max_frame_chars
                ),
            );
            if let Some(provider_id) = &self.provider_id {
                error = error.with_provider(provider_id.clone());
            }
            return Err(error.into_wire());
        }
        Ok(events)
    }

    /// Flushes what the stream ended on.
    ///
    /// A stream that ends without its final blank line is common enough
    /// (several providers close the socket right after `data: [DONE]`) that
    /// discarding the frame would drop the terminator the adapter is waiting
    /// for.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        if !self.pending_bytes.is_empty() {
            // An incomplete sequence at the very end is malformed, not
            // pending: it becomes U+FFFD like any other bad byte.
            self.buffer.push('\u{FFFD}');
            self.pending_bytes.clear();
        }
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.buffer_units = 0;
            self.consume(&line);
        }
        self.frame().into_iter().collect()
    }

    /// Decodes as much of `pending_bytes + chunk` as forms complete UTF-8.
    ///
    /// A chunk boundary can split a multi-byte character, so an incomplete
    /// trailing sequence waits for the next chunk; anything genuinely
    /// malformed becomes U+FFFD rather than an error, because a mangled byte
    /// in prose must not lose the turn.
    fn decode(&mut self, chunk: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.pending_bytes);
        bytes.extend_from_slice(chunk);
        let mut out = String::with_capacity(bytes.len());
        let mut rest: &[u8] = &bytes;
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    out.push_str(valid);
                    break;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    out.push_str(std::str::from_utf8(&rest[..valid_up_to]).unwrap_or_default());
                    if let Some(bad) = error.error_len() {
                        out.push('\u{FFFD}');
                        rest = &rest[valid_up_to + bad..];
                    } else {
                        self.pending_bytes = rest[valid_up_to..].to_vec();
                        break;
                    }
                }
            }
        }
        out
    }

    fn consume(&mut self, line: &str) {
        // A leading colon marks a comment. Providers use them as keep-alives.
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.find(':') {
            Some(colon) => (&line[..colon], &line[colon + 1..]),
            None => (line, ""),
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => self.data_lines.push(value.to_owned()),
            "event" => value.clone_into(&mut self.event_name),
            // `id` and `retry` govern reconnection, which never applies: a
            // resumed completion would duplicate tokens, so a broken stream is
            // an error here.
            _ => {}
        }
    }

    fn frame(&mut self) -> Option<SseEvent> {
        let event_name = std::mem::take(&mut self.event_name);
        if self.data_lines.is_empty() {
            return None;
        }
        Some(SseEvent {
            event: if event_name.is_empty() {
                "message".to_owned()
            } else {
                event_name
            },
            data: std::mem::take(&mut self.data_lines).join("\n"),
        })
    }
}

/// The first line terminator in `text`: its byte offset and length.
fn find_terminator(text: &str) -> Option<(usize, usize)> {
    let index = text.find(['\r', '\n'])?;
    let length = if text[index..].starts_with("\r\n") {
        2
    } else {
        1
    };
    Some((index, length))
}

/// Parses a whole byte sequence delivered in `chunks`, for tests and fixtures.
pub fn parse_sse_chunks<'a>(
    chunks: impl IntoIterator<Item = &'a [u8]>,
    options: SseOptions,
) -> Result<Vec<SseEvent>> {
    let mut parser = SseParser::new(options);
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(parser.push(chunk)?);
    }
    events.extend(parser.finish());
    Ok(events)
}

/// Reads a byte stream as SSE frames.
///
/// An error from `source` ends the stream with that error; a frame past the
/// cap ends it with a `stream_parse` error. Nothing follows an error.
pub fn parse_sse<'a, B>(
    source: impl Stream<Item = Result<B>> + Send + 'a,
    options: SseOptions,
) -> BoxStream<'a, Result<SseEvent>>
where
    B: AsRef<[u8]> + Send + 'a,
{
    struct State<S> {
        source: S,
        parser: SseParser,
        pending: std::collections::VecDeque<Result<SseEvent>>,
        finished: bool,
    }

    let state = State {
        source: source.boxed(),
        parser: SseParser::new(options),
        pending: std::collections::VecDeque::new(),
        finished: false,
    };

    futures::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(next) = state.pending.pop_front() {
                if next.is_err() {
                    state.finished = true;
                }
                return Some((next, state));
            }
            if state.finished {
                return None;
            }
            match state.source.next().await {
                Some(Ok(chunk)) => match state.parser.push(chunk.as_ref()) {
                    Ok(events) => state.pending.extend(events.into_iter().map(Ok)),
                    Err(error) => state.pending.push_back(Err(error)),
                },
                Some(Err(error)) => {
                    state.finished = true;
                    return Some((Err(error), state));
                }
                None => {
                    state.finished = true;
                    state
                        .pending
                        .extend(state.parser.finish().into_iter().map(Ok));
                }
            }
        }
    })
    .boxed()
}
