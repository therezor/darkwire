//! `src/pickers/palette.rs` — turning a syntax line into something typeable.
//!
//! The rows here are a fixture rather than the real command table, which lives
//! with the slash commands. What is under test is the syntax grammar — aliases,
//! required placeholders, optional ones, multi-word commands — and a fixture
//! states each of those shapes once. That the real table round-trips through
//! these functions belongs with the table.

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

use ghostai::i18n::Translations;
use ghostai::pickers::palette::{
    CommandChoice, PaletteRow, command_items, command_value, complete_command,
};
use ghostai_i18n::keys;

fn english() -> Translations {
    Translations::default()
}

/// One of each shape the real table contains.
fn rows() -> Vec<PaletteRow> {
    vec![
        PaletteRow::new("/help", keys::slash::help::HELP),
        PaletteRow::new("/context", keys::slash::help::CONTEXT),
        PaletteRow::new("/exit, /quit", keys::slash::help::EXIT),
        PaletteRow::new("/agent [id]", keys::slash::help::AGENT),
        PaletteRow::new("/rename <title>", keys::slash::help::RENAME),
        PaletteRow::new("/workspaces", keys::slash::help::WORKSPACES),
        PaletteRow::new("/workspace <id>", keys::slash::help::WORKSPACE),
        PaletteRow::variant("/workspace new <name>"),
        PaletteRow::new(
            "/workspace move <from> <to>",
            keys::slash::help::WORKSPACE_MOVE,
        ),
    ]
}

#[test]
fn stops_at_the_first_placeholder_so_what_lands_on_the_line_is_typeable() {
    assert_eq!(command_value("/agent [id]"), "/agent");
    assert_eq!(command_value("/rename <title>"), "/rename");
    assert_eq!(
        command_value("/workspace move <from> <to>"),
        "/workspace move"
    );
}

#[test]
fn keeps_a_command_that_takes_nothing_whole() {
    assert_eq!(command_value("/help"), "/help");
    assert_eq!(command_value("/context"), "/context");
}

#[test]
fn takes_the_first_of_a_pair_of_aliases_because_they_are_the_same_command() {
    assert_eq!(command_value("/exit, /quit"), "/exit");
}

#[test]
fn offers_every_command_the_help_page_lists() {
    let t = english();
    let items = command_items(&rows(), &t);
    assert_eq!(items.len(), rows().len());
    assert!(items.iter().any(|item| item.label == "/agent [id]"));
}

#[test]
fn shows_the_syntax_with_its_description_beside_it() {
    let t = english();
    let items = command_items(&rows(), &t);
    let help = items.iter().find(|item| item.label == "/help").unwrap();
    assert_eq!(help.hint.as_deref(), Some("this list"));
}

#[test]
fn submits_a_command_that_needs_nothing_and_only_types_one_that_does() {
    // `/rename` alone is a usage error the operator would have to read and then
    // retype around. Putting it in the editor and stopping is the better answer.
    let t = english();
    let items = command_items(&rows(), &t);

    let agent = items
        .iter()
        .find(|item| item.label == "/agent [id]")
        .unwrap();
    assert_eq!(
        agent.value,
        CommandChoice {
            command: "/agent".to_owned(),
            submit: true,
        }
    );

    let rename = items
        .iter()
        .find(|item| item.label == "/rename <title>")
        .unwrap();
    assert_eq!(
        rename.value,
        CommandChoice {
            command: "/rename".to_owned(),
            submit: false,
        }
    );
}

#[test]
fn leaves_a_variant_row_without_a_description_rather_than_inventing_one() {
    let t = english();
    let items = command_items(&rows(), &t);
    let variant = items
        .iter()
        .find(|item| item.label == "/workspace new <name>")
        .unwrap();
    assert_eq!(variant.hint, None);
}

#[test]
fn completes_a_slash_command_from_the_same_table_the_help_page_uses() {
    let (hits, line) = complete_command("/work", &rows());
    assert_eq!(line, "/work");
    assert!(hits.iter().any(|hit| hit == "/workspaces"));
    assert!(hits.iter().any(|hit| hit == "/workspace move"));
}

#[test]
fn offers_nothing_for_prose_which_is_what_a_prompt_is_mostly_made_of() {
    assert_eq!(
        complete_command("what is", &rows()),
        (Vec::new(), "what is".to_owned())
    );
    assert_eq!(complete_command("", &rows()), (Vec::new(), String::new()));
}

#[test]
fn offers_each_command_once_however_many_rows_describe_it() {
    let (hits, _) = complete_command("/", &rows());
    let mut unique = hits.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), hits.len());
    // `/workspace` has three syntax lines and is two things to complete to.
    assert!(hits.iter().any(|hit| hit == "/workspace"));
}

#[test]
fn offers_nothing_for_a_command_that_does_not_exist() {
    let (hits, _) = complete_command("/zzz", &rows());
    assert!(hits.is_empty());
}
