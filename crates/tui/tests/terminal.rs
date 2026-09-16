//! Sizes that treat zero as unknown, and a keyboard that owns only the raw mode it set.

mod common;

use common::{FakeInput, FakeOutput};
use darkwire_tui::{KeyName, columns_of, open_keyboard, rows_of};

#[test]
fn takes_the_device_at_its_word_when_it_reports_a_size() {
    let out = FakeOutput::new(132, 43);
    assert_eq!(columns_of(&out, None), 132);
    assert_eq!(rows_of(&out, None), 43);
}

#[test]
fn falls_back_when_the_device_reports_zero() {
    // Zero is a value, so `unwrap_or(80)` reads it as a real answer and every
    // width collapses to nothing. `script(1)` allocates a pty with no size, and
    // a terminal mid-resize can answer 0 as well.
    let out = FakeOutput::new(0, 0);
    assert_eq!(columns_of(&out, None), 80);
    assert_eq!(rows_of(&out, None), 24);
    assert_eq!(columns_of(&out, Some(100)), 100);
    assert_eq!(rows_of(&out, Some(50)), 50);
    assert!(out.is_tty);
}

#[test]
fn delivers_a_decoded_key_for_each_keystroke_in_a_chunk() {
    // A terminal hands over `\x1b[B\x1b[B\r` as one chunk routinely, and a
    // reader that answered one key per chunk would drop the rest of a paste.
    let mut input = FakeInput::tty();
    input.type_text("\x1b[B\x1b[B\r");
    let mut keyboard = open_keyboard(input, None).unwrap();

    let names: Vec<KeyName> = keyboard
        .read_keys()
        .unwrap()
        .iter()
        .map(|key| key.name)
        .collect();
    assert_eq!(names, [KeyName::Down, KeyName::Down, KeyName::Enter]);
    // End of input reads as no keys, not as an error.
    assert!(keyboard.read_keys().unwrap().is_empty());
    keyboard.stop().unwrap();
}

#[test]
fn turns_raw_mode_on_for_a_terminal_and_off_again_on_the_way_out() {
    let mut keyboard = open_keyboard(FakeInput::tty(), None).unwrap();
    assert_eq!(keyboard.input().raw_mode_calls, [true]);
    assert!(keyboard.input().is_raw);

    keyboard.stop().unwrap();
    assert_eq!(keyboard.input().raw_mode_calls, [true, false]);
    assert!(!keyboard.input().is_raw);
    assert!(keyboard.is_stopped());
}

#[test]
fn leaves_a_mode_somebody_else_set_exactly_where_it_found_it() {
    // Toggling raw mode underneath whatever set it is how a terminal ends up
    // with no echo after the process exits.
    let mut input = FakeInput::tty();
    input.is_raw = true;
    let mut keyboard = open_keyboard(input, None).unwrap();
    keyboard.stop().unwrap();

    assert!(keyboard.input().raw_mode_calls.is_empty());
    assert!(keyboard.input().is_raw);
}

#[test]
fn does_not_touch_the_mode_of_something_that_is_not_a_terminal() {
    let mut keyboard = open_keyboard(FakeInput::pipe(), None).unwrap();
    keyboard.stop().unwrap();
    assert!(keyboard.input().raw_mode_calls.is_empty());

    // Asking for raw mode on a pipe is refused the same way: there is no mode.
    let mut keyboard = open_keyboard(FakeInput::pipe(), Some(true)).unwrap();
    keyboard.stop().unwrap();
    assert!(keyboard.input().raw_mode_calls.is_empty());
}

#[test]
fn leaves_the_mode_alone_when_told_not_to_take_it() {
    // What a test passes: a terminal, but no raw mode wanted.
    let mut keyboard = open_keyboard(FakeInput::tty(), Some(false)).unwrap();
    keyboard.stop().unwrap();
    assert!(keyboard.input().raw_mode_calls.is_empty());
}

#[test]
fn stops_once_however_often_it_is_asked() {
    let mut keyboard = open_keyboard(FakeInput::tty(), None).unwrap();
    keyboard.stop().unwrap();
    keyboard.stop().unwrap();
    assert_eq!(keyboard.input().raw_mode_calls, [true, false]);
}

#[test]
fn delivers_nothing_after_it_has_stopped() {
    let mut input = FakeInput::tty();
    input.type_text("a");
    let mut keyboard = open_keyboard(input, None).unwrap();
    keyboard.stop().unwrap();

    assert!(keyboard.read_keys().unwrap().is_empty());
}
