//! The settings tree: defaults, stripping, the strict patch and the nullable
//! corners the fixtures do not reach.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_protocol::{
    AgentEntry, Config, ConfigPatch, DEFAULT_AGENT_ID, DEFAULT_AGENT_TOOLS, ToolPermission,
    default_agent_tools, parse_config,
};
use garde::Validate;
use serde_json::json;

#[test]
fn an_empty_object_is_a_fully_populated_tree() {
    let config = parse_config(json!({})).unwrap();
    assert_eq!(config, Config::default());
    let agent = &config.agents.list[DEFAULT_AGENT_ID];
    assert_eq!(agent.settings.max_tool_iterations, 40);
    assert_eq!(config.server.port, 3000);
    assert!(config.server.auth.enabled);
    // The tool layer is the agent's: `tools` holds the MCP servers alone.
    assert!(config.tools.mcp_servers.is_empty());
    assert_eq!(agent.settings.approval_timeout_ms, 5 * 60 * 1000);
    assert_eq!(agent.settings.max_output_chars, 8192);
    assert_eq!(agent.settings.exec.max_output_bytes, 1024 * 1024);
    assert!(!agent.settings.lazy_discovery);
    assert!(agent.settings.pinned_tools.is_empty());
    assert_eq!(config.ui.timezone, "UTC");
    assert!(config.validate().is_ok());
}

#[test]
fn a_config_re_serialises_in_declaration_order() {
    let text = serde_json::to_string_pretty(&Config::default()).unwrap();
    let workspaces = text.find("\"workspaces\"").unwrap();
    let agents = text.find("\"agents\"").unwrap();
    let ui = text.find("\"ui\"").unwrap();
    assert!(workspaces < agents && agents < ui);
    let again: Config = serde_json::from_str(&text).unwrap();
    assert_eq!(again, Config::default());
}

#[test]
fn unknown_keys_are_stripped_everywhere_but_the_patch() {
    let config =
        parse_config(json!({"bogus": 1, "server": {"port": 4242, "bogus": true}})).unwrap();
    assert_eq!(config.server.port, 4242);
    assert!(serde_json::from_value::<ConfigPatch>(json!({"bogus": 1})).is_err());
    assert!(serde_json::from_value::<ConfigPatch>(json!({"agents": {"defaults": {}}})).is_err());
    assert!(
        serde_json::from_value::<ConfigPatch>(json!({"agents": {"list": {"x": {"bogus": 1}}}}))
            .is_ok()
    );
}

#[test]
fn channels_keep_what_they_do_not_know() {
    let config = parse_config(json!({"channels": {"telegram": {"enabled": true}}})).unwrap();
    assert_eq!(config.channels.extra["telegram"], json!({"enabled": true}));
    assert!(config.channels.send_progress);
    let text = serde_json::to_value(&config.channels).unwrap();
    assert_eq!(text["telegram"], json!({"enabled": true}));
}

#[test]
fn null_deletes_and_absent_leaves_alone() {
    let patch: ConfigPatch = serde_json::from_value(json!({
        "providers": {"gone": null, "kept": {"label": "x"}},
        "tools": {"mcpServers": {"a": {"oauth": null}, "b": {}, "c": null}},
        "extensions": {"settings": {"d": null}},
    }))
    .unwrap();
    let providers = patch.providers.as_ref().unwrap();
    assert!(providers["gone"].is_none());
    assert_eq!(
        providers["kept"].as_ref().unwrap().label.as_deref(),
        Some("x")
    );
    let servers = patch.tools.as_ref().unwrap().mcp_servers.as_ref().unwrap();
    assert_eq!(servers["a"].as_ref().unwrap().oauth, Some(None));
    assert_eq!(servers["b"].as_ref().unwrap().oauth, None);
    assert!(servers["c"].is_none());
    assert!(
        patch
            .extensions
            .as_ref()
            .unwrap()
            .settings
            .as_ref()
            .unwrap()["d"]
            .is_none()
    );
    assert!(patch.validate().is_ok());

    let text = serde_json::to_value(&patch).unwrap();
    assert_eq!(text["tools"]["mcpServers"]["a"], json!({"oauth": null}));
    assert_eq!(text["tools"]["mcpServers"]["b"], json!({}));
    assert_eq!(text["providers"]["gone"], json!(null));
}

#[test]
fn a_patch_parsed_from_nothing_carries_nothing() {
    let patch: ConfigPatch = serde_json::from_value(json!({})).unwrap();
    assert_eq!(patch, ConfigPatch::default());
    assert_eq!(serde_json::to_value(&patch).unwrap(), json!({}));
}

#[test]
fn the_seed_tools_are_the_built_ins_at_their_band() {
    let tools = default_agent_tools();
    assert_eq!(tools.len(), DEFAULT_AGENT_TOOLS.len());
    assert_eq!(tools["exec"], ToolPermission::Ask);
    assert_eq!(tools["read"], ToolPermission::Allow);
    assert_eq!(AgentEntry::default().tools, tools);
}

#[test]
fn validation_reaches_into_maps_and_lists() {
    let bad_provider = parse_config(json!({"providers": {"x": {"type": ""}}})).unwrap();
    assert!(bad_provider.validate().is_err());
    let bad_agent = parse_config(json!({"agents": {"list": {"a": {"provider": ""}}}})).unwrap();
    assert!(bad_agent.validate().is_err());
    let bad_temperature =
        parse_config(json!({"agents": {"list": {"a": {"temperature": 3}}}})).unwrap();
    assert!(bad_temperature.validate().is_err());
    let bad_patch: ConfigPatch =
        serde_json::from_value(json!({"agents": {"list": {"a": {"maxTokens": 0}}}})).unwrap();
    assert!(bad_patch.validate().is_err());
    let bad_server: ConfigPatch = serde_json::from_value(json!({"tools": {"mcpServers": {"a": {"oauth": {"authUrl": "", "tokenUrl": "t", "clientId": "c"}}}}})).unwrap();
    assert!(bad_server.validate().is_err());
}

#[test]
fn a_port_outside_the_range_is_refused() {
    assert!(parse_config(json!({"server": {"port": 70000}})).is_err());
    let zero = parse_config(json!({"server": {"port": 0}})).unwrap();
    assert!(zero.validate().is_err());
}

/// The two fields the one allow-list replaced are refused, not ignored.
///
/// `Config` is loose almost everywhere, so without `deny_unknown_fields` on this
/// one struct a stale `hosts:` would parse, be dropped, and leave an agent with
/// an empty allow-list that the file still appears to describe. A security field
/// is the wrong place to be forgiving.
#[test]
fn the_egress_fields_this_replaced_are_refused_by_name() {
    for stale in ["hosts", "dns"] {
        let error = parse_config(json!({"agents": {"list": {"net": {
            "environment": {"network": {"mode": "allowlist", stale: ["example.com"]}},
        }}}}))
        .unwrap_err()
        .to_string();
        // Naming the field is this layer's job. `darkwire-core` wraps the same
        // error with the dotted path, which is what names the agent.
        assert!(error.contains(stale), "{stale}: {error}");
        assert!(error.contains("expected `mode` or `allow`"), "{error}");
    }
}

#[test]
fn the_one_allow_list_survives_a_round_trip() {
    let config = parse_config(json!({"agents": {"list": {"net": {
        "environment": {
            "name": "dev",
            "network": {"mode": "allowlist", "allow": ["10.0.0.0/8", ".example.com"]},
        },
    }}}}))
    .unwrap();
    let network = &config.agents.list["net"].environment.network;
    assert_eq!(network.allow, ["10.0.0.0/8", ".example.com"]);
}
