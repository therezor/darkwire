//! The live area, asserted against what a terminal emulator would be showing.

use std::io::Write;

use darkwire_tui::terminal::Terminal;
use darkwire_tui::testkit::VT100Backend;
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

/// A terminal `width` by `height` with its live area at `top`, `rows` tall.
fn open(width: u16, height: u16, top: u16, rows: u16) -> Terminal<VT100Backend> {
    let backend = VT100Backend::new(width, height);
    let mut terminal =
        Terminal::anchored(backend, Size { width, height }, Position { x: 0, y: top });
    terminal.set_viewport_area(Rect::new(0, top, width, rows));
    terminal
}

/// Draws these rows into the live area, one per row.
#[allow(
    clippy::expect_used,
    reason = "a frame the backend refused is the failure the test is looking for"
)]
fn draw_rows(terminal: &mut Terminal<VT100Backend>, rows: &[&str]) {
    let rows: Vec<Line<'static>> = rows
        .iter()
        .map(|row| Line::from((*row).to_owned()))
        .collect();
    terminal
        .draw(|frame| {
            let area = frame.area;
            for (offset, line) in rows.iter().enumerate() {
                let Ok(offset) = u16::try_from(offset) else {
                    break;
                };
                if offset >= area.height {
                    break;
                }
                Widget::render(
                    line,
                    Rect::new(area.x, area.y + offset, area.width, 1),
                    frame.buffer,
                );
            }
        })
        .expect("draw");
}

#[test]
fn a_frame_lands_where_the_live_area_is() {
    let mut terminal = open(20, 6, 3, 3);
    draw_rows(&mut terminal, &["first", "second", "third"]);

    let screen = terminal.backend().rows();
    assert_eq!(screen[0], "");
    assert_eq!(screen[3], "first");
    assert_eq!(screen[4], "second");
    assert_eq!(screen[5], "third");
}

#[test]
fn nothing_is_written_above_the_live_area() {
    let mut terminal = open(20, 6, 3, 3);
    // Something the shell left behind.
    write!(terminal.backend_mut(), "\x1b[H$ ls").expect("prefill");
    terminal
        .set_cursor_position(Position { x: 0, y: 3 })
        .expect("cursor");

    draw_rows(&mut terminal, &["a frame"]);

    assert_eq!(terminal.backend().row(0), "$ ls");
}

#[test]
fn a_row_that_got_shorter_does_not_keep_its_tail() {
    let mut terminal = open(20, 4, 1, 2);
    draw_rows(&mut terminal, &["a long first row", "second"]);
    assert_eq!(terminal.backend().row(1), "a long first row");

    draw_rows(&mut terminal, &["short", "second"]);
    assert_eq!(
        terminal.backend().row(1),
        "short",
        "the tail of the longer row survived"
    );
}

#[test]
fn an_unchanged_frame_writes_nothing() {
    let mut terminal = open(20, 4, 1, 2);
    draw_rows(&mut terminal, &["one", "two"]);
    let before = terminal.backend().rows();

    draw_rows(&mut terminal, &["one", "two"]);
    assert_eq!(terminal.backend().rows(), before);
}

#[test]
fn clearing_after_a_position_leaves_what_is_above_it() {
    let mut terminal = open(20, 5, 2, 3);
    draw_rows(&mut terminal, &["one", "two", "three"]);
    write!(terminal.backend_mut(), "\x1b[H kept").expect("prefill");

    terminal
        .clear_after(Position { x: 0, y: 2 })
        .expect("clear");

    assert_eq!(terminal.backend().row(0), " kept");
    assert_eq!(terminal.backend().row(2), "");
    assert_eq!(terminal.backend().row(3), "");
}

#[test]
fn a_frame_after_an_invalidate_writes_every_cell_again() {
    let mut terminal = open(20, 4, 1, 2);
    draw_rows(&mut terminal, &["one", "two"]);

    // Damage nothing announced: another program wrote over the live area.
    write!(terminal.backend_mut(), "\x1b[2;1Hnoise").expect("damage");
    assert_eq!(terminal.backend().row(1), "noise");

    terminal.invalidate();
    draw_rows(&mut terminal, &["one", "two"]);
    assert_eq!(terminal.backend().row(1), "one");
}

#[test]
fn a_frame_that_placed_the_caret_shows_it_there() {
    let mut terminal = open(20, 4, 1, 2);
    terminal
        .draw(|frame| {
            frame.set_cursor_position(Position { x: 5, y: 2 });
        })
        .expect("draw");

    assert!(terminal.backend().cursor_visible());
    assert_eq!(terminal.backend().cursor(), (2, 5));
}

#[test]
fn a_frame_that_placed_no_caret_hides_it() {
    let mut terminal = open(20, 4, 1, 2);
    draw_rows(&mut terminal, &["nothing being typed"]);
    assert!(!terminal.backend().cursor_visible());
}

#[test]
fn a_style_a_frame_drew_reaches_the_screen() {
    let mut terminal = open(20, 4, 1, 2);
    terminal
        .draw(|frame| {
            let line = Line::from(vec![Span::styled("red", Style::default().fg(Color::Red))]);
            Widget::render(&line, Rect::new(0, 1, 20, 1), frame.buffer);
        })
        .expect("draw");

    let cell = terminal.backend().cell(0, 1).expect("a cell at 0,1");
    assert_eq!(cell.contents(), "r");
    assert_eq!(cell.fgcolor(), vt100::Color::Idx(1));
}

#[test]
fn moving_the_live_area_moves_what_the_next_frame_draws() {
    let mut terminal = open(20, 6, 4, 2);
    draw_rows(&mut terminal, &["low"]);
    assert_eq!(terminal.backend().row(4), "low");

    terminal.set_viewport_area(Rect::new(0, 1, 20, 2));
    draw_rows(&mut terminal, &["high"]);
    assert_eq!(terminal.backend().row(1), "high");
}

#[test]
fn the_cursor_position_is_remembered_across_a_draw() {
    let mut terminal = open(20, 4, 1, 2);
    terminal
        .draw(|frame| {
            frame.set_cursor_position(Position { x: 3, y: 1 });
        })
        .expect("draw");
    assert_eq!(terminal.last_known_cursor_pos, Position { x: 3, y: 1 });
}

#[test]
fn opening_asks_the_terminal_nothing_it_has_to_answer() {
    // Asking where the cursor is (`ESC[6n`) is a read of stdin waiting for a
    // reply a terminal need not send. What that looks like when it does not
    // is a prompt showing nothing at all until the first key is pressed.
    let backend = VT100Backend::new(20, 8);
    let mut terminal = Terminal::new(backend).expect("open");

    assert_eq!(
        terminal.backend().rows().join(""),
        "",
        "something was written before the first frame"
    );
    // An empty area at the foot of the screen: the first draw makes room for
    // itself by scrolling what is above it.
    assert_eq!(terminal.viewport_area.y, 8);
    assert_eq!(terminal.viewport_area.height, 0);
    let _ = &mut terminal;
}
