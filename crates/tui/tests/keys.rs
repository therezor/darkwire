//! What Crossterm reports, named.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use darkwire_tui::{Key, KeyName, is_ctrl};

/// A key event as a terminal reports it.
fn event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

/// The key that event decodes to, for a case that has one.
#[allow(
    clippy::expect_used,
    reason = "a press that does not decode is the failure the test is looking for"
)]
fn decoded(code: KeyCode, modifiers: KeyModifiers) -> Key {
    Key::from_event(event(code, modifiers)).expect("a press decodes")
}

#[test]
fn names_every_key_a_menu_acts_on() {
    for (code, name) in [
        (KeyCode::Enter, KeyName::Enter),
        (KeyCode::Esc, KeyName::Escape),
        (KeyCode::Tab, KeyName::Tab),
        (KeyCode::Backspace, KeyName::Backspace),
        (KeyCode::Delete, KeyName::Delete),
        (KeyCode::Up, KeyName::Up),
        (KeyCode::Down, KeyName::Down),
        (KeyCode::Left, KeyName::Left),
        (KeyCode::Right, KeyName::Right),
        (KeyCode::Home, KeyName::Home),
        (KeyCode::End, KeyName::End),
        (KeyCode::PageUp, KeyName::PageUp),
        (KeyCode::PageDown, KeyName::PageDown),
    ] {
        assert_eq!(decoded(code, KeyModifiers::NONE).name, name, "{code:?}");
    }
}

#[test]
fn a_character_carries_itself() {
    let key = decoded(KeyCode::Char('q'), KeyModifiers::NONE);
    assert_eq!(key.name, KeyName::Char);
    assert_eq!(key.character, "q");
    assert!(!key.ctrl && !key.meta);
}

#[test]
fn a_shifted_character_is_the_character_that_was_typed() {
    let key = decoded(KeyCode::Char('Q'), KeyModifiers::SHIFT);
    assert_eq!(key.character, "Q");
    assert!(key.shift);
}

#[test]
fn a_control_chord_is_the_letter_with_the_bit_set() {
    let key = decoded(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(key.name, KeyName::Char);
    assert!(is_ctrl(&key, 'c'));
}

#[test]
fn a_control_chord_answers_to_one_spelling_however_it_arrives() {
    // A terminal in the kitty protocol reports the shifted letter with the
    // control bit; `is_ctrl` compares against the lowercase one.
    let key = decoded(
        KeyCode::Char('C'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert!(is_ctrl(&key, 'c'));
}

#[test]
fn the_three_control_bytes_that_are_keys_are_named_as_keys() {
    // Ctrl-H, Ctrl-I and Ctrl-M *are* Backspace, Tab and Enter. A terminal
    // reporting them as letters must not make Backspace stop deleting.
    for (letter, name) in [
        ('h', KeyName::Backspace),
        ('i', KeyName::Tab),
        ('m', KeyName::Enter),
    ] {
        let key = decoded(KeyCode::Char(letter), KeyModifiers::CONTROL);
        assert_eq!(key.name, name);
        assert!(!is_ctrl(&key, letter));
    }
}

#[test]
fn shift_tab_is_tab_with_shift() {
    let key = decoded(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(key.name, KeyName::Tab);
    assert!(key.shift);
}

#[test]
fn alt_is_meta() {
    let key = decoded(KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(key.name, KeyName::Left);
    assert!(key.meta);
}

#[test]
fn a_key_it_does_not_name_is_unknown_rather_than_nothing() {
    let key = decoded(KeyCode::F(5), KeyModifiers::NONE);
    assert_eq!(key.name, KeyName::Unknown);
    assert!(key.character.is_empty());
}

#[test]
fn a_release_is_not_a_keystroke() {
    // Windows and the kitty protocol report both halves. Acting on both types
    // every letter twice.
    let event = KeyEvent::new_with_kind(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );
    assert!(Key::from_event(event).is_none());
}

#[test]
fn a_repeat_is_a_keystroke() {
    // Holding a key down is how anyone scrolls a long list.
    let event = KeyEvent::new_with_kind(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Repeat);
    assert_eq!(
        Key::from_event(event).map(|key| key.name),
        Some(KeyName::Down)
    );
}

#[test]
fn the_constructors_say_what_a_test_means() {
    assert_eq!(Key::named(KeyName::Enter).name, KeyName::Enter);
    assert_eq!(Key::char('x').character, "x");
    assert!(is_ctrl(&Key::ctrl('G'), 'g'));
    assert!(Key::named(KeyName::Tab).with_shift().shift);
    assert!(Key::char('b').with_meta().meta);
}
