//! Folding a styled line to a width.

use darkwire_tui::wrap::{leading_whitespace, line_width, wrap_line, wrap_lines, wrapped_height};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The text of each folded row, styles dropped.
fn texts(rows: &[Line<'static>]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn plain(text: &str) -> Line<'static> {
    Line::from(text.to_owned())
}

#[test]
fn a_line_that_fits_is_one_row() {
    let line = plain("short enough");
    assert_eq!(texts(&wrap_line(&line, 20, "")), vec!["short enough"]);
}

#[test]
fn a_line_folds_at_a_space() {
    let line = plain("one two three four");
    assert_eq!(
        texts(&wrap_line(&line, 8, "")),
        vec!["one two", "three", "four"]
    );
}

#[test]
fn the_space_a_fold_replaces_is_dropped() {
    let rows = wrap_line(&plain("aaa bbb"), 4, "");
    assert_eq!(texts(&rows), vec!["aaa", "bbb"]);
}

#[test]
fn a_word_wider_than_the_window_is_broken_rather_than_lost() {
    let rows = wrap_line(&plain("abcdefghij"), 4, "");
    assert_eq!(texts(&rows), vec!["abcd", "efgh", "ij"]);
}

#[test]
fn nothing_is_lost_when_a_line_folds() {
    let text = "the quick brown fox jumps over the lazy dog";
    let rows = wrap_line(&plain(text), 11, "");
    let rejoined = texts(&rows).join(" ");
    assert_eq!(rejoined, text);
}

#[test]
fn a_style_survives_the_fold_it_crosses() {
    let line = Line::from(vec![Span::styled(
        "green words here",
        Style::default().fg(Color::Green),
    )]);
    let rows = wrap_line(&line, 6, "");
    assert!(rows.len() > 1, "expected a fold, got {:?}", texts(&rows));
    for row in &rows {
        for span in &row.spans {
            assert_eq!(
                span.style.fg,
                Some(Color::Green),
                "a fold dropped the colour: {:?}",
                texts(&rows)
            );
        }
    }
}

#[test]
fn a_fold_inside_a_span_keeps_both_halves_styled() {
    let line = Line::from(vec![
        Span::raw("plain "),
        Span::styled("bold text", Style::default().add_modifier(Modifier::BOLD)),
    ]);
    let rows = wrap_line(&line, 8, "");
    assert_eq!(texts(&rows), vec!["plain", "bold", "text"]);
    for row in rows.iter().skip(1) {
        for span in &row.spans {
            assert!(
                span.style.add_modifier.contains(Modifier::BOLD),
                "the bold span lost its weight after the fold"
            );
        }
    }
}

#[test]
fn clusters_of_one_style_become_one_span() {
    let line = Line::from(vec![Span::styled(
        "aaaa bbbb",
        Style::default().fg(Color::Red),
    )]);
    let rows = wrap_line(&line, 4, "");
    for row in &rows {
        assert_eq!(
            row.spans.len(),
            1,
            "a row of one style should be one span: {row:?}"
        );
    }
}

#[test]
fn the_rows_after_the_first_start_under_the_first_ones_text() {
    let line = plain("    indented text that folds");
    let indent = leading_whitespace(&line);
    assert_eq!(indent, "    ");
    let rows = wrap_line(&line, 12, &indent);
    assert!(
        rows.iter()
            .skip(1)
            .all(|row| texts(std::slice::from_ref(row))[0].starts_with("    ")),
        "continuation rows lost the indent: {:?}",
        texts(&rows)
    );
}

#[test]
fn a_width_of_zero_gives_the_line_back_rather_than_looping() {
    let line = plain("anything at all");
    assert_eq!(texts(&wrap_line(&line, 0, "")), vec!["anything at all"]);
}

#[test]
fn a_wide_glyph_is_never_split_down_the_middle() {
    // Each of these is two columns, so three of them do not fit in five.
    let line = plain("日本語");
    let rows = wrap_line(&line, 5, "");
    assert_eq!(texts(&rows), vec!["日本", "語"]);
}

#[test]
fn a_line_wider_than_the_window_reports_the_rows_it_needs() {
    assert_eq!(wrapped_height(&plain("one two three"), 6), 3);
    assert_eq!(wrapped_height(&plain("short"), 40), 1);
}

#[test]
fn every_line_folds_in_turn_and_in_order() {
    let lines = vec![plain("aaa bbb"), plain("ccc")];
    assert_eq!(texts(&wrap_lines(&lines, 4)), vec!["aaa", "bbb", "ccc"]);
}

#[test]
fn the_width_of_a_line_is_the_columns_it_draws_in() {
    assert_eq!(line_width(&plain("abc")), 3);
    assert_eq!(line_width(&plain("日本")), 4);
    assert_eq!(
        line_width(&Line::from(vec![Span::raw("ab"), Span::raw("cd")])),
        4
    );
}

#[test]
fn a_line_of_nothing_is_one_empty_row() {
    assert_eq!(texts(&wrap_line(&plain(""), 10, "")), vec![""]);
}

#[test]
fn no_folded_row_is_wider_than_the_window_it_was_folded_for() {
    // A run of spaces in the middle of a line used to ride past the right
    // edge, because a space that does not fit is allowed to overhang. The row
    // then arrived at the terminal wider than the window, the terminal folded
    // it again, and every count taken on this side was short.
    let padded = format!("start{}continues here", " ".repeat(30));
    for width in [10u16, 20, 40] {
        let rows = wrap_line(&plain(&padded), width, "");
        for row in &rows {
            assert!(
                line_width(row) <= usize::from(width),
                "a row of {} columns came back for a window of {width}: {:?}",
                line_width(row),
                texts(&rows)
            );
        }
    }
}

#[test]
fn a_line_that_ends_in_spaces_does_not_end_wider_than_the_window() {
    let trailing = format!("{}{}", "word ".repeat(4), " ".repeat(40));
    let rows = wrap_line(&plain(&trailing), 12, "");
    for row in &rows {
        assert!(
            line_width(row) <= 12,
            "a row of {} columns: {:?}",
            line_width(row),
            texts(&rows)
        );
    }
}

#[test]
fn the_words_around_a_run_of_spaces_all_survive() {
    let padded = format!("before{}after", " ".repeat(30));
    let rows = wrap_line(&plain(&padded), 12, "");
    let seen = texts(&rows).join(" ");

    assert!(seen.contains("before"), "{seen:?}");
    assert!(seen.contains("after"), "{seen:?}");
}
