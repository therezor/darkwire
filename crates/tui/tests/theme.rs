//! Seven roles over a palette, the identity under no colour, and the bytes that come out.

use ghostai_tui::{PLAIN_THEME, Style, Theme, palette_for, strip_ansi, theme_for, theme_from};

/// SGR 2, "faint" — the attribute this crate deliberately does not use.
const FAINT: &str = "\x1b[2m";
/// SGR 90, bright black — the colour it uses instead.
const BRIGHT_BLACK: &str = "\x1b[90m";

fn roles(theme: &Theme) -> [Style; 7] {
    [
        theme.text,
        theme.dim,
        theme.cursor,
        theme.match_,
        theme.title,
        theme.accent,
        theme.warn,
    ]
}

#[test]
fn plain_theme_is_the_identity_in_every_role() {
    // What a pipe, `--no-color` and every test get. It is also why an
    // assertion can be written against the text rather than escape sequences.
    for role in roles(&PLAIN_THEME) {
        assert_eq!(role.apply("hello"), "hello");
        assert!(role.is_identity());
    }
}

#[test]
fn theme_from_fills_every_role_from_the_palette() {
    let theme = theme_from(&palette_for(Some(true)));
    for role in roles(&theme) {
        assert_eq!(strip_ansi(&role.apply("hello")), "hello");
        assert!(!role.is_identity());
    }
}

#[test]
fn theme_from_distinguishes_the_cursor_from_an_ordinary_row() {
    let theme = theme_from(&palette_for(Some(true)));
    assert_ne!(theme.cursor.apply("x"), theme.text.apply("x"));
}

#[test]
fn theme_for_produces_plain_text_when_colour_is_off() {
    for role in roles(&theme_for(Some(false))) {
        assert_eq!(role.apply("hello"), "hello");
    }
    assert!(!palette_for(Some(false)).is_color_supported);
}

#[test]
fn theme_for_produces_escape_sequences_when_colour_is_on() {
    assert_ne!(theme_for(Some(true)).cursor.apply("hello"), "hello");
    assert!(palette_for(Some(true)).is_color_supported);
}

#[test]
fn theme_for_detects_when_nobody_decided() {
    // Whatever the environment says, the text survives and both routes agree.
    let detected = theme_for(None);
    for role in roles(&detected) {
        assert_eq!(strip_ansi(&role.apply("hello")), "hello");
    }
    assert_eq!(detected, theme_from(&palette_for(None)));
}

#[test]
fn the_accent_is_the_cursor_colour_and_nothing_else_is() {
    // The caret, the banner and the selected row. Colour that appears
    // everywhere stops meaning anything, and a transcript is mostly somebody
    // else's words.
    let theme = theme_for(Some(true));
    assert_ne!(theme.accent.apply("x"), "x");
    assert_eq!(theme.accent.apply("x"), theme.cursor.apply("x"));
    assert_eq!(theme_for(Some(false)).accent.apply("x"), "x");
}

#[test]
fn secondary_text_is_a_colour_rather_than_the_faint_attribute() {
    // SGR 2 is optional in ECMA-48, and the Linux kernel console and PuTTY are
    // two of the terminals that do not implement it: colour worked there and
    // every hint, header label and status row drew at the weight of ordinary
    // prose. SGR 90 is a colour, and a terminal that ignores it draws plain
    // text — which is what those terminals were already doing.
    let dim = palette_for(Some(true)).dim.apply("x");
    assert!(dim.contains(BRIGHT_BLACK));
    assert!(!dim.contains(FAINT));
}

#[test]
fn secondary_text_reaches_the_theme_by_both_routes() {
    assert!(theme_for(Some(true)).dim.apply("x").contains(BRIGHT_BLACK));
    assert!(
        theme_from(&palette_for(Some(true)))
            .dim
            .apply("x")
            .contains(BRIGHT_BLACK)
    );
    assert!(!theme_for(Some(true)).dim.apply("x").contains(FAINT));
}

#[test]
fn the_plain_path_is_left_alone() {
    let plain = palette_for(Some(false));
    assert_eq!(plain.dim.apply("hello"), "hello");
    assert_eq!(plain.green.apply("hello"), "hello");
    assert_eq!(theme_for(Some(false)).dim.apply("hello"), "hello");
}

#[test]
fn the_bytes_are_the_familiar_ones() {
    let palette = palette_for(Some(true));
    assert_eq!(palette.green.apply("x"), "\x1b[32mx\x1b[39m");
    assert_eq!(palette.yellow.apply("x"), "\x1b[33mx\x1b[39m");
    assert_eq!(palette.bold.apply("x"), "\x1b[1mx\x1b[22m");
    assert_eq!(palette.reset.apply("x"), "\x1b[0mx\x1b[0m");
    assert_eq!(palette.red.apply("x"), "\x1b[31mx\x1b[39m");
    assert_eq!(palette.gray.apply("x"), "\x1b[90mx\x1b[39m");
}

#[test]
fn a_nested_close_reopens_the_outer_style() {
    // A dim hint inside a green cursor row closes with `\x1b[39m`, which would
    // end the green too. The outer style's close, found inside the text, is
    // replaced by the outer style's opener.
    let palette = palette_for(Some(true));
    let inner = palette.gray.apply("hint");
    let outer = palette.green.apply(&format!("label {inner}"));
    assert_eq!(outer, "\x1b[32mlabel \x1b[90mhint\x1b[32m\x1b[39m");
    // Bold re-opens with both sequences, because faint shares its close.
    let nested_bold = palette
        .bold
        .apply(&format!("a{}b", palette.bold.apply("x")));
    assert_eq!(nested_bold, "\x1b[1ma\x1b[1mx\x1b[22m\x1b[1mb\x1b[22m");
}
