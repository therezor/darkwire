//! Text arriving a piece at a time, and what settles out of it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture that does not hold is a failing test either way"
)]

use darkwire::stream::StreamController;
use ratatui::text::Line;

/// The text of each row, styles dropped.
fn texts(rows: &[Line<'static>]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn nothing_has_arrived_yet() {
    let stream = StreamController::new();
    assert!(stream.is_empty());
    assert_eq!(stream.settled(), 0);
}

#[test]
fn a_chunk_with_no_newline_stays_live() {
    let mut stream = StreamController::new();
    stream.push("half a sen", 0);

    assert_eq!(stream.settled(), 0, "an unfinished line settled");
    assert_eq!(texts(&stream.rows(40)), vec!["half a sen"]);
}

#[test]
fn a_newline_settles_the_line_before_it() {
    let mut stream = StreamController::new();
    stream.push("first line\nsecond", 0);

    assert_eq!(stream.settled(), 1);
    assert_eq!(texts(&stream.rows(40)), vec!["first line", "second"]);
}

#[test]
fn a_line_arriving_in_pieces_is_still_one_line() {
    let mut stream = StreamController::new();
    stream.push("one ", 0);
    stream.push("two ", 0);
    stream.push("three\n", 0);

    assert_eq!(stream.settled(), 1);
    assert_eq!(texts(&stream.rows(40)), vec!["one two three"]);
}

#[test]
fn ending_the_line_settles_what_was_open() {
    let mut stream = StreamController::new();
    stream.push("unfinished", 0);
    stream.end_line();

    assert_eq!(stream.settled(), 1);
}

#[test]
fn ending_a_line_that_is_not_open_settles_nothing() {
    let mut stream = StreamController::new();
    stream.push("done\n", 0);
    stream.end_line();
    stream.end_line();

    assert_eq!(stream.settled(), 1, "an empty line was invented");
}

#[test]
fn only_settled_lines_are_taken() {
    let mut stream = StreamController::new();
    stream.push("one\ntwo\nthree\nstill going", 0);

    let taken = stream.take_settled(0);
    assert_eq!(texts(&taken), vec!["one", "two", "three"]);
    assert_eq!(
        texts(&stream.rows(40)),
        vec!["still going"],
        "the live tail went out with the settled rows"
    );
}

#[test]
fn a_few_settled_rows_are_kept_back() {
    let mut stream = StreamController::new();
    stream.push("one\ntwo\nthree\nfour\n", 0);

    let taken = stream.take_settled(2);
    assert_eq!(texts(&taken), vec!["one", "two"]);
    assert_eq!(texts(&stream.rows(40)), vec!["three", "four"]);
}

#[test]
fn nothing_is_taken_when_there_is_nothing_to_spare() {
    let mut stream = StreamController::new();
    stream.push("one\ntwo\n", 0);

    assert!(stream.take_settled(5).is_empty());
    assert_eq!(stream.settled(), 2);
}

#[test]
fn taking_everything_closes_the_line_that_was_open() {
    let mut stream = StreamController::new();
    stream.push("finished\nunfinished", 0);

    let all = stream.take_all();
    assert_eq!(texts(&all), vec!["finished", "unfinished"]);
    assert!(stream.is_empty());
}

#[test]
fn an_indent_goes_on_every_line_of_a_chunk() {
    let mut stream = StreamController::new();
    stream.push("one\ntwo\n", 1);

    assert_eq!(texts(&stream.take_all()), vec!["  one", "  two"]);
}

#[test]
fn an_indent_does_not_push_a_chunk_that_began_mid_line() {
    let mut stream = StreamController::new();
    stream.push("start", 1);
    stream.push(" and the rest\n", 1);

    assert_eq!(texts(&stream.take_all()), vec!["  start and the rest"]);
}

#[test]
fn a_style_survives_becoming_a_row() {
    let mut stream = StreamController::new();
    stream.push("\x1b[32mgreen\x1b[39m\n", 0);

    let rows = stream.take_all();
    assert_eq!(texts(&rows), vec!["green"]);
    assert_eq!(
        rows[0].spans[0].style.fg,
        Some(ratatui::style::Color::Green)
    );
}

#[test]
fn a_chunk_that_ended_a_line_in_colour_still_counts_as_ended() {
    let mut stream = StreamController::new();
    // Dimmed text ends in a closing escape, so testing the raw string would
    // report "mid-line" for a chunk that plainly finished one, and the next
    // chunk would be run onto the end of it rather than indented.
    stream.push("\x1b[2mthinking\x1b[22m\n", 1);
    stream.push("next\n", 1);

    assert_eq!(texts(&stream.take_all()), vec!["  thinking", "  next"]);
}

#[test]
fn a_row_wider_than_the_window_is_folded_for_the_live_area() {
    let mut stream = StreamController::new();
    stream.push("one two three four\n", 0);

    assert_eq!(
        texts(&stream.rows(8)),
        vec!["one two", "three", "four"],
        "the live area would be drawn over its own history"
    );
}

#[test]
fn a_line_that_arrived_whole_needs_no_newline() {
    let mut stream = StreamController::new();
    stream.push_line("a notice");

    assert_eq!(stream.settled(), 1);
    assert_eq!(texts(&stream.take_all()), vec!["a notice"]);
}

#[test]
fn a_whole_line_closes_whatever_was_open_first() {
    let mut stream = StreamController::new();
    stream.push("half", 0);
    stream.push_line("a notice");

    assert_eq!(texts(&stream.take_all()), vec!["half", "a notice"]);
}

#[test]
fn an_empty_chunk_changes_nothing() {
    let mut stream = StreamController::new();
    stream.push("", 0);

    assert!(stream.is_empty());
}

#[test]
fn a_complete_chunk_with_newlines_in_it_is_a_row_per_line() {
    let mut stream = StreamController::new();
    stream.push_line("first\nsecond\nthird");

    assert_eq!(stream.settled(), 3);
    assert_eq!(texts(&stream.take_all()), vec!["first", "second", "third"]);
}

#[test]
fn no_row_a_stream_settles_holds_a_newline() {
    let mut stream = StreamController::new();
    stream.push_line("a paragraph\nwith two lines\n");
    stream.push("streamed\ntext\n", 0);

    for row in texts(&stream.take_all()) {
        assert!(!row.contains('\n'), "a row carries a newline: {row:?}");
    }
}

#[test]
fn a_tab_becomes_spaces_counted_from_where_its_line_began() {
    let mut stream = StreamController::new();
    stream.push("ab", 0);
    stream.push("\tc\n", 0);

    assert_eq!(texts(&stream.take_all()), vec!["ab  c"]);
}

#[test]
fn a_control_character_never_reaches_a_row() {
    let mut stream = StreamController::new();
    stream.push("ding\u{7}\r\n", 0);
    stream.push_line("col\tumn\u{1b}");

    assert_eq!(texts(&stream.take_all()), vec!["ding", "col umn"]);
}

#[test]
fn the_renderers_own_styles_survive_the_cleaning() {
    let mut stream = StreamController::new();
    stream.push("\u{1b}[2mdim\u{1b}[0m\n", 0);

    let rows = stream.take_all();
    assert_eq!(texts(&rows), vec!["dim"]);
    assert!(
        rows[0].spans.iter().any(|span| span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::DIM)),
        "the style was cleaned away: {rows:?}"
    );
}
