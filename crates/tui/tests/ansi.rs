//! SGR in, Ratatui spans out.

use darkwire_tui::{CURSOR_MARKER, cursor_in, styled_line, theme_for};
use ratatui::style::{Color, Modifier, Style};

/// The style each span carries, paired with its text.
fn spans(row: &str) -> Vec<(String, Style)> {
    styled_line(row)
        .spans
        .iter()
        .map(|span| (span.content.to_string(), span.style))
        .collect()
}

/// Everything visible in the row, with the styling dropped.
fn text(row: &str) -> String {
    styled_line(row)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn plain_text_is_one_unstyled_span() {
    assert_eq!(spans("hello"), vec![("hello".to_owned(), Style::default())]);
}

#[test]
fn an_empty_row_has_no_spans() {
    assert!(styled_line("").spans.is_empty());
}

#[test]
fn a_colour_opens_and_a_close_ends_it() {
    assert_eq!(
        spans("\x1b[32mgo\x1b[39mstop"),
        vec![
            ("go".to_owned(), Style::default().fg(Color::Green)),
            ("stop".to_owned(), Style::default().fg(Color::Reset)),
        ]
    );
}

#[test]
fn a_reset_ends_every_attribute_at_once() {
    let spans = spans("\x1b[1m\x1b[31mloud\x1b[0mquiet");
    assert_eq!(spans[1], ("quiet".to_owned(), Style::default()));
}

#[test]
fn an_empty_parameter_list_is_a_reset() {
    let spans = spans("\x1b[1mbold\x1b[mplain");
    assert_eq!(spans[1].1, Style::default());
}

#[test]
fn bold_and_faint_share_one_closer() {
    let spans = spans("\x1b[1m\x1b[2mboth\x1b[22mneither");
    assert!(spans[0].1.add_modifier.contains(Modifier::BOLD));
    assert!(spans[0].1.add_modifier.contains(Modifier::DIM));
    assert!(!spans[1].1.add_modifier.contains(Modifier::BOLD));
    assert!(!spans[1].1.add_modifier.contains(Modifier::DIM));
}

#[test]
fn every_attribute_has_a_closer_that_leaves_the_others_alone() {
    for (open, close, modifier) in [
        ("\x1b[3m", "\x1b[23m", Modifier::ITALIC),
        ("\x1b[4m", "\x1b[24m", Modifier::UNDERLINED),
        ("\x1b[7m", "\x1b[27m", Modifier::REVERSED),
        ("\x1b[8m", "\x1b[28m", Modifier::HIDDEN),
        ("\x1b[9m", "\x1b[29m", Modifier::CROSSED_OUT),
    ] {
        let row = format!("\x1b[1m{open}on{close}off");
        let spans = spans(&row);
        assert!(spans[0].1.add_modifier.contains(modifier), "{open}");
        assert!(!spans[1].1.add_modifier.contains(modifier), "{close}");
        // The bold around it survives its neighbour closing.
        assert!(spans[1].1.add_modifier.contains(Modifier::BOLD), "{close}");
    }
}

#[test]
fn the_bright_set_is_its_own_eight_colours() {
    assert_eq!(spans("\x1b[90mgrey")[0].1.fg, Some(Color::DarkGray));
    assert_eq!(spans("\x1b[97mwhite")[0].1.fg, Some(Color::White));
    assert_eq!(spans("\x1b[100mon")[0].1.bg, Some(Color::DarkGray));
}

#[test]
fn a_background_is_not_a_foreground() {
    let style = spans("\x1b[41mred")[0].1;
    assert_eq!(style.bg, Some(Color::Red));
    assert_eq!(style.fg, None);
    assert_eq!(spans("\x1b[41m\x1b[49mnone")[0].1.bg, Some(Color::Reset));
}

#[test]
fn the_two_hundred_and_fifty_six_colour_form_is_an_index() {
    assert_eq!(
        spans("\x1b[38;5;201mpink")[0].1.fg,
        Some(Color::Indexed(201))
    );
    assert_eq!(spans("\x1b[48;5;17mnavy")[0].1.bg, Some(Color::Indexed(17)));
}

#[test]
fn the_truecolour_form_is_three_channels() {
    assert_eq!(
        spans("\x1b[38;2;12;34;56mexact")[0].1.fg,
        Some(Color::Rgb(12, 34, 56))
    );
}

#[test]
fn an_extended_colour_does_not_swallow_what_follows_it() {
    let style = spans("\x1b[38;5;201;1mboth")[0].1;
    assert_eq!(style.fg, Some(Color::Indexed(201)));
    assert!(style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn a_truncated_extended_colour_sets_nothing_and_keeps_the_text() {
    assert_eq!(spans("\x1b[38;5mtext")[0].1.fg, None);
    assert_eq!(spans("\x1b[38;2;1;2mtext")[0].1.fg, None);
    assert_eq!(text("\x1b[38;5mtext"), "text");
}

#[test]
fn a_sub_parameter_keeps_the_attribute_it_varies() {
    // `4:3` is a curly underline. This cannot draw the curl; it draws the
    // underline rather than nothing.
    assert!(
        spans("\x1b[4:3mwavy")[0]
            .1
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
}

#[test]
fn a_code_it_does_not_know_changes_nothing_and_is_not_drawn() {
    assert_eq!(
        spans("\x1b[73mup"),
        vec![("up".to_owned(), Style::default())]
    );
}

#[test]
fn everything_that_is_not_a_colour_sequence_is_dropped() {
    // A window title, a save and restore, and the cursor marker: none of them
    // mean anything to a cell, and a terminal that got one mid-frame would act
    // on it.
    let row = format!("\x1b]0;title\x07a\x1b7b\x1b8c{CURSOR_MARKER}d");
    assert_eq!(text(&row), "abcd");
}

#[test]
fn a_theme_role_survives_the_round_trip() {
    let theme = theme_for(Some(true));
    let row = theme.accent.apply("go");
    assert_eq!(
        spans(&row),
        vec![("go".to_owned(), Style::default().fg(Color::Green))]
    );
}

#[test]
fn a_nested_close_leaves_the_outer_style_standing() {
    // What `Style::apply` builds: the inner close is rewritten as the outer
    // opener, so the row that reaches here has the outer colour twice.
    let theme = theme_for(Some(true));
    let row = theme
        .cursor
        .apply(&format!("a{}b", theme.match_.apply("hit")));
    let spans = spans(&row);
    assert_eq!(
        spans[0],
        ("a".to_owned(), Style::default().fg(Color::Green))
    );
    assert_eq!(
        spans[1],
        ("hit".to_owned(), Style::default().fg(Color::Yellow))
    );
    assert_eq!(
        spans[2],
        ("b".to_owned(), Style::default().fg(Color::Green))
    );
}

#[test]
fn the_cursor_is_where_the_marker_is() {
    let rows = vec!["plain".to_owned(), format!("› typed{CURSOR_MARKER} more")];
    assert_eq!(cursor_in(&rows), Some((1, 7)));
}

#[test]
fn the_cursor_column_is_measured_in_columns_not_bytes() {
    let rows = vec![format!("\x1b[32m›\x1b[39m 日本{CURSOR_MARKER}")];
    assert_eq!(cursor_in(&rows), Some((0, 6)));
}

#[test]
fn the_first_marker_wins() {
    let rows = vec![format!("a{CURSOR_MARKER}"), format!("bb{CURSOR_MARKER}")];
    assert_eq!(cursor_in(&rows), Some((0, 1)));
}

#[test]
fn no_marker_is_no_cursor() {
    assert_eq!(cursor_in(&["nothing here".to_owned()]), None);
    assert_eq!(cursor_in(&[]), None);
}
