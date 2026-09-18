//! The built-in set, and what packages below this one assume about it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_protocol::{
    BUILTIN_TOOL_NAMES, DEFAULT_AGENT_TOOLS, ToolPermission, ToolRisk, ToolSource,
};
use darkwire_tools::builtin::all_builtin_tools;
use darkwire_tools::{BuiltinOptions, ToolRegistry, builtin_tools, register_builtins};

#[test]
fn registers_every_built_in_under_the_builtin_source() {
    let registry = ToolRegistry::new();
    register_builtins(&registry, BuiltinOptions::default()).unwrap();
    assert_eq!(
        registry.names(),
        vec![
            "automation",
            "edit",
            "exec",
            "find",
            "grep",
            "ls",
            "memory",
            "read",
            "skill",
            "tool_search",
            "write"
        ]
    );
    assert_eq!(registry.source_of("exec"), Some(ToolSource::Builtin));
    assert_eq!(registry.size(), all_builtin_tools().len());
}

#[test]
fn does_not_advertise_automation_when_the_scheduler_is_switched_off() {
    let registry = ToolRegistry::new();
    register_builtins(&registry, BuiltinOptions { scheduler: false }).unwrap();
    assert!(!registry.has("automation"));
    assert!(registry.has("exec"));
    assert_eq!(registry.size(), all_builtin_tools().len() - 1);
}

#[test]
fn registers_everything_else_whatever_an_agent_decides() {
    // `exec` and `tool_search` are narrowed per agent by the loop, not here:
    // the registry is shared, so a per-agent decision cannot be made in it.
    let names: Vec<String> = builtin_tools(BuiltinOptions::default())
        .iter()
        .map(|tool| tool.definition().name.clone())
        .collect();
    assert!(names.contains(&"exec".to_owned()));
    assert!(names.contains(&"tool_search".to_owned()));
}

fn names() -> Vec<String> {
    let mut names: Vec<String> = all_builtin_tools()
        .iter()
        .map(|tool| tool.definition().name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn matches_the_name_list_protocol_publishes() {
    let mut published: Vec<String> = BUILTIN_TOOL_NAMES.iter().map(|n| (*n).to_owned()).collect();
    published.sort();
    assert_eq!(published, names());
}

#[test]
fn is_what_a_new_agent_is_seeded_with_save_for_the_two_deliberate_omissions() {
    let mut seeded: Vec<String> = DEFAULT_AGENT_TOOLS
        .iter()
        .map(|(n, _)| (*n).to_owned())
        .collect();
    seeded.sort();
    // `automation` is granted by an operator; `tool_search` takes no permission
    // at all, so a seed entry for it would be a row that decides nothing.
    let expected: Vec<String> = names()
        .into_iter()
        .filter(|n| n != "automation" && n != "tool_search")
        .collect();
    assert_eq!(seeded, expected);
    assert!(!DEFAULT_AGENT_TOOLS.iter().any(|(n, _)| *n == "automation"));
    assert!(BUILTIN_TOOL_NAMES.contains(&"automation"));
    assert!(BUILTIN_TOOL_NAMES.contains(&"tool_search"));
}

#[test]
fn seeds_each_tool_at_the_permission_its_risk_band_implies() {
    for tool in all_builtin_tools() {
        let Some((_, seeded)) = DEFAULT_AGENT_TOOLS
            .iter()
            .find(|(name, _)| *name == tool.definition().name)
        else {
            continue;
        };
        let expected = match tool.risk() {
            ToolRisk::Safe | ToolRisk::Write => ToolPermission::Allow,
            ToolRisk::Exec | ToolRisk::Network => ToolPermission::Ask,
        };
        assert_eq!(*seeded, expected, "{}", tool.definition().name);
    }
}
