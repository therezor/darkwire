//! A model's `args` value, turned into the argv it meant.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_tools::coerce_argv;
use serde_json::{Value, json};

fn argv(value: &Value) -> Vec<String> {
    coerce_argv(value)
}

#[test]
fn passes_an_argv_through_unchanged() {
    assert_eq!(
        argv(&json!(["--json", "-n", "3", "sqlite wal"])),
        vec!["--json", "-n", "3", "sqlite wal"]
    );
}

#[test]
fn recovers_the_argument_list_a_model_half_serialised() {
    // Verbatim from a real call. An index marker at the front, a stray quote
    // and bracket at the back, the query in the middle.
    assert_eq!(
        argv(&json!(
            r#"[0] SUFFOLK wildfire 2024 fires reports UK US news updates"]"#
        )),
        vec![
            "SUFFOLK", "wildfire", "2024", "fires", "reports", "UK", "US", "news", "updates"
        ]
    );
}

#[test]
fn parses_a_properly_stringified_array() {
    assert_eq!(
        argv(&json!(r#"["--json", "-n", "3", "sqlite wal mode"]"#)),
        vec!["--json", "-n", "3", "sqlite wal mode"]
    );
}

#[test]
fn keeps_a_quoted_phrase_as_one_argument() {
    assert_eq!(
        argv(&json!(r#"--site docs.python.org "async task group""#)),
        vec!["--site", "docs.python.org", "async task group"]
    );
    assert_eq!(
        argv(&json!("--query 'two words' --json")),
        vec!["--query", "two words", "--json"]
    );
}

#[test]
fn treats_an_empty_quoted_string_as_an_argument() {
    assert_eq!(
        argv(&json!(r#"--name "" --json"#)),
        vec!["--name", "", "--json"]
    );
}

#[test]
fn never_builds_a_pipeline_out_of_a_string() {
    // Shell operators survive as literal text inside whichever argument they
    // landed in, and are never interpreted.
    assert_eq!(
        argv(&json!("a | b > c ; d")),
        vec!["a", "|", "b", ">", "c", ";", "d"]
    );
    assert_eq!(argv(&json!("echo $HOME")), vec!["echo", "$HOME"]);
}

#[test]
fn gives_an_empty_argv_for_nothing_at_all() {
    // Whether empty is *allowed* is `requires_args`' decision, not this one's.
    assert!(argv(&json!("")).is_empty());
    assert!(argv(&json!("   ")).is_empty());
    assert!(argv(&Value::Null).is_empty());
    assert!(argv(&json!(42)).is_empty());
    assert!(argv(&json!({"a": 1})).is_empty());
}

#[test]
fn stringifies_whatever_an_array_happens_to_hold() {
    // A model that sends `["-n", 3]` means `-n 3`, and the program takes text.
    assert_eq!(
        argv(&json!(["-n", 3, true, null])),
        vec!["-n", "3", "true", "null"]
    );
}

#[test]
fn leaves_an_empty_json_array_empty_rather_than_falling_through_to_splitting() {
    assert!(argv(&json!("[]")).is_empty());
}

#[test]
fn an_unterminated_quote_still_yields_what_was_typed() {
    assert_eq!(argv(&json!(r#"--q "half open"#)), vec!["--q", "half open"]);
}
