//! Widgets that say how tall they are and then draw themselves.

use darkwire_tui::renderable::{Column, Renderable, StyledRows, Wrapped};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

/// Something two rows tall that writes its name on its first row.
struct Fixed {
    name: &'static str,
    height: u16,
    caret: Option<(u16, u16)>,
}

impl Fixed {
    fn new(name: &'static str, height: u16) -> Self {
        Self {
            name,
            height,
            caret: None,
        }
    }

    fn with_caret(mut self, offset: (u16, u16)) -> Self {
        self.caret = Some(offset);
        self
    }
}

impl Renderable for Fixed {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        for offset in 0..area.height.min(self.height) {
            let line = Line::from(format!("{}{offset}", self.name));
            ratatui::widgets::Widget::render(
                &line,
                Rect::new(area.x, area.y + offset, area.width, 1),
                buffer,
            );
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.height
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.caret.map(|(x, y)| (area.x + x, area.y + y))
    }
}

/// The rows of a buffer, trailing blanks trimmed.
fn rows(buffer: &Buffer) -> Vec<String> {
    let area = buffer.area;
    (0..area.height)
        .map(|row| {
            (0..area.width)
                .map(|column| buffer[(area.x + column, area.y + row)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn buffer(width: u16, height: u16) -> Buffer {
    Buffer::empty(Rect::new(0, 0, width, height))
}

#[test]
fn a_column_is_as_tall_as_its_children_together() {
    let column = Column::new()
        .with(Fixed::new("a", 2))
        .with(Fixed::new("b", 3));
    assert_eq!(column.desired_height(20), 5);
}

#[test]
fn a_column_stacks_its_children_top_to_bottom() {
    let column = Column::new()
        .with(Fixed::new("a", 2))
        .with(Fixed::new("b", 1));
    let mut buffer = buffer(10, 3);
    column.render(buffer.area, &mut buffer);

    assert_eq!(rows(&buffer), vec!["a0", "a1", "b0"]);
}

#[test]
fn a_child_past_the_bottom_of_the_area_is_not_drawn() {
    let column = Column::new()
        .with(Fixed::new("a", 2))
        .with(Fixed::new("b", 2));
    let mut buffer = buffer(10, 3);
    column.render(buffer.area, &mut buffer);

    // Room for both of "a" and only the first of "b".
    assert_eq!(rows(&buffer), vec!["a0", "a1", "b0"]);
}

#[test]
fn a_column_hands_up_the_caret_of_whichever_child_has_one() {
    let column = Column::new()
        .with(Fixed::new("a", 2))
        .with(Fixed::new("b", 2).with_caret((3, 1)));

    // "b" starts on row 2, so its own row 1 is row 3 of the area.
    assert_eq!(column.cursor_pos(Rect::new(0, 0, 10, 4)), Some((3, 3)));
}

#[test]
fn a_column_of_nothing_takes_no_rows() {
    let column = Column::new();
    assert_eq!(column.desired_height(20), 0);
    assert_eq!(column.cursor_pos(Rect::new(0, 0, 10, 4)), None);
    assert!(column.is_empty());
}

#[test]
fn nothing_takes_no_rows() {
    assert_eq!(Renderable::desired_height(&(), 20), 0);
    assert_eq!(None::<Fixed>.desired_height(20), 0);
    assert_eq!(Some(Fixed::new("a", 2)).desired_height(20), 2);
}

#[test]
fn rows_already_the_right_width_are_one_row_each() {
    let lines = vec![Line::from("one".to_owned()), Line::from("two".to_owned())];
    assert_eq!(lines.desired_height(20), 2);

    let mut buffer = buffer(10, 2);
    lines.render(buffer.area, &mut buffer);
    assert_eq!(rows(&buffer), vec!["one", "two"]);
}

#[test]
fn rows_past_the_bottom_of_the_area_are_dropped_rather_than_wrapping_round() {
    let lines = vec![
        Line::from("one".to_owned()),
        Line::from("two".to_owned()),
        Line::from("three".to_owned()),
    ];
    let mut buffer = buffer(10, 2);
    lines.render(buffer.area, &mut buffer);
    assert_eq!(rows(&buffer), vec!["one", "two"]);
}

#[test]
fn wrapped_rows_are_folded_at_whatever_width_they_are_asked_for() {
    let wrapped = Wrapped(vec![Line::from("one two three".to_owned())]);
    assert_eq!(wrapped.desired_height(40), 1);
    assert_eq!(wrapped.desired_height(7), 2);

    let mut buffer = buffer(7, 2);
    wrapped.render(buffer.area, &mut buffer);
    assert_eq!(rows(&buffer), vec!["one two", "three"]);
}

#[test]
fn a_line_is_one_row_however_wide_it_is() {
    let line = Line::from("a rather long line indeed".to_owned());
    assert_eq!(Renderable::desired_height(&line, 5), 1);
}

#[test]
fn a_boxed_widget_answers_the_way_the_widget_does() {
    let boxed: Box<dyn Renderable> = Box::new(Fixed::new("a", 3));
    assert_eq!(boxed.desired_height(20), 3);
}

#[test]
fn rows_a_component_produced_become_rows_a_frame_draws() {
    let styled = StyledRows::new(&["one".to_owned(), "two".to_owned()]);
    assert_eq!(styled.desired_height(20), 2);

    let mut buffer = buffer(10, 2);
    styled.render(buffer.area, &mut buffer);
    assert_eq!(rows(&buffer), vec!["one", "two"]);
}

#[test]
fn a_components_colour_survives_the_crossing() {
    let styled = StyledRows::new(&["\x1b[32mgreen\x1b[39m".to_owned()]);
    let mut buffer = buffer(10, 1);
    styled.render(buffer.area, &mut buffer);

    assert_eq!(buffer[(0, 0)].fg, ratatui::style::Color::Green);
    assert_eq!(rows(&buffer), vec!["green"]);
}

#[test]
fn a_components_caret_marker_becomes_a_caret_position() {
    let rows = vec![
        "prompt".to_owned(),
        format!("ab{}c", darkwire_tui::CURSOR_MARKER),
    ];
    let styled = StyledRows::new(&rows);

    assert_eq!(styled.cursor_pos(Rect::new(0, 4, 20, 2)), Some((2, 5)));
}

#[test]
fn a_caret_below_the_area_is_not_drawn_outside_it() {
    let rows = vec![
        "one".to_owned(),
        "two".to_owned(),
        format!("{}", darkwire_tui::CURSOR_MARKER),
    ];
    let styled = StyledRows::new(&rows);
    assert_eq!(styled.cursor_pos(Rect::new(0, 0, 20, 2)), None);
}

#[test]
fn rows_with_no_marker_own_no_caret() {
    let styled = StyledRows::new(&["plain".to_owned()]);
    assert_eq!(styled.cursor_pos(Rect::new(0, 0, 20, 1)), None);
}

#[test]
fn a_borrowed_widget_answers_the_way_the_widget_does() {
    let fixed = Fixed::new("a", 3).with_caret((1, 2));
    let borrowed: &dyn Renderable = &fixed;

    assert_eq!(borrowed.desired_height(20), 3);
    assert_eq!(borrowed.cursor_pos(Rect::new(0, 0, 10, 4)), Some((1, 2)));
    assert_eq!(
        borrowed.cursor_style(Rect::new(0, 0, 10, 4)),
        crossterm::cursor::SetCursorStyle::DefaultUserShape
    );
}

#[test]
fn a_boxed_widget_hands_up_its_caret_too() {
    let boxed: Box<dyn Renderable> = Box::new(Fixed::new("a", 2).with_caret((4, 1)));

    assert_eq!(boxed.cursor_pos(Rect::new(2, 3, 10, 4)), Some((6, 4)));
    assert_eq!(
        boxed.cursor_style(Rect::new(0, 0, 10, 4)),
        crossterm::cursor::SetCursorStyle::DefaultUserShape
    );
}

#[test]
fn nothing_owns_no_caret() {
    assert_eq!(None::<Fixed>.cursor_pos(Rect::new(0, 0, 10, 4)), None);
    assert_eq!(
        None::<Fixed>.cursor_style(Rect::new(0, 0, 10, 4)),
        crossterm::cursor::SetCursorStyle::DefaultUserShape
    );
    assert_eq!(
        Some(Fixed::new("a", 1)).cursor_style(Rect::new(0, 0, 10, 4)),
        crossterm::cursor::SetCursorStyle::DefaultUserShape
    );
    let mut buffer = buffer(4, 1);
    Renderable::render(&(), buffer.area, &mut buffer);
    None::<Fixed>.render(buffer.area, &mut buffer);
    assert_eq!(rows(&buffer), vec![""]);
}

#[test]
fn a_column_can_be_built_a_child_at_a_time() {
    let mut column = Column::new();
    assert!(column.is_empty());

    column.push(Fixed::new("a", 1));
    column.push(Fixed::new("b", 1));

    assert_eq!(column.len(), 2);
    assert!(!column.is_empty());
    assert_eq!(column.desired_height(20), 2);
}

#[test]
fn a_column_with_no_child_holding_the_caret_has_none() {
    let column = Column::new().with(Fixed::new("a", 2));
    assert_eq!(column.cursor_pos(Rect::new(0, 0, 10, 4)), None);
    assert_eq!(
        column.cursor_style(Rect::new(0, 0, 10, 4)),
        crossterm::cursor::SetCursorStyle::DefaultUserShape
    );
}

#[test]
fn a_line_drawn_on_its_own_lands_where_it_was_put() {
    let line = Line::from("here".to_owned());
    let mut buffer = buffer(10, 2);
    Renderable::render(&line, Rect::new(0, 1, 10, 1), &mut buffer);

    assert_eq!(rows(&buffer), vec!["", "here"]);
}

#[test]
fn rows_a_component_produced_can_be_taken_as_rows_again() {
    let styled = StyledRows::new(&["one".to_owned(), "two".to_owned()]);
    let lines = styled.into_lines();

    assert_eq!(lines.len(), 2);
}

#[test]
fn a_caret_past_the_right_edge_is_kept_inside_the_area() {
    let rows = vec![format!("{}{}", "x".repeat(20), darkwire_tui::CURSOR_MARKER)];
    let styled = StyledRows::new(&rows);

    assert_eq!(styled.cursor_pos(Rect::new(0, 0, 10, 1)), Some((9, 0)));
}

#[test]
fn a_wrapped_block_of_nothing_takes_no_rows() {
    let wrapped = Wrapped(Vec::new());
    assert_eq!(wrapped.desired_height(20), 0);
}
