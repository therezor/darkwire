//! The registry covers every published type.
//!
//! A type that derives `JsonSchema` and is not registered is invisible to the
//! drift gate, so this scans the source for such types and diffs them against
//! the registry. The allow-list names the types the browser's registry has no
//! entry for either: unions' bodies, patch shapes it never exports, and the
//! marker types that exist only to spell a literal.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a source file that cannot be read is a failing test"
)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use ghostai_protocol::PROTOCOL_SCHEMAS;
use regex::Regex;

/// `JsonSchema` types the browser does not publish on its own.
const UNREGISTERED: &[&str] = &[
    // Wire plumbing: a literal as a type, the sequence wrapper, a marker.
    "Nullable",
    "True",
    "ProtocolVersion",
    "Sequenced",
    // Sequenced events' bodies; the registry lists them with their `seq`.
    "MessageAck",
    "MessageQueued",
    "TurnStart",
    "AssistantDelta",
    "ReasoningDelta",
    "ToolCallStarted",
    "ToolProgress",
    "ToolResult",
    "ToolApprovalRequest",
    "Notice",
    "TurnEnd",
    "SubagentEventBody",
    "ContextUsage",
    "SessionStatus",
    "SessionReset",
    "SessionReplay",
    "SessionTruncated",
    "NotificationBody",
    "ToolsChanged",
    "Steer",
    // Inline enums the browser spells as a literal list inside a field.
    "SeccompProfile",
    "NotificationLevel",
    "HealthCheckStatus",
    "HealthStatus",
    "CredentialNamespace",
    // Nested objects the browser declares inline.
    "ExtensionEngines",
    "AutomationJobCreator",
    "ErrorBody",
    // Patch shapes, which the browser derives from the full shape and never
    // exports by name.
    "AgentSettingsPatch",
    "ExecToolConfigPatch",
    "EnvironmentNetworkPatch",
    "AgentEnvironmentPatch",
    "AgentEntryPatch",
    "AgentsConfigPatch",
    "ProviderConfigPatch",
    "AuthConfigPatch",
    "ServerConfigPatch",
    "McpServerConfigPatch",
    "ToolsConfigPatch",
    "ChannelsConfigPatch",
    "SchedulerConfigPatch",
    "ExtensionsConfigPatch",
    "UiConfigPatch",
];

fn json_schema_types() -> BTreeSet<String> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let derive =
        Regex::new(r"(?s)#\[derive\(([^)]*)\)\]\s*(?:#\[[^\]]*\]\s*)*pub (?:struct|enum) (\w+)")
            .unwrap();
    let literal = Regex::new(r#"pub struct (\w+) = ""#).unwrap();
    let by_hand = Regex::new(r"impl(?:<[^>]*>)? JsonSchema for (\w+)").unwrap();
    let mut out = BTreeSet::new();
    for entry in fs::read_dir(src).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        for captures in derive.captures_iter(&text) {
            if captures[1].contains("JsonSchema") {
                out.insert(captures[2].to_owned());
            }
        }
        for captures in literal
            .captures_iter(&text)
            .chain(by_hand.captures_iter(&text))
        {
            out.insert(captures[1].to_owned());
        }
    }
    out
}

#[test]
fn every_json_schema_type_is_registered_or_listed() {
    let registered: BTreeSet<&str> = PROTOCOL_SCHEMAS.iter().map(|e| e.name).collect();
    let tag = Regex::new(r"(Tag|Kind|Role)$").unwrap();
    let missing: Vec<String> = json_schema_types()
        .into_iter()
        .filter(|name| !registered.contains(name.as_str()))
        .filter(|name| !UNREGISTERED.contains(&name.as_str()))
        // A literal marker is named for the field it fills.
        .filter(|name| !tag.is_match(name) || registered.contains(name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "JsonSchema types the registry does not name: {missing:?}"
    );
}

#[test]
fn the_registry_names_no_type_twice() {
    let mut seen = BTreeSet::new();
    let duplicates: Vec<&str> = PROTOCOL_SCHEMAS
        .iter()
        .map(|e| e.name)
        .filter(|name| !seen.insert(*name))
        .collect();
    assert!(duplicates.is_empty(), "registered twice: {duplicates:?}");
}

#[test]
fn the_allow_list_is_not_stale() {
    let types = json_schema_types();
    let stale: Vec<&&str> = UNREGISTERED
        .iter()
        .filter(|name| !types.contains(**name))
        .collect();
    assert!(
        stale.is_empty(),
        "allow-listed types that no longer exist: {stale:?}"
    );
}

#[test]
fn every_entry_generates() {
    for entry in PROTOCOL_SCHEMAS {
        let schema = serde_json::to_value(entry.generate()).unwrap();
        assert!(
            schema.is_object(),
            "{} did not generate an object schema",
            entry.name
        );
        assert_eq!(
            ghostai_protocol::registered(entry.name).map(|e| e.name),
            Some(entry.name)
        );
    }
    assert!(ghostai_protocol::registered("NoSuchSchema").is_none());
}
