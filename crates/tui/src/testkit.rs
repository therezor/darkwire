//! A terminal emulator to draw into, so a test can read what a terminal shows.
//!
//! Ratatui's own `TestBackend` records the cells a frame asked for, which
//! answers "what did the program draw" and not "what does the screen say".
//! Most of what this crate does now is the second question: a scroll region is
//! set, rows are printed into it, the region is reset and the cursor is put
//! back, and whether that worked is a fact about the emulator's screen rather
//! than about any buffer this program holds.
//!
//! So the bytes go through a real VT parser. What comes back is the screen a
//! terminal would be showing, cell by cell, with the styles it would have
//! applied. The escape sequences are exercised rather than asserted on, which
//! is what makes these tests worth more than the ones they replace.
//!
//! Behind the `testkit` feature and skipped by the coverage gate: it runs on
//! every test and would score for the crate rather than with it.

use std::fmt;
use std::io::{self, Write};
use std::num::NonZeroU16;

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// What the emulator was asked to do besides draw: the bells it rang and the
/// notifications it was sent. A terminal that drops these is where a test
/// would otherwise lose them.
#[derive(Debug, Default)]
pub struct Recorder {
    bells: usize,
    osc: Vec<Vec<String>>,
}

impl vt100::Callbacks for Recorder {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bells += 1;
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        self.osc.push(
            params
                .iter()
                .map(|param| String::from_utf8_lossy(param).into_owned())
                .collect(),
        );
    }
}

/// The parser, as something to write to. vt100 implements `Write` only for a
/// parser without callbacks.
struct Emulator(vt100::Parser<Recorder>);

impl Write for Emulator {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.process(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A terminal that parses what is written to it.
pub struct VT100Backend {
    inner: CrosstermBackend<Emulator>,
    size: Size,
}

impl VT100Backend {
    /// An emulator `width` by `height`, with an empty screen.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        crossterm::style::force_color_output(true);
        Self {
            inner: CrosstermBackend::new(Emulator(vt100::Parser::new_with_callbacks(
                at_least_one(height),
                at_least_one(width),
                0,
                Recorder::default(),
            ))),
            size: Size { width, height },
        }
    }

    /// The parser holding the screen.
    #[must_use]
    pub fn parser(&self) -> &vt100::Parser<Recorder> {
        &self.inner.writer().0
    }

    /// How many times the bell rang.
    #[must_use]
    pub fn bells(&self) -> usize {
        self.parser().callbacks().bells
    }

    /// Every OSC the emulator does not act on itself, such as a desktop
    /// notification, split at its semicolons.
    #[must_use]
    pub fn osc(&self) -> &[Vec<String>] {
        &self.parser().callbacks().osc
    }

    /// Every row of the visible screen, trailing blanks trimmed.
    #[must_use]
    pub fn rows(&self) -> Vec<String> {
        let screen = self.parser().screen();
        let (height, width) = screen.size();
        (0..height.get())
            .map(|row| {
                screen
                    .contents_between(row, 0, row, width.get())
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// One row of the visible screen, trailing blanks trimmed.
    ///
    /// A row off the screen is empty rather than an error: a test comparing
    /// against `""` says what it means, and a test that wanted a row there
    /// fails on the comparison with the row number in the message.
    #[must_use]
    pub fn row(&self, row: u16) -> String {
        self.rows()
            .get(usize::from(row))
            .cloned()
            .unwrap_or_default()
    }

    /// Where the emulator thinks the cursor is.
    #[must_use]
    pub fn cursor(&self) -> (u16, u16) {
        self.parser().screen().cursor_position()
    }

    /// Whether the emulator is showing a cursor.
    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        !self.parser().screen().hide_cursor()
    }

    /// One cell of the screen, for a test that cares about its style.
    #[must_use]
    pub fn cell(&self, column: u16, row: u16) -> Option<&vt100::Cell> {
        self.parser().screen().cell(row, column)
    }

    /// Tells the emulator the window changed size.
    pub fn set_size(&mut self, width: u16, height: u16) {
        self.size = Size { width, height };
        self.inner
            .writer_mut()
            .0
            .screen_mut()
            .set_size(at_least_one(height), at_least_one(width));
    }
}

/// vt100 has no empty screen, so a zero side is the smallest one it has.
fn at_least_one(cells: u16) -> NonZeroU16 {
    NonZeroU16::new(cells).unwrap_or(NonZeroU16::MIN)
}

impl fmt::Debug for VT100Backend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VT100Backend")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for VT100Backend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for row in self.rows() {
            writeln!(formatter, "{row}")?;
        }
        Ok(())
    }
}

impl Write for VT100Backend {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.inner.writer_mut().write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.writer_mut().flush()
    }
}

// Nothing here asks the real terminal anything: the size is what the test set
// and the cursor is where the parser says, so no method writes to stdout.
impl Backend for VT100Backend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        let (row, column) = self.cursor();
        Ok(Position { x: column, y: row })
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, region: ClearType) -> io::Result<()> {
        self.inner.clear_region(region)
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.size)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size {
                width: 0,
                height: 0,
            },
        })
    }

    fn scroll_region_up(&mut self, region: std::ops::Range<u16>, amount: u16) -> io::Result<()> {
        self.inner.scroll_region_up(region, amount)
    }

    fn scroll_region_down(&mut self, region: std::ops::Range<u16>, amount: u16) -> io::Result<()> {
        self.inner.scroll_region_down(region, amount)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(self)
    }
}
