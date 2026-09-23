//! The key map over a list: chosen, cancelled, or still open.

use crate::common;

use darkwire_tui::{
    CHROME_ROWS, CURSOR_MARKER, Component, DEFAULT_MAX_ROWS, PLAIN_THEME, Select, SelectAction,
    SelectItem, SelectLabels, SelectOptions, SelectOutcome, theme_for, visible_width,
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
const CTRL_X: &str = "\x18";
const CTRL_R: &str = "\x12";

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
        actions: Vec::new(),
    })
}

fn action(chord: char, label: &str) -> SelectAction {
    SelectAction {
        chord,
        label: label.to_owned(),
    }
}

/// The same menu, with verbs on its rows.
fn menu_with(actions: Vec<SelectAction>) -> Select<String> {
    Select::new(SelectOptions {
        items: items(),
        labels: labels(),
        theme: Some(PLAIN_THEME),
        index: None,
        max_rows: None,
        actions,
    })
}

fn menu() -> Select<String> {
    menu_at(None)
}

/// Feeds bytes the way the real key loop does, and returns the last outcome.
fn press(subject: &mut Select<String>, sequences: &[&str]) -> SelectOutcome<String> {
    let mut outcome = subject.handle_key(&common::key(CTRL_U));
    for bytes in sequences {
        outcome = subject.handle_key(&common::key(bytes));
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
    assert_eq!(subject.handle_key(&common::key(ENTER)), SelectOutcome::Open);
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
    let alt = common::key(&format!("{ESC}x"));
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
    assert!(subject.prompt().contains("re"));
    press(&mut subject, &["r", "e", "🚀", BACKSPACE]);
    assert_eq!(subject.list().filter(), "re");
    press(&mut subject, &["r", "e", CTRL_U]);
    assert_eq!(subject.list().filter(), "");
}

#[test]
fn asks_its_question_and_shows_the_filter_on_one_row() {
    let mut subject = menu();
    press(&mut subject, &["r", "e"]);
    let prompt = subject.prompt();
    assert!(prompt.starts_with("Pick an agent re"), "{prompt:?}");
    // The caret sits after the filter, so the row is typed into where it reads
    // as being typed into.
    assert!(prompt.ends_with(CURSOR_MARKER), "{prompt:?}");
    // And nothing the menu renders repeats it.
    assert!(!subject.render(40).join("\n").contains("Pick an agent"));
}

#[test]
fn takes_another_separator_between_the_question_and_the_filter() {
    let mut subject = Select::new(SelectOptions {
        items: items(),
        labels: SelectLabels {
            filter_prefix: Some(" ? ".to_owned()),
            ..labels()
        },
        theme: None,
        index: None,
        max_rows: Some(2),
        actions: Vec::new(),
    });
    press(&mut subject, &["x"]);
    assert!(subject.prompt().starts_with("Pick an agent ? x"));
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
        actions: Vec::new(),
    });
    let rows = subject.render(40);
    assert!(subject.prompt().starts_with("\x1b[1mPick an agent"));
    assert!(rows.last().unwrap().contains("move choose cancel"));
    // One row of chrome: the footer. The question and the filter are the
    // host's prompt row, and the scroll counter shares the footer.
    assert_eq!(CHROME_ROWS, 1);
    assert_eq!(DEFAULT_MAX_ROWS, 10);
}

#[test]
fn counts_the_rows_it_is_not_showing_in_its_footer() {
    let mut subject = menu();
    subject.set_rows(1);
    let rows = subject.render(40);
    let footer = rows.last().unwrap();
    assert!(footer.contains("(1/3)"), "{footer:?}");
    assert!(footer.contains("move choose cancel"), "{footer:?}");

    // Nothing off screen, nothing to count.
    subject.set_rows(9);
    assert!(!subject.render(40).join("\n").contains('/'));
}

// Actions

#[test]
fn a_chord_fires_its_verb_on_the_row_under_the_cursor() {
    let mut subject = menu_with(vec![action('x', "delete"), action('r', "rename")]);

    assert_eq!(
        press(&mut subject, &[DOWN, CTRL_X]),
        SelectOutcome::Acted {
            action: 0,
            value: "research".to_owned(),
        }
    );
    // `press` clears the filter first, which puts the cursor back on the top
    // row, so this is the first one and not the one above.
    assert_eq!(
        press(&mut subject, &[CTRL_R]),
        SelectOutcome::Acted {
            action: 1,
            value: "default".to_owned(),
        }
    );
}

#[test]
fn a_chord_nothing_bound_is_still_nothing() {
    // The menu is left open rather than answering, which is what every other
    // key it does not know does.
    assert_eq!(
        press(&mut menu_with(Vec::new()), &[CTRL_X]),
        SelectOutcome::Open
    );
    assert_eq!(
        press(&mut menu_with(vec![action('r', "rename")]), &[CTRL_X]),
        SelectOutcome::Open
    );
}

#[test]
fn a_verb_never_reaches_the_filter() {
    // The reason verbs are chords and not letters. A menu of tasks with a bare
    // `d` for delete is a menu nobody can type `d` into.
    let mut subject = menu_with(vec![action('x', "delete")]);
    let outcome = subject.handle_key(&common::key(CTRL_X));

    assert!(matches!(outcome, SelectOutcome::Acted { .. }));
    assert_eq!(subject.list().filter(), "");
}

#[test]
fn a_verb_on_a_disabled_row_does_nothing() {
    // Consistent with Enter: the row is on screen and refusing, so a key that
    // did nothing is the honest answer.
    // Filtered down to it, because End lands on the last row the cursor may
    // rest on and that is never a disabled one.
    let mut subject = menu_with(vec![action('x', "delete")]);

    assert_eq!(
        press(&mut subject, &["r", "e", "t", "i", "r", CTRL_X]),
        SelectOutcome::Open
    );
}

#[test]
fn a_verb_on_a_filter_that_matches_nothing_does_nothing() {
    let mut subject = menu_with(vec![action('x', "delete")]);

    assert_eq!(press(&mut subject, &["z", CTRL_X]), SelectOutcome::Open);
}

#[test]
fn a_menu_cannot_take_a_chord_the_list_already_owns() {
    // `ctrl-n` moves. A menu that bound it to a verb would lose the movement
    // key and gain nothing, so the verb is dropped instead.
    let mut subject = menu_with(vec![action('n', "new"), action('x', "delete")]);

    assert_eq!(press(&mut subject, &[CTRL_N, ENTER]), chosen("research"));
    assert_eq!(
        press(&mut subject, &[CTRL_X]),
        SelectOutcome::Acted {
            action: 0,
            value: "default".to_owned(),
        }
    );
}

#[test]
fn the_footer_names_every_verb() {
    let mut subject = menu_with(vec![action('x', "delete"), action('r', "rename")]);
    let rows = Component::render(&mut subject, 80);
    let footer = rows.last().expect("a footer");

    assert!(footer.contains("^x delete"), "{footer:?}");
    assert!(footer.contains("^r rename"), "{footer:?}");
}

#[test]
fn a_paste_goes_into_the_filter_as_one_line() {
    let mut subject = menu_at(None);
    subject.paste("rese\r\n");

    assert_eq!(subject.list().filter(), "rese");
    assert_eq!(
        subject.handle_key(&common::key(ENTER)),
        SelectOutcome::Chosen("research".to_owned())
    );

    let mut subject = menu_at(None);
    subject.paste("two\nwords");
    assert_eq!(subject.list().filter(), "two words");
}
