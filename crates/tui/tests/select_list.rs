//! The cursor, the substring filter and the visible window, with no terminal attached.

use ghostai_tui::{PLAIN_THEME, SelectItem, SelectList, Theme, theme_for, visible_width};

/// Values are their own labels, which keeps every assertion below readable.
fn items(labels: &[&str]) -> Vec<SelectItem<String>> {
    labels
        .iter()
        .map(|label| SelectItem::new((*label).to_owned(), label))
        .collect()
}

fn list(labels: &[&str]) -> SelectList<String> {
    SelectList::new(items(labels), None, None)
}

fn labels_of<T>(list: &SelectList<T>) -> Vec<&str> {
    list.matches()
        .into_iter()
        .map(|item| item.label.as_str())
        .collect()
}

/// Whether a rendered line is the `(n/total)` counter rather than a row.
fn is_counter(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('(') && trimmed.ends_with(')') && trimmed.contains('/')
}

/// The rows only — `render` also emits a counter once some are off-screen.
fn rows_of(list: &SelectList<String>, width: usize) -> Vec<String> {
    list.render(width, &PLAIN_THEME)
        .into_iter()
        .filter(|line| !is_counter(line))
        .collect()
}

/// The rows with the two-column cursor marker and the padding taken off.
fn bodies_of(list: &SelectList<String>) -> Vec<String> {
    rows_of(list, 40)
        .iter()
        .map(|row| {
            row[row.char_indices().nth(2).map_or(row.len(), |(at, _)| at)..]
                .trim()
                .to_owned()
        })
        .collect()
}

#[test]
fn the_cursor_starts_at_the_top_or_wherever_the_caller_asked() {
    assert_eq!(list(&["a", "b", "c"]).index(), 0);
    assert_eq!(
        SelectList::new(items(&["a", "b", "c"]), None, Some(2)).index(),
        2
    );
}

#[test]
fn clamps_a_starting_index_the_list_cannot_honour() {
    assert_eq!(
        SelectList::new(items(&["a", "b"]), None, Some(9)).index(),
        1
    );
    assert_eq!(
        SelectList::<String>::new(Vec::new(), None, Some(9)).index(),
        0
    );
}

#[test]
fn wraps_at_both_ends_rather_than_stopping() {
    // Reaching the last of four agents by pressing up once beats pressing down
    // three times, and "the key did nothing" is the worst answer a menu gives.
    let mut list = list(&["a", "b", "c"]);
    list.move_up();
    assert_eq!(list.index(), 2);
    list.move_down();
    assert_eq!(list.index(), 0);
}

#[test]
fn moves_by_more_than_one_wrapping_the_same_way() {
    let mut list = list(&["a", "b", "c", "d", "e"]);
    list.move_by(3);
    assert_eq!(list.index(), 3);
    list.move_by(4);
    assert_eq!(list.index(), 2);
    list.move_by(-7);
    assert_eq!(list.index(), 0);
    list.move_by(0);
    assert_eq!(list.index(), 0);
}

#[test]
fn goes_to_the_first_and_last_rows() {
    let mut list = list(&["a", "b", "c"]);
    list.last();
    assert_eq!(list.index(), 2);
    list.first();
    assert_eq!(list.index(), 0);
}

#[test]
fn does_nothing_at_all_on_an_empty_list() {
    let mut list = SelectList::<String>::new(Vec::new(), None, None);
    list.move_down();
    list.last();
    list.first();
    list.set_filter("x");
    assert_eq!(list.index(), 0);
    assert!(list.selected().is_none());
    assert!(list.render(40, &PLAIN_THEME).is_empty());
}

#[test]
fn steps_over_a_disabled_row() {
    let mut list = SelectList::new(
        vec![
            SelectItem::new("a", "a"),
            SelectItem::new("b", "b").disabled(),
            SelectItem::new("c", "c"),
        ],
        None,
        None,
    );
    list.move_down();
    assert_eq!(list.selected().map(|item| item.value), Some("c"));
    list.move_up();
    assert_eq!(list.selected().map(|item| item.value), Some("a"));

    // `first` and `last` step off a disabled end too.
    let mut ends = SelectList::new(
        vec![
            SelectItem::new("a", "a").disabled(),
            SelectItem::new("b", "b"),
            SelectItem::new("c", "c").disabled(),
        ],
        None,
        None,
    );
    ends.first();
    assert_eq!(ends.selected().map(|item| item.value), Some("b"));
    ends.last();
    assert_eq!(ends.selected().map(|item| item.value), Some("b"));
}

#[test]
fn gives_up_rather_than_spinning_when_every_row_is_disabled() {
    let mut list = SelectList::new(
        vec![
            SelectItem::new("a", "a").disabled(),
            SelectItem::new("b", "b").disabled(),
        ],
        None,
        None,
    );
    list.move_down();
    assert!(list.selected().is_some_and(|item| item.disabled));
}

#[test]
fn the_filter_matches_case_insensitively_across_label_hint_and_keywords() {
    let mut list = SelectList::new(
        vec![
            SelectItem::new("a", "Reviewer"),
            SelectItem::new("b", "Scout").with_hint("claude-opus-5"),
            SelectItem::new("c", "Archivist").with_keywords("memory recall"),
        ],
        None,
        None,
    );

    list.set_filter("REVIEW");
    assert_eq!(labels_of(&list), ["Reviewer"]);
    assert_eq!(list.filter(), "REVIEW");
    list.set_filter("opus");
    assert_eq!(labels_of(&list), ["Scout"]);
    list.set_filter("recall");
    assert_eq!(labels_of(&list), ["Archivist"]);
}

#[test]
fn ranks_an_earlier_match_first_which_puts_a_label_hit_above_a_hint_hit() {
    // Not a rule written anywhere: the haystack is built label-first, so a hit
    // in the label simply has a lower index than a hit in the hint.
    let mut list = SelectList::new(
        vec![
            SelectItem::new("a", "scout").with_hint("sonnet"),
            SelectItem::new("b", "sonnet-runner"),
        ],
        None,
        None,
    );
    list.set_filter("sonnet");
    assert_eq!(labels_of(&list), ["sonnet-runner", "scout"]);
}

#[test]
fn returns_every_item_for_an_empty_or_whitespace_filter() {
    let mut list = list(&["a", "b"]);
    list.set_filter("   ");
    assert_eq!(labels_of(&list), ["a", "b"]);
}

#[test]
fn puts_the_cursor_back_at_the_top_so_enter_never_chooses_an_unseen_row() {
    // Keeping the cursor where it was would mean a keystroke that removes rows
    // above it silently moves the selection to a different item.
    let mut list = list(&["alpha", "beta", "gamma"]);
    list.last();
    assert_eq!(list.index(), 2);
    list.set_filter("a");
    assert_eq!(list.index(), 0);
    assert_eq!(
        list.selected().map(|item| item.label.as_str()),
        Some("alpha")
    );
}

#[test]
fn leaves_nothing_selected_when_nothing_matches() {
    let mut list = list(&["a", "b"]);
    list.set_filter("zzz");
    assert!(list.matches().is_empty());
    assert!(list.selected().is_none());
}

#[test]
fn shows_no_more_rows_than_it_was_given_room_for() {
    let list = SelectList::new(items(&["a", "b", "c", "d", "e"]), Some(3), None);
    assert_eq!(rows_of(&list, 40).len(), 3);
    assert_eq!(list.rows(), 3);
}

#[test]
fn follows_the_cursor_down_moving_the_least_it_can() {
    let mut list = SelectList::new(items(&["a", "b", "c", "d", "e"]), Some(3), None);
    for _ in 0..3 {
        list.move_down();
    }
    // The cursor is on `d`; the window slid by exactly one to reach it.
    assert_eq!(bodies_of(&list), ["b", "c", "d"]);
}

#[test]
fn follows_the_cursor_back_up() {
    let mut list = SelectList::new(items(&["a", "b", "c", "d", "e"]), Some(2), None);
    list.last();
    list.first();
    assert_eq!(bodies_of(&list), ["a", "b"]);
}

#[test]
fn shows_the_last_rows_when_the_cursor_wraps_to_the_end() {
    let mut list = SelectList::new(items(&["a", "b", "c", "d", "e"]), Some(2), None);
    list.move_up();
    assert_eq!(bodies_of(&list), ["d", "e"]);
}

#[test]
fn adds_a_counter_only_once_some_rows_are_off_screen() {
    let roomy = SelectList::new(items(&["a", "b"]), Some(5), None);
    assert!(
        !roomy
            .render(40, &PLAIN_THEME)
            .iter()
            .any(|line| line.contains('/'))
    );

    let cramped = SelectList::new(items(&["a", "b", "c"]), Some(2), None);
    let rendered = cramped.render(40, &PLAIN_THEME);
    assert!(rendered.last().unwrap().contains("(1/3)"));
}

#[test]
fn renders_nothing_at_all_when_nothing_matches() {
    let mut list = list(&["a"]);
    list.set_filter("zzz");
    assert!(list.render(40, &PLAIN_THEME).is_empty());
}

#[test]
fn takes_a_new_row_count_when_the_window_is_resized() {
    let mut list = SelectList::new(items(&["a", "b", "c", "d"]), Some(4), None);
    list.set_rows(2);
    assert_eq!(rows_of(&list, 40).len(), 2);
    // A window with no room at all still shows one row rather than none.
    list.set_rows(0);
    assert_eq!(rows_of(&list, 40).len(), 1);
    assert_eq!(list.rows(), 1);
}

#[test]
fn never_produces_a_line_wider_than_it_was_given_even_in_colour() {
    // Asserted with `visible_width` rather than `len`, so it holds for the
    // coloured output too — which is the case that would actually wrap.
    let list = SelectList::new(
        vec![
            SelectItem::new("a", "a-very-long-label-that-will-not-fit")
                .with_hint("and a long hint too"),
            SelectItem::new("b", "日本語のラベルもある").with_hint("ヒント"),
        ],
        Some(2),
        None,
    );

    for width in [3, 10, 20, 32, 60] {
        for theme in [PLAIN_THEME, theme_for(Some(true))] {
            for line in list.render(width, &theme) {
                assert!(
                    visible_width(&line) <= width,
                    "{line:?} is wider than {width}"
                );
            }
        }
    }
}

#[test]
fn marks_the_selected_row_with_a_glyph_so_colour_is_never_the_only_signal() {
    // Under NO_COLOR every formatter is the identity function. A menu whose
    // selection was indicated by colour alone would become unusable, not
    // plainer.
    let list = list(&["alpha", "beta"]);
    let rendered = list.render(40, &PLAIN_THEME);
    assert!(rendered[0].starts_with("❯ "));
    assert!(rendered[1].starts_with("  "));
}

#[test]
fn colours_the_cursor_row_and_a_disabled_row_whole() {
    let theme: Theme = theme_for(Some(true));
    let list = SelectList::new(
        vec![
            SelectItem::new("a", "alpha").with_hint("one"),
            SelectItem::new("b", "beta").with_hint("two"),
            SelectItem::new("c", "gone").with_hint("three").disabled(),
        ],
        None,
        None,
    );
    let rendered = list.render(40, &theme);
    assert!(rendered[0].starts_with("\x1b[32m❯ "));
    assert!(rendered[1].starts_with("  \x1b[0m"));
    assert!(rendered[2].starts_with("\x1b[90m  gone"));
}

#[test]
fn drops_the_hint_column_before_it_squeezes_the_label() {
    // A label the operator cannot read is a menu they cannot use; a model id
    // they cannot read is only a menu that tells them less.
    let list = SelectList::new(
        vec![SelectItem::new("a", "reviewer").with_hint("claude-opus-5")],
        None,
        None,
    );
    assert!(list.render(40, &PLAIN_THEME)[0].contains("claude-opus-5"));
    assert!(!list.render(14, &PLAIN_THEME)[0].contains("claude"));
}

#[test]
fn aligns_the_hint_column_across_rows() {
    let list = SelectList::new(
        vec![
            SelectItem::new("a", "x").with_hint("one"),
            SelectItem::new("b", "a-longer-label").with_hint("two"),
        ],
        None,
        None,
    );
    let rendered = list.render(60, &PLAIN_THEME);
    // Compared in columns, not bytes: the cursor glyph is three bytes wide and
    // one column.
    let column = |row: &str, hint: &str| visible_width(&row[..row.find(hint).unwrap()]);
    assert_eq!(column(&rendered[0], "one"), column(&rendered[1], "two"));
}

#[test]
fn highlights_the_matched_span_of_the_label() {
    let theme = theme_for(Some(true));
    let mut list = SelectList::new(
        vec![
            SelectItem::new("a", "Reviewer"),
            SelectItem::new("b", "Scout"),
        ],
        None,
        None,
    );
    list.set_filter("view");
    let rendered = list.render(40, &theme);
    // Inside the green cursor row, yellow's close would end the green too, so
    // it is replaced by green's opener.
    assert!(rendered[0].contains("Re\x1b[33mview\x1b[32mer"));

    // A hit in the hint alone leaves the label unmarked.
    let mut hinted = SelectList::new(
        vec![SelectItem::new("a", "Scout").with_hint("sonnet")],
        None,
        None,
    );
    hinted.set_filter("sonnet");
    assert!(hinted.render(40, &theme)[0].contains("Scout"));
    assert!(!hinted.render(40, &theme)[0].contains(&theme.match_.apply("sonnet")));
}

#[test]
fn leaves_a_label_unmarked_when_lower_casing_changes_its_length() {
    // `İ` lower-cases to two characters, so a span found in the lower-cased
    // label would name the wrong bytes of the original.
    let theme = theme_for(Some(true));
    let mut list = SelectList::new(vec![SelectItem::new("a", "İstanbul")], None, None);
    list.set_filter("stanbul");
    let rendered = list.render(40, &theme);
    assert_eq!(rendered.len(), 1);
    assert!(rendered[0].contains("İstanbul"));
}
