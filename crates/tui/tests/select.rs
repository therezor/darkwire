//! The key map over a list: chosen, cancelled, or still open.

use darkwire_tui::{
    CHROME_ROWS, Component, DEFAULT_MAX_ROWS, PLAIN_THEME, Select, SelectItem, SelectLabels,
    SelectOptions, SelectOutcome, parse_key, theme_for, visible_width,
};

const ESC: &str = "\x1b";
const DOWN: &str = "\x1b[B";
const UP: &str = "\x1b[A";
const ENTER: &str = "\r";
const CTRL_C: &str = "\x03";
const CTRL_D: &str = "\x04";
const CTRL_U: &str = "\x15";
const CTRL_N: &str = "\x0e";
const CTRL_P: &str = "\x10";
const TAB: &str = "\t";
const SHIFT_TAB: &str = "\x1b[Z";
const PAGE_UP: &str = "\x1b[5~";
const PAGE_DOWN: &str = "\x1b[6~";
const HOME: &str = "\x1b[H";
const END: &str = "\x1b[F";
const BACKSPACE: &str = "\x7f";

fn labels() -> SelectLabels {
    SelectLabels {
        title: "Pick an agent".to_owned(),
        empty: "nothing matches".to_owned(),
        footer: "move choose cancel".to_owned(),
        filter_prefix: None,
    }
}

fn items() -> Vec<SelectItem<String>> {
    vec![
        SelectItem::new("default".to_owned(), "Default").with_hint("qwen3"),
        SelectItem::new("research".to_owned(), "Research").with_hint("sonnet"),
        SelectItem::new("gone".to_owned(), "Retired").disabled(),
    ]
}

fn menu_at(index: Option<usize>) -> Select<String> {
    Select::new(SelectOptions {
        items: items(),
        labels: labels(),
        theme: Some(PLAIN_THEME),
        index,
        max_rows: None,
    })
}

fn menu() -> Select<String> {
    menu_at(None)
}

/// Feeds bytes the way the real key loop does, and returns the last outcome.
#[allow(
    clippy::unwrap_used,
    reason = "a fixture that does not decode is a failing test"
)]
fn press(subject: &mut Select<String>, sequences: &[&str]) -> SelectOutcome<String> {
    let mut outcome = subject.handle_key(&parse_key(CTRL_U).unwrap());
    for bytes in sequences {
        outcome = subject.handle_key(&parse_key(bytes).unwrap());
    }
    outcome
}

fn chosen(value: &str) -> SelectOutcome<String> {
    SelectOutcome::Chosen(value.to_owned())
}

#[test]
fn answers_the_row_the_cursor_is_on() {
    assert_eq!(press(&mut menu(), &[DOWN, ENTER]), chosen("research"));
    assert_eq!(press(&mut menu(), &[ENTER]), chosen("default"));
}

#[test]
fn answers_nothing_on_escape() {
    assert_eq!(press(&mut menu(), &[ESC]), SelectOutcome::Cancelled);
}

#[test]
fn answers_nothing_on_ctrl_c_and_ctrl_d_which_raw_mode_delivers_as_bytes() {
    // The terminal stops turning Ctrl-C into SIGINT, so a menu that did not
    // read `0x03` itself would be a menu Ctrl-C could not close.
    assert_eq!(press(&mut menu(), &[CTRL_C]), SelectOutcome::Cancelled);
    assert_eq!(press(&mut menu(), &[CTRL_D]), SelectOutcome::Cancelled);
}

#[test]
fn stays_open_when_enter_lands_on_a_row_that_cannot_be_chosen() {
    // A disabled row is on screen, so the key doing nothing is the honest
    // answer — unlike a filter matching nothing, where there is no row at all.
    // Reached by starting on it: the arrow keys step over disabled rows.
    let mut subject = menu_at(Some(2));
    assert_eq!(
        subject.handle_key(&parse_key(ENTER).unwrap()),
        SelectOutcome::Open
    );
}

#[test]
fn gives_up_when_the_filter_matches_nothing_and_enter_is_pressed() {
    assert_eq!(
        press(&mut menu(), &["z", "q", ENTER]),
        SelectOutcome::Cancelled
    );
}

#[test]
fn every_movement_key_moves() {
    // Ctrl-P/Ctrl-N are plain control bytes, the only movement keys that
    // survive a terminal whose cursor sequences arrive in a form nothing
    // recognises.
    assert_eq!(press(&mut menu(), &[CTRL_N, ENTER]), chosen("research"));
    assert_eq!(
        press(&mut menu(), &[CTRL_N, CTRL_P, ENTER]),
        chosen("default")
    );
    assert_eq!(press(&mut menu(), &[TAB, ENTER]), chosen("research"));
    assert_eq!(
        press(&mut menu(), &[TAB, SHIFT_TAB, ENTER]),
        chosen("default")
    );
    assert_eq!(press(&mut menu(), &[DOWN, UP, ENTER]), chosen("default"));
    assert_eq!(press(&mut menu(), &[END, ENTER]), chosen("research"));
    assert_eq!(press(&mut menu(), &[END, HOME, ENTER]), chosen("default"));
    // A page is the visible row count, ten by default, and the list wraps: ten
    // rows down from the top of three lands on the second, and ten up lands on
    // the disabled third and steps back to the second.
    assert_eq!(press(&mut menu(), &[PAGE_DOWN, ENTER]), chosen("research"));
    assert_eq!(press(&mut menu(), &[PAGE_UP, ENTER]), chosen("research"));
}

#[test]
fn a_key_it_does_not_bind_leaves_the_menu_open_and_unchanged() {
    let mut subject = menu();
    let alt = parse_key(&format!("{ESC}x")).unwrap();
    assert_eq!(subject.handle_key(&alt), SelectOutcome::Open);
    assert_eq!(subject.list().filter(), "");
    assert_eq!(press(&mut subject, &["\x1b[3~", ENTER]), chosen("default"));
}

#[test]
fn never_draws_a_row_wider_than_it_was_given() {
    for width in [8, 24, 40] {
        for row in menu().render(width) {
            assert!(
                visible_width(&row) <= width,
                "{row:?} is wider than {width}"
            );
        }
    }
}

#[test]
fn says_so_when_the_filter_matches_nothing() {
    let mut subject = menu();
    press(&mut subject, &["z", "q"]);
    assert!(subject.render(40).join("\n").contains("nothing matches"));
}

#[test]
fn shows_the_filter_as_it_is_typed_and_backspace_takes_a_cluster_off_it() {
    let mut subject = menu();
    press(&mut subject, &["r", "e"]);
    assert!(subject.render(40)[1].contains("/re"));
    press(&mut subject, &["r", "e", "🚀", BACKSPACE]);
    assert_eq!(subject.list().filter(), "re");
    press(&mut subject, &["r", "e", CTRL_U]);
    assert_eq!(subject.list().filter(), "");
}

#[test]
fn takes_another_filter_prefix() {
    let mut subject = Select::new(SelectOptions {
        items: items(),
        labels: SelectLabels {
            filter_prefix: Some("? ".to_owned()),
            ..labels()
        },
        theme: None,
        index: None,
        max_rows: Some(2),
    });
    press(&mut subject, &["x"]);
    assert_eq!(subject.render(40)[1], "? x");
}

#[test]
fn clamps_the_list_to_the_rows_it_is_told_it_can_have() {
    let mut roomy = menu();
    roomy.set_rows(3);
    let mut cramped = menu();
    cramped.set_rows(1);

    assert!(cramped.render(40).len() < roomy.render(40).len());
}

#[test]
fn draws_the_chrome_in_the_theme() {
    let mut subject = Select::new(SelectOptions {
        items: items(),
        labels: labels(),
        theme: Some(theme_for(Some(true))),
        index: None,
        max_rows: None,
    });
    let rows = subject.render(40);
    assert!(rows[0].starts_with("\x1b[1mPick an agent"));
    assert!(rows[1].starts_with("\x1b[90m/"));
    assert!(rows.last().unwrap().contains("move choose cancel"));
    // Five rows of chrome is what the caller budgets for around the list.
    assert_eq!(CHROME_ROWS, 5);
    assert_eq!(DEFAULT_MAX_ROWS, 10);
}
