//! The bridge from styled rows to a Ratatui buffer.
//!
//! Every component in this crate draws a row as a `String` with SGR escapes in
//! it, and so does the turn renderer in the binary, which has to produce the
//! same bytes for a pipe. Ratatui draws styled cells rather than bytes, so
//! something has to turn one into the other. This is that, and it is the only
//! module here that knows Ratatui exists.
//!
//! **It is a bridge, not a destination.** Folding a style into SGR and parsing
//! it straight back out is work nobody needs; the end state is components that
//! produce [`Line`] directly and a pipe path that renders those to SGR once.
//! Keeping the conversion in one function with one entry point is what makes
//! that a later change rather than a rewrite of this one.
//!
//! Anything that is not SGR is dropped. A cursor marker, a window title, a save
//! and restore: none of them mean anything to a cell in a buffer, and a
//! terminal that received them mid-frame would act on them.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::component::CURSOR_MARKER;
use crate::text::{segments, visible_width};

/// The row a component drew, as spans Ratatui can put in a buffer.
///
/// Styles accumulate the way a terminal accumulates them: an opener stays in
/// force until something closes it, and `0` closes everything. A row that ends
/// with a style still open simply ends, because a row is one line and the next
/// one starts clean.
pub fn styled_line(row: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut style = Style::default();

    for segment in segments(row) {
        if segment.ansi {
            if let Some(params) = sgr_params(segment.text) {
                style = apply_sgr(style, &params);
            }
            continue;
        }
        spans.push(Span::styled(segment.text.to_owned(), style));
    }

    Line::from(spans)
}

/// Where the cursor belongs, as a row and a column of `rows`.
///
/// The first marker wins, which is the rule the frame has always used: a
/// component says "here" inline, and a second one would be a component
/// disagreeing with itself.
pub fn cursor_in(rows: &[String]) -> Option<(usize, usize)> {
    rows.iter().enumerate().find_map(|(at, row)| {
        let found = row.find(CURSOR_MARKER)?;
        Some((at, visible_width(row.get(..found).unwrap_or(""))))
    })
}

/// The numeric parameters of an SGR sequence, or `None` for anything else.
///
/// `\x1b[m` is `\x1b[0m` with the zero left out, which is why an empty
/// parameter list answers with one rather than with nothing.
fn sgr_params(escape: &str) -> Option<Vec<u16>> {
    let body = escape.strip_prefix("\x1b[")?.strip_suffix('m')?;
    if body.is_empty() {
        return Some(vec![0]);
    }
    Some(
        body.split(';')
            // A sub-parameter (`4:3`, a curly underline) is the attribute with
            // a variation this cannot draw, so the attribute is what survives.
            .map(|part| part.split(':').next().unwrap_or(part))
            .map(|part| part.parse::<u16>().unwrap_or(0))
            .collect(),
    )
}

/// One SGR sequence applied to the style in force.
fn apply_sgr(style: Style, params: &[u16]) -> Style {
    let mut style = style;
    let mut at = 0;
    while at < params.len() {
        let code = params[at];
        at += 1;
        match code {
            0 => style = Style::default(),
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            5 | 6 => style = style.add_modifier(Modifier::SLOW_BLINK),
            7 => style = style.add_modifier(Modifier::REVERSED),
            8 => style = style.add_modifier(Modifier::HIDDEN),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            // Bold and faint share one closer, which is the whole reason
            // `Style::with_replace` exists in the theme.
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            25 => style = style.remove_modifier(Modifier::SLOW_BLINK),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            28 => style = style.remove_modifier(Modifier::HIDDEN),
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style = style.fg(basic_color(code - 30, false)),
            38 => {
                let (color, used) = extended_color(&params[at..]);
                at += used;
                if let Some(color) = color {
                    style = style.fg(color);
                }
            }
            39 => style = style.fg(Color::Reset),
            40..=47 => style = style.bg(basic_color(code - 40, false)),
            48 => {
                let (color, used) = extended_color(&params[at..]);
                at += used;
                if let Some(color) = color {
                    style = style.bg(color);
                }
            }
            49 => style = style.bg(Color::Reset),
            90..=97 => style = style.fg(basic_color(code - 90, true)),
            100..=107 => style = style.bg(basic_color(code - 100, true)),
            // Unknown, and deliberately not an error: a sequence this does not
            // draw is better dropped than turned into a visible fragment.
            _ => {}
        }
    }
    style
}

/// One of the sixteen, by its offset within its half of the set.
fn basic_color(offset: u16, bright: bool) -> Color {
    match (offset, bright) {
        (0, false) => Color::Black,
        (1, false) => Color::Red,
        (2, false) => Color::Green,
        (3, false) => Color::Yellow,
        (4, false) => Color::Blue,
        (5, false) => Color::Magenta,
        (6, false) => Color::Cyan,
        (0, true) => Color::DarkGray,
        (1, true) => Color::LightRed,
        (2, true) => Color::LightGreen,
        (3, true) => Color::LightYellow,
        (4, true) => Color::LightBlue,
        (5, true) => Color::LightMagenta,
        (6, true) => Color::LightCyan,
        (_, true) => Color::White,
        (_, false) => Color::Gray,
    }
}

/// The colour a `38`/`48` introduces, and how many parameters it consumed.
///
/// A truncated sequence consumes what there is and sets nothing. Terminals
/// differ on what they do with one; drawing no colour is the reading that
/// cannot put the wrong one on screen.
fn extended_color(rest: &[u16]) -> (Option<Color>, usize) {
    match rest.first() {
        Some(5) => match rest.get(1) {
            // The 256-colour cube, which Ratatui indexes the same way.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "an index above 255 is not one"
            )]
            Some(&index) if index <= 255 => (Some(Color::Indexed(index as u8)), 2),
            Some(_) => (None, 2),
            None => (None, 1),
        },
        Some(2) => match (rest.get(1), rest.get(2), rest.get(3)) {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a channel above 255 is not one"
            )]
            (Some(&red), Some(&green), Some(&blue))
                if red <= 255 && green <= 255 && blue <= 255 =>
            {
                (Some(Color::Rgb(red as u8, green as u8, blue as u8)), 4)
            }
            (Some(_), Some(_), Some(_)) => (None, 4),
            _ => (None, rest.len()),
        },
        Some(_) => (None, 1),
        None => (None, 0),
    }
}
