//! Shell bindings, grapheme-wise movement, history, and folding at the real window edge.

use darkwire_tui::{
    CURSOR_MARKER, Component, Editor, EditorOutcome, Key, KeyName, PLAIN_THEME, parse_key,
    theme_for,
};

const ESC: &str = "\x1b";
const LEFT: &str = "\x1b[D";
const RIGHT: &str = "\x1b[C";
const UP: &str = "\x1b[A";
const DOWN: &str = "\x1b[B";
const BACKSPACE: &str = "\x7f";

fn editor() -> Editor {
    Editor::new(&PLAIN_THEME)
}

/// A key the decoder never produces from bytes, for the modified forms.
fn key(name: KeyName, meta: bool) -> Key {
    Key {
        name,
        character: String::new(),
        ctrl: false,
        shift: false,
        meta,
        sequence: String::new(),
    }
}

/// The bytes a terminal would send, decoded the way the real loop decodes them.
#[allow(
    clippy::unwrap_used,
    reason = "a fixture that does not decode is a failing test"
)]
fn press(subject: &mut Editor, bytes: &str) -> EditorOutcome {
    subject.handle_key(&parse_key(bytes).unwrap())
}

fn type_text(subject: &mut Editor, text: &str) {
    for ch in text.chars() {
        press(subject, &ch.to_string());
    }
}

/// Where the caret is, in bytes, on the row it sits on.
#[allow(
    clippy::expect_used,
    reason = "an editor that drew no caret is a failing test"
)]
fn caret(subject: &mut Editor) -> usize {
    subject
        .render(40)
        .iter()
        .find_map(|row| row.find(CURSOR_MARKER))
        .expect("the editor drew no caret")
}

#[test]
fn shows_what_was_typed_with_the_caret_after_it() {
    let mut subject = editor();
    type_text(&mut subject, "hello");

    assert_eq!(subject.text(), "hello");
    assert_eq!(caret(&mut subject), "› hello".len());
}

#[test]
fn inserts_at_the_caret_rather_than_at_the_end() {
    let mut subject = editor();
    type_text(&mut subject, "helo");
    press(&mut subject, LEFT);
    type_text(&mut subject, "l");

    assert_eq!(subject.text(), "hello");
}

#[test]
fn deletes_what_a_person_can_see_not_a_code_point() {
    let mut subject = editor();
    type_text(&mut subject, "ab");
    let family = "👩\u{200d}👩\u{200d}👧";
    subject.handle_key(&Key {
        name: KeyName::Char,
        character: family.to_owned(),
        ctrl: false,
        shift: false,
        meta: false,
        sequence: family.to_owned(),
    });
    press(&mut subject, BACKSPACE);

    assert_eq!(subject.text(), "ab");
}

#[test]
fn backspace_at_the_start_and_delete_at_the_end_do_nothing() {
    let mut subject = editor();
    subject.set_text("ab");
    press(&mut subject, "\x01");
    press(&mut subject, BACKSPACE);
    press(&mut subject, "\x05");
    press(&mut subject, "\x1b[3~");
    assert_eq!(subject.text(), "ab");
}

#[test]
fn steps_over_a_whole_grapheme_cluster() {
    let mut subject = editor();
    subject.set_text("a👩\u{200d}👩\u{200d}👧b");
    press(&mut subject, LEFT);
    press(&mut subject, LEFT);

    // Two steps left from the end lands before the family, not inside it.
    press(&mut subject, BACKSPACE);
    assert_eq!(subject.text(), "👩\u{200d}👩\u{200d}👧b");

    press(&mut subject, RIGHT);
    press(&mut subject, "\x1b[3~");
    assert_eq!(subject.text(), "👩\u{200d}👩\u{200d}👧");
}

#[test]
fn honours_the_shell_bindings_for_the_ends_of_the_line() {
    let mut subject = editor();
    type_text(&mut subject, "hello");
    press(&mut subject, "\x01"); // ctrl-a
    assert_eq!(caret(&mut subject), "› ".len());

    press(&mut subject, "\x05"); // ctrl-e
    assert_eq!(caret(&mut subject), "› hello".len());

    press(&mut subject, "\x1b[H"); // home
    assert_eq!(caret(&mut subject), "› ".len());
    press(&mut subject, "\x1b[F"); // end
    assert_eq!(caret(&mut subject), "› hello".len());
}

#[test]
fn steps_a_word_at_a_time_when_the_key_was_modified() {
    let mut subject = editor();
    subject.set_text("one two three");
    subject.handle_key(&key(KeyName::Left, true));
    press(&mut subject, "\x0b"); // ctrl-k
    assert_eq!(subject.text(), "one two ");

    subject.handle_key(&key(KeyName::Home, false));
    subject.handle_key(&key(KeyName::Right, true));
    press(&mut subject, "\x15"); // ctrl-u
    assert_eq!(subject.text(), " two ");
}

#[test]
fn honours_ctrl_b_and_ctrl_f_which_survive_terminals_that_mangle_arrows() {
    let mut subject = editor();
    type_text(&mut subject, "hello");
    press(&mut subject, "\x02"); // ctrl-b
    press(&mut subject, "\x02");
    assert_eq!(caret(&mut subject), "› hel".len());

    press(&mut subject, "\x06"); // ctrl-f
    assert_eq!(caret(&mut subject), "› hell".len());
}

#[test]
fn ctrl_u_drops_what_is_behind_the_caret_and_ctrl_k_what_is_ahead() {
    let mut subject = editor();
    type_text(&mut subject, "hello world");
    press(&mut subject, "\x01");
    press(&mut subject, "\x0b"); // ctrl-k
    assert_eq!(subject.text(), "");

    type_text(&mut subject, "hello world");
    press(&mut subject, "\x15"); // ctrl-u
    assert_eq!(subject.text(), "");
}

#[test]
fn ctrl_w_takes_one_word() {
    let mut subject = editor();
    type_text(&mut subject, "hello brave world");
    press(&mut subject, "\x17");

    assert_eq!(subject.text(), "hello brave ");
}

#[test]
fn an_unbound_control_letter_and_an_unknown_key_change_nothing() {
    let mut subject = editor();
    type_text(&mut subject, "abc");
    press(&mut subject, "\x07"); // ctrl-g
    subject.handle_key(&key(KeyName::Unknown, false));
    subject.handle_key(&key(KeyName::PageUp, false));
    assert_eq!(subject.text(), "abc");
    assert_eq!(caret(&mut subject), "› abc".len());
}

#[test]
fn submits_the_line_and_clears_it() {
    let mut subject = editor();
    type_text(&mut subject, "hello");
    let outcome = press(&mut subject, "\r");

    assert_eq!(outcome, EditorOutcome::Submit("hello".to_owned()));
    assert_eq!(subject.text(), "");
}

#[test]
fn reads_ctrl_c_as_an_interrupt_and_ctrl_d_on_an_empty_line_as_eof() {
    let mut subject = editor();
    assert_eq!(press(&mut subject, "\x03"), EditorOutcome::Interrupt);
    assert_eq!(press(&mut subject, "\x04"), EditorOutcome::Eof);
}

#[test]
fn reads_ctrl_d_with_a_line_in_progress_as_forward_delete() {
    // Which is what a shell does, and the reason end-of-input is spelled "an
    // empty line" rather than "the key".
    let mut subject = editor();
    subject.set_text("hello");
    press(&mut subject, "\x01");
    let outcome = press(&mut subject, "\x04");

    assert_eq!(outcome, EditorOutcome::None);
    assert_eq!(subject.text(), "ello");
}

#[test]
fn history_walks_back_through_what_was_submitted_and_forward_to_the_draft() {
    let mut subject = editor();
    subject.remember("first");
    subject.remember("second");
    type_text(&mut subject, "half writ");

    press(&mut subject, UP);
    assert_eq!(subject.text(), "second");
    press(&mut subject, UP);
    assert_eq!(subject.text(), "first");
    // Past the oldest line there is nowhere to go.
    press(&mut subject, UP);
    assert_eq!(subject.text(), "first");
    press(&mut subject, DOWN);
    assert_eq!(subject.text(), "second");
    press(&mut subject, DOWN);
    assert_eq!(subject.text(), "half writ");
    // And past the draft, nothing happens either.
    press(&mut subject, DOWN);
    assert_eq!(subject.text(), "half writ");
}

#[test]
fn history_does_not_keep_the_same_line_twice_in_a_row_nor_an_empty_one() {
    let mut subject = editor();
    subject.remember("again");
    subject.remember("again");
    subject.remember("");

    press(&mut subject, UP);
    press(&mut subject, UP);
    assert_eq!(subject.text(), "again");
}

#[test]
fn submitting_leaves_the_history_position_at_the_end() {
    let mut subject = editor();
    subject.remember("first");
    subject.remember("second");
    press(&mut subject, UP);
    press(&mut subject, UP);
    press(&mut subject, "\r");
    press(&mut subject, UP);
    assert_eq!(subject.text(), "second");
}

#[test]
fn wraps_a_long_line_rather_than_cutting_it_and_indents_the_fold() {
    let mut subject = editor();
    subject.set_text("one two three four five six seven");
    let rows = subject.render(16);

    assert!(rows.len() > 1);
    assert!(rows[0].contains("› one"));
    assert!(rows[1].starts_with("  "));
    assert!(rows.join(" ").contains("seven"));
}

#[test]
fn folds_at_the_window_edge_not_fifteen_columns_short_of_it() {
    // The caret marker is an APC string, and an APC string the width scanner
    // did not know about measured as its own payload — so the line folded
    // fifteen columns early, and where it folded moved as the caret moved.
    let mut with_caret = editor();
    with_caret.set_text("abcdefghij klmnopqrst uvwxyzabcd");
    let mut parked = editor();
    parked.set_text("abcdefghij klmnopqrst uvwxyzabcd");
    parked.handle_key(&key(KeyName::Home, false));

    assert_eq!(with_caret.render(24).len(), 2);
    assert_eq!(parked.render(24).len(), 2);
}

#[test]
fn draws_a_placeholder_with_the_caret_on_it_when_nothing_is_typed() {
    let mut subject = Editor::new(&PLAIN_THEME).with_placeholder("ask something");

    let rows = subject.render(40);
    assert!(rows[0].contains("ask something"));
    assert!(rows[0].contains(CURSOR_MARKER));

    // Typing anything replaces it.
    type_text(&mut subject, "x");
    assert!(!subject.render(40)[0].contains("ask something"));
}

#[test]
fn colours_the_prompt_and_the_placeholder_but_not_the_text() {
    let mut subject = Editor::new(&theme_for(Some(true)))
        .with_prompt("> ")
        .with_placeholder("hint");
    let rows = subject.render(40);
    assert!(rows[0].starts_with(&format!("{ESC}[32m> {ESC}[39m")));
    assert!(rows[0].contains(&format!("{ESC}[90mhint{ESC}[39m")));

    subject.set_text("typed");
    assert_eq!(
        subject.render(40)[0],
        format!("{ESC}[32m> {ESC}[39mtyped{CURSOR_MARKER}")
    );
}

#[test]
fn a_window_narrower_than_the_prompt_still_draws_one_column_of_text() {
    let mut subject = editor();
    subject.set_text("ab");
    let rows = subject.render(1);
    assert!(rows.len() >= 2);
    assert!(rows[0].ends_with('a'));
    assert!(rows[1].trim_start().starts_with('b'));
}
