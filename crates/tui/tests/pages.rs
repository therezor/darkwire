//! The tabbed overlay: what a key does to it, and what it draws.

mod common;

use darkwire_tui::{Component, Key, KeyName, Page, Pages, PagesLabels, PagesOptions, PagesOutcome};
use darkwire_tui::{parse_key, visible_width};

fn page(title: &str, rows: &[&str]) -> Page {
    Page {
        title: title.to_owned(),
        rows: rows.iter().map(|row| (*row).to_owned()).collect(),
    }
}

fn pages(max_rows: usize) -> Pages {
    Pages::new(PagesOptions {
        pages: vec![
            page("Basics", &["one", "two", "three"]),
            page("Keys", &["ctrl-g", "ctrl-t"]),
            page("Long", &(1..=20).map(|_| "row").collect::<Vec<_>>()),
        ],
        labels: PagesLabels {
            footer: "←→ tabs · esc closes".to_owned(),
        },
        theme: None,
        max_rows: Some(max_rows),
    })
}

fn key(name: KeyName) -> Key {
    Key {
        name,
        character: String::new(),
        ctrl: false,
        meta: false,
        shift: false,
        sequence: String::new(),
    }
}

#[test]
fn opens_on_the_first_tab() {
    let mut view = pages(10);
    assert_eq!(view.current(), 0);
    let drawn = view.render(40);
    assert!(drawn[0].contains("Basics"), "{drawn:?}");
    assert!(drawn.iter().any(|row| row.contains("one")), "{drawn:?}");
}

#[test]
fn right_and_left_move_between_tabs_and_wrap() {
    let mut view = pages(10);
    assert_eq!(view.handle_key(&key(KeyName::Right)), PagesOutcome::Open);
    assert_eq!(view.current(), 1);
    view.handle_key(&key(KeyName::Left));
    assert_eq!(view.current(), 0);
    // Wrapping both ways, so neither end is a dead key.
    view.handle_key(&key(KeyName::Left));
    assert_eq!(view.current(), 2);
    view.handle_key(&key(KeyName::Right));
    assert_eq!(view.current(), 0);
}

#[test]
fn tab_and_shift_tab_move_too() {
    let mut view = pages(10);
    view.handle_key(&key(KeyName::Tab));
    assert_eq!(view.current(), 1);
    let mut back = key(KeyName::Tab);
    back.shift = true;
    view.handle_key(&back);
    assert_eq!(view.current(), 0);
}

#[test]
fn changing_tab_starts_at_the_top_of_it() {
    // A reader coming back to a tab wants the start of it, not wherever they
    // had got to before they went looking somewhere else.
    let mut view = pages(8);
    view.handle_key(&key(KeyName::Right));
    view.handle_key(&key(KeyName::Right));
    view.handle_key(&key(KeyName::Down));
    view.handle_key(&key(KeyName::Down));
    assert!(view.top() > 0);
    view.handle_key(&key(KeyName::Left));
    assert_eq!(view.top(), 0);
}

#[test]
fn a_page_that_fits_does_not_scroll() {
    let mut view = pages(10);
    view.handle_key(&key(KeyName::Down));
    view.handle_key(&key(KeyName::End));
    assert_eq!(view.top(), 0);
}

#[test]
fn a_long_page_scrolls_and_stops_at_the_end() {
    let mut view = pages(8);
    view.handle_key(&key(KeyName::Right));
    view.handle_key(&key(KeyName::Right));
    view.handle_key(&key(KeyName::End));
    let at_end = view.top();
    view.handle_key(&key(KeyName::Down));
    assert_eq!(view.top(), at_end, "it stops rather than running off");
    view.handle_key(&key(KeyName::Home));
    assert_eq!(view.top(), 0);
}

#[test]
fn escape_return_and_q_all_close_it() {
    // There is nothing to choose, so every key that means "done" means done.
    for closing in [key(KeyName::Escape), key(KeyName::Enter)] {
        let mut view = pages(10);
        assert_eq!(view.handle_key(&closing), PagesOutcome::Closed);
    }
    let mut view = pages(10);
    assert_eq!(
        view.handle_key(&parse_key("q").unwrap()),
        PagesOutcome::Closed
    );
    let mut view = pages(10);
    assert_eq!(
        view.handle_key(&parse_key("\u{3}").unwrap()),
        PagesOutcome::Closed
    );
}

#[test]
fn it_is_the_same_height_whichever_tab_is_showing() {
    // The rows under the overlay must not move when the reader changes to a
    // shorter tab: the composer is one of them.
    let mut view = pages(9);
    let first = view.render(40).len();
    view.handle_key(&key(KeyName::Right));
    let second = view.render(40).len();
    assert_eq!(first, second);
    assert_eq!(first, 9);
}

#[test]
fn no_row_is_wider_than_the_window() {
    let mut view = Pages::new(PagesOptions {
        pages: vec![page(
            "Wide",
            &["a very long row that will not fit in twenty"],
        )],
        labels: PagesLabels {
            footer: "a footer long enough to need cutting as well".to_owned(),
        },
        theme: None,
        max_rows: Some(6),
    });
    for row in view.render(20) {
        assert!(visible_width(&row) <= 20, "{row:?}");
    }
}

#[test]
fn the_footer_counts_only_when_there_is_something_off_screen() {
    let mut view = pages(10);
    assert!(!view.render(60).last().unwrap().contains(" of "));

    view.handle_key(&key(KeyName::Right));
    view.handle_key(&key(KeyName::Right));
    let drawn = view.render(60);
    assert!(drawn.last().unwrap().contains("of 20"), "{drawn:?}");
}
