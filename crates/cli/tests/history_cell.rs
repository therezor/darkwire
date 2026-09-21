//! What each kind of finished cell writes, and what it keeps back.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture that does not hold is a failing test either way"
)]

use darkwire::history_cell::{
    AssistantCell, DividerCell, FoldedCell, NoticeCell, SessionHeaderCell, TurnStatsCell, UserCell,
};
use darkwire_tui::{HistoryCell, PLAIN_THEME, styled_line};
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

fn row(text: &str) -> Line<'static> {
    styled_line(text)
}

#[test]
fn a_message_opens_an_exchange_with_a_blank_row_and_a_caret() {
    let cell = UserCell::new("what is this", &PLAIN_THEME);
    assert_eq!(texts(&cell.display_lines(40)), vec!["", "› what is this"]);
}

#[test]
fn an_answer_is_the_rows_it_became() {
    let cell = AssistantCell::new(vec![row("one"), row("two")]);
    assert_eq!(texts(&cell.display_lines(40)), vec!["one", "two"]);
    assert_eq!(cell.desired_height(40), 2);
}

#[test]
fn an_answer_that_said_nothing_writes_nothing() {
    let cell = AssistantCell::new(Vec::new());
    assert!(cell.is_empty());
    assert_eq!(cell.desired_height(40), 0);
}

#[test]
fn a_folded_cell_writes_its_summary_and_keeps_its_body() {
    let cell = FoldedCell::new(
        row("┄ reasoning 4s"),
        vec![row("first thought"), row("second thought")],
        false,
    );

    assert_eq!(texts(&cell.display_lines(40)), vec!["┄ reasoning 4s"]);
    assert_eq!(
        texts(&cell.transcript_lines(40)),
        vec!["┄ reasoning 4s", "first thought", "second thought"]
    );
    assert_eq!(cell.desired_height(40), 1);
}

#[test]
fn an_expanded_cell_writes_its_body_too() {
    let cell = FoldedCell::new(row("ran a command"), vec![row("output")], true);

    assert!(cell.is_expanded());
    assert_eq!(
        texts(&cell.display_lines(40)),
        vec!["ran a command", "output"]
    );
    assert_eq!(cell.desired_height(40), 2);
}

#[test]
fn a_notice_is_one_row() {
    let cell = NoticeCell::new("attached to a session");
    assert_eq!(
        texts(&cell.display_lines(40)),
        vec!["attached to a session"]
    );
}

#[test]
fn a_hidden_cost_writes_nothing_and_stays_in_the_transcript() {
    let cell = TurnStatsCell::new("1.2k tokens", false);

    assert!(cell.display_lines(40).is_empty());
    assert_eq!(cell.desired_height(40), 0);
    assert_eq!(texts(&cell.transcript_lines(40)), vec!["1.2k tokens"]);
}

#[test]
fn a_shown_cost_writes_its_row() {
    let cell = TurnStatsCell::new("1.2k tokens", true);
    assert_eq!(texts(&cell.display_lines(40)), vec!["1.2k tokens"]);
}

#[test]
fn a_header_becomes_one_row_per_line_of_it() {
    let cell = SessionHeaderCell::new("darkwire\na session\n");
    assert_eq!(
        texts(&cell.display_lines(40)),
        vec!["darkwire", "a session"]
    );
}

#[test]
fn a_header_of_nothing_writes_nothing() {
    let cell = SessionHeaderCell::new("");
    assert!(cell.display_lines(40).is_empty());
}

#[test]
fn a_divider_says_where_one_conversation_ended() {
    let cell = DividerCell::new(10, &PLAIN_THEME);
    let rows = texts(&cell.display_lines(40));

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], "");
    assert_eq!(rows[1].chars().count(), 10);
}

#[test]
fn a_cells_colour_reaches_the_rows_it_writes() {
    let cell = AssistantCell::new(vec![row("\x1b[32mgreen\x1b[39m")]);
    let rows = cell.display_lines(40);

    assert_eq!(
        rows[0].spans[0].style.fg,
        Some(ratatui::style::Color::Green)
    );
}

#[test]
fn a_notice_that_explains_itself_is_a_row_per_line_of_it() {
    // A warning arrives as one event and is a paragraph. A row holding a
    // newline is a row the terminal breaks wherever it likes, at column zero,
    // ignoring the width everything else was folded for.
    let cell = NoticeCell::new("⚠ no provider\n  run darkwire init\n  or pass --provider");

    assert_eq!(
        texts(&cell.display_lines(40)),
        vec![
            "⚠ no provider",
            "  run darkwire init",
            "  or pass --provider"
        ]
    );
}

#[test]
fn no_row_a_cell_writes_holds_a_newline() {
    let cells: Vec<Box<dyn HistoryCell>> = vec![
        Box::new(NoticeCell::new("one\ntwo\n")),
        Box::new(SessionHeaderCell::new("a\nb\n")),
        Box::new(UserCell::new("a question", &PLAIN_THEME)),
        Box::new(DividerCell::new(10, &PLAIN_THEME)),
    ];
    for cell in &cells {
        for line in cell.display_lines(40) {
            assert!(
                !texts(std::slice::from_ref(&line))[0].contains('\n'),
                "a row carries a newline: {line:?}"
            );
        }
    }
}
