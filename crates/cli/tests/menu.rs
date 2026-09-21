//! Whether a terminal can have a menu, and the seam a menu is opened through.

// `allow-unwrap-in-tests` covers a `#[test]` body; the fixture helpers beside
// them are ordinary functions, and a helper that returned `Result` would make
// every case carry a `?` for a temporary directory that cannot fail in a way
// the case could act on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture helpers in an integration test"
)]

use darkwire::i18n::Env;
use darkwire::menu::{MenuAvailable, MenuRequest, MenuRow, menu_available};
use darkwire_tui::SelectLabels;

/// Whether stdin is a terminal, as the predicate is told.
fn input(is_tty: bool) -> bool {
    is_tty
}

/// A stdout of a stated size, as the predicate is told.
fn output(is_tty: bool, columns: u16) -> (bool, Option<u16>) {
    (is_tty, Some(columns))
}

fn labels() -> SelectLabels {
    SelectLabels {
        title: "t".to_owned(),
        empty: "e".to_owned(),
        footer: "f".to_owned(),
        filter_prefix: None,
    }
}

fn available(json: bool, env: &Env, stdin_tty: bool, output: (bool, Option<u16>)) -> bool {
    let (stdout_tty, columns) = output;
    menu_available(&MenuAvailable {
        stdin_tty,
        stdout_tty,
        columns,
        json,
        env,
    })
}

#[test]
fn says_yes_for_a_terminal_on_both_ends() {
    assert!(available(
        false,
        &Env::empty(),
        input(true),
        output(true, 40)
    ));
}

#[test]
fn says_no_when_stdin_is_a_pipe() {
    assert!(!available(
        false,
        &Env::empty(),
        input(false),
        output(true, 40)
    ));
}

#[test]
fn says_no_when_stdout_is_a_pipe() {
    assert!(!available(
        false,
        &Env::empty(),
        input(true),
        output(false, 40)
    ));
}

#[test]
fn says_no_under_json_whose_stdout_carries_one_event_per_line_and_nothing_else() {
    assert!(!available(
        true,
        &Env::empty(),
        input(true),
        output(true, 40)
    ));
}

#[test]
fn says_no_on_a_dumb_terminal_which_prints_escape_sequences_as_text() {
    // Emacs' `M-x shell` is the case that actually happens.
    let env: Env = [("TERM", "dumb")].into_iter().collect();
    assert!(!available(false, &env, input(true), output(true, 40)));
}

#[test]
fn says_yes_for_a_terminal_that_reports_no_size_which_a_recorded_pty_does() {
    // Zero is not absent, so a naive fallback refused to draw a menu on a
    // terminal that was perfectly capable of showing one.
    assert!(available(
        false,
        &Env::empty(),
        input(true),
        output(true, 0)
    ));
}

#[test]
fn says_no_in_a_window_too_narrow_to_hold_a_label_beside_a_cursor_marker() {
    assert!(!available(
        false,
        &Env::empty(),
        input(true),
        output(true, 8)
    ));
}

#[test]
fn numbers_every_row_by_its_own_position_so_an_answer_names_one() {
    // The value is the index, stated once here rather than at every frame that
    // draws a menu.
    let request = MenuRequest::new(
        vec![
            MenuRow::new("Default").with_hint("qwen3"),
            MenuRow::new("Research").with_keywords("papers").disabled(),
        ],
        labels(),
    );
    let items = request.select_items();

    assert_eq!(items.len(), 2);
    assert_eq!(items[0].value, 0);
    assert_eq!(items[0].hint.as_deref(), Some("qwen3"));
    assert_eq!(items[1].value, 1);
    assert_eq!(items[1].keywords.as_deref(), Some("papers"));
    assert!(items[1].disabled);
}

#[test]
fn opens_on_the_row_the_caller_named() {
    let request = MenuRequest::new(vec![MenuRow::new("a"), MenuRow::new("b")], labels()).at(1);
    assert_eq!(request.index, Some(1));
}
