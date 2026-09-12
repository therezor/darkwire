//! The prompts, driven through a scripted reader rather than a terminal.
//!
//! `choose_many` is what this file is really for — the other four are exercised
//! end to end by `init.rs` driving the whole wizard. What is asserted here is
//! the parsing: every separator somebody's fingers might produce, and the
//! re-ask that stops a misspelt name from silently installing three things when
//! four were asked for.

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

use ghostai::ask::{Ask, LineReader, ScriptedReader, StdinReader};
use ghostai::i18n::Translations;

/// The three rows every case in this file chooses from.
///
/// Deliberately not bare ids: a real listing carries a label and a hint beside
/// the id, and prefix matching has to work against the whole rendered row.
const OPTIONS: [&str; 3] = ["coder (Coder)  coding", "lead (Team lead)", "nano (Nano)"];

fn options() -> Vec<String> {
    OPTIONS.iter().map(|row| (*row).to_owned()).collect()
}

/// Runs `choose_many` against a scripted set of answers.
fn choosing(answers: &[&str], marks: &[&str]) -> (Vec<usize>, String) {
    let t = Translations::default();
    let mut reader = ScriptedReader::new(answers.iter().copied());
    let mut ask = Ask::new(&mut reader, Some(false), &t);
    let mut out: Vec<u8> = Vec::new();
    let marks: Vec<String> = marks.iter().map(|mark| (*mark).to_owned()).collect();
    let chosen = ask
        .choose_many(&mut out, "Which?", &options(), &marks)
        .expect("the scripted reader answers every prompt");
    (chosen, String::from_utf8_lossy(&out).into_owned())
}

#[test]
fn takes_numbers_separated_by_spaces_or_commas_or_both() {
    for answer in ["1 3", "1,3", "1, 3", " 1  ,3 "] {
        let (chosen, _) = choosing(&[answer], &[]);
        assert_eq!(chosen, vec![0, 2], "answer {answer:?}");
    }
}

#[test]
fn takes_names_matched_by_prefix_exactly_as_choose_does() {
    // Sorted by index, not by the order they were typed: the caller uses this
    // to order installs, and "as typed" would make `install b a` and
    // `install a b` two different runs.
    let (chosen, _) = choosing(&["nano coder"], &[]);
    assert_eq!(chosen, vec![0, 2]);
}

#[test]
fn takes_all_which_is_what_somebody_wanting_the_lot_types_first() {
    let (chosen, _) = choosing(&["all"], &[]);
    assert_eq!(chosen, vec![0, 1, 2]);
}

#[test]
fn takes_an_empty_line_as_none_which_is_how_the_question_is_declined() {
    let (chosen, _) = choosing(&[""], &[]);
    assert!(chosen.is_empty());
}

#[test]
fn names_a_repeat_once() {
    let (chosen, _) = choosing(&["1 1 coder"], &[]);
    assert_eq!(chosen, vec![0]);
}

#[test]
fn re_asks_rather_than_dropping_what_it_could_not_read() {
    // Selecting three things and getting two because one was misspelt is
    // invisible until much later, when the agent that was supposed to exist
    // does not.
    let (chosen, written) = choosing(&["1 ghost 3", "1 3"], &[]);
    assert_eq!(chosen, vec![0, 2]);
    assert!(
        written.contains("“ghost” is not one of these"),
        "expected the refusal to name the token: {written}"
    );
}

#[test]
fn refuses_a_number_outside_the_list() {
    let (chosen, written) = choosing(&["9", "2"], &[]);
    assert_eq!(chosen, vec![1]);
    assert!(written.contains("“9” is not one of these"), "{written}");
}

#[test]
fn annotates_a_row_without_making_the_annotation_typeable() {
    // `[installed]` is a mark, not part of the name — matching on it would let
    // one word select every installed agent at once.
    let (chosen, written) = choosing(&["installed", "2"], &["[installed]", "", ""]);
    assert_eq!(chosen, vec![1]);
    assert!(written.contains("[installed]"), "{written}");
    assert!(written.contains("is not one of these"), "{written}");
}

#[test]
fn text_falls_back_to_the_suggestion_on_an_empty_line() {
    let t = Translations::default();
    let mut reader = ScriptedReader::new(["", "typed"]);
    let mut ask = Ask::new(&mut reader, Some(false), &t);
    let mut out: Vec<u8> = Vec::new();

    assert_eq!(ask.text(&mut out, "Where?", Some("/tmp")).unwrap(), "/tmp");
    assert_eq!(ask.text(&mut out, "Where?", Some("/tmp")).unwrap(), "typed");

    let written = String::from_utf8_lossy(&out);
    assert!(written.contains("Where? [/tmp]: "), "{written}");
}

#[test]
fn text_with_no_suggestion_takes_an_empty_line_as_an_empty_answer() {
    // The API-key question relies on this: an endpoint that needs no key is
    // answered by pressing return, not by typing a sentinel.
    let t = Translations::default();
    let mut reader = ScriptedReader::new([""]);
    let mut ask = Ask::new(&mut reader, Some(false), &t);
    let mut out: Vec<u8> = Vec::new();

    assert_eq!(ask.text(&mut out, "Key", None).unwrap(), "");
}

#[test]
fn choose_takes_a_name_as_readily_as_a_number() {
    let t = Translations::default();
    let mut reader = ScriptedReader::new(["nano"]);
    let mut ask = Ask::new(&mut reader, Some(false), &t);
    let mut out: Vec<u8> = Vec::new();

    assert_eq!(ask.choose(&mut out, "Which?", &options(), 0).unwrap(), 2);
}

#[test]
fn choose_takes_the_suggestion_when_the_line_is_empty() {
    let t = Translations::default();
    let mut reader = ScriptedReader::new([""]);
    let mut ask = Ask::new(&mut reader, Some(false), &t);
    let mut out: Vec<u8> = Vec::new();

    assert_eq!(ask.choose(&mut out, "Which?", &options(), 1).unwrap(), 1);
}

#[test]
fn choose_re_asks_rather_than_guessing() {
    let t = Translations::default();
    let mut reader = ScriptedReader::new(["zzz", "2"]);
    let mut ask = Ask::new(&mut reader, Some(false), &t);
    let mut out: Vec<u8> = Vec::new();

    assert_eq!(ask.choose(&mut out, "Which?", &options(), 0).unwrap(), 1);
    let written = String::from_utf8_lossy(&out);
    assert!(
        written.contains("Enter a number between 1 and 3."),
        "{written}"
    );
}

#[test]
fn secret_reads_a_line_and_closes_it_with_a_newline() {
    // The terminal echoed nothing, so the cursor is still sitting on the
    // prompt; without the newline the next line printed would continue it.
    let t = Translations::default();
    let mut reader = ScriptedReader::new(["hunter2"]);
    let mut ask = Ask::new(&mut reader, Some(false), &t);
    let mut out: Vec<u8> = Vec::new();

    assert_eq!(ask.secret(&mut out, "Key").unwrap(), "hunter2");
    let written = String::from_utf8_lossy(&out);
    assert_eq!(written, "Key: \n");
    assert!(!written.contains("hunter2"), "the key must not be echoed");
}

#[test]
fn confirm_takes_the_localised_letter_and_the_english_one() {
    let t = Translations::default();
    for (answer, expected) in [
        ("y", true),
        ("yes", true),
        ("n", false),
        ("no", false),
        // Neither: the suggestion stands.
        ("maybe", true),
    ] {
        let mut reader = ScriptedReader::new([answer]);
        let mut ask = Ask::new(&mut reader, Some(false), &t);
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(
            ask.confirm(&mut out, "Approve?", true).unwrap(),
            expected,
            "answer {answer:?}"
        );
    }
}

#[test]
fn confirm_shows_which_way_return_goes() {
    let t = Translations::default();
    for (fallback, hint) in [(true, "[Y/n]"), (false, "[y/N]")] {
        let mut reader = ScriptedReader::new([""]);
        let mut ask = Ask::new(&mut reader, Some(false), &t);
        let mut out: Vec<u8> = Vec::new();
        let answered = ask.confirm(&mut out, "Approve?", fallback).unwrap();
        assert_eq!(answered, fallback);
        let written = String::from_utf8_lossy(&out);
        assert!(written.contains(hint), "{written}");
    }
}

#[test]
fn end_of_input_is_an_abort_rather_than_an_empty_answer() {
    // A reader that ran out is somebody who left. Answering the rest of the
    // wizard with empty lines would write a configuration nobody chose.
    let t = Translations::default();
    let mut reader = ScriptedReader::new(Vec::<String>::new());
    let mut ask = Ask::new(&mut reader, Some(false), &t);
    let mut out: Vec<u8> = Vec::new();

    let error = ask.text(&mut out, "Where?", Some("/tmp")).unwrap_err();
    assert!(error.is_aborted(), "{error:?}");
}

#[test]
fn a_scripted_reader_reports_what_is_left() {
    let mut reader = ScriptedReader::new(["one", "two"]);
    assert_eq!(reader.remaining(), 2);
    let t = Translations::default();
    let mut ask = Ask::new(&mut reader, Some(false), &t);
    let mut out: Vec<u8> = Vec::new();
    ask.text(&mut out, "?", None).unwrap();
    // The borrow of `reader` ends at the last use of `ask`, which is the line
    // above; the count below is what the reader has left after one read.
    assert_eq!(reader.remaining(), 1);
}

#[test]
fn a_fractional_number_is_not_an_entry_and_a_trailing_zero_is() {
    // `2.0` is the second entry and `2.5` is not an entry at all — the answer a
    // person would give — and no value makes the trip through a floating-point
    // representation on its way to being an index.
    let t = Translations::default();
    let mut reader = ScriptedReader::new(["2.5", "2.0"]);
    let mut ask = Ask::new(&mut reader, Some(false), &t);
    let mut out: Vec<u8> = Vec::new();

    assert_eq!(ask.choose(&mut out, "Which?", &options(), 0).unwrap(), 1);
    assert!(
        String::from_utf8_lossy(&out).contains("Enter a number between"),
        "{}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn the_prompt_keeps_its_reader_out_of_the_debug_output() {
    // A reader is a stdin handle or a script, and neither renders usefully; a
    // scripted one printed into a failure message would dump every answer.
    let t = Translations::default();
    let mut reader = ScriptedReader::new(["secret-answer"]);
    let ask = Ask::new(&mut reader, Some(false), &t);

    let rendered = format!("{ask:?}");
    assert!(rendered.contains("Ask"), "{rendered}");
    assert!(!rendered.contains("secret-answer"), "{rendered}");
}

#[test]
fn the_real_reader_reports_a_closed_stream_as_end_of_input() {
    // The test runner gives every test an empty stdin, which is the same shape
    // as a pipe that has been closed — and reading it as an empty answer rather
    // than as an abort is what would make the wizard write a config nobody
    // chose.
    let mut reader = StdinReader::new();

    assert_eq!(reader.read_line().unwrap(), None);
    // No terminal, so raw mode is unavailable and the secret question falls
    // back to the same line read rather than refusing.
    assert_eq!(reader.read_secret().unwrap(), None);
}
