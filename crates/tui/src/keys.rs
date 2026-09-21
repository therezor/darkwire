//! What a keypress is called, once Crossterm has decoded it.
//!
//! A name and its modifiers, and nothing about how a terminal spelled them.
//! Crossterm is the decoder — it reads console events on Windows and escape
//! sequences everywhere else, which is one of the two reasons this crate no
//! longer has a byte decoder of its own. The other is the kitty protocol, which
//! reports a release for every press.
//!
//! Three things about the mapping are worth knowing before changing it.
//!
//! **A release is not a keystroke.** Acting on both halves types every letter
//! twice, and both Windows and a terminal in the kitty protocol send both.
//! A *repeat* is a keystroke: holding a key down is how anyone scrolls.
//!
//! **Ctrl-H, Ctrl-I and Ctrl-M are Backspace, Tab and Enter.** They are the
//! same bytes, and a terminal reporting the letter form must not make Backspace
//! stop deleting. [`is_ctrl`] answers `false` for those three, which is the
//! useful answer: a menu binding Ctrl-H to something is a menu that eats
//! Backspace.
//!
//! **A control chord carries the lowercase letter.** A terminal may report the
//! shifted form, and a caller matching Ctrl-C should not have to know which one
//! it got.

/// The keys a menu can act on. Everything else is `Unknown`.
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
}

impl Key {
    /// A key with nothing held and no character.
    fn new(name: KeyName) -> Self {
        Self {
            name,
            character: String::new(),
            ctrl: false,
            shift: false,
            meta: false,
        }
    }

    /// A key with no character and no modifiers, by name.
    ///
    /// The three constructors below are how a caller states a keystroke
    /// without a terminal in the room. A test that had to spell `\x1b[B` to
    /// mean Down would be asserting the decoder as well as the thing under
    /// test.
    #[must_use]
    pub fn named(name: KeyName) -> Self {
        Self::new(name)
    }

    /// A printable character, typed with nothing held.
    #[must_use]
    pub fn char(character: char) -> Self {
        Self {
            character: character.to_string(),
            ..Self::new(KeyName::Char)
        }
    }

    /// A letter typed with Control held.
    #[must_use]
    pub fn ctrl(letter: char) -> Self {
        Self {
            character: letter.to_lowercase().to_string(),
            ctrl: true,
            ..Self::new(KeyName::Char)
        }
    }

    /// The same key, with Shift held.
    #[must_use]
    pub fn with_shift(mut self) -> Self {
        self.shift = true;
        self
    }

    /// The same key, with Alt/Option held.
    #[must_use]
    pub fn with_meta(mut self) -> Self {
        self.meta = true;
        self
    }

    /// What Crossterm just reported, named.
    ///
    /// `None` for a key *release* and for anything this does not name. Release
    /// events arrive on Windows and from terminals speaking the kitty
    /// protocol, and acting on both halves of a keypress types every letter
    /// twice. A repeat is a press: holding a key down is how anyone scrolls.
    ///
    /// Ctrl-H, Ctrl-I and Ctrl-M are folded back onto Backspace, Tab and Enter.
    /// A terminal in the kitty protocol reports them as the letter with the
    /// control bit set, and everything downstream is written against the names.
    #[must_use]
    pub fn from_event(event: crossterm::event::KeyEvent) -> Option<Self> {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

        if event.kind == KeyEventKind::Release {
            return None;
        }
        let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
        let name = match event.code {
            KeyCode::Char('h') if ctrl => KeyName::Backspace,
            KeyCode::Char('i') if ctrl => KeyName::Tab,
            KeyCode::Char('m') if ctrl => KeyName::Enter,
            KeyCode::Char(_) => KeyName::Char,
            KeyCode::Enter => KeyName::Enter,
            KeyCode::Esc => KeyName::Escape,
            KeyCode::Tab | KeyCode::BackTab => KeyName::Tab,
            KeyCode::Backspace => KeyName::Backspace,
            KeyCode::Delete => KeyName::Delete,
            KeyCode::Up => KeyName::Up,
            KeyCode::Down => KeyName::Down,
            KeyCode::Left => KeyName::Left,
            KeyCode::Right => KeyName::Right,
            KeyCode::Home => KeyName::Home,
            KeyCode::End => KeyName::End,
            KeyCode::PageUp => KeyName::PageUp,
            KeyCode::PageDown => KeyName::PageDown,
            _ => KeyName::Unknown,
        };
        let character = match event.code {
            // Lowercased for a control chord so `is_ctrl` has one spelling to
            // compare against, and left alone otherwise: the shifted form is
            // the character the operator typed.
            KeyCode::Char(character) if name == KeyName::Char => {
                if ctrl {
                    character.to_lowercase().to_string()
                } else {
                    character.to_string()
                }
            }
            _ => String::new(),
        };
        Some(Self {
            name,
            character,
            ctrl,
            shift: event.modifiers.contains(KeyModifiers::SHIFT) || event.code == KeyCode::BackTab,
            meta: event.modifiers.contains(KeyModifiers::ALT),
        })
    }
}

/// Whether a key is a particular Ctrl-letter.
///
/// Ctrl-M, Ctrl-I and Ctrl-H are named `Enter`, `Tab` and `Backspace` instead,
/// so this answers `false` for those three — which is the useful answer: a menu
/// binding Ctrl-H to something is a menu that eats Backspace.
pub fn is_ctrl(input: &Key, letter: char) -> bool {
    input.ctrl && input.name == KeyName::Char && input.character.chars().eq(letter.to_lowercase())
}
