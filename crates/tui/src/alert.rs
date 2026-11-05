//! Telling the person at a terminal that something is waiting for them.
//!
//! A terminal has two ways, and which works depends on the terminal. The bell
//! works everywhere: the terminal turns it into a sound, a badge on the tab or a
//! bounce in the dock, and tmux passes it on to its window list. A desktop
//! notification says what the matter is, but only some terminals show one, and
//! some of the rest print an OSC they do not know as text. So a notification
//! goes only to a terminal that is known to show it, and everything else rings.

use crate::text::strip_ansi;

/// How a terminal can be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    /// A desktop notification by OSC 9: iTerm2, WezTerm and Ghostty.
    Osc9,
    /// A desktop notification by OSC 99, Kitty's own protocol.
    Osc99,
    /// The bell.
    Bell,
    /// Nothing.
    Off,
}

impl Alert {
    /// The way the terminal this runs in can be told, from its environment.
    ///
    /// The environment is passed in rather than read, so a test can say which
    /// terminal it is.
    ///
    /// Under tmux the outer terminal is not the one in `TERM_PROGRAM`, and an
    /// OSC needs passthrough turned on to reach it at all. The bell needs
    /// nothing.
    pub fn detect(env: impl Fn(&str) -> Option<String>) -> Alert {
        if env("TMUX").is_some_and(|value| !value.is_empty()) {
            return Alert::Bell;
        }
        if env("TERM").as_deref() == Some("xterm-kitty") || env("KITTY_WINDOW_ID").is_some() {
            return Alert::Osc99;
        }
        match env("TERM_PROGRAM").as_deref() {
            Some("iTerm.app" | "WezTerm" | "ghostty") => Alert::Osc9,
            _ => Alert::Bell,
        }
    }

    /// The bytes that say `text` this way.
    ///
    /// The text is the caller's, and a control character in it could end the
    /// sequence early and write the rest to the screen, so they are dropped.
    #[must_use]
    pub fn sequence(self, text: &str) -> Vec<u8> {
        let text: String = strip_ansi(text)
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        match self {
            Alert::Osc9 => format!("\x1b]9;{text}\x1b\\").into_bytes(),
            Alert::Osc99 => format!("\x1b]99;;{text}\x1b\\").into_bytes(),
            Alert::Bell => b"\x07".to_vec(),
            Alert::Off => Vec::new(),
        }
    }
}
