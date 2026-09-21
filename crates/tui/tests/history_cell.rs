//! A finished cell, and the two things it can be asked for.

use darkwire_tui::history_cell::{HistoryCell, PlainCell};
use ratatui::text::Line;

/// A cell that shows one row and holds three.
struct Folded;

impl HistoryCell for Folded {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![Line::from("summary".to_owned())]
    }

    fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![
            Line::from("summary".to_owned()),
            Line::from("body one".to_owned()),
            Line::from("body two".to_owned()),
        ]
    }
}

fn texts(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn a_cell_that_hides_nothing_says_the_same_thing_twice() {
    let cell = PlainCell::new(vec![
        Line::from("one".to_owned()),
        Line::from("two".to_owned()),
    ]);
    assert_eq!(texts(&cell.display_lines(20)), vec!["one", "two"]);
    assert_eq!(texts(&cell.transcript_lines(20)), vec!["one", "two"]);
}

#[test]
fn the_height_of_a_cell_is_the_rows_it_shows() {
    let cell = PlainCell::new(vec![
        Line::from("one".to_owned()),
        Line::from("two".to_owned()),
    ]);
    assert_eq!(cell.desired_height(20), 2);
}

#[test]
fn a_folded_cell_shows_less_than_it_holds() {
    assert_eq!(texts(&Folded.display_lines(20)), vec!["summary"]);
    assert_eq!(
        texts(&Folded.transcript_lines(20)),
        vec!["summary", "body one", "body two"]
    );
}

#[test]
fn a_folded_cell_is_as_tall_as_what_it_shows() {
    assert_eq!(Folded.desired_height(20), 1);
}

#[test]
fn a_cell_of_nothing_takes_no_rows() {
    let cell = PlainCell::new(Vec::new());
    assert_eq!(cell.desired_height(20), 0);
    assert!(cell.display_lines(20).is_empty());
}

#[test]
fn a_boxed_cell_answers_the_way_the_cell_does() {
    let boxed: Box<dyn HistoryCell> = Box::new(Folded);
    assert_eq!(boxed.desired_height(20), 1);
    assert_eq!(texts(&boxed.transcript_lines(20)).len(), 3);
}
