//! What a terminal would have sent, as the keys a widget acts on.
//!
//! The keys a widget is driven with are named — Enter, Down, Ctrl-U — and a
//! test that built them field by field would say less than the byte sequence a
//! terminal sends for them. This maps the one to the other, so a test reads as
//! "press Escape" while naming the thing an operator actually pressed.
//!
//! It is a fixture and not a decoder. Crossterm decodes keys in production, on
//! every platform; this exists because a test should not have to build a
//! `crossterm::event::KeyEvent` to press a key.
#![allow(dead_code)]

use darkwire_tui::{Key, KeyName};

/// The key a terminal sends `bytes` for.
///
/// A sequence this does not know fails the test that used it, which is a
/// fixture naming something nobody has taught it rather than a case worth
/// handling.
#[must_use]
pub fn key(bytes: &str) -> Key {
    match bytes {
        "\r" | "\n" => Key::named(KeyName::Enter),
        "\t" => Key::named(KeyName::Tab),
        "\x1b[Z" => Key::named(KeyName::Tab).with_shift(),
        "\x7f" | "\x08" => Key::named(KeyName::Backspace),
        "\x1b" => Key::named(KeyName::Escape),
        "\x1b[A" | "\x1bOA" => Key::named(KeyName::Up),
        "\x1b[B" | "\x1bOB" => Key::named(KeyName::Down),
        "\x1b[C" | "\x1bOC" => Key::named(KeyName::Right),
        "\x1b[D" | "\x1bOD" => Key::named(KeyName::Left),
        "\x1b[H" | "\x1bOH" | "\x1b[1~" => Key::named(KeyName::Home),
        "\x1b[F" | "\x1bOF" | "\x1b[4~" => Key::named(KeyName::End),
        "\x1b[3~" => Key::named(KeyName::Delete),
        "\x1b[5~" => Key::named(KeyName::PageUp),
        "\x1b[6~" => Key::named(KeyName::PageDown),
        other => from_text(other),
    }
}

/// A control byte, an Alt chord, or a character typed on its own.
#[allow(
    clippy::expect_used,
    reason = "a fixture naming nothing is a failing test, said where it happened"
)]
fn from_text(bytes: &str) -> Key {
    let mut chars = bytes.chars();
    let first = chars.next().expect("a fixture has to name a key");
    // Alt: the terminal's own spelling is an escape in front of the character.
    if first == '\x1b' {
        let Some(letter) = chars.next() else {
            return Key::named(KeyName::Escape);
        };
        assert!(chars.next().is_none(), "unnamed sequence: {bytes:?}");
        return Key::char(letter).with_meta();
    }
    assert!(chars.next().is_none(), "unnamed sequence: {bytes:?}");
    if ('\x01'..='\x1a').contains(&first) {
        // `\x01` is Ctrl-A, and so on up the alphabet. Enter, Tab and
        // Backspace are named above rather than reaching here, which is what
        // `is_ctrl` documents about them.
        let letter = char::from(b'a' + (first as u8) - 1);
        return Key::ctrl(letter);
    }
    Key::char(first)
}
