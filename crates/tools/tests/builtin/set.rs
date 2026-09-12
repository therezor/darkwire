//! The built-in set, and what packages below this one assume about it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_protocol::{
    BUILTIN_TOOL_NAMES, DEFAULT_AGENT_TOOLS, ToolPermission, ToolRisk, ToolSource, ToolsConfig,
};
use ghostai_tools::builtin::all_builtin_tools;
use ghostai_tools::{BuiltinOptions, ToolRegistry, builtin_tools, register_builtins};

#[test]
fn registers_every_built_in_under_the_builtin_source() {
    let registry = ToolRegistry::new();
    register_builtins(&registry, None, BuiltinOptions::default()).unwrap();
    assert_eq!(
        registry.names(),
        vec![
            "automation",
            "edit_file",
            "exec",
            "list_dir",
            "memory",
            "read_file",
            "skill",
            "write_file"
        ]
    );
    assert_eq!(registry.source_of("exec"), Some(ToolSource::Builtin));
}

#[test]
fn does_not_advertise_exec_when_config_disables_it() {
    let registry = ToolRegistry::new();
    let mut config = ToolsConfig::default();
    config.exec.enable = false;
    register_builtins(&registry, Some(&config), BuiltinOptions::default()).unwrap();
    assert!(!registry.has("exec"));
    assert_eq!(registry.size(), all_builtin_tools().len() - 1);
}

#[test]
fn does_not_advertise_automation_when_the_scheduler_is_switched_off() {
    let registry = ToolRegistry::new();
    register_builtins(
        &registry,
        Some(&ToolsConfig::default()),
        BuiltinOptions { scheduler: false },
    )
    .unwrap();
    assert!(!registry.has("automation"));
    assert!(registry.has("exec"));
}

#[test]
fn includes_both_by_default_and_with_no_config_at_all() {
    assert_eq!(
        builtin_tools(None, BuiltinOptions::default()).len(),
        all_builtin_tools().len()
    );
    assert_eq!(
        builtin_tools(Some(&ToolsConfig::default()), BuiltinOptions::default()).len(),
        all_builtin_tools().len()
    );
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
fn is_what_a_new_agent_is_seeded_with_save_for_the_one_deliberate_omission() {
    let mut seeded: Vec<String> = DEFAULT_AGENT_TOOLS
        .iter()
        .map(|(n, _)| (*n).to_owned())
        .collect();
    seeded.sort();
    let expected: Vec<String> = names().into_iter().filter(|n| n != "automation").collect();
    assert_eq!(seeded, expected);
    assert!(!DEFAULT_AGENT_TOOLS.iter().any(|(n, _)| *n == "automation"));
    assert!(BUILTIN_TOOL_NAMES.contains(&"automation"));
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
