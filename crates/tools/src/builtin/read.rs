//! `read` — the workspace read.
//!
//! Three constraints shape it, and all three are about the large file rather
//! than the ordinary one:
//!
//!  - **The read is bounded before it happens.** Reading a 2 GB log whole
//!    allocates 2 GB and then the registry truncates it to 8 000 characters.
//!    The handler streams the file in chunks and keeps at most a UTF-8 worst
//!    case of the output budget, so the memory cost is bounded by the budget
//!    rather than by whatever the model happened to point at.
//!
//!  - **Every line is reachable.** Bounding the read by taking the first N
//!    bytes and then selecting lines inside them makes every line past the
//!    budget unreachable, and a tool that says "use offset to continue" and
//!    then answers "past the end" is worse than one that never offered. The
//!    skip happens while streaming, so `offset` addresses the whole file.
//!
//!  - **Binary content is refused, not dumped.** A model given a few thousand
//!    bytes of a `.png` learns nothing and pays for the tokens twice: once in
//!    the tool result and again in every subsequent turn of history. The
//!    NUL-byte test is the cheap, reliable half of binary detection and is
//!    exactly the case that matters here.
//!
//! `offset`/`limit` are lines rather than bytes because that is the unit the
//! model reasons in, and because a byte range can split a codepoint. A read
//! with neither stops at [`DEFAULT_LINE_LIMIT`] and says how much is left,
//! which is the difference between a model that pages through a long file and
//! one that believes it has seen the end.

use std::io;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::AsyncReadExt as _;

use crate::builtin::built;
use crate::builtin::shared::{
    assert_regular, clamp_note, fs_failure, in_root, open_options, root_failure,
};
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

/// UTF-8's worst case, so the byte budget can never cut short the char budget.
const BYTES_PER_CHAR: u64 = 4;

/// Lines returned when the model asks for no window.
const DEFAULT_LINE_LIMIT: u64 = 2000;

/// How much is read at a time.
const CHUNK: usize = 64 * 1024;

/// How far past the window the line count is allowed to scan.
///
/// Counting to the end of a multi-gigabyte log costs the whole read's time for
/// one number in a notice. Past this, the notice says "more than" and the model
/// has lost nothing it was going to use.
const COUNT_SCAN_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    #[schemars(
        length(min = 1),
        description = "File to read. Rooted at the workspace."
    )]
    path: String,
    #[schemars(
        range(min = 1),
        description = "1-based line number to start from. Omit to start at the beginning."
    )]
    offset: Option<u64>,
    #[schemars(
        range(min = 1),
        description = "Maximum number of lines to return. Omit for the first 2000."
    )]
    limit: Option<u64>,
}

/// Newlines in a slice.
///
/// `memchr` is what `ignore` and the searcher already pull in, so counting with
/// it costs no dependency and clippy stops asking for `bytecount`.
fn newlines(bytes: &[u8]) -> u64 {
    memchr::memchr_iter(b'\n', bytes).count() as u64
}

/// Which phase of the stream the reader is in.
#[derive(Debug, PartialEq, Eq)]
enum Phase {
    /// Counting newlines up to `offset`, keeping nothing.
    Skip,
    /// Keeping lines until the line or byte ceiling.
    Keep,
    /// Counting the rest, to say how much was left.
    Count,
}

/// What one streamed read produced.
#[derive(Debug, Default)]
struct Window {
    /// The bytes of the selected lines.
    bytes: Vec<u8>,
    /// Lines kept.
    kept: u64,
    /// Lines in the whole file, or as far as the count scanned.
    total: u64,
    /// The count scan stopped early, so `total` is a floor.
    counted_partially: bool,
    /// The byte budget, not the line limit, ended the window.
    byte_capped: bool,
    /// A NUL byte appeared in the part that was read.
    binary: bool,
}

/// Lines `[offset, offset + limit)` of a file, streamed.
///
/// Reads at most `budget` bytes into memory plus one chunk, whatever the file's
/// size. Never returns a partial line except when a single line is larger than
/// the whole budget.
async fn read_window(
    mut file: tokio::fs::File,
    offset: u64,
    limit: u64,
    budget: u64,
) -> io::Result<Window> {
    let mut window = Window::default();
    let mut buffer = vec![0u8; CHUNK];
    let mut phase = Phase::Skip;
    // The 1-based number of the line the cursor is on.
    let mut line: u64 = 1;
    let mut scanned: u64 = 0;
    let mut ends_with_newline = false;

    if offset <= 1 {
        phase = Phase::Keep;
    }

    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        scanned = scanned.saturating_add(read as u64);
        if phase != Phase::Count && chunk.contains(&0) {
            window.binary = true;
            return Ok(window);
        }
        ends_with_newline = chunk[read - 1] == b'\n';

        let mut at = 0usize;
        while at < read {
            match phase {
                Phase::Skip => {
                    let rest = &chunk[at..];
                    match rest.iter().position(|byte| *byte == b'\n') {
                        Some(found) => {
                            line = line.saturating_add(1);
                            at += found + 1;
                            if line >= offset {
                                phase = Phase::Keep;
                            }
                        }
                        None => at = read,
                    }
                }
                Phase::Keep => {
                    let rest = &chunk[at..];
                    let held = window.bytes.len() as u64;
                    if let Some(found) = rest.iter().position(|byte| *byte == b'\n') {
                        {
                            let piece = &rest[..=found];
                            if held.saturating_add(piece.len() as u64) > budget {
                                window.byte_capped = true;
                                phase = Phase::Count;
                                continue;
                            }
                            window.bytes.extend_from_slice(piece);
                            window.kept = window.kept.saturating_add(1);
                            line = line.saturating_add(1);
                            at += found + 1;
                            if window.kept >= limit {
                                phase = Phase::Count;
                            }
                        }
                    } else {
                        // A trailing partial line: keep what the budget allows
                        // and stop there rather than growing past it.
                        if held.saturating_add(rest.len() as u64) > budget {
                            let room = usize::try_from(budget.saturating_sub(held))
                                .unwrap_or(rest.len())
                                .min(rest.len());
                            window.bytes.extend_from_slice(&rest[..room]);
                            window.byte_capped = true;
                            phase = Phase::Count;
                        } else {
                            window.bytes.extend_from_slice(rest);
                        }
                        at = read;
                    }
                }
                Phase::Count => {
                    line = line.saturating_add(newlines(&chunk[at..]));
                    at = read;
                }
            }
        }

        if phase == Phase::Count && scanned > COUNT_SCAN_LIMIT {
            window.counted_partially = true;
            break;
        }
    }

    // A file whose last byte is not a newline still ends in a line.
    window.total = if ends_with_newline {
        line.saturating_sub(1)
    } else {
        line
    };
    if !window.bytes.is_empty() && !window.bytes.ends_with(b"\n") {
        window.kept = window.kept.saturating_add(1);
    }
    Ok(window)
}

struct Read;

impl ToolHandler for Read {
    type Args = ReadArgs;

    fn execute<'a>(
        &'a self,
        args: ReadArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "read")?;
            let accepted = ctx.jail.accept(&args.path)?;
            // Report where the read actually happened, not what was asked for:
            // they differ whenever the path was clamped, and naming the request
            // would teach the model that `/etc/hosts` is a path this workspace
            // has.
            let where_ = accepted.relative.as_str();
            let note = clamp_note(&args.path, &accepted);
            let budget = ctx.config.max_output_chars.saturating_mul(BYTES_PER_CHAR);

            let file = in_root(&ctx.jail, &accepted, "read", |root, inside| {
                let mut options = open_options();
                options.read(true);
                root.open_with(inside, &options)
                    .map(cap_std::fs::File::into_std)
            })
            .await?
            .map_err(|error| root_failure(&error, &args.path, where_, &note))?;
            let file = tokio::fs::File::from_std(file);
            let stats = file
                .metadata()
                .await
                .map_err(|error| fs_failure(&error, where_, &note))?;
            assert_regular(&stats, where_, &note)?;
            if stats.is_dir() {
                return Err(WireError::new(
                    ErrorKind::InvalidInput,
                    format!("{where_} is a directory. Use ls instead."),
                )
                .with_detail("path", where_));
            }
            if stats.len() == 0 {
                return Ok(ToolOutput::text(format!("{where_} is empty.")));
            }

            let from = args.offset.unwrap_or(1);
            let limit = args.limit.unwrap_or(DEFAULT_LINE_LIMIT);
            let window = read_window(file, from, limit, budget)
                .await
                .map_err(|error| fs_failure(&error, where_, &note))?;

            if window.binary {
                return Err(WireError::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "{where_} looks like a binary file ({} bytes) and was not read.",
                        stats.len()
                    ),
                )
                .with_detail("path", where_)
                .with_detail("size", stats.len()));
            }

            // Past the end is an answer, not a failure: the model asked a
            // question about a file that exists and gets told.
            if window.kept == 0 {
                return Ok(ToolOutput::text(format!(
                    "{where_} has {} lines; offset {from} is past the end.",
                    window.total
                )));
            }

            assert_not_aborted(&ctx.token, "read")?;
            let mut text = String::from_utf8_lossy(&window.bytes).into_owned();
            let trailer = notice(&window, &args, from, budget, stats.len());
            if !trailer.is_empty() && text.ends_with('\n') {
                text.pop();
            }
            // A successful read of a clamped path needs the note as much as a
            // failed one: content came back, and without this the model
            // believes it is holding the host's file.
            let noted = if note.is_empty() {
                String::new()
            } else {
                format!("\n\n[read:{note}]")
            };
            Ok(ToolOutput::text(format!("{text}{trailer}{noted}"))
                .with_detail("path", where_)
                .with_detail("lines", window.kept)
                .with_detail("totalLines", window.total))
        })
    }
}

/// The sentence telling the model what it has and how to get the rest.
fn notice(window: &Window, args: &ReadArgs, from: u64, budget: u64, size: u64) -> String {
    let to = from.saturating_add(window.kept).saturating_sub(1);
    let total = window.total;
    let about = if window.counted_partially {
        format!("more than {total}")
    } else {
        total.to_string()
    };

    if window.byte_capped {
        return format!(
            "\n\n[read: showing the first {budget} of {size} bytes; output stopped on line {to}. Use offset={to} with a smaller limit to continue.]"
        );
    }
    if to >= total && !window.counted_partially {
        return String::new();
    }
    let next = to.saturating_add(1);
    if args.offset.is_some() || args.limit.is_some() {
        return format!(
            "\n\n[read: showing lines {from}-{to} of {about}. Use offset={next} to continue.]"
        );
    }
    let remaining = total.saturating_sub(to);
    let left = if window.counted_partially {
        format!("more than {remaining}")
    } else {
        remaining.to_string()
    };
    format!("\n\n[read: {left} more lines in file. Use offset={next} to continue.]")
}

/// The `read` tool.
pub fn read_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "read",
            "Read a UTF-8 text file from the workspace. The workspace is the root: \"/x\" and \"../x\" both resolve inside it, never outside. Returns the first 2000 lines and says how many are left; use offset and limit to page through the rest.",
        )
        .risk(ToolRisk::Safe)
        .annotations(ToolAnnotations {
            title: Some("Read file".to_owned()),
            read_only_hint: Some(true),
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        Read,
    ))
}
