//! Shell bindings, grapheme-wise movement, history, and folding at the real window edge.

use crate::common;

use darkwire_tui::{
    CURSOR_MARKER, Component, Editor, EditorOutcome, Key, KeyName, PLAIN_THEME, theme_for,
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

/// A word-wise movement, which a terminal sends as an Alt chord.
fn key(name: KeyName, meta: bool) -> Key {
    let key = Key::named(name);
    if meta { key.with_meta() } else { key }
}

/// The key a terminal would have sent these bytes for.
fn press(subject: &mut Editor, bytes: &str) -> EditorOutcome {
    subject.handle_key(&common::key(bytes))
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
        character: family.to_owned(),
        ..Key::named(KeyName::Char)
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

// ---------------------------------------------------- more than one line

#[test]
fn shift_return_adds_a_line_rather_than_sending_one() {
    let mut editor = Editor::new(&PLAIN_THEME);
    type_text(&mut editor, "first");
    assert_eq!(
        editor.handle_key(&Key::named(KeyName::Enter).with_shift()),
        EditorOutcome::None,
        "a message was sent when a line was asked for"
    );
    type_text(&mut editor, "second");

    assert_eq!(editor.text(), "first\nsecond");
}

#[test]
fn the_terminals_that_cannot_say_shift_return_have_two_spellings_that_work() {
    for key in [Key::named(KeyName::Enter).with_meta(), Key::ctrl('j')] {
        let mut editor = Editor::new(&PLAIN_THEME);
        type_text(&mut editor, "one");
        assert_eq!(editor.handle_key(&key), EditorOutcome::None, "{key:?}");
        type_text(&mut editor, "two");
        assert_eq!(editor.text(), "one\ntwo", "{key:?}");
    }
}

#[test]
fn return_still_sends_the_whole_message() {
    let mut editor = Editor::new(&PLAIN_THEME);
    type_text(&mut editor, "first");
    editor.handle_key(&Key::named(KeyName::Enter).with_shift());
    type_text(&mut editor, "second");

    assert_eq!(
        editor.handle_key(&Key::named(KeyName::Enter)),
        EditorOutcome::Submit("first\nsecond".to_owned())
    );
    assert_eq!(editor.text(), "");
}

#[test]
fn a_line_somebody_asked_for_is_a_row_of_its_own() {
    let mut editor = Editor::new(&PLAIN_THEME);
    editor.set_text("one\ntwo");

    let rows = editor.render(40);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].contains("one"));
    assert!(rows[1].contains("two"));
}

#[test]
fn a_short_line_before_a_break_is_not_run_into_the_next() {
    let mut editor = Editor::new(&PLAIN_THEME);
    editor.set_text("a\nb");

    assert_eq!(editor.render(40).len(), 2);
}

#[test]
fn the_arrows_move_between_the_lines_of_a_message() {
    let mut editor = Editor::new(&PLAIN_THEME);
    editor.set_text("first\nsecond");
    // The caret is at the end of "second".
    editor.handle_key(&Key::named(KeyName::Up));
    type_text(&mut editor, "!");

    assert_eq!(editor.text(), "first!\nsecond");
}

#[test]
fn the_arrows_keep_their_column_across_a_line() {
    let mut editor = Editor::new(&PLAIN_THEME);
    editor.set_text("abcdef\nxy");
    // From the end of "xy", column 2, up to column 2 of "abcdef".
    editor.handle_key(&Key::named(KeyName::Up));
    type_text(&mut editor, "-");

    assert_eq!(editor.text(), "ab-cdef\nxy");
}

#[test]
fn a_caret_on_a_shorter_line_lands_at_its_end() {
    let mut editor = Editor::new(&PLAIN_THEME);
    editor.set_text("ab\nlonger line");
    editor.handle_key(&Key::named(KeyName::Up));
    type_text(&mut editor, "!");

    assert_eq!(editor.text(), "ab!\nlonger line");
}

#[test]
fn the_history_is_still_reachable_from_the_ends_of_a_message() {
    let mut editor = Editor::new(&PLAIN_THEME);
    editor.remember("what was asked before");
    editor.set_text("one\ntwo");

    // Up from the first line, not the last, is what leaves the message.
    editor.handle_key(&Key::named(KeyName::Up));
    editor.handle_key(&Key::named(KeyName::Up));

    assert_eq!(editor.text(), "what was asked before");
}

#[test]
fn a_message_of_one_line_reaches_the_history_on_the_first_up() {
    let mut editor = Editor::new(&PLAIN_THEME);
    editor.remember("what was asked before");
    type_text(&mut editor, "a draft");

    editor.handle_key(&Key::named(KeyName::Up));

    assert_eq!(editor.text(), "what was asked before");
}

#[test]
fn the_line_keys_work_on_the_line_the_caret_is_on() {
    let mut editor = Editor::new(&PLAIN_THEME);
    editor.set_text("first\nsecond");

    editor.handle_key(&Key::named(KeyName::Home));
    type_text(&mut editor, ">");
    assert_eq!(editor.text(), "first\n>second");

    editor.handle_key(&Key::named(KeyName::End));
    type_text(&mut editor, "<");
    assert_eq!(editor.text(), "first\n>second<");
}

#[test]
fn clearing_to_the_start_clears_the_line_and_not_the_message() {
    let mut editor = Editor::new(&PLAIN_THEME);
    editor.set_text("keep this\nthrow this");

    editor.handle_key(&Key::ctrl('u'));

    assert_eq!(editor.text(), "keep this\n");
}

#[test]
fn clearing_to_the_end_stops_at_the_end_of_the_line() {
    let mut editor = Editor::new(&PLAIN_THEME);
    editor.set_text("cut here\nkeep this");
    editor.handle_key(&Key::named(KeyName::Up));
    editor.handle_key(&Key::named(KeyName::Home));
    editor.handle_key(&Key::ctrl('k'));

    assert_eq!(editor.text(), "\nkeep this");
}

#[test]
fn a_paste_keeps_its_lines_whichever_way_the_terminal_spelled_them() {
    let mut subject = editor();
    subject.insert_text("one\r\ntwo\rthree\nfour");

    assert_eq!(subject.text(), "one\ntwo\nthree\nfour");
    let rows = subject.render(40);
    assert_eq!(rows.len(), 4, "{rows:?}");
}

#[test]
fn a_paste_lands_at_the_caret_and_leaves_the_caret_after_it() {
    let mut subject = editor();
    type_text(&mut subject, "ad");
    press(&mut subject, LEFT);
    subject.insert_text("bc");

    assert_eq!(subject.text(), "abcd");
    assert_eq!(caret(&mut subject), "› abc".len());
}

#[test]
fn a_paste_drops_control_characters_and_escapes_but_keeps_tabs() {
    let mut subject = editor();
    subject.insert_text("a\u{7}b\u{1b}[31mc\u{0}\td");

    assert_eq!(subject.text(), "abc\td", "a tab is part of what is sent");
}

#[test]
fn a_tab_is_drawn_as_the_spaces_it_stands_for() {
    let mut subject = editor();
    subject.insert_text("a\tb");

    let rows = subject.render(40);
    assert_eq!(rows[0], format!("› a   b{CURSOR_MARKER}"));
}
