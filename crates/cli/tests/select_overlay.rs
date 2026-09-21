//! A menu drawn over the whole window, for a list that is searched.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture that does not hold is a failing test either way"
)]

use darkwire::select_overlay::SelectOverlay;
use darkwire_tui::{
    Key, KeyName, PLAIN_THEME, Select, SelectItem, SelectLabels, SelectOptions, SelectOutcome,
    strip_ansi,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn overlay(height: u16, count: usize) -> SelectOverlay {
    let select = Select::new(SelectOptions {
        items: (0..count)
            .map(|at| SelectItem::new(at, &format!("session {at}")))
            .collect(),
        labels: SelectLabels {
            title: "Which session?".to_owned(),
            empty: "nothing matches".to_owned(),
            footer: "enter choose · esc cancel".to_owned(),
            filter_prefix: None,
        },
        theme: Some(PLAIN_THEME),
        index: None,
        max_rows: None,
        actions: Vec::new(),
    });
    SelectOverlay::new(select, height)
}

/// Every row it drew, as plain text.
fn screen(subject: &mut SelectOverlay, width: u16, height: u16) -> Vec<String> {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    subject.render(area, &mut buffer);
    (0..height)
        .map(|row| {
            (0..width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .map(|row| strip_ansi(&row))
        .collect()
}

#[test]
fn asks_its_question_at_the_top_and_shows_the_list_under_it() {
    let mut subject = overlay(24, 4);
    let rows = screen(&mut subject, 40, 24);

    assert_eq!(rows[0], "Which session?");
    assert_eq!(rows[1], "", "the question needs a row of air under it");
    assert!(rows[2].starts_with('❯'), "{rows:?}");
    assert!(rows[2].contains("session 0"), "{rows:?}");
    assert!(rows[5].contains("session 3"), "{rows:?}");
    assert!(rows[6].contains("esc cancel"), "{rows:?}");
}

#[test]
fn shows_far_more_rows_than_a_menu_in_the_prompt_could() {
    // The whole point of the placement. A prompt has five rows to spare; a
    // window has as many as it is tall, less the question and the footer.
    let mut subject = overlay(24, 40);
    let rows = screen(&mut subject, 40, 24);
    let shown = rows.iter().filter(|row| row.contains("session ")).count();
    assert!(shown > 15, "only {shown} rows: {rows:?}");
}

#[test]
fn typing_filters_it_and_the_question_carries_what_was_typed() {
    let mut subject = overlay(24, 20);
    for character in ['1', '2'] {
        assert!(matches!(
            subject.handle_key(&Key::char(character)),
            SelectOutcome::Open
        ));
    }
    let rows = screen(&mut subject, 40, 24);

    assert_eq!(rows[0], "Which session? 12");
    assert!(rows[2].contains("session 12"), "{rows:?}");
    assert!(
        !rows.iter().any(|row| row.contains("session 3")),
        "the filter did not apply: {rows:?}"
    );
}

#[test]
fn the_caret_sits_after_the_filter() {
    let mut subject = overlay(24, 4);
    subject.handle_key(&Key::char('s'));
    let area = Rect::new(0, 0, 40, 24);

    // "Which session?" is 14 columns, then the separator, then the one
    // character typed.
    assert_eq!(subject.cursor_pos(area), Some((16, 0)));
}

#[test]
fn enter_chooses_and_escape_gives_up() {
    let mut subject = overlay(24, 4);
    subject.handle_key(&Key::named(KeyName::Down));
    assert!(matches!(
        subject.handle_key(&Key::named(KeyName::Enter)),
        SelectOutcome::Chosen(1)
    ));

    let mut other = overlay(24, 4);
    assert!(matches!(
        other.handle_key(&Key::named(KeyName::Escape)),
        SelectOutcome::Cancelled
    ));
}

#[test]
fn a_window_that_shrank_shows_fewer_rows_rather_than_overflowing() {
    let mut subject = overlay(24, 40);
    subject.resize(8);
    let rows = screen(&mut subject, 40, 8);

    assert_eq!(rows.len(), 8);
    let shown = rows.iter().filter(|row| row.contains("session ")).count();
    assert!(shown <= 5, "{rows:?}");
}
