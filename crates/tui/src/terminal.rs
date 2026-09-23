//! The live area, and the screen it sits at the bottom of.
//!
//! Ratatui's own `Terminal` can draw into an inline viewport, but it fixes the
//! height when it is built and offers no way to change it. The live area here
//! is never one height for long — a spinner row appears, a plan grows, the
//! composer wraps — so building a new terminal per height change would mean
//! clearing and repainting the screen several times a second. This is the same
//! double-buffered diff, with the viewport as a field rather than a
//! constructor argument.
//!
//! Two invariants the rest of the crate leans on:
//!
//! - **The viewport is a rectangle on the ordinary screen**, not a screen of
//!   its own. Rows above it belong to the terminal, and writing there is
//!   [`crate::insert_history`]'s job rather than this one's.
//! - **`last_known_cursor_pos` is the truth about the cursor.** Anything that
//!   writes escape sequences behind this type's back puts the cursor back
//!   before returning, because the next diff addresses cells relative to it.

use std::io::{self, Write};

use crossterm::cursor::SetCursorStyle;
use crossterm::queue;
use ratatui::backend::{Backend, ClearType};
use ratatui::buffer::{Buffer, Cell, CellDiffOption, CellWidth};
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::Color;

/// What a draw callback is handed: an area, a buffer and a caret to place.
pub struct Frame<'a> {
    /// The rectangle this frame may draw in.
    pub area: Rect,
    /// The cells it draws into.
    pub buffer: &'a mut Buffer,
    cursor: Option<Position>,
    cursor_style: SetCursorStyle,
}

impl Frame<'_> {
    /// Shows the caret at this position after the frame is flushed.
    ///
    /// Not calling this hides the caret, which is what a frame with nothing
    /// being typed into it wants.
    pub fn set_cursor_position(&mut self, position: impl Into<Position>) {
        self.cursor = Some(position.into());
    }

    /// The shape the caret takes.
    pub fn set_cursor_style(&mut self, style: SetCursorStyle) {
        self.cursor_style = style;
    }
}

/// A terminal whose live area is a rectangle on the ordinary screen.
pub struct Terminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    backend: B,
    /// This frame and the last one. The difference between them is what gets
    /// written, which is why a redraw of an unchanged frame costs nothing.
    buffers: [Buffer; 2],
    current: usize,
    hidden_cursor: bool,
    last_cursor_style: Option<SetCursorStyle>,
    /// Where the live area sits on the screen.
    pub viewport_area: Rect,
    /// The size the last draw was laid out for.
    pub last_known_screen_size: Size,
    /// Where the cursor was left, so anything writing behind this type's back
    /// knows where to put it back.
    pub last_known_cursor_pos: Position,
}

impl<B> Terminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    /// Opens over a backend with an empty live area at the foot of the screen.
    ///
    /// **Nothing is asked of the terminal.** The obvious way to find out where
    /// the live area belongs is to ask where the cursor is (`ESC[6n`) and put
    /// it there, and that is a read of stdin waiting for a reply that a
    /// terminal need not send. What that looks like when it does not is a
    /// prompt that shows nothing at all until the first key is pressed, which
    /// is the keystroke that finally gives the read something to return.
    ///
    /// So the live area starts with no height at the foot of the screen, and
    /// the first draw makes room for itself by scrolling what is above it. The
    /// shell's screen goes into its own scrollback, which is where it was
    /// going anyway, and no reply is waited for.
    pub fn new(backend: B) -> io::Result<Self> {
        let screen_size = backend.size()?;
        Ok(Self::anchored(
            backend,
            screen_size,
            Position {
                x: 0,
                y: screen_size.height,
            },
        ))
    }

    /// Opens over a backend with the cursor position already known.
    pub fn anchored(backend: B, screen_size: Size, cursor: Position) -> Self {
        Self {
            backend,
            buffers: [Buffer::empty(Rect::ZERO), Buffer::empty(Rect::ZERO)],
            current: 0,
            hidden_cursor: false,
            last_cursor_style: None,
            viewport_area: Rect::new(0, cursor.y, 0, 0),
            last_known_screen_size: screen_size,
            last_known_cursor_pos: cursor,
        }
    }

    /// The backend, for something that has to write escape sequences itself.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// The backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// What the backend says the screen is.
    pub fn size(&self) -> io::Result<Size> {
        self.backend.size()
    }

    /// Moves or resizes the live area.
    ///
    /// Both buffers follow, so the next draw diffs like for like. Nothing is
    /// cleared: a caller that moved the area over rows it did not own calls
    /// [`Terminal::clear_after`] itself, because only it knows which rows those
    /// were.
    pub fn set_viewport_area(&mut self, area: Rect) {
        if self.viewport_area == area {
            return;
        }
        self.buffers[0].resize(area);
        self.buffers[1].resize(area);
        self.viewport_area = area;
        self.invalidate();
    }

    /// Records a new screen size without touching what is drawn.
    pub fn set_screen_size(&mut self, size: Size) {
        self.last_known_screen_size = size;
    }

    /// Draws one frame: the callback fills a buffer, the difference goes out.
    pub fn draw(&mut self, render: impl FnOnce(&mut Frame)) -> io::Result<()> {
        let area = self.viewport_area;
        let mut frame = Frame {
            area,
            buffer: &mut self.buffers[self.current],
            cursor: None,
            cursor_style: SetCursorStyle::DefaultUserShape,
        };
        render(&mut frame);
        let cursor = frame.cursor;
        let cursor_style = frame.cursor_style;

        let updates = self.diff();
        // A terminal that is not hiding intermediate moves would otherwise
        // show the caret skipping across the frame as it is painted.
        if !updates.is_empty() && !self.hidden_cursor {
            self.hide_cursor()?;
        }
        self.flush_updates(updates)?;

        match cursor {
            Some(position) => {
                self.set_cursor_style(cursor_style)?;
                self.set_cursor_position(position)?;
                self.show_cursor()?;
            }
            None if !self.hidden_cursor => self.hide_cursor()?,
            None => {}
        }

        self.swap_buffers();
        Backend::flush(&mut self.backend)
    }

    /// Hides the cursor.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        self.hidden_cursor = true;
        Ok(())
    }

    /// Shows the cursor.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.backend.show_cursor()?;
        self.hidden_cursor = false;
        Ok(())
    }

    /// Sets the caret's shape, skipping the write when it is already that.
    pub fn set_cursor_style(&mut self, style: SetCursorStyle) -> io::Result<()> {
        if self.last_cursor_style == Some(style) {
            return Ok(());
        }
        queue!(self.backend, style)?;
        self.last_cursor_style = Some(style);
        Ok(())
    }

    /// Puts the cursor somewhere and remembers that it is there.
    pub fn set_cursor_position(&mut self, position: impl Into<Position>) -> io::Result<()> {
        let position = position.into();
        self.backend.set_cursor_position(position)?;
        self.last_known_cursor_pos = position;
        Ok(())
    }

    /// Erases from a position to the end of the screen and forgets the frame.
    ///
    /// The forgetting is the point: after this the screen holds nothing the
    /// last buffer describes, so the next draw has to write every cell rather
    /// than the ones that changed.
    pub fn clear_after(&mut self, position: Position) -> io::Result<()> {
        self.backend.set_cursor_position(position)?;
        self.last_known_cursor_pos = position;
        self.backend.clear_region(ClearType::AfterCursor)?;
        self.invalidate();
        Ok(())
    }

    /// Erases the live area and forgets the frame.
    pub fn clear(&mut self) -> io::Result<()> {
        if self.viewport_area.is_empty() {
            return Ok(());
        }
        self.clear_after(self.viewport_area.as_position())
    }

    /// Erases everything the window is showing and forgets the frame.
    ///
    /// For a resize, and only for a resize. A terminal that reflows its screen
    /// when the width changes moves the rows this program drew to wherever the
    /// new width puts them, which is not where it left them and not somewhere
    /// it can address: what is left behind is a copy of the live area for
    /// every step of a drag. Erasing the whole window is the only way to be
    /// sure none of them survived, and what was scrolled into the scrollback
    /// before the resize is untouched.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        self.backend.set_cursor_position(Position { x: 0, y: 0 })?;
        self.last_known_cursor_pos = Position { x: 0, y: 0 };
        self.backend.clear_region(ClearType::All)?;
        self.backend.set_cursor_position(Position { x: 0, y: 0 })?;
        self.invalidate();
        Write::flush(&mut self.backend)
    }

    /// Makes the next draw write every cell.
    ///
    /// For damage no signal announces: another program wrote to the same
    /// terminal, or this one moved rows around with escape sequences the diff
    /// knows nothing about. Resetting the last buffer is not enough on its
    /// own, because a blank cell equals a blank cell and stale text would show
    /// through the spaces; marking each cell as always differing is.
    pub fn invalidate(&mut self) {
        let previous = &mut self.buffers[1 - self.current];
        for cell in &mut previous.content {
            cell.reset();
            cell.set_diff_option(CellDiffOption::AlwaysUpdate);
        }
    }

    /// What has to be written for this frame to match the last one.
    fn diff(&self) -> Vec<Update> {
        let previous = &self.buffers[1 - self.current];
        let current = &self.buffers[self.current];
        let mut updates: Vec<Update> = previous
            .diff(current)
            .into_iter()
            .map(|(x, y, cell)| Update::Put {
                x,
                y,
                cell: cell.clone(),
            })
            .collect();

        // A row that lost its tail — a composer that shrank, a status line
        // that got shorter — has blanks where glyphs were. Ratatui's diff
        // reports those as cells to write, which works, but one erase is
        // cheaper than a run of spaces and says what was meant.
        let area = current.area;
        for row in 0..area.height {
            if let Some(from) = trailing_blank_start(current, row) {
                let previous_tail_differs = (from..area.width).any(|column| {
                    previous[(area.x + column, area.y + row)]
                        != current[(area.x + column, area.y + row)]
                });
                if previous_tail_differs {
                    updates.retain(|update| !update.is_in_row_from(area.y + row, area.x + from));
                    updates.push(Update::ClearToEnd {
                        x: area.x + from,
                        y: area.y + row,
                        background: current[(area.x + from, area.y + row)].bg,
                    });
                }
            }
        }
        updates
    }

    fn flush_updates(&mut self, updates: Vec<Update>) -> io::Result<()> {
        let mut puts: Vec<(u16, u16, Cell)> = Vec::new();
        for update in &updates {
            if let Update::Put { x, y, cell } = update {
                puts.push((*x, *y, cell.clone()));
            }
        }
        if let Some((x, y, _)) = puts.last() {
            self.last_known_cursor_pos = Position { x: *x, y: *y };
        }
        self.backend
            .draw(puts.iter().map(|(x, y, cell)| (*x, *y, cell)))?;

        for update in updates {
            if let Update::ClearToEnd { x, y, background } = update {
                self.backend.set_cursor_position(Position { x, y })?;
                if background != Color::Reset {
                    queue!(
                        self.backend,
                        crossterm::style::SetBackgroundColor(to_crossterm(background))
                    )?;
                }
                // To the end of the row, not the end of the screen: the rows
                // below are other rows of this same frame.
                self.backend.clear_region(ClearType::UntilNewLine)?;
                if background != Color::Reset {
                    queue!(
                        self.backend,
                        crossterm::style::SetBackgroundColor(crossterm::style::Color::Reset)
                    )?;
                }
                self.last_known_cursor_pos = Position { x, y };
            }
        }
        Ok(())
    }

    fn swap_buffers(&mut self) {
        self.buffers[1 - self.current].reset();
        self.current = 1 - self.current;
    }
}

/// One thing the backend has to do for this frame to be on the screen.
enum Update {
    Put { x: u16, y: u16, cell: Cell },
    ClearToEnd { x: u16, y: u16, background: Color },
}

impl Update {
    fn is_in_row_from(&self, row: u16, column: u16) -> bool {
        match self {
            Update::Put { x, y, .. } | Update::ClearToEnd { x, y, .. } => *y == row && *x >= column,
        }
    }
}

/// Where a row's run of unstyled blanks starts, when it has one.
///
/// Only an unstyled blank counts: a space with a background colour is
/// something the frame drew, and erasing it would lose it.
fn trailing_blank_start(buffer: &Buffer, row: u16) -> Option<u16> {
    let area = buffer.area;
    let mut start = None;
    for column in (0..area.width).rev() {
        let cell = &buffer[(area.x + column, area.y + row)];
        if cell.symbol() == " "
            && cell.bg == Color::Reset
            && cell.modifier == ratatui::style::Modifier::empty()
        {
            start = Some(column);
        } else {
            break;
        }
    }
    // A wide glyph's right half is a blank cell too. An erase that starts
    // there takes the whole glyph with it, so start after the glyph instead.
    let from = start?;
    let Some(glyph) = from.checked_sub(1) else {
        return Some(from);
    };
    let end = glyph.saturating_add(buffer[(area.x + glyph, area.y + row)].cell_width());
    let from = from.max(end);
    (from < area.width).then_some(from)
}

/// Ratatui's colour as crossterm spells it.
fn to_crossterm(color: Color) -> crossterm::style::Color {
    use ratatui::backend::IntoCrossterm;
    color.into_crossterm()
}
