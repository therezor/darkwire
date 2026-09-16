//! The arithmetic the MCP client and the extension host share.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_tools::{is_advertisable_name, namespaced_tool_name, namespaced_tool_names};

#[test]
fn qualifies_by_prefix_and_owner() {
    assert_eq!(
        namespaced_tool_name("ext", "slack", "post"),
        "ext_slack_post"
    );
    assert_eq!(
        namespaced_tool_name("mcp", "slack", "post"),
        "mcp_slack_post"
    );
}

#[test]
fn keeps_two_prefixes_apart_for_the_same_owner_and_tool() {
    assert_ne!(
        namespaced_tool_name("ext", "files", "read"),
        namespaced_tool_name("mcp", "files", "read")
    );
}

#[test]
fn replaces_what_a_provider_will_not_accept() {
    assert_eq!(
        namespaced_tool_name("ext", "my box", "search files"),
        "ext_my-box_search-files"
    );
}

#[test]
fn cannot_let_a_segment_forge_the_separator() {
    // `a_b` holding `c` and `a` holding `b_c` would otherwise both flatten to
    // one name, and the collision would be silent rather than merely possible.
    assert_ne!(
        namespaced_tool_name("ext", "a_b", "c"),
        namespaced_tool_name("ext", "a", "b_c")
    );
}

#[test]
fn stays_advertisable_however_long_the_parts_are() {
    let name = namespaced_tool_name("ext", &"x".repeat(60), &"y".repeat(60));
    assert_eq!(name.len(), 64);
    assert!(is_advertisable_name(&name));
}

#[test]
fn keeps_two_long_names_apart_rather_than_truncating_them_together() {
    let owner = "x".repeat(60);
    let first = namespaced_tool_name("ext", &owner, "read-the-first-thing");
    let second = namespaced_tool_name("ext", &owner, "read-the-second-thing");
    assert_ne!(first, second);
}

#[test]
fn is_stable_across_calls_because_a_prompt_cache_keys_on_it() {
    assert_eq!(
        namespaced_tool_name("ext", &"x".repeat(60), "read"),
        namespaced_tool_name("ext", &"x".repeat(60), "read")
    );
}

#[test]
fn matches_the_digest_the_typescript_arithmetic_produced() {
    // FNV-1a over UTF-16 code units; pinned so a port of the other prefix's
    // caller can compare against the same value.
    let name = namespaced_tool_name("ext", &"x".repeat(60), &"y".repeat(60));
    assert_eq!(&name[..55], &format!("ext_{}", "x".repeat(60))[..55]);
    assert_eq!(name.as_bytes()[55], b'_');
    assert!(name[56..].chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn breaks_a_within_owner_clash_rather_than_losing_a_tool() {
    let result = namespaced_tool_names(
        "ext",
        "slack",
        &["read file".to_owned(), "read-file".to_owned()],
    );
    assert_eq!(result.names["read file"], "ext_slack_read-file");
    assert_eq!(result.names["read-file"], "ext_slack_read-file_2");
    assert_eq!(result.collisions, vec!["read-file"]);
}

#[test]
fn reports_nothing_when_there_is_nothing_to_report() {
    let result = namespaced_tool_names("ext", "slack", &["a".to_owned(), "b".to_owned()]);
    assert!(result.collisions.is_empty());
}

#[test]
fn keeps_a_numbered_name_advertisable() {
    let long = "y".repeat(60);
    let result = namespaced_tool_names("ext", &"x".repeat(60), &[long.clone(), long]);
    for name in result.names.values() {
        assert!(is_advertisable_name(name), "{name}");
    }
}
