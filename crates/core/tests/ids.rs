//! Id rules, as this crate re-exports them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_core::ids::{
    DEFAULT_AGENT_ID, DEFAULT_WORKSPACE_ID, MAX_SLUG_ID_LENGTH, RESERVED_AGENT_IDS,
    RESERVED_WORKSPACE_IDS, SLUG_ID_PATTERN, derive_agent_id, derive_workspace_id, is_agent_id,
    is_extension_id, is_slug_id, is_workspace_id, slugify,
};
use proptest::prelude::*;

#[test]
fn accepts_legal_agent_ids() {
    for (what, id) in [
        ("a single character", "a"),
        ("digits", "2024"),
        ("hyphens inside", "code-reviewer"),
        ("the default", "default"),
        ("forty characters", &"a".repeat(40)),
    ] {
        assert!(is_agent_id(id), "{what}");
    }
}

#[test]
fn refuses_illegal_agent_ids() {
    for (what, id) in [
        ("empty", ""),
        ("a traversal", ".."),
        ("a separator", "a/b"),
        ("a backslash", "a\\b"),
        ("a colon", "c:"),
        ("a NUL byte", "a\0b"),
        ("a home prefix", "~agent"),
        ("a leading hyphen", "-agent"),
        ("a trailing hyphen", "agent-"),
        ("uppercase", "Reviewer"),
        ("a space", "my agent"),
        ("forty-one characters", &"a".repeat(41)),
    ] {
        assert!(!is_agent_id(id), "{what}");
    }
}

#[test]
fn refuses_uppercase_because_case_folding_filesystems_would_share_one_directory() {
    assert!(!is_agent_id("Reviewer"));
    assert_eq!(derive_agent_id("Reviewer"), "reviewer");
}

#[test]
fn derives_ids_from_labels() {
    for (label, expected) in [
        ("Code Reviewer", "code-reviewer"),
        ("  Spaced  out  ", "spaced-out"),
        ("Ünïcödé", "n-c-d"),
        ("///", "agent"),
        ("", "agent"),
        ("default", "agent"),
        ("CON", "agent"),
    ] {
        assert_eq!(derive_agent_id(label), expected, "{label:?}");
    }
}

#[test]
fn reserves_the_default_as_a_name_to_create_but_not_as_one_to_resolve() {
    // `agents.list.default` is legal to write; what is refused is minting a
    // *second* agent under that name from a label.
    assert!(is_agent_id(DEFAULT_AGENT_ID));
    assert!(RESERVED_AGENT_IDS.contains(&DEFAULT_AGENT_ID));
    assert_eq!(derive_agent_id("Default"), "agent");
}

#[test]
fn workspace_ids_follow_the_same_rules_with_their_own_fallback() {
    assert!(is_workspace_id(DEFAULT_WORKSPACE_ID));
    assert!(RESERVED_WORKSPACE_IDS.contains(&DEFAULT_WORKSPACE_ID));
    assert_eq!(derive_workspace_id("Client ACME"), "client-acme");
    assert!(is_workspace_id(&derive_workspace_id("default")));
}

#[test]
fn slug_ids_are_bounded_and_pattern_checked() {
    assert_eq!(MAX_SLUG_ID_LENGTH, 40);
    assert!(SLUG_ID_PATTERN.is_match("a-b"));
    assert!(!SLUG_ID_PATTERN.is_match("A"));
    assert!(is_slug_id("x1"));
    assert!(!is_slug_id("-x"));
    assert_eq!(slugify("Hello World", &[], "fallback"), "hello-world");
    assert_eq!(slugify("", &[], "fallback"), "fallback");
}

#[test]
fn extension_ids_are_slugs() {
    assert!(is_extension_id("slack"));
    for id in ["..", "a/b", "Slack", "", "~evil"] {
        assert!(!is_extension_id(id), "{id:?}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn always_derives_something_legal_for_any_label_at_all(label in ".*") {
        let id = derive_agent_id(&label);
        prop_assert!(is_agent_id(&id));
        prop_assert!(!RESERVED_AGENT_IDS.contains(&id.as_str()));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn agent_and_workspace_ids_share_one_rule_set(value in ".*") {
        // The two are separate so their reservations can differ, but the
        // character rules must not drift: both become directory names.
        prop_assert_eq!(is_agent_id(&value), is_workspace_id(&value));
    }
}
