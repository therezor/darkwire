//! Bytes from a terminal → keys.
//!
//! A decoder rather than a key-event library, because the whole point of this
//! crate is a menu that draws its own frame: the bytes come off the tty in raw
//! mode and something has to name them. Three decisions shape it.
//!
//! **`parse_keys` is the primary form, not `parse_key`.** A terminal does not
//! deliver one key per read. Holding an arrow key, or pasting, arrives as
//! `"\x1b[B\x1b[B\r"` in a single chunk, and a decoder that returns the first
//! key silently drops the rest — which reads as "the menu skipped a row" rather
//! than as a bug in the parser.
//!
//! **Both cursor encodings are decoded.** A terminal in DECCKM (application
//! cursor keys) sends `\x1bOA` where the normal mode sends `\x1b[A`, and plenty
//! send the former by default. Handling only CSI is the single most likely
//! cause of "the arrow keys do nothing on my machine", and it costs four lines
//! to avoid.
//!
//! **A chunk is decoded whole, with no timer.** A sequence split across two
//! reads could be reassembled with a timeout, and that state machine is not
//! worth its weight here: a terminal emits a cursor sequence in one write, and
//! the worst case if one is ever split is a single dropped keypress in a menu
//! the operator can reopen. A lone trailing `\x1b` is Escape, which is what a
//! user pressing Escape actually sends.

/// The escape byte, spelled as an escape and never as a raw control character
/// in a source file, where it is invisible and unsearchable.
const ESC: char = '\x1b';

/// The keys a menu can act on. Everything else decodes as `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyName {
    /// A printable character, or a control byte named by its letter.
    Char,
    /// Return, in either of the two bytes a terminal sends for it.
    Enter,
    /// A lone `\x1b`.
    Escape,
    /// Tab, or Shift-Tab (`CSI Z`) with `shift` set.
    Tab,
    /// DEL or BS.
    Backspace,
    /// Forward delete, `CSI 3 ~`.
    Delete,
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// Home, in any of its three encodings.
    Home,
    /// End, in any of its three encodings.
    End,
    /// Page Up, `CSI 5 ~`.
    PageUp,
    /// Page Down, `CSI 6 ~`.
    PageDown,
    /// A sequence this decoder does not name. The bytes are kept.
    Unknown,
}

/// One decoded keypress.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    /// What was pressed.
    pub name: KeyName,
    /// The character for [`KeyName::Char`], empty for everything else.
    ///
    /// For a control byte this is the letter it was typed with — `\x03`
    /// decodes as `Char` with `character: "c"` and `ctrl: true` — so a caller
    /// matches Ctrl-C the same way it matches `c`.
    pub character: String,
    /// Control held.
    pub ctrl: bool,
    /// Shift held.
    pub shift: bool,
    /// Alt/Option, which a terminal sends as an `\x1b` prefix.
    pub meta: bool,
    /// Exactly the bytes this key was decoded from.
    pub sequence: String,
}

impl Key {
    fn new(name: KeyName, sequence: &str) -> Self {
        Self {
            name,
            character: String::new(),
            ctrl: false,
            shift: false,
            meta: false,
            sequence: sequence.to_owned(),
        }
    }

    fn escape() -> Self {
        Self::new(KeyName::Escape, "\x1b")
    }
}

/// A key and how many bytes of the input it consumed.
struct Decoded {
    key: Key,
    length: usize,
}

/// Applies the `1 + bitmask` a terminal puts in a CSI sequence's second
/// parameter.
///
/// `\x1b[1;5A` is Ctrl-Up: `5` is `1 + 4`, and 4 is the control bit. Absent or
/// `1` means no modifier at all.
fn apply_modifiers(mut key: Key, parameter: Option<&str>) -> Key {
    // `5:3` carries a sub-parameter after the colon; the modifier is the part
    // before it.
    let Some(value) = parameter
        .and_then(|text| text.split(':').next())
        .and_then(|text| text.parse::<u32>().ok())
    else {
        return key;
    };
    if value < 2 {
        return key;
    }
    let bits = value - 1;
    key.shift = bits & 1 != 0;
    key.meta = bits & 2 != 0;
    key.ctrl = bits & 4 != 0;
    key
}

/// The letter forms both encodings share: `\x1b[A` and `\x1bOA`.
fn by_final(final_byte: char) -> Option<KeyName> {
    Some(match final_byte {
        'A' => KeyName::Up,
        'B' => KeyName::Down,
        'C' => KeyName::Right,
        'D' => KeyName::Left,
        'F' => KeyName::End,
        'H' => KeyName::Home,
        _ => return None,
    })
}

/// The `\x1b[<n>~` family, keyed by the numeric parameter.
fn by_tilde(parameter: &str) -> Option<KeyName> {
    Some(match parameter {
        "1" | "7" => KeyName::Home,
        "3" => KeyName::Delete,
        "4" | "8" => KeyName::End,
        "5" => KeyName::PageUp,
        "6" => KeyName::PageDown,
        _ => return None,
    })
}

/// A CSI final byte, `@`–`~`.
fn is_final_byte(ch: char) -> bool {
    ('@'..='~').contains(&ch)
}

/// `\x1b[…` — the encoding a terminal uses outside DECCKM.
///
/// `data` starts at the escape.
fn decode_csi(data: &str) -> Decoded {
    let body = &data[2..];
    let Some((offset, final_byte)) = body.char_indices().find(|&(_, ch)| is_final_byte(ch)) else {
        // An unterminated CSI is a truncated read. Treat the escape alone as
        // Escape and let the rest decode as ordinary characters, which is noisy
        // but never swallows the remainder of the chunk.
        return Decoded {
            key: Key::escape(),
            length: 1,
        };
    };

    let length = 2 + offset + final_byte.len_utf8();
    let sequence = &data[..length];
    let mut parameters = body[..offset].split(';');
    let first = parameters.next().unwrap_or("");
    let second = parameters.next();

    // CSI Z is Shift-Tab on every terminal, and carries no modifier parameter.
    if final_byte == 'Z' {
        let mut key = Key::new(KeyName::Tab, sequence);
        key.shift = true;
        return Decoded { key, length };
    }

    if let Some(name) = by_final(final_byte) {
        let key = apply_modifiers(Key::new(name, sequence), second);
        return Decoded { key, length };
    }

    if final_byte == '~'
        && let Some(name) = by_tilde(first)
    {
        let key = apply_modifiers(Key::new(name, sequence), second);
        return Decoded { key, length };
    }

    Decoded {
        key: Key::new(KeyName::Unknown, sequence),
        length,
    }
}

/// `\x1bO…` — SS3, what DECCKM sends for the cursor keys.
fn decode_ss3(data: &str) -> Decoded {
    let Some(final_byte) = data[2..].chars().next() else {
        return Decoded {
            key: Key::escape(),
            length: 1,
        };
    };
    let length = 2 + final_byte.len_utf8();
    let name = by_final(final_byte).unwrap_or(KeyName::Unknown);
    Decoded {
        key: Key::new(name, &data[..length]),
        length,
    }
}

/// A control byte, or a printable character.
///
/// `data` is non-empty and starts at the character to decode.
fn decode_plain(data: &str) -> Decoded {
    let ch = data.chars().next().unwrap_or('\0');
    let length = ch.len_utf8();
    let sequence = &data[..length];
    let single = |name: KeyName| Decoded {
        key: Key::new(name, sequence),
        length,
    };

    match ch {
        '\r' | '\n' => single(KeyName::Enter),
        '\t' => single(KeyName::Tab),
        '\x7f' | '\x08' => single(KeyName::Backspace),
        // 0x01–0x1a are Ctrl-A through Ctrl-Z. Enter, Tab and Backspace are in
        // that range too (Ctrl-M, Ctrl-I, Ctrl-H) and are named above, because
        // a caller wanting "the user pressed Return" should not have to know it
        // is Ctrl-M.
        '\x01'..='\x1a' => {
            let mut key = Key::new(KeyName::Char, sequence);
            key.character = char::from(u8::try_from(u32::from(ch) + 96).unwrap_or(b'?')).into();
            key.ctrl = true;
            Decoded { key, length }
        }
        '\0'..='\x1f' => single(KeyName::Unknown),
        _ => {
            let mut key = Key::new(KeyName::Char, sequence);
            sequence.clone_into(&mut key.character);
            Decoded { key, length }
        }
    }
}

/// Decodes the escape-prefixed key at the start of `data`.
fn decode_escape(data: &str) -> Decoded {
    match data[1..].chars().next() {
        Some('[') => decode_csi(data),
        Some('O') => decode_ss3(data),
        None => Decoded {
            key: Key::escape(),
            length: 1,
        },
        // `\x1b` then anything else is Alt held with that key.
        Some(_) => {
            let alt = decode_plain(&data[1..]);
            let mut key = alt.key;
            key.meta = true;
            key.sequence = format!("{ESC}{}", key.sequence);
            Decoded {
                key,
                length: 1 + alt.length,
            }
        }
    }
}

/// Every key in one chunk of terminal input.
///
/// Never fails and never returns a partial key: anything it cannot name comes
/// back as [`KeyName::Unknown`] with `sequence` intact, so a caller can log
/// what a terminal actually sent without the decoder having to know about it
/// first.
pub fn parse_keys(data: &str) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut at = 0;

    while at < data.len() {
        let rest = &data[at..];
        let decoded = if rest.starts_with(ESC) {
            decode_escape(rest)
        } else {
            decode_plain(rest)
        };
        keys.push(decoded.key);
        at += decoded.length;
    }

    keys
}

/// The first key in `data`, for a caller that knows there is only one.
pub fn parse_key(data: &str) -> Option<Key> {
    parse_keys(data).into_iter().next()
}

/// Whether a key is a particular Ctrl-letter.
///
/// Ctrl-M, Ctrl-I and Ctrl-H are named `Enter`, `Tab` and `Backspace` instead,
/// so this answers `false` for those three — which is the useful answer: a menu
/// binding Ctrl-H to something is a menu that eats Backspace.
pub fn is_ctrl(input: &Key, letter: char) -> bool {
    input.ctrl && input.name == KeyName::Char && input.character.chars().eq(letter.to_lowercase())
}
