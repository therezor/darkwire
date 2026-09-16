//! Slug rules: the character set, the reservations and the fallback.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_protocol::{
    DEFAULT_AGENT_ID, DEFAULT_WORKSPACE_ID, MAX_SLUG_ID_LENGTH, RESERVED_AGENT_IDS,
    RESERVED_DEVICE_NAMES, RESERVED_WORKSPACE_IDS, SLUG_ID_PATTERN, SLUG_ID_PATTERN_SOURCE,
    derive_agent_id, derive_workspace_id, is_agent_id, is_extension_id, is_slug_id,
    is_workspace_id, slugify, subagent_tool_name,
};
use proptest::prelude::*;

#[test]
fn the_pattern_is_the_published_source() {
    assert_eq!(SLUG_ID_PATTERN.as_str(), SLUG_ID_PATTERN_SOURCE);
    assert_eq!(MAX_SLUG_ID_LENGTH, 40);
}

#[test]
fn the_three_kinds_share_one_rule() {
    for value in [
        "a",
        "code-reviewer",
        "",
        "Reviewer",
        "-x",
        "x-",
        "a".repeat(41).as_str(),
    ] {
        assert_eq!(is_slug_id(value), is_agent_id(value));
        assert_eq!(is_slug_id(value), is_workspace_id(value));
        assert_eq!(is_slug_id(value), is_extension_id(value));
    }
}

#[test]
fn reservations_hold_the_default_and_every_device_name() {
    assert!(RESERVED_AGENT_IDS.contains(&DEFAULT_AGENT_ID));
    assert!(RESERVED_WORKSPACE_IDS.contains(&DEFAULT_WORKSPACE_ID));
    for name in RESERVED_DEVICE_NAMES {
        assert!(RESERVED_AGENT_IDS.contains(name), "{name}");
        assert!(RESERVED_WORKSPACE_IDS.contains(name), "{name}");
    }
    assert_eq!(RESERVED_DEVICE_NAMES.len(), 4 + 9 + 9);
    // Legal to resolve, refused as a name to create.
    assert!(is_agent_id(DEFAULT_AGENT_ID));
    assert_eq!(derive_agent_id("Default"), "agent");
    assert_eq!(derive_workspace_id("default"), "workspace");
}

#[test]
fn slugify_honours_the_caller_reservations() {
    assert_eq!(slugify("Nul", &["nul"], "thing"), "thing");
    assert_eq!(slugify("Nul", &[], "thing"), "nul");
}

#[test]
fn a_subagent_tool_name_is_a_legal_tool_name() {
    assert_eq!(subagent_tool_name("code-review"), "ask_code_review");
    let longest = subagent_tool_name(&"a".repeat(40));
    assert_eq!(longest.len(), 44);
}

proptest! {
    #[test]
    fn every_label_derives_a_legal_unreserved_agent_id(label in ".*") {
        let id = derive_agent_id(&label);
        prop_assert!(is_agent_id(&id), "{id:?} from {label:?}");
        prop_assert!(!RESERVED_AGENT_IDS.contains(&id.as_str()));
    }

    #[test]
    fn every_name_derives_a_legal_unreserved_workspace_id(name in ".*") {
        let id = derive_workspace_id(&name);
        prop_assert!(is_workspace_id(&id), "{id:?} from {name:?}");
        prop_assert!(!RESERVED_WORKSPACE_IDS.contains(&id.as_str()));
    }

    #[test]
    fn agent_and_workspace_ids_share_one_character_rule(value in ".*") {
        prop_assert_eq!(is_agent_id(&value), is_workspace_id(&value));
    }
}
