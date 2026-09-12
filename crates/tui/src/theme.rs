//! Colour, as seven named roles rather than seven colour names.
//!
//! A [`Theme`] is the vocabulary a component draws with — `text`, `dim`,
//! `cursor`, `match_`, `title`, `accent`, `warn` — and a [`Palette`] is the
//! sixteen-colour SGR set a theme is built from. Colour is one boolean over
//! the whole palette: with it off every [`Style`] is the identity, which is
//! what lets a test assert on text instead of on escape sequences, and why
//! `--no-color` is one flag rather than a branch at every call site.
//!
//! The one rule worth stating out loud: **colour is never the only signal.** A
//! selected row is marked by a leading glyph, and `theme.cursor` only makes
//! the mark easier to find. Under `NO_COLOR` every formatter here is the
//! identity, and a menu whose selection was indicated by colour alone would
//! become unusable rather than merely plainer — which is also what makes
//! [`PLAIN_THEME`] a meaningful thing to test against.
//!
//! A style is an opener, a closer, and a rule for a nested close: a `dim` hint
//! inside a `cursor` row closes with `\x1b[39m`, which would also end the green
//! around it, so every close of the outer style found inside the text is
//! replaced by the outer style's opener.

use std::io::IsTerminal;

/// One way of marking text: an opener, its closer, and what to put in place
/// of a closer found inside the text so the outer style survives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    open: &'static str,
    close: &'static str,
    replace: &'static str,
}

impl Style {
    /// The style that changes nothing. What a pipe and every test get.
    pub const IDENTITY: Self = Self {
        open: "",
        close: "",
        replace: "",
    };

    /// A style whose nested close is replaced by its own opener.
    const fn new(open: &'static str, close: &'static str) -> Self {
        Self {
            open,
            close,
            replace: open,
        }
    }

    /// A style whose nested close is replaced by something other than the
    /// opener — bold and faint share `\x1b[22m`, so re-opening one after a
    /// nested close of the other needs both sequences.
    const fn with_replace(open: &'static str, close: &'static str, replace: &'static str) -> Self {
        Self {
            open,
            close,
            replace,
        }
    }

    /// Whether this style emits anything at all.
    pub fn is_identity(&self) -> bool {
        self.open.is_empty()
    }

    /// The text, styled.
    pub fn apply(&self, text: &str) -> String {
        if self.is_identity() {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len() + self.open.len() + self.close.len());
        out.push_str(self.open);
        out.push_str(&text.replace(self.close, self.replace));
        out.push_str(self.close);
        out
    }
}

/// The SGR set, in the shape the theme reads.
///
/// Every field is a [`Style`]; with colour off every one of them is
/// [`Style::IDENTITY`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Whether the styles below emit anything.
    pub is_color_supported: bool,
    /// SGR 0 on both ends.
    pub reset: Style,
    /// SGR 1.
    pub bold: Style,
    /// The secondary-text role. Bright black, not SGR 2 — see [`palette_for`].
    pub dim: Style,
    /// SGR 3.
    pub italic: Style,
    /// SGR 4.
    pub underline: Style,
    /// SGR 7.
    pub inverse: Style,
    /// SGR 8.
    pub hidden: Style,
    /// SGR 9.
    pub strikethrough: Style,
    /// SGR 30.
    pub black: Style,
    /// SGR 31.
    pub red: Style,
    /// SGR 32.
    pub green: Style,
    /// SGR 33.
    pub yellow: Style,
    /// SGR 34.
    pub blue: Style,
    /// SGR 35.
    pub magenta: Style,
    /// SGR 36.
    pub cyan: Style,
    /// SGR 37.
    pub white: Style,
    /// SGR 90, bright black.
    pub gray: Style,
}

const COLOR_CLOSE: &str = "\x1b[39m";

/// Every style the identity.
const PLAIN_PALETTE: Palette = Palette {
    is_color_supported: false,
    reset: Style::IDENTITY,
    bold: Style::IDENTITY,
    dim: Style::IDENTITY,
    italic: Style::IDENTITY,
    underline: Style::IDENTITY,
    inverse: Style::IDENTITY,
    hidden: Style::IDENTITY,
    strikethrough: Style::IDENTITY,
    black: Style::IDENTITY,
    red: Style::IDENTITY,
    green: Style::IDENTITY,
    yellow: Style::IDENTITY,
    blue: Style::IDENTITY,
    magenta: Style::IDENTITY,
    cyan: Style::IDENTITY,
    white: Style::IDENTITY,
    gray: Style::IDENTITY,
};

/// The sixteen-colour palette as a terminal draws it.
const COLOR_PALETTE: Palette = Palette {
    is_color_supported: true,
    reset: Style::new("\x1b[0m", "\x1b[0m"),
    bold: Style::with_replace("\x1b[1m", "\x1b[22m", "\x1b[22m\x1b[1m"),
    // Rebound to bright black; `palette_for` says why.
    dim: Style::new("\x1b[90m", COLOR_CLOSE),
    italic: Style::new("\x1b[3m", "\x1b[23m"),
    underline: Style::new("\x1b[4m", "\x1b[24m"),
    inverse: Style::new("\x1b[7m", "\x1b[27m"),
    hidden: Style::new("\x1b[8m", "\x1b[28m"),
    strikethrough: Style::new("\x1b[9m", "\x1b[29m"),
    black: Style::new("\x1b[30m", COLOR_CLOSE),
    red: Style::new("\x1b[31m", COLOR_CLOSE),
    green: Style::new("\x1b[32m", COLOR_CLOSE),
    yellow: Style::new("\x1b[33m", COLOR_CLOSE),
    blue: Style::new("\x1b[34m", COLOR_CLOSE),
    magenta: Style::new("\x1b[35m", COLOR_CLOSE),
    cyan: Style::new("\x1b[36m", COLOR_CLOSE),
    white: Style::new("\x1b[37m", COLOR_CLOSE),
    gray: Style::new("\x1b[90m", COLOR_CLOSE),
};

/// Whether the process should colour its output when nobody said.
///
/// `NO_COLOR` or `--no-color` wins outright. Otherwise `FORCE_COLOR`,
/// `--color`, Windows (whose console always draws colour), a `CI` variable, or
/// standard output being a terminal whose `TERM` is not `dumb`.
fn color_supported() -> bool {
    let has = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let flag = |wanted: &str| std::env::args().any(|argument| argument == wanted);

    if has("NO_COLOR") || flag("--no-color") {
        return false;
    }
    has("FORCE_COLOR")
        || flag("--color")
        || cfg!(windows)
        || has("CI")
        || (std::io::stdout().is_terminal()
            && std::env::var_os("TERM").as_deref() != Some("dumb".as_ref()))
}

/// The palette for a colour decision, with `dim` bound to bright black
/// instead of faint.
///
/// `dim` is SGR 2, and SGR 2 is **optional** in ECMA-48. The Linux kernel
/// console does not implement it, PuTTY does not implement it, and several
/// other emulators quietly drop it — so on all of them the hints, the header
/// labels and both status rows drew at exactly the weight of ordinary prose.
/// Those terminals do colour perfectly well. They do not do that one
/// attribute, and this crate has no capability detection to notice: colour
/// here is one boolean over sixteen named colours, so the answer is to spend
/// the role on something every terminal can draw rather than to probe for one
/// only some can.
///
/// SGR 90 is bright black — a *colour* rather than an attribute, and an
/// aixterm extension implemented essentially everywhere colour is at all. The
/// swap is strictly no worse than what it replaces: a terminal that does not
/// know 90 draws plain text, which is precisely what a terminal that does not
/// know 2 already draws today.
///
/// Bound here rather than at the call sites because a decision spread over
/// forty places is one a fresh call site gets wrong by typing the obvious
/// thing. The plain path is untouched: with colour off every style is the
/// identity, `gray` included, so the rebinding is a no-op and [`PLAIN_THEME`]
/// still describes what a pipe gets.
///
/// `None` detects: it honours `NO_COLOR`, `FORCE_COLOR`, `CI`, the
/// `--color`/`--no-color` arguments and whether standard output is a
/// terminal — so a caller that has no opinion should pass nothing rather than
/// guess.
pub fn palette_for(colors: Option<bool>) -> Palette {
    if colors.unwrap_or_else(color_supported) {
        COLOR_PALETTE
    } else {
        PLAIN_PALETTE
    }
}

/// The roles a component draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// An ordinary row.
    pub text: Style,
    /// Secondary information: hints, counts, the footer.
    pub dim: Style,
    /// The row the cursor is on.
    pub cursor: Style,
    /// The span of a label the filter matched.
    pub match_: Style,
    /// A heading.
    pub title: Style,
    /// The product's own colour, for the few marks that are always ours.
    ///
    /// Deliberately few: the caret, the banner, and the row a menu is sitting
    /// on. Colour that appears everywhere stops meaning anything, and a
    /// transcript is mostly somebody else's words.
    pub accent: Style,
    /// Something the operator should notice.
    pub warn: Style,
}

/// Every role is the identity. What a pipe and every test get.
pub const PLAIN_THEME: Theme = Theme {
    text: Style::IDENTITY,
    dim: Style::IDENTITY,
    cursor: Style::IDENTITY,
    match_: Style::IDENTITY,
    title: Style::IDENTITY,
    accent: Style::IDENTITY,
    warn: Style::IDENTITY,
};

/// The roles, over a palette.
///
/// `cursor` is green rather than an inverse video block: inverse spans the
/// padded column width, so a row's highlight would be as wide as the longest
/// label rather than as wide as the label — which reads as a ragged rectangle.
/// The glyph carries the position; the colour only has to draw the eye to it.
///
/// Green because it is the one colour the CLI claims for itself, and it is
/// spent on three things only: the caret, the banner, and the selected row.
/// Everything else is the terminal's own foreground or dim — a transcript is
/// mostly the model's words and the operator's, and neither of those is ours
/// to paint.
///
/// `dim` reads `gray` rather than the palette's own `dim` slot, so a caller
/// that built a palette some other way lands on bright black too. The two
/// routes in cannot disagree about the one attribute that does not render —
/// see [`palette_for`] for why that is the whole point.
pub fn theme_from(palette: &Palette) -> Theme {
    Theme {
        text: palette.reset,
        dim: palette.gray,
        cursor: palette.green,
        match_: palette.yellow,
        title: palette.bold,
        accent: palette.green,
        warn: palette.yellow,
    }
}

/// The roles for a given colour decision; `None` detects, as [`palette_for`]
/// does.
pub fn theme_for(colors: Option<bool>) -> Theme {
    theme_from(&palette_for(colors))
}
