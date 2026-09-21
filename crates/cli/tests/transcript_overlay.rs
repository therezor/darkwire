//! Everything that was said, over the whole window.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture that does not hold is a failing test either way"
)]

use darkwire::transcript_overlay::{OverlayOutcome, TranscriptOverlay};
use darkwire_tui::{HistoryCell, Key, KeyName, PLAIN_THEME, Renderable, strip_ansi};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

/// A cell of numbered rows, hiding nothing.
struct Rows(usize);

impl HistoryCell for Rows {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        (0..self.0)
            .map(|at| Line::from(format!("row {at}")))
            .collect()
    }
}

/// A cell that shows one row and holds three.
struct Folded;

impl HistoryCell for Folded {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![Line::from("summary".to_owned())]
    }

    fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![
            Line::from("summary".to_owned()),
            Line::from("hidden one".to_owned()),
            Line::from("hidden two".to_owned()),
        ]
    }
}

fn overlay(cells: &[Box<dyn HistoryCell>]) -> TranscriptOverlay {
    TranscriptOverlay::new(cells, 40, &PLAIN_THEME, "escape to close")
}

/// The rows the overlay draws into a window `height` tall.
fn drawn(overlay: &TranscriptOverlay, height: u16) -> Vec<String> {
    let area = Rect::new(0, 0, 40, height);
    let mut buffer = Buffer::empty(area);
    overlay.render(area, &mut buffer);
    (0..height)
        .map(|row| {
            strip_ansi(
                &(0..40)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>(),
            )
            .trim_end()
            .to_owned()
        })
        .collect()
}

#[test]
fn it_shows_what_the_screen_folded_away() {
    let overlay = overlay(&[Box::new(Folded)]);
    let rows = drawn(&overlay, 5);

    assert!(rows.iter().any(|row| row == "hidden one"), "{rows:?}");
    assert!(rows.iter().any(|row| row == "hidden two"), "{rows:?}");
}

#[test]
fn it_opens_at_the_end_because_that_is_what_was_just_read() {
    let overlay = overlay(&[Box::new(Rows(20))]);
    let rows = drawn(&overlay, 6);

    assert!(rows.iter().any(|row| row == "row 19"), "{rows:?}");
    assert!(!rows.iter().any(|row| row == "row 0"), "{rows:?}");
}

#[test]
fn page_up_moves_back_and_page_down_moves_on() {
    let mut overlay = overlay(&[Box::new(Rows(30))]);
    assert_eq!(
        overlay.handle_key(&Key::named(KeyName::PageUp), 6),
        OverlayOutcome::Open
    );
    let back = drawn(&overlay, 6);

    overlay.handle_key(&Key::named(KeyName::PageDown), 6);
    let forward = drawn(&overlay, 6);

    assert_ne!(back, forward, "the page keys did nothing");
}

#[test]
fn the_arrows_move_one_row_at_a_time() {
    let mut overlay = overlay(&[Box::new(Rows(30))]);
    let before = drawn(&overlay, 6);

    overlay.handle_key(&Key::named(KeyName::Up), 6);
    let after = drawn(&overlay, 6);

    assert_ne!(before, after);
}

#[test]
fn it_will_not_scroll_past_either_end() {
    let mut overlay = overlay(&[Box::new(Rows(30))]);
    for _ in 0..50 {
        overlay.handle_key(&Key::named(KeyName::PageUp), 6);
    }
    assert!(drawn(&overlay, 6).iter().any(|row| row == "row 0"));

    for _ in 0..50 {
        overlay.handle_key(&Key::named(KeyName::PageDown), 6);
    }
    assert!(drawn(&overlay, 6).iter().any(|row| row == "row 29"));
}

#[test]
fn home_and_end_go_to_the_ends() {
    let mut overlay = overlay(&[Box::new(Rows(30))]);

    overlay.handle_key(&Key::named(KeyName::Home), 6);
    assert!(drawn(&overlay, 6).iter().any(|row| row == "row 0"));

    overlay.handle_key(&Key::named(KeyName::End), 6);
    assert!(drawn(&overlay, 6).iter().any(|row| row == "row 29"));
}

#[test]
fn escape_closes_it() {
    let mut overlay = overlay(&[Box::new(Rows(4))]);
    assert_eq!(
        overlay.handle_key(&Key::named(KeyName::Escape), 6),
        OverlayOutcome::Closed
    );
}

#[test]
fn q_closes_it() {
    let mut overlay = overlay(&[Box::new(Rows(4))]);
    assert_eq!(
        overlay.handle_key(&Key::char('q'), 6),
        OverlayOutcome::Closed
    );
}

#[test]
fn the_key_that_opened_it_closes_it() {
    let mut overlay = overlay(&[Box::new(Rows(4))]);
    assert_eq!(
        overlay.handle_key(&Key::ctrl('t'), 6),
        OverlayOutcome::Closed
    );
}

#[test]
fn a_q_typed_as_a_chord_is_not_a_close() {
    let mut overlay = overlay(&[Box::new(Rows(4))]);
    assert_eq!(overlay.handle_key(&Key::ctrl('q'), 6), OverlayOutcome::Open);
}

#[test]
fn the_footer_says_how_to_leave() {
    let overlay = overlay(&[Box::new(Rows(2))]);
    let rows = drawn(&overlay, 6);

    assert_eq!(rows[5], "escape to close");
}

#[test]
fn a_conversation_that_said_nothing_still_draws_its_footer() {
    let overlay = overlay(&[]);

    assert!(overlay.is_empty());
    assert_eq!(drawn(&overlay, 4)[3], "escape to close");
}

#[test]
fn a_row_wider_than_the_window_is_folded_rather_than_cut() {
    struct Wide;
    impl HistoryCell for Wide {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            vec![Line::from("word ".repeat(20))]
        }
    }

    let overlay = overlay(&[Box::new(Wide)]);
    assert!(overlay.len() > 1, "the row was not folded");
}
