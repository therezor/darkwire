//! Rows going above the live area, into the terminal's own scrollback.

use darkwire_tui::insert_history::{InsertMode, insert_history_lines};
use darkwire_tui::terminal::Terminal;
use darkwire_tui::testkit::VT100Backend;
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::{Color, Modifier, Style};
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

fn plain(text: &str) -> Line<'static> {
    Line::from(text.to_owned())
}

/// Which row of the screen holds this text.
///
/// A row that is not there answers past the bottom of the screen, so the
/// assertion that wanted it fails naming the text rather than the index.
fn row_of(rows: &[String], text: &str) -> u16 {
    rows.iter()
        .position(|row| row == text)
        .and_then(|row| u16::try_from(row).ok())
        .unwrap_or(u16::MAX)
}

/// Draws a marker into every row of the live area, so a test can see it move.
#[allow(
    clippy::expect_used,
    reason = "a frame the backend refused is the failure the test is looking for"
)]
fn draw_live(terminal: &mut Terminal<VT100Backend>, text: &str) {
    let text = text.to_owned();
    terminal
        .draw(|frame| {
            let area = frame.area;
            for offset in 0..area.height {
                let line = Line::from(format!("{text}{offset}"));
                Widget::render(
                    &line,
                    Rect::new(area.x, area.y + offset, area.width, 1),
                    frame.buffer,
                );
            }
        })
        .expect("draw");
}

#[test]
fn rows_land_above_the_live_area() {
    let mut terminal = open(20, 8, 5, 3);
    draw_live(&mut terminal, "live");

    insert_history_lines(
        &mut terminal,
        &[plain("history")],
        InsertMode::ScrollRegion,
        0,
    )
    .expect("insert");

    let rows = terminal.backend().rows();
    assert!(
        rows[..5].iter().any(|row| row == "history"),
        "the row did not land above the live area: {rows:?}"
    );
}

#[test]
fn the_live_area_is_not_written_over() {
    let mut terminal = open(20, 8, 5, 3);
    draw_live(&mut terminal, "live");
    let before: Vec<String> = terminal.backend().rows()[5..].to_vec();

    insert_history_lines(
        &mut terminal,
        &[plain("history")],
        InsertMode::ScrollRegion,
        0,
    )
    .expect("insert");

    let area = terminal.viewport_area;
    let after: Vec<String> = terminal.backend().rows()[usize::from(area.y)..].to_vec();
    assert_eq!(after, before, "the live area's rows changed");
}

#[test]
fn a_row_wider_than_the_window_is_folded_rather_than_cut() {
    // This is the defect the old insert_before path had: a logical line wider
    // than the window lost its tail, permanently, in the scrollback.
    let mut terminal = open(10, 8, 5, 2);
    draw_live(&mut terminal, "live");

    insert_history_lines(
        &mut terminal,
        &[plain("one two three four")],
        InsertMode::ScrollRegion,
        0,
    )
    .expect("insert");

    let rows = terminal.backend().rows();
    let written: String = rows[..usize::from(terminal.viewport_area.y)].join(" ");
    for word in ["one", "two", "three", "four"] {
        assert!(
            written.contains(word),
            "{word:?} was lost when the row was written: {rows:?}"
        );
    }
}

#[test]
fn a_live_area_above_the_bottom_moves_down_to_make_room() {
    let mut terminal = open(20, 10, 2, 2);
    draw_live(&mut terminal, "live");
    let before = terminal.viewport_area.y;

    insert_history_lines(
        &mut terminal,
        &[plain("one"), plain("two")],
        InsertMode::ScrollRegion,
        0,
    )
    .expect("insert");

    assert!(
        terminal.viewport_area.y > before,
        "the live area stayed at {before} with room below it"
    );
}

#[test]
fn a_live_area_on_the_bottom_stays_where_it_is() {
    let mut terminal = open(20, 8, 6, 2);
    draw_live(&mut terminal, "live");

    insert_history_lines(&mut terminal, &[plain("one")], InsertMode::ScrollRegion, 0)
        .expect("insert");

    assert_eq!(terminal.viewport_area.y, 6);
}

#[test]
fn the_cursor_is_where_it_was() {
    let mut terminal = open(20, 8, 5, 2);
    draw_live(&mut terminal, "live");
    terminal
        .set_cursor_position(Position { x: 4, y: 6 })
        .expect("cursor");

    insert_history_lines(
        &mut terminal,
        &[plain("history")],
        InsertMode::ScrollRegion,
        0,
    )
    .expect("insert");

    assert_eq!(terminal.backend().cursor(), (6, 4));
}

#[test]
fn a_style_reaches_the_scrollback() {
    let mut terminal = open(20, 8, 5, 2);
    draw_live(&mut terminal, "live");

    let line = Line::from(vec![Span::styled(
        "green",
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )]);
    insert_history_lines(&mut terminal, &[line], InsertMode::ScrollRegion, 0).expect("insert");

    let rows = terminal.backend().rows();
    let row = row_of(&rows, "green");
    let cell = terminal.backend().cell(0, row).expect("a cell");
    assert_eq!(cell.fgcolor(), vt100::Color::Idx(2));
    assert!(cell.bold(), "the row lost its weight on the way out");
}

#[test]
fn a_style_does_not_leak_onto_the_row_after_it() {
    let mut terminal = open(20, 10, 6, 2);
    draw_live(&mut terminal, "live");

    let coloured = Line::from(vec![Span::styled("red", Style::default().fg(Color::Red))]);
    insert_history_lines(
        &mut terminal,
        &[coloured, plain("plain")],
        InsertMode::ScrollRegion,
        0,
    )
    .expect("insert");

    let rows = terminal.backend().rows();
    let row = row_of(&rows, "plain");
    let cell = terminal.backend().cell(0, row).expect("a cell");
    assert_eq!(cell.fgcolor(), vt100::Color::Default);
}

#[test]
fn a_live_area_at_the_top_of_the_screen_still_gets_its_rows_out() {
    // A scroll region needs two rows above the live area. This one has none,
    // so the repaint path has to take over rather than writing nothing.
    let mut terminal = open(20, 6, 0, 2);
    draw_live(&mut terminal, "live");

    insert_history_lines(
        &mut terminal,
        &[plain("history")],
        InsertMode::ScrollRegion,
        0,
    )
    .expect("insert");

    let rows = terminal.backend().rows();
    assert!(
        rows.iter().any(|row| row == "history"),
        "nothing was written: {rows:?}"
    );
}

#[test]
fn the_repaint_path_writes_the_rows_and_moves_the_live_area_down() {
    let mut terminal = open(20, 10, 3, 2);
    draw_live(&mut terminal, "live");

    insert_history_lines(
        &mut terminal,
        &[plain("one"), plain("two")],
        InsertMode::Repaint,
        0,
    )
    .expect("insert");

    let rows = terminal.backend().rows();
    assert_eq!(rows[3], "one");
    assert_eq!(rows[4], "two");
    assert_eq!(terminal.viewport_area.y, 5);
}

#[test]
fn nothing_is_written_for_no_rows() {
    let mut terminal = open(20, 8, 5, 2);
    draw_live(&mut terminal, "live");
    let before = terminal.backend().rows();

    insert_history_lines(&mut terminal, &[], InsertMode::ScrollRegion, 0).expect("insert");

    assert_eq!(terminal.backend().rows(), before);
}

#[test]
fn several_rows_arrive_in_the_order_they_were_written() {
    let mut terminal = open(20, 12, 8, 2);
    draw_live(&mut terminal, "live");

    insert_history_lines(
        &mut terminal,
        &[plain("first"), plain("second"), plain("third")],
        InsertMode::ScrollRegion,
        0,
    )
    .expect("insert");

    let rows = terminal.backend().rows();
    assert!(row_of(&rows, "first") < row_of(&rows, "second"));
    assert!(row_of(&rows, "second") < row_of(&rows, "third"));
}
