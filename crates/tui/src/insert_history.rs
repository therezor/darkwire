//! Writing rows above the live area, into the terminal's own scrollback.
//!
//! This is the one place in the program that writes escape sequences by hand,
//! and it is deliberate: a finished row is not something this program draws,
//! it is something it hands over. Once the bytes are out, the row belongs to
//! the emulator — its selection, its search, its wheel, its scrollback with no
//! limit this program sets. Drawing it into a buffer instead would mean owning
//! all of that badly.
//!
//! The trick is a scroll region. Setting one to the rows above the live area
//! means a newline printed inside it scrolls *only* those rows: the live area
//! stays where it is, and the row that leaves the top goes into scrollback the
//! way any other row does. Without it a newline at the bottom of the screen
//! would push the live area up and out.
//!
//! ```text
//! ┌─Screen───────────────────────┐
//! │┌╌Scroll region╌╌╌╌╌╌╌╌╌╌╌╌╌╌┐│
//! │┆                            ┆│
//! │█╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┘│   ← the cursor writes here
//! │╭─Live area──────────────────╮│
//! │╰────────────────────────────╯│
//! └──────────────────────────────┘
//! ```
//!
//! Rows are folded to today's width before they go. The terminal would fold
//! them itself, but then this program's count of how many rows it wrote would
//! be short by however many folded, and the live area would be drawn over its
//! own history. The cost is that they do not reflow when the window widens,
//! which is the same trade every program that prints to a terminal makes.

use std::fmt;
use std::io::{self, Write};

use crossterm::cursor::{MoveTo, MoveToColumn};
use crossterm::style::{Attribute, Print, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{Command, queue};
use ratatui::backend::{Backend, IntoCrossterm};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use crate::terminal::Terminal;
use crate::wrap::{leading_whitespace, wrap_line};

/// How rows get above the live area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertMode {
    /// A scroll region above the live area, which keeps native scrollback.
    ScrollRegion,
    /// Repaint instead, for terminals that lose rows scrolled out of a partial
    /// region rather than moving them into scrollback. Windows Terminal is the
    /// one this exists for; a live area at the very top of the screen needs it
    /// too, because a scroll region needs two distinct rows.
    Repaint,
}

/// Writes `lines` above the live area and leaves the cursor where it was.
///
/// `reclaim` is how many blank rows sit directly above the live area because it
/// shrank and stayed on the bottom. They are filled before anything scrolls,
/// and what comes back is however many are left. A row written into the hole
/// costs nothing and closes it; scrolling past it makes it permanent, because
/// nothing can address a row once it is history.
///
/// # Errors
///
/// Whatever the backend gives back when a write fails.
pub fn insert_history_lines<B>(
    terminal: &mut Terminal<B>,
    lines: &[Line<'static>],
    mode: InsertMode,
    reclaim: u16,
) -> io::Result<u16>
where
    B: Backend<Error = io::Error> + Write,
{
    if lines.is_empty() {
        return Ok(reclaim);
    }

    let screen = terminal.last_known_screen_size;
    let mut area = terminal.viewport_area;
    let width = area.width.max(1);
    let wrapped = fold(lines, width);
    let cursor = terminal.last_known_cursor_pos;

    // A scroll region needs a row above the live area to scroll, and two rows
    // to be a region at all.
    let mode = if area.top() < 2 {
        InsertMode::Repaint
    } else {
        mode
    };

    // The hole is above the live area, and a repaint writes from the live
    // area's own top downwards, so it starts that much higher up.
    let reclaim = reclaim.min(area.top());
    let rows = u16::try_from(wrapped.len()).unwrap_or(u16::MAX);
    match mode {
        InsertMode::Repaint => {
            area.y = area.top().saturating_sub(reclaim);
            area.height = area.height.saturating_add(reclaim);
            repaint(terminal, &wrapped, &mut area, screen.height)?;
            area.height = area.height.saturating_sub(reclaim);
        }
        InsertMode::ScrollRegion => {
            scroll_region(
                terminal,
                &wrapped,
                &mut area,
                screen.height,
                cursor,
                reclaim,
            )?;
        }
    }

    if area != terminal.viewport_area {
        terminal.set_viewport_area(area);
    }
    Ok(reclaim.saturating_sub(rows))
}

/// Every line folded to the width it will be written at.
fn fold(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let indent = leading_whitespace(line);
        out.extend(wrap_line(line, width, &indent));
    }
    out
}

/// Writes the rows where the live area was and puts the live area back below.
///
/// Nothing is scrolled: the rows are printed at the top of where the live area
/// used to be and the live area moves down after them, until it reaches the
/// bottom of the screen and the terminal's own scrolling takes over.
fn repaint<B>(
    terminal: &mut Terminal<B>,
    wrapped: &[Line<'static>],
    area: &mut Rect,
    screen_height: u16,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let rows = u16::try_from(wrapped.len()).unwrap_or(u16::MAX);
    terminal.clear_after(area.as_position())?;
    let height = area.height;
    let writer = terminal.backend_mut();
    queue!(writer, MoveTo(0, area.top()))?;
    for (index, line) in wrapped.iter().enumerate() {
        if index > 0 {
            queue!(writer, Print("\r\n"))?;
        }
        write_line(writer, line)?;
    }
    // Empty rows where the live area will be drawn, so the terminal scrolls
    // itself rather than leaving the live area on top of the last history row.
    for _ in 0..height {
        queue!(writer, Print("\r\n"), Clear(ClearType::UntilNewLine))?;
    }
    Write::flush(writer)?;

    area.y = area
        .top()
        .saturating_add(rows)
        .min(screen_height.saturating_sub(height));
    Ok(())
}

/// Writes the rows into a scroll region above the live area.
fn scroll_region<B>(
    terminal: &mut Terminal<B>,
    wrapped: &[Line<'static>],
    area: &mut Rect,
    screen_height: u16,
    cursor: Position,
    reclaim: u16,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let rows = u16::try_from(wrapped.len()).unwrap_or(u16::MAX);
    let writer = terminal.backend_mut();

    // A live area above the bottom of the screen has room to move down, which
    // is better than scrolling rows off the top that nothing has replaced yet.
    let cursor_row = if area.bottom() < screen_height {
        let by = rows.min(screen_height - area.bottom());
        queue!(writer, SetScrollRegion(area.top() + 1..screen_height))?;
        queue!(writer, MoveTo(0, area.top()))?;
        for _ in 0..by {
            // Reverse index: scroll the region down by one, at its top.
            queue!(writer, Print("\x1bM"))?;
        }
        queue!(writer, ResetScrollRegion)?;
        let row = area.top().saturating_sub(1);
        area.y += by;
        row
    } else {
        // Back up over the blank rows the live area gave back, so the first
        // line lands in the first of them instead of scrolling past the lot.
        area.top().saturating_sub(1 + reclaim)
    };

    queue!(writer, SetScrollRegion(1..area.top()))?;
    // MoveTo rather than the terminal's own cursor setter: this whole
    // operation has to leave `last_known_cursor_pos` true, and it does that by
    // putting the cursor back rather than by telling the terminal it moved.
    queue!(writer, MoveTo(0, cursor_row))?;
    for line in wrapped {
        queue!(writer, Print("\r\n"))?;
        write_line(writer, line)?;
    }
    queue!(writer, ResetScrollRegion)?;
    queue!(writer, MoveTo(cursor.x, cursor.y))?;
    Write::flush(writer)
}

/// One row: the line's own style under each span's, and an erase to the end.
fn write_line<W: Write>(writer: &mut W, line: &Line<'static>) -> io::Result<()> {
    queue!(writer, MoveToColumn(0), Clear(ClearType::UntilNewLine))?;
    let mut last = Style::reset();
    for span in &line.spans {
        // The line's style is the floor and the span's is what sits on it, so
        // a green blockquote keeps its colour through a span that only asked
        // to be bold.
        let style = line.style.patch(span.style);
        write_style(writer, last, style)?;
        queue!(writer, Print(span.content.as_ref()))?;
        last = style;
    }
    queue!(writer, SetAttribute(Attribute::Reset))
}

/// Only what changed between two styles.
///
/// A reset per span would work and would be four times the bytes; on a slow
/// link that is the difference between a scroll that keeps up and one that
/// does not.
fn write_style<W: Write>(writer: &mut W, from: Style, to: Style) -> io::Result<()> {
    let removed = from.sub_modifier.complement() & from.add_modifier & !to.add_modifier;
    if !removed.is_empty() {
        // There is no "un-bold" that does not also un-dim, so anything coming
        // off means starting from nothing and putting back what stays.
        queue!(writer, SetAttribute(Attribute::Reset))?;
        return write_style(writer, Style::reset(), to);
    }

    let added = to.add_modifier & !from.add_modifier;
    for (modifier, attribute) in [
        (Modifier::BOLD, Attribute::Bold),
        (Modifier::DIM, Attribute::Dim),
        (Modifier::ITALIC, Attribute::Italic),
        (Modifier::UNDERLINED, Attribute::Underlined),
        (Modifier::SLOW_BLINK, Attribute::SlowBlink),
        (Modifier::RAPID_BLINK, Attribute::RapidBlink),
        (Modifier::REVERSED, Attribute::Reverse),
        (Modifier::HIDDEN, Attribute::Hidden),
        (Modifier::CROSSED_OUT, Attribute::CrossedOut),
    ] {
        if added.contains(modifier) {
            queue!(writer, SetAttribute(attribute))?;
        }
    }

    if from.fg != to.fg {
        queue!(
            writer,
            SetForegroundColor(to.fg.unwrap_or(Color::Reset).into_crossterm())
        )?;
    }
    if from.bg != to.bg {
        queue!(
            writer,
            SetBackgroundColor(to.bg.unwrap_or(Color::Reset).into_crossterm())
        )?;
    }
    Ok(())
}

/// DECSTBM: confine scrolling to these rows, counted from one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetScrollRegion(pub std::ops::Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write!(out, "\x1b[{};{}r", self.0.start, self.0.end)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(io::Error::other(
            "a scroll region is an escape sequence, not a console call",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

/// DECSTBM with no arguments: the whole screen scrolls again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write!(out, "\x1b[r")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(io::Error::other(
            "a scroll region is an escape sequence, not a console call",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}
