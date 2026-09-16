//! `src/pickers/effort.rs` — the rows a level menu is made of.

// `allow-unwrap-in-tests` covers a `#[test]` body; the fixture helpers beside
// them are ordinary functions, and a helper that returned `Result` would make
// every case carry a `?` for a temporary directory that cannot fail in a way
// the case could act on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture helpers in an integration test"
)]

use darkwire::i18n::Translations;
use darkwire::pickers::effort::{
    DEFAULT_LEVEL, LEVELS, effort_items, effort_listing, effort_value, parse_effort,
};
use darkwire_protocol::ReasoningEffort;

fn english() -> Translations {
    Translations::default()
}

#[test]
fn offers_every_level_the_type_has_so_a_new_one_needs_no_second_list() {
    let t = english();
    let values: Vec<String> = effort_items(None, &t)
        .into_iter()
        .map(|item| item.value)
        .collect();
    let mut expected = vec![DEFAULT_LEVEL.to_owned()];
    expected.extend(
        LEVELS
            .into_iter()
            .map(|level| effort_value(level).to_owned()),
    );
    assert_eq!(values, expected);
}

#[test]
fn every_level_has_a_distinct_word_so_the_list_cannot_hide_a_duplicate() {
    // `effort_value`'s match is exhaustive, so a new variant is a compile error
    // there. This is the other half: that no two variants collapse onto one row.
    let mut words: Vec<&str> = LEVELS.into_iter().map(effort_value).collect();
    let total = words.len();
    words.sort_unstable();
    words.dedup();
    assert_eq!(words.len(), total);
    assert!(!words.contains(&DEFAULT_LEVEL));
}

#[test]
fn reads_a_level_back_from_what_an_operator_typed() {
    assert_eq!(parse_effort("high"), Some(ReasoningEffort::High));
    assert_eq!(parse_effort("off"), Some(ReasoningEffort::Off));
    // `default` is the absence of a level, not one of them.
    assert_eq!(parse_effort(DEFAULT_LEVEL), None);
    assert_eq!(parse_effort("enormous"), None);
}

#[test]
fn puts_default_first_because_it_is_the_state_an_agent_starts_in() {
    let t = english();
    assert_eq!(effort_items(None, &t)[0].value, DEFAULT_LEVEL);
}

#[test]
fn marks_stating_none_as_the_current_row_which_is_a_real_answer() {
    // The distinction the whole setting is built on: `default` means the agent
    // states no effort, and that is a row like any other rather than the
    // absence of a selection.
    let t = english();
    let items = effort_items(None, &t);
    assert!(items[0].hint.as_deref().unwrap().contains("current"));
    let marked = items
        .iter()
        .filter(|item| {
            item.hint
                .as_deref()
                .is_some_and(|hint| hint.contains("current"))
        })
        .count();
    assert_eq!(marked, 1);
}

#[test]
fn marks_the_level_in_force_when_there_is_one() {
    let t = english();
    let items = effort_items(Some(ReasoningEffort::High), &t);
    let high = items.iter().find(|item| item.value == "high").unwrap();
    assert!(high.hint.as_deref().unwrap().contains("current"));
    assert!(!items[0].hint.as_deref().unwrap().contains("current"));
}

#[test]
fn says_how_off_differs_from_default_and_stays_quiet_about_the_rest() {
    // Which levels *mean* anything is the model's business, not this project's
    // — so the only other row that carries a hint is the one whose mechanism
    // differs, and the rest say only whether they are in force.
    let t = english();
    let items = effort_items(Some(ReasoningEffort::Medium), &t);
    let off = items.iter().find(|item| item.value == "off").unwrap();
    assert!(off.hint.as_deref().unwrap().contains("asking for none"));
    let high = items.iter().find(|item| item.value == "high").unwrap();
    assert_eq!(high.hint.as_deref(), Some(""));
}

#[test]
fn the_listing_is_what_a_pipe_gets_marking_the_level_in_force() {
    let t = english();
    let listing = effort_listing(Some(ReasoningEffort::Low), &t);
    assert!(listing.contains("* low"));
    assert!(listing.contains("  high"));
}

#[test]
fn the_listing_marks_default_when_the_agent_states_no_level() {
    let t = english();
    assert!(effort_listing(None, &t).contains(&format!("* {DEFAULT_LEVEL}")));
}
