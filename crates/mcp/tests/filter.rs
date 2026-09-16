//! `enabledTools` against what a server advertised.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_mcp::{McpToolDescriptor, select_tools};
use serde_json::json;

fn descriptor(name: &str) -> McpToolDescriptor {
    McpToolDescriptor {
        name: name.to_owned(),
        title: None,
        description: None,
        input_schema: json!({ "type": "object" }),
        annotations: None,
    }
}

fn advertised() -> Vec<McpToolDescriptor> {
    vec![
        descriptor("repo_list"),
        descriptor("repo_create"),
        descriptor("issue_list"),
    ]
}

fn names(descriptors: &[McpToolDescriptor]) -> Vec<&str> {
    descriptors.iter().map(|d| d.name.as_str()).collect()
}

#[test]
fn takes_everything_under_the_schema_default() {
    let selection = select_tools(&advertised(), &["*"]);
    assert_eq!(
        names(&selection.selected),
        ["repo_list", "repo_create", "issue_list"]
    );
    assert!(selection.unmatched.is_empty());
}

#[test]
fn matches_an_exact_upstream_name() {
    // The upstream name, not the flattened one: it is what an operator reads in
    // the server's own documentation.
    let selection = select_tools(&advertised(), &["issue_list"]);
    assert_eq!(names(&selection.selected), ["issue_list"]);
}

#[test]
fn matches_a_prefix_for_a_server_that_groups_its_tools() {
    let selection = select_tools(&advertised(), &["repo_*"]);
    assert_eq!(names(&selection.selected), ["repo_list", "repo_create"]);
}

#[test]
fn reports_an_entry_that_matches_nothing() {
    let selection = select_tools(&advertised(), &["repo_list", "typo_*"]);
    assert_eq!(names(&selection.selected), ["repo_list"]);
    assert_eq!(selection.unmatched, ["typo_*"]);
}

#[test]
fn selects_nothing_for_an_empty_list_which_is_a_real_answer() {
    let selection = select_tools::<&str>(&advertised(), &[]);
    assert!(selection.selected.is_empty());
}

#[test]
fn keeps_a_wildcard_winning_over_a_narrower_sibling() {
    let selection = select_tools(&advertised(), &["*", "nope"]);
    assert_eq!(selection.selected.len(), 3);
    assert!(selection.unmatched.is_empty());
}
