//! A tool call cut off by the output token limit.

use darkwire_agent::length_cut::{length_cut_correction, length_cut_notice, split_cut_calls};
use darkwire_agent::testkit::raw_tool_call;

#[test]
fn calls_that_parse_are_kept_and_the_rest_are_named() {
    let (kept, cut) = split_cut_calls(&[
        raw_tool_call("c1", "read", r#"{"path": "a"}"#),
        raw_tool_call("c2", "ls", ""),
        raw_tool_call("c3", "write", r#"{"path": "b", "content": "half of"#),
    ]);
    let ids: Vec<&str> = kept.iter().map(|call| call.id.as_str()).collect();
    assert_eq!(ids, vec!["c1", "c2"]);
    assert_eq!(cut, vec!["write".to_owned()]);
}

#[test]
fn the_correction_names_the_call_and_the_limit() {
    let one = length_cut_correction(&["write".to_owned()], 8192);
    assert!(one.starts_with("## Correction"));
    assert!(one.contains("Your call to `write` was cut off at the 8192-token output limit"));
    assert!(one.contains("Retry with shorter arguments"));

    let two = length_cut_correction(&["write".to_owned(), "edit".to_owned()], 100);
    assert!(two.contains("Your calls to `write`, `edit` were cut off"));
}

#[test]
fn the_notice_agrees_with_how_many_calls_were_cut() {
    assert_eq!(
        length_cut_notice(&["write".to_owned()], 100),
        "The call to `write` was cut off at the 100-token limit. Asking the model to retry."
    );
    assert!(
        length_cut_notice(&["write".to_owned(), "edit".to_owned()], 100)
            .starts_with("The calls to `write`, `edit` were cut off")
    );
}
