//! Draws the strip at the bottom of the screen, and redraws it when it changes.
//!
//! The strip is everything this program still owns: the run a turn has open,
//! the line being typed, the status rows. Finished conversation is not in it.
//! That went to the terminal with [`Renderer::print_above`] and belongs to the
//! terminal from then on, which is what lets it reflow on a resize, be selected
//! with a mouse and be found with the emulator's own search.
//!
//! The strip is held as one `Vec` of drawn rows so a redraw can diff against
//! what is on screen rather than repaint.
//!
//! ## Why a repaint never clears the screen
//!
//! A terminal rewraps its own screen when the window changes width, and it
//! does that *before* the process is told anything: by the time `SIGWINCH`
//! arrives, every row the program drew has already been folded or joined, and
//! the cursor has moved with the cell it was on. An erase is relative to the
//! cursor, so a repaint has to know where the cursor now is.
//!
//! It is not asked. Every width this renderer drew at is known, so how many
//! rows each drawn line takes at the *new* width is arithmetic, and the cursor
//! sits a computable number of rows below the strip's first one. The repaint
//! walks up by that number and erases downward from there.
//!
//! What it must never do is erase the whole screen. `\x1b[2J` moves the rows it
//! erases into the scrollback on most emulators, including every one shipped on
//! macOS, so a repaint left a copy of whatever was on screen in the history
//! every time the window moved. On a young session that is the welcome banner,
//! once per resize, for as long as the session lasts. Erasing downward from a
//! row this renderer owns cannot reach the history at all.
//!
//! A terminal that does not rewrap, which is xterm and Alacritty, is
//! overcounted by that arithmetic. It erases rows of conversation from the
//! visible screen rather than stranding fragments of a strip on it, and the
//! conversation is still in the history where it was printed. Of the two ways
//! to be wrong it is the one that tidies up after itself.
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
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const SYNC_ON: &str = "\x1b[?2026h";
const SYNC_OFF: &str = "\x1b[?2026l";
/// Autowrap off, for the rows this renderer addresses.
///
/// One `Vec` entry has to be one physical row, or every later row's address is
/// out by one. Cutting each row to the window nearly gets there: a row exactly
/// as wide as the window leaves some emulators in a pending-wrap state that
/// costs a row anyway. With autowrap off there is no such state, so a full
/// width rule is safe to draw and the invariant is a fact rather than a hope.
///
/// It also keeps the strip out of a rewrap when the window widens, which is
/// what lets the repaint walk up to its first row and find it there.
const AUTOWRAP_OFF: &str = "\x1b[?7l";
/// Autowrap back on, which is where every terminal starts and where the
/// conversation needs it: those lines are the terminal's to fold and refold.
const AUTOWRAP_ON: &str = "\x1b[?7h";

/// How many physical rows `line` takes at `columns`.
///
/// Trailing blanks are trimmed first, because an emulator rewrapping a row
/// drops them: counting them would put the strip's top a row too far up and
/// erase a line of conversation that is still on screen.
fn rows_needed(line: &str, columns: usize) -> usize {
    if columns == 0 {
        return 1;
    }
    let width = visible_width(line.trim_end());
    if width == 0 {
        1
    } else {
        width.div_ceil(columns)
    }
}

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
    /// Scroll the screen away on the first frame, so the strip sits at the
    /// bottom of the window.
    ///
    /// A screenful of newlines, and deliberately not an erase. It moves
    /// whatever the shell had printed into the terminal's own history on every
    /// emulator, where an erase either loses it or, on most of them, copies it.
    /// The cursor lands on the last row, and because the conversation only ever
    /// grows downward from there, it stays on the last row for the rest of the
    /// session. That is the whole of the composer being pinned to the bottom.
    ///
    /// Off by default, and that default is the library's answer rather than the
    /// chat's: a renderer drawing four rows of a picker under a shell prompt has
    /// no business scrolling away what the shell printed. A caller that owns the
    /// window for the whole session does.
    pub take_screen_on_open: bool,
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            columns: None,
            rows: None,
            synchronized: true,
            take_screen_on_open: false,
        }
    }
}

/// What a whole print erases first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clear {
    /// Print over whatever is there. Only a first frame does this.
    None,
    /// Erase the strip and everything under it, then print.
    Strip,
    /// Scroll the screen into the history, then print at the bottom of it.
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
    /// Where the last paint left the cursor, in frame coordinates.
    ///
    /// Kept because it is the only fixed point a resize has. The terminal
    /// rewraps the strip before the program hears about the window, and the
    /// cursor moves with the cell it was on, so its offset from the top of the
    /// strip is the one thing that can be worked out at the new width.
    previous_cursor: (usize, usize),
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
            previous_cursor: (0, 0),
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

    /// How many rows the window has lost since the last paint, if any.
    ///
    /// A window that loses rows scrolls the strip's top into the history to
    /// keep the cursor visible. Anything up there is the terminal's now, so a
    /// caller holding rows that were on screen has to let that many go rather
    /// than print them a second time.
    #[must_use]
    pub fn rows_lost(&self) -> usize {
        if self.previous_height == 0 {
            return 0;
        }
        self.previous_height.saturating_sub(self.rows())
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

    /// One paint, bracketed so the terminal shows all of it or none of it, and
    /// left with autowrap on however the body used it.
    fn frame(&self, body: &str) -> String {
        if self.options.synchronized {
            format!("{SYNC_ON}{body}{AUTOWRAP_ON}{SYNC_OFF}")
        } else {
            format!("{body}{AUTOWRAP_ON}")
        }
    }

    /// Moves from `hardware_row` to `row`, then to `column`.
    ///
    /// Records where it landed. A resize has nothing else to measure from: the
    /// terminal has already rewrapped the strip by the time the program hears
    /// about the window, and the cursor is the one cell whose new position
    /// follows from where it was.
    fn move_to(&mut self, row: usize, column: usize) -> String {
        self.previous_cursor = (row, column);
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

    /// How far above the cursor the strip's first row is, at `columns`.
    ///
    /// The whole of the resize arithmetic, and it needs no reply from the
    /// terminal. Every width this renderer drew at is known, so the rows each
    /// drawn line occupies at the new width is arithmetic; the cursor is
    /// somewhere inside the row it was left on, and a rewrap carries it along
    /// with its own cell.
    ///
    /// A terminal that does not rewrap at all, which is xterm and Alacritty,
    /// is overcounted here. That erases rows of conversation from the visible
    /// screen rather than stranding fragments of a strip on it, and the
    /// conversation is still in the history where it was printed. Of the two
    /// ways to be wrong it is the one that tidies up after itself.
    fn rows_above_cursor(&self, columns: usize) -> usize {
        let (row, column) = self.previous_cursor;
        let above: usize = self
            .previous
            .iter()
            .take(row)
            .map(|line| rows_needed(line, columns))
            .sum();
        let into = column.checked_div(columns).unwrap_or(0);
        above + into
    }

    /// Moves to the strip's first row and erases it and everything below.
    ///
    /// What used to be `\x1b[2J`. Erasing the whole screen is the one thing
    /// that cannot be done here: most emulators move the erased rows into the
    /// scrollback, so a repaint left a copy of the conversation in the history
    /// every time the window moved, one per resize. Erasing downward from a row
    /// this renderer owns never touches the history at all.
    fn erase_strip(&mut self, columns: usize) -> String {
        let up = self.rows_above_cursor(columns);
        self.hardware_row = 0;
        let mut body = if up > 0 { cursor_up(up) } else { String::new() };
        body.push('\r');
        body.push_str(ERASE_BELOW);
        body
    }

    /// Prints every row, after the erase asked for.
    fn print_whole(&mut self, built: &Built, clear: Clear, columns: usize, screen_rows: usize) {
        let lines = &built.lines;
        self.full_redraws += 1;
        self.hardware_row = lines.len().saturating_sub(1);
        self.viewport_top = lines.len().saturating_sub(screen_rows);
        let erase = match clear {
            Clear::Strip => self.erase_strip(columns),
            Clear::Screen => "\n".repeat(screen_rows),
            Clear::None => String::new(),
        };
        // After the erase the cursor is on the strip's first row, which is
        // where the rows below are about to be written from.
        self.hardware_row = lines.len().saturating_sub(1);
        let (cursor_row, cursor_column) = cursor_of(built);
        let mut body = erase;
        body.push_str(AUTOWRAP_OFF);
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
            let clear = if forced || !self.previous.is_empty() {
                Clear::Strip
            } else if self.options.take_screen_on_open {
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
            self.print_whole(&built, Clear::Strip, columns, screen_rows);
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

        let mut body = String::from(AUTOWRAP_OFF);
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
        // Up to the strip's first row, then erase from there down. A forced
        // repaint and an ordinary commit take the same route now: there is no
        // second, more violent erase to fall back to, because erasing the
        // screen is what put copies of the conversation in the history.
        let mut body = if !forced && self.viewport_top == 0 {
            let mut up = self.move_to(0, 0);
            up.push_str(ERASE_BELOW);
            up
        } else {
            self.erase_strip(columns)
        };
        for line in lines {
            body.push_str(line);
            body.push_str("\r\n");
        }

        self.full_redraws += 1;
        self.hardware_row = built.lines.len().saturating_sub(1);
        self.viewport_top = built.lines.len().saturating_sub(screen_rows);
        // The committed lines above went out with autowrap on, because they are
        // the terminal's to fold and refold. From here down is the strip.
        body.push_str(AUTOWRAP_OFF);
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
