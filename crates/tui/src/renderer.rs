//! Draws a frame, and redraws it when it changes.
//!
//! The frame is the *whole* of what this program has put on the screen — the
//! transcript, the editor, the status rows — held as one `Vec` of drawn rows.
//! Everything else follows from holding all of it rather than only the part
//! that moves, and the reason to hold all of it is a resize.
//!
//! ## Why a footer cannot be patched in place
//!
//! A terminal rewraps its own screen when the window changes width, and it
//! does that *before* the process is told anything: by the time `SIGWINCH`
//! arrives, every row the program drew has already been folded or joined,
//! moved up or down, and the cursor is somewhere the program has no way to ask
//! about. An erase is relative to the cursor, so it reaches whatever the reflow
//! left below it and cannot touch what the reflow carried above it. Measured
//! on a narrowing from 120 to 80 columns, a three-row footer became six rows,
//! three of which were now above the cursor — and stayed on screen, one
//! stranded copy per resize. No amount of arithmetic fixes that, because the
//! arithmetic is applied to coordinates the reflow already invalidated.
//!
//! So a width change is not patched here. It erases the screen and prints the
//! frame again at the new width. That is only possible because the frame is all
//! of it; a renderer holding just a footer would have nothing to print the
//! transcript back from. The scrollback is left alone: a reflow can strand a
//! fragment of an old frame up there, and scrolling past one is cheaper than
//! losing the conversation to be sure it is gone.
//!
//! ## The rest of the time
//!
//! Every other render is differential: the new frame is compared against the
//! last one, and only rows that actually changed are rewritten, from the first
//! changed row down. A keystroke touches the editor row and the status rows,
//! so that is what gets redrawn — the transcript above is not touched, and a
//! long conversation costs the same as a short one.
//!
//! Two invariants make the row arithmetic sound, and both are checked here
//! rather than trusted from callers:
//!
//!  - **One entry is one row.** Anything wider than the window is cut, so the
//!    terminal is never the one deciding how tall the frame is.
//!  - **Rows are addressed within the frame, not the screen.** When the frame
//!    grows past the bottom the terminal scrolls, and every row moves up
//!    together — which a frame-relative offset survives and a screen coordinate
//!    does not. `viewport_top` records how much of the frame has scrolled off,
//!    and a change above it forces the full redraw, because a row in the
//!    scrollback cannot be moved to.
//!
//! A size change reaches the program as a signal rather than as anything this
//! renderer can see, so there is no resize subscription here: the renderer
//! compares the width it drew at against the width it sees now, on every
//! render, and the caller renders when it hears the window moved.
//! [`Renderer::invalidate`] is the same answer for a screen that is wrong for
//! any other reason.

use std::cmp::Ordering;

use crate::component::{CURSOR_MARKER, Component};
use crate::terminal::{TerminalOutput, columns_of, rows_of};
use crate::text::{truncate_to_width, visible_width};

const ERASE_ROW: &str = "\x1b[2K";
const ERASE_BELOW: &str = "\x1b[0J";
/// Clear the screen and go home, leaving the scrollback alone.
///
/// Deliberately without `3J`. A rewrap can strand fragments of a frame this
/// renderer drew up in the history, and dropping the scrollback is the only way
/// to be certain they are gone. It also drops the conversation, which is
/// the thing the operator scrolls back to read, plus whatever their shell
/// printed before the program started. A stale fragment costs a scroll past it.
/// A wiped history costs the session.
pub const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const SYNC_ON: &str = "\x1b[?2026h";
const SYNC_OFF: &str = "\x1b[?2026l";

fn cursor_up(rows: usize) -> String {
    format!("\x1b[{rows}A")
}

fn cursor_down(rows: usize) -> String {
    format!("\x1b[{rows}B")
}

fn cursor_right(columns: usize) -> String {
    format!("\x1b[{columns}C")
}

/// How long a frame gets, in milliseconds.
///
/// 30 frames a second. Deliberately not the spinner's interval: a spinner
/// advances at whatever rate reads well as rotation, and a frame goes out at
/// whatever rate reads as motion. Tying the two made every streamed token draw
/// a frame, which on a slow machine is the whole cost of the session paid per
/// word.
pub const FRAME_INTERVAL_MS: u64 = 33;

/// How a renderer is set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererOptions {
    /// Used when the device reports no width, as a recorded pty does.
    pub columns: Option<usize>,
    /// Used when the device reports no height.
    pub rows: Option<usize>,
    /// Synchronized output around each frame. Default `true`.
    pub synchronized: bool,
    /// Clear the screen for the first frame, as a width change already does.
    ///
    /// Off by default, and that default is the library's answer rather than
    /// the chat's: a renderer drawing four rows of a picker under a shell
    /// prompt has no business erasing what the shell printed. A caller that
    /// owns the window for the whole session does, and the frame then starts
    /// at the top of it instead of wherever the prompt happened to leave the
    /// cursor — which is the difference between "it laid out properly once I
    /// resized" and "it laid out".
    ///
    /// It also makes the row arithmetic true rather than usually-true. A whole
    /// print sets `viewport_top` from `lines.len() - screen_rows`, which
    /// assumes the frame begins at screen row 0; only a homed cursor makes
    /// that so. Without it a shell that had filled the window scrolls the frame
    /// further than `viewport_top` records, and a later differential render
    /// can address a row that is already in the scrollback.
    pub clear_on_first_frame: bool,
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            columns: None,
            rows: None,
            synchronized: true,
            clear_on_first_frame: false,
        }
    }
}

/// What a whole print erases first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clear {
    None,
    Screen,
}

/// What the next draw owes the screen.
///
/// One field rather than a pair of flags, because two of the three states are
/// "a frame was asked for" and the difference between them is only how much of
/// the screen it may trust. Holding them apart invited the fourth combination,
/// which does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// Nobody asked.
    None,
    /// Asked for, and the screen is what this renderer last wrote.
    Frame,
    /// Asked for, and what is on the screen is anybody's guess.
    Whole,
}

/// The frame with the marker removed, and where in it the cursor goes.
struct Built {
    lines: Vec<String>,
    row: Option<usize>,
    column: usize,
}

/// Finds the cursor marker and strips every copy of it.
///
/// The first marker wins the cursor; every one of them is still removed.
/// Scrubbing only the first would let a second reach the terminal, and what a
/// terminal does with an APC string it does not recognise is its own business
/// rather than something to find out in front of an operator.
fn extract_cursor(lines: Vec<String>) -> Built {
    let mut out = Vec::with_capacity(lines.len());
    let mut row = None;
    let mut column = 0;

    for line in lines {
        let Some(at) = line.find(CURSOR_MARKER) else {
            out.push(line);
            continue;
        };
        if row.is_none() {
            row = Some(out.len());
            column = visible_width(&line[..at]);
        }
        out.push(line.replace(CURSOR_MARKER, ""));
    }

    Built {
        lines: out,
        row,
        column,
    }
}

/// Owns the frame on screen and the bytes that keep it correct.
pub struct Renderer<O: TerminalOutput> {
    output: O,
    options: RendererOptions,
    previous: Vec<String>,
    previous_width: usize,
    previous_height: usize,
    /// Where the terminal's cursor sits, as a row of the frame.
    hardware_row: usize,
    /// How many rows of the frame have scrolled off the top.
    viewport_top: usize,
    full_redraws: usize,
    pending: Pending,
    stopped: bool,
    cursor_visible: bool,
}

impl<O: TerminalOutput> Renderer<O> {
    /// A renderer over `output`, nothing drawn yet.
    pub fn new(output: O, options: RendererOptions) -> Self {
        Self {
            output,
            options,
            previous: Vec::new(),
            previous_width: 0,
            previous_height: 0,
            hardware_row: 0,
            viewport_top: 0,
            full_redraws: 0,
            pending: Pending::None,
            stopped: false,
            cursor_visible: true,
        }
    }

    /// The device being drawn on.
    pub fn output(&self) -> &O {
        &self.output
    }

    /// The device being drawn on, mutably — for a test that resizes it.
    pub fn output_mut(&mut self) -> &mut O {
        &mut self.output
    }

    /// The width the next frame will be drawn at.
    pub fn columns(&self) -> usize {
        columns_of(&self.output, self.options.columns)
    }

    /// The height of the window the frame is drawn in.
    pub fn rows(&self) -> usize {
        rows_of(&self.output, self.options.rows)
    }

    /// How many frames were printed whole rather than patched. For tests.
    pub fn full_redraws(&self) -> usize {
        self.full_redraws
    }

    /// How many rows of the frame have scrolled off the top of the window.
    pub fn viewport_top(&self) -> usize {
        self.viewport_top
    }

    /// Asks for a frame on the caller's next turn, once, however often this is
    /// called. [`Renderer::render_if_requested`] is that turn.
    pub fn request_render(&mut self) {
        if !self.stopped && self.pending == Pending::None {
            self.pending = Pending::Frame;
        }
    }

    /// Throws the screen away and prints the next frame whole.
    ///
    /// The answer to a window that moved and to anything else that left the
    /// screen disagreeing with what this renderer believes is on it. It erases
    /// the screen but *not* the scrollback: what is up there is the
    /// conversation, and a redraw is asked for to fix the visible rows rather
    /// than to forget the ones that have scrolled past.
    pub fn invalidate(&mut self) {
        if self.stopped {
            return;
        }
        self.pending = Pending::Whole;
    }

    /// Draws the frame if one was requested since the last draw.
    ///
    /// A streaming turn asks for a render per token; drawing each one would be
    /// one frame per word arriving. Returns whether it drew.
    pub fn render_if_requested(&mut self, root: &mut dyn Component) -> bool {
        if self.stopped || self.pending == Pending::None {
            return false;
        }
        self.render(root);
        true
    }

    /// Whether a frame was asked for, answered once.
    ///
    /// For a caller with its own work to do before the draw (committing finished
    /// rows to the scrollback, say) and so cannot hand the component
    /// straight to [`Renderer::render_if_requested`]. A pending invalidation is
    /// left standing, because the draw that follows is what consumes it.
    pub fn take_request(&mut self) -> bool {
        if self.stopped || self.pending == Pending::None {
            return false;
        }
        if self.pending == Pending::Frame {
            self.pending = Pending::None;
        }
        true
    }

    fn frame(&self, body: &str) -> String {
        if self.options.synchronized {
            format!("{SYNC_ON}{body}{SYNC_OFF}")
        } else {
            body.to_owned()
        }
    }

    /// Moves from `hardware_row` to `row`, then to `column`.
    fn move_to(&mut self, row: usize, column: usize) -> String {
        let vertical = match row.cmp(&self.hardware_row) {
            Ordering::Greater => cursor_down(row - self.hardware_row),
            Ordering::Less => cursor_up(self.hardware_row - row),
            Ordering::Equal => String::new(),
        };
        self.hardware_row = row;
        let horizontal = if column > 0 {
            cursor_right(column)
        } else {
            String::new()
        };
        format!("{vertical}\r{horizontal}")
    }

    fn settle(&mut self, lines: Vec<String>, columns: usize, screen_rows: usize) {
        self.previous = lines;
        self.previous_width = columns;
        self.previous_height = screen_rows;
    }

    /// Prints every row, after the erase asked for.
    fn print_whole(&mut self, built: &Built, clear: Clear, columns: usize, screen_rows: usize) {
        let lines = &built.lines;
        self.full_redraws += 1;
        self.hardware_row = lines.len().saturating_sub(1);
        self.viewport_top = lines.len().saturating_sub(screen_rows);
        let erase = match clear {
            Clear::Screen => CLEAR_SCREEN,
            Clear::None => "",
        };
        let (cursor_row, cursor_column) = cursor_of(built);
        let mut body = String::from(erase);
        body.push_str(
            &lines
                .iter()
                .map(|line| fit(line, columns))
                .collect::<Vec<_>>()
                .join("\r\n"),
        );
        body.push_str(&self.move_to(cursor_row, cursor_column));
        let bytes = self.frame(&body);
        self.output.write_str(&bytes);
    }

    /// Draws the frame now.
    pub fn render(&mut self, root: &mut dyn Component) {
        if self.stopped {
            return;
        }

        let columns = self.columns();
        let screen_rows = self.rows();
        let built = extract_cursor(root.render(columns));

        let width_changed = self.previous_width != 0 && self.previous_width != columns;
        let height_changed = self.previous_height != 0 && self.previous_height != screen_rows;

        // Nothing on screen yet, or the window moved and every row on it has
        // already been rewrapped by the terminal into places this cannot
        // address. A launch erases the screen only if asked; everything else
        // erases it because the alternative is drawing over a reflow.
        let forced = std::mem::replace(&mut self.pending, Pending::None) == Pending::Whole;
        if forced || self.previous.is_empty() || width_changed || height_changed {
            // The screen, never the scrollback. What is up there is the
            // conversation this program printed, and it is what the operator
            // scrolls back to read.
            let clear = if forced || !self.previous.is_empty() || self.options.clear_on_first_frame
            {
                Clear::Screen
            } else {
                Clear::None
            };
            self.print_whole(&built, clear, columns, screen_rows);
            self.settle(built.lines, columns, screen_rows);
            return;
        }

        let Some((first_changed, last_changed)) = changed_span(&built.lines, &self.previous) else {
            let (cursor_row, cursor_column) = cursor_of(&built);
            let body = self.move_to(cursor_row, cursor_column);
            let bytes = self.frame(&body);
            self.output.write_str(&bytes);
            self.settle(built.lines, columns, screen_rows);
            return;
        };

        // A row that has scrolled into the scrollback cannot be moved to, so
        // the only honest answer is to print the frame again.
        if first_changed < self.viewport_top {
            self.print_whole(&built, Clear::Screen, columns, screen_rows);
            self.settle(built.lines, columns, screen_rows);
            return;
        }

        let body = self.patch(&built, first_changed, last_changed, columns, screen_rows);
        let bytes = self.frame(&body);
        self.output.write_str(&bytes);
        self.settle(built.lines, columns, screen_rows);
    }

    /// The bytes that turn the previous frame into this one, rows
    /// `first_changed..=last_changed` differing.
    fn patch(
        &mut self,
        built: &Built,
        first_changed: usize,
        last_changed: usize,
        columns: usize,
        screen_rows: usize,
    ) -> String {
        let lines = &built.lines;
        // A row past the end of the last frame does not exist yet, and `CUD`
        // cannot make one: moving down from the bottom row of a screen does
        // nothing at all, where a newline scrolls and creates one. A frame that
        // grew — which is what every turn does to the transcript — would
        // otherwise have its new rows written over its last old row.
        //
        // So the walk starts at the last row that *was* drawn and steps into
        // the new ones with `\r\n`, the only sequence that adds a row.
        let anchor = first_changed.min(self.previous.len().saturating_sub(1));
        // Stopping at the last row that *differs*, rather than running to the
        // bottom of the frame. Rows below an edit are usually identical — the
        // rules, the status — and rewriting them costs a repaint of the whole
        // lower frame for a one-row change. That is what a spinner does ten
        // times a second, and it is the difference between one row of traffic
        // per tick and six.
        let end = last_changed.min(lines.len().saturating_sub(1));

        let mut body = String::new();
        if !lines.is_empty() && end >= anchor {
            body.push_str(&self.move_to(anchor, 0));
            for at in anchor..=end {
                if at > anchor {
                    body.push_str("\r\n");
                    self.hardware_row += 1;
                }
                if at >= first_changed {
                    body.push_str(ERASE_ROW);
                    body.push_str(&fit(lines.get(at).map_or("", String::as_str), columns));
                }
            }
        } else {
            // Only deletions: nothing to write, but the erase below has to
            // start from the last row that survives.
            body.push_str(&self.move_to(lines.len().saturating_sub(1), 0));
        }
        // Rows the frame no longer has. Erasing below the last one takes them
        // all in a single sequence, and it cannot reach anything else:
        // everything under the frame is this renderer's too.
        if self.previous.len() > lines.len() {
            body.push_str("\r\n");
            body.push_str(ERASE_BELOW);
            body.push_str(&cursor_up(1));
        }

        // Writing past the bottom scrolls, and the whole frame moves up with
        // it. Frame-relative rows survive that; the record of what has gone is
        // the only thing that has to be corrected.
        self.viewport_top = self
            .viewport_top
            .max(lines.len().saturating_sub(screen_rows));
        let (cursor_row, cursor_column) = cursor_of(built);
        body.push_str(&self.move_to(cursor_row, cursor_column));
        body
    }

    /// Writes `lines` into the terminal's own scrollback, above the frame.
    ///
    /// The escape hatch from holding the whole session. Rows handed over here
    /// stop being this renderer's problem: the terminal owns them, it reflows
    /// them itself when the window moves, and nothing here can address them
    /// again. That last part is the cost, and it is why a caller commits only
    /// what has stopped changing.
    ///
    /// They go out **unwrapped**. Every other row this renderer writes is cut to
    /// the window, because the frame's arithmetic needs one entry to be one row.
    /// A committed line is never addressed again, so letting the terminal fold it
    /// is both free and the only way a later resize can refold it.
    ///
    /// One write, and that is not an optimisation. Erasing the live region and
    /// painting it back in two brackets shows the gap in between, and at one
    /// commit per batch of a streamed answer that is a blank flash per batch.
    pub fn print_above(&mut self, lines: &[String], root: &mut dyn Component) {
        if self.stopped {
            return;
        }
        if lines.is_empty() {
            self.render(root);
            return;
        }

        let columns = self.columns();
        let screen_rows = self.rows();
        let built = extract_cursor(root.render(columns));
        // An invalidation outstanding when a commit comes through is still an
        // invalidation. Taking it here rather than leaving it for a later
        // `render` is the difference between a resize being repaired on the
        // next frame and being swallowed by whichever frame happened to commit.
        let forced = std::mem::replace(&mut self.pending, Pending::None) == Pending::Whole;

        // Up to the first row of the frame, then erase from there down:
        // everything below it is this renderer's and all of it is about to be
        // written again.
        //
        // Unless the frame has outgrown the window, in which case its first row
        // is already in the history and cannot be moved to. A caller committing
        // on every frame keeps that from happening; one that has not yet caught
        // up gets the screen erased instead, which is correct and merely
        // expensive.
        let mut body = if !forced && self.viewport_top == 0 {
            let mut up = self.move_to(0, 0);
            up.push_str(ERASE_BELOW);
            up
        } else {
            self.hardware_row = 0;
            String::from(CLEAR_SCREEN)
        };
        for line in lines {
            body.push_str(line);
            body.push_str("\r\n");
        }

        self.full_redraws += 1;
        self.hardware_row = built.lines.len().saturating_sub(1);
        self.viewport_top = built.lines.len().saturating_sub(screen_rows);
        body.push_str(
            &built
                .lines
                .iter()
                .map(|line| fit(line, columns))
                .collect::<Vec<_>>()
                .join("\r\n"),
        );
        let (cursor_row, cursor_column) = cursor_of(&built);
        body.push_str(&self.move_to(cursor_row, cursor_column));

        let bytes = self.frame(&body);
        self.output.write_str(&bytes);
        self.settle(built.lines, columns, screen_rows);
    }

    /// Shows or hides the terminal's cursor.
    ///
    /// Tracked rather than sent every time: a spinner repaints ten times a
    /// second, and a terminal taking DECTCEM on every frame is doing ten times
    /// the work for no change.
    pub fn set_cursor_visible(&mut self, visible: bool) {
        if self.stopped || visible == self.cursor_visible {
            return;
        }
        self.cursor_visible = visible;
        self.output
            .write_str(if visible { SHOW_CURSOR } else { HIDE_CURSOR });
    }

    /// Leaves the cursor below the frame and stops drawing.
    ///
    /// Idempotent, and it has to be: it runs from the ordinary return, from a
    /// signal handler and from a `Drop`, and a terminal left with no cursor is
    /// the same class of damage as one left in raw mode.
    pub fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        // Below the last row of the frame, so a shell prompt does not land on
        // it.
        let below = self
            .previous
            .len()
            .saturating_sub(1)
            .saturating_sub(self.hardware_row);
        let down = if below > 0 {
            cursor_down(below)
        } else {
            String::new()
        };
        self.output.write_str(&format!("{down}\r\n{SHOW_CURSOR}"));
        self.cursor_visible = true;
    }

    /// Whether [`Renderer::stop`] has run.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }
}

impl<O: TerminalOutput> Drop for Renderer<O> {
    /// The exit path nobody wrote: a renderer that unwinds still shows the
    /// cursor and leaves it below the frame.
    fn drop(&mut self) {
        self.stop();
    }
}

/// The row and column the terminal's cursor is parked at after a frame.
///
/// Without a marker the cursor rests at the start of the last row.
fn cursor_of(built: &Built) -> (usize, usize) {
    match built.row {
        Some(row) => (row, built.column),
        None => (built.lines.len().saturating_sub(1), 0),
    }
}

/// The row as it goes to the terminal.
///
/// The cut is enforced here rather than trusted from the component, because
/// everything else assumes one entry is one row, and a single wrapped line puts
/// every later row's address out by one. Applied at the moment a row is
/// written rather than to the whole frame, so a keystroke costs the rows it
/// redraws and not the length of the conversation.
fn fit(line: &str, columns: usize) -> String {
    truncate_to_width(line, columns, "…")
}

/// The first and last rows that differ between two frames, if any do.
fn changed_span(lines: &[String], previous: &[String]) -> Option<(usize, usize)> {
    let total = lines.len().max(previous.len());
    let mut span = None;
    for at in 0..total {
        let now = lines.get(at).map_or("", String::as_str);
        let before = previous.get(at).map_or("", String::as_str);
        if now != before {
            span = Some(span.map_or((at, at), |(first, _)| (first, at)));
        }
    }
    span
}
