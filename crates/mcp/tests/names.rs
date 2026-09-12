//! Name flattening under the `mcp` prefix, and the prefix as a parameter.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_mcp::{
    MCP_TOOL_PREFIX, flatten_mcp_tool_name, flatten_tool_name, flatten_tool_names,
    is_advertisable_name,
};
use proptest::prelude::*;

fn mcp(server: &str, tool: &str) -> String {
    flatten_tool_name(MCP_TOOL_PREFIX, server, tool)
}

#[test]
fn qualifies_a_tool_by_the_server_that_offers_it() {
    assert_eq!(mcp("github", "create_issue"), "mcp_github_create-issue");
    assert_eq!(
        flatten_mcp_tool_name("github", "create_issue"),
        "mcp_github_create-issue"
    );
}

#[test]
fn the_prefix_is_a_parameter_so_the_extension_host_can_pass_its_own() {
    assert_eq!(
        flatten_tool_name("ext", "linear", "create_issue"),
        "ext_linear_create-issue"
    );
}

#[test]
fn replaces_everything_a_provider_would_reject() {
    assert_eq!(
        mcp("my server", "search files!"),
        "mcp_my-server_search-files-"
    );
}

#[test]
fn keeps_underscore_as_the_separator_and_nowhere_else() {
    // Otherwise server `a_b` holding `c` and server `a` holding `b_c` would
    // both flatten to `mcp_a_b_c`, and the collision would be silent.
    assert_ne!(mcp("a_b", "c"), mcp("a", "b_c"));
}

#[test]
fn keeps_a_long_name_legal_and_distinct_from_one_sharing_its_prefix() {
    let server = "a".repeat(20);
    let first = mcp(&server, &format!("{}-one", "b".repeat(60)));
    let second = mcp(&server, &format!("{}-two", "b".repeat(60)));

    assert!(first.len() <= 64);
    assert!(second.len() <= 64);
    assert_ne!(first, second);
    // The server prefix survives: it is how an operator recognises the row.
    assert!(first.starts_with(&format!("mcp_{server}_")));
}

#[test]
fn is_stable_because_the_prompt_prefix_a_provider_caches_keys_on_it() {
    assert_eq!(mcp("github", "create_issue"), mcp("github", "create_issue"));
}

proptest! {
    #[test]
    fn always_produces_a_name_a_provider_will_accept(
        server in "\\PC{1,40}",
        tool in "\\PC{1,120}",
    ) {
        prop_assert!(is_advertisable_name(&mcp(&server, &tool)));
    }
}

#[test]
fn leaves_a_clash_free_list_alone() {
    let flattened = flatten_tool_names(
        MCP_TOOL_PREFIX,
        "github",
        &["create_issue".to_owned(), "list_issues".to_owned()],
    );
    let names: Vec<&String> = flattened.names.values().collect();
    assert_eq!(names, ["mcp_github_create-issue", "mcp_github_list-issues"]);
    assert!(flattened.collisions.is_empty());
}

#[test]
fn keeps_both_tools_when_two_upstream_names_flatten_to_one() {
    // The registry would refuse the second as a conflict, losing a tool for a
    // reason nothing reports.
    let flattened = flatten_tool_names(
        MCP_TOOL_PREFIX,
        "files",
        &["read file".to_owned(), "read_file".to_owned()],
    );
    assert_eq!(flattened.names["read file"], "mcp_files_read-file");
    assert_eq!(flattened.names["read_file"], "mcp_files_read-file_2");
    assert_eq!(flattened.collisions, ["read_file"]);
}

#[test]
fn gives_the_plain_name_to_whichever_the_server_advertised_first() {
    let flattened = flatten_tool_names(
        MCP_TOOL_PREFIX,
        "files",
        &["read_file".to_owned(), "read file".to_owned()],
    );
    assert_eq!(flattened.names["read_file"], "mcp_files_read-file");
    assert_eq!(flattened.names["read file"], "mcp_files_read-file_2");
}

#[test]
fn keeps_a_numbered_name_legal_too() {
    let long = "b".repeat(80);
    let flattened = flatten_tool_names(
        MCP_TOOL_PREFIX,
        &"a".repeat(20),
        &[long.clone(), format!("{long}!")],
    );
    for name in flattened.names.values() {
        assert!(is_advertisable_name(name));
    }
    let distinct: std::collections::HashSet<&String> = flattened.names.values().collect();
    assert_eq!(distinct.len(), 2);
}
