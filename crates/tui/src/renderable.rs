//! What a widget is, now that Ratatui draws.
//!
//! A [`Renderable`] answers two questions: how tall it wants to be at a width,
//! and what to put in a buffer given an area. That is the whole contract. It
//! replaces the older `Component`, which answered with rows of text carrying
//! SGR escapes — a spelling that could not say where the caret was without
//! hiding a marker in the text, and could not be composed without measuring
//! strings a terminal was about to measure again.
//!
//! Height comes first and drawing second, because the live area at the bottom
//! of the screen is sized before it is drawn: the program asks the root how
//! tall it wants to be, gives the terminal that many rows, and only then hands
//! down an area. A widget that measures one thing and draws another leaves a
//! gap or loses its last row.

use crossterm::cursor::SetCursorStyle;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;

use crate::wrap::wrap_lines;

/// Something that can say how tall it is and draw itself.
pub trait Renderable {
    /// Draws into `area`. Nothing outside it may be written.
    fn render(&self, area: Rect, buffer: &mut Buffer);

    /// How many rows this wants at `width`.
    fn desired_height(&self, width: u16) -> u16;

    /// Where the caret belongs, in screen coordinates, if this owns it.
    ///
    /// Returning `None` means the caret is somebody else's or hidden, which is
    /// the common case: only the composer has one.
    fn cursor_pos(&self, _area: Rect) -> Option<(u16, u16)> {
        None
    }

    /// The shape the caret takes while this owns it.
    fn cursor_style(&self, _area: Rect) -> SetCursorStyle {
        SetCursorStyle::DefaultUserShape
    }
}

impl Renderable for () {
    fn render(&self, _area: Rect, _buffer: &mut Buffer) {}

    fn desired_height(&self, _width: u16) -> u16 {
        0
    }
}

impl Renderable for Line<'_> {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        Widget::render(self, area, buffer);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        1
    }
}

/// Rows that are already the width they will be drawn at.
impl Renderable for Vec<Line<'static>> {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        draw_rows(self, area, buffer);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        u16::try_from(self.len()).unwrap_or(u16::MAX)
    }
}

/// Logical lines folded to whatever width they are asked for.
///
/// The difference from a bare `Vec<Line>` is where the folding happens. These
/// are the lines as they were written, so a resize asks again and gets however
/// many rows the new width needs. Pre-folded rows would keep the old width.
pub struct Wrapped(pub Vec<Line<'static>>);

impl Renderable for Wrapped {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        draw_rows(&wrap_lines(&self.0, area.width), area, buffer);
    }

    fn desired_height(&self, width: u16) -> u16 {
        u16::try_from(wrap_lines(&self.0, width).len()).unwrap_or(u16::MAX)
    }
}

/// One row per line from the top of the area, dropping what does not fit.
fn draw_rows(lines: &[Line<'static>], area: Rect, buffer: &mut Buffer) {
    for (offset, line) in lines.iter().enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            break;
        };
        if offset >= area.height {
            break;
        }
        let row = Rect::new(area.x, area.y + offset, area.width, 1);
        Widget::render(line, row, buffer);
    }
}

impl<T: Renderable + ?Sized> Renderable for &T {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        (**self).render(area, buffer);
    }

    fn desired_height(&self, width: u16) -> u16 {
        (**self).desired_height(width)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        (**self).cursor_pos(area)
    }

    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        (**self).cursor_style(area)
    }
}

impl<T: Renderable + ?Sized> Renderable for Box<T> {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        (**self).render(area, buffer);
    }

    fn desired_height(&self, width: u16) -> u16 {
        (**self).desired_height(width)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        (**self).cursor_pos(area)
    }

    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        (**self).cursor_style(area)
    }
}

/// Nothing to draw and no rows taken, when there is nothing to show.
impl<T: Renderable> Renderable for Option<T> {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        if let Some(inner) = self {
            inner.render(area, buffer);
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.as_ref().map_or(0, |inner| inner.desired_height(width))
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.as_ref().and_then(|inner| inner.cursor_pos(area))
    }

    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        self.as_ref()
            .map_or(SetCursorStyle::DefaultUserShape, |inner| {
                inner.cursor_style(area)
            })
    }
}

/// Children stacked top to bottom, each as tall as it asked to be.
///
/// The last child is clipped rather than dropped when the area runs out, which
/// is what a live area that will not fit the window has to do: something is
/// better than a blank row, and the row that survives is the one nearest the
/// composer.
#[derive(Default)]
pub struct Column<'a> {
    children: Vec<Box<dyn Renderable + 'a>>,
}

impl<'a> Column<'a> {
    /// An empty column.
    #[must_use]
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    /// Adds a child below the ones already there.
    #[must_use]
    pub fn with(mut self, child: impl Renderable + 'a) -> Self {
        self.children.push(Box::new(child));
        self
    }

    /// Adds a child below the ones already there.
    pub fn push(&mut self, child: impl Renderable + 'a) {
        self.children.push(Box::new(child));
    }

    /// How many children there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Whether there is nothing to draw.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

impl Renderable for Column<'_> {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        let mut y = area.y;
        for child in &self.children {
            if y >= area.bottom() {
                break;
            }
            let height = child.desired_height(area.width);
            let child_area = Rect::new(area.x, y, area.width, height).intersection(area);
            if !child_area.is_empty() {
                child.render(child_area, buffer);
            }
            y = y.saturating_add(height);
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.children.iter().fold(0u16, |total, child| {
            total.saturating_add(child.desired_height(width))
        })
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.child_areas(area)
            .find_map(|(child, child_area)| child.cursor_pos(child_area))
    }

    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        self.child_areas(area)
            .find(|(child, child_area)| child.cursor_pos(*child_area).is_some())
            .map_or(SetCursorStyle::DefaultUserShape, |(child, child_area)| {
                child.cursor_style(child_area)
            })
    }
}

impl Column<'_> {
    /// Each child with the area it would be drawn into.
    fn child_areas(&self, area: Rect) -> impl Iterator<Item = (&dyn Renderable, Rect)> {
        let mut y = area.y;
        self.children.iter().map(move |child| {
            let height = child.desired_height(area.width);
            let child_area = Rect::new(area.x, y, area.width, height).intersection(area);
            y = y.saturating_add(height);
            (child.as_ref() as &dyn Renderable, child_area)
        })
    }
}

/// Rows a component produced, as something a frame can draw.
///
/// The widgets in this crate still answer with rows of text carrying SGR
/// escapes, and the caret's position with a marker hidden in one of them. That
/// is a bridge and not a destination — see [`crate::ansi`] — but it is a
/// bridge that has to hold while the rest moves across, and this is the one
/// place it is crossed.
///
/// Rows arrive already folded to the width they were rendered at, so this
/// draws one per row and does not fold again.
pub struct StyledRows {
    rows: Vec<Line<'static>>,
    /// Where the caret sits inside the rows, in rows and columns.
    caret: Option<(u16, u16)>,
}

impl StyledRows {
    /// Converts rows a component rendered at some width.
    #[must_use]
    pub fn new(rows: &[String]) -> Self {
        let caret = crate::ansi::cursor_in(rows).and_then(|(row, column)| {
            Some((u16::try_from(column).ok()?, u16::try_from(row).ok()?))
        });
        Self {
            rows: rows
                .iter()
                .map(|row| crate::ansi::styled_line(row))
                .collect(),
            caret,
        }
    }

    /// The rows, for something that wants to commit them rather than draw.
    #[must_use]
    pub fn into_lines(self) -> Vec<Line<'static>> {
        self.rows
    }
}

impl Renderable for StyledRows {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        draw_rows(&self.rows, area, buffer);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        u16::try_from(self.rows.len()).unwrap_or(u16::MAX)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let (column, row) = self.caret?;
        if row >= area.height {
            return None;
        }
        Some((
            area.x + column.min(area.width.saturating_sub(1)),
            area.y + row,
        ))
    }
}
