//! Applying a settings patch: per-field merge, wholesale replacement,
//! delete-by-null and array replacement.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_core::ErrorKind;
use darkwire_protocol::{Config, ToolPermission};
use darkwire_runtime::{DELETE_BY_NULL, REPLACE_WHOLESALE, merge_config_patch};
use serde_json::{Value, json};

fn merged(base: &Config, patch: &Value) -> Config {
    merge_config_patch(base, patch).unwrap()
}

fn refused(base: &Config, patch: &Value) -> darkwire_core::WireError {
    match merge_config_patch(base, patch) {
        Ok(config) => panic!("expected a refusal, got {config:?}"),
        Err(error) => error,
    }
}

#[test]
fn leaves_every_field_the_patch_does_not_mention() {
    let mut base = Config::default();
    base.server.port = 4321;
    base.ui.locale = "de".to_owned();
    let next = merged(&base, &json!({"server": {"host": "0.0.0.0"}}));
    assert_eq!(next.server.host, "0.0.0.0");
    assert_eq!(next.server.port, 4321);
    assert_eq!(next.ui.locale, "de");
}

#[test]
fn is_the_identity_on_an_empty_patch() {
    let base = Config::default();
    assert_eq!(merged(&base, &json!({})), base);
}

#[test]
fn does_not_mutate_the_config_it_was_given() {
    let base = Config::default();
    let before = serde_json::to_value(&base).unwrap();
    let _ = merged(&base, &json!({"server": {"port": 9}}));
    assert_eq!(serde_json::to_value(&base).unwrap(), before);
}

#[test]
fn replaces_an_array_rather_than_appending_to_it() {
    // A top-level array on a struct that merges per key, so what is asserted
    // is the array rule alone and not a wholesale replacement of its parent.
    let base = merged(
        &Config::default(),
        &json!({"extensions": {"load": ["git", "rg"]}}),
    );
    let next = merged(&base, &json!({"extensions": {"load": ["git"]}}));
    assert_eq!(next.extensions.load, vec!["git".to_owned()]);
}

#[test]
fn replaces_extra_headers_wholesale_so_a_header_can_be_deleted() {
    let base = merged(
        &Config::default(),
        &json!({"providers": {"openai": {"type": "openai", "extraHeaders": {"X-One": "1", "X-Two": "2"}}}}),
    );
    let next = merged(
        &base,
        &json!({"providers": {"openai": {"extraHeaders": {"X-One": "1"}}}}),
    );
    let headers = &next.providers["openai"].extra_headers;
    assert_eq!(headers.len(), 1);
    assert_eq!(headers["X-One"], "1");
}

#[test]
fn merges_the_providers_record_per_instance_id() {
    let base = merged(
        &Config::default(),
        &json!({"providers": {"ollama": {"type": "ollama", "apiBase": "http://a/v1"}}}),
    );
    let base = merged(
        &base,
        &json!({"providers": {"openai": {"type": "openai", "apiBase": "http://b/v1"}}}),
    );
    let next = merged(
        &base,
        &json!({"providers": {"ollama": {"apiBase": "http://c/v1"}}}),
    );
    assert_eq!(next.providers.len(), 2);
    assert_eq!(
        next.providers["ollama"].api_base.as_deref(),
        Some("http://c/v1")
    );
    assert_eq!(
        next.providers["openai"].api_base.as_deref(),
        Some("http://b/v1")
    );
    // The type survives a patch that did not restate it, which is the whole
    // difference between merging a record per entry and replacing it.
    assert_eq!(next.providers["ollama"].kind, "ollama");
}

#[test]
fn deletes_a_provider_instance_on_an_explicit_null() {
    let base = merged(
        &Config::default(),
        &json!({"providers": {
            "laptop": {"type": "ollama"},
            "gpu": {"type": "ollama", "apiBase": "http://gpu.lan:11434/v1"},
        }}),
    );
    let next = merged(&base, &json!({"providers": {"gpu": null}}));
    assert_eq!(next.providers.keys().collect::<Vec<_>>(), vec!["laptop"]);
}

#[test]
fn deletes_an_mcp_server_on_an_explicit_null() {
    let base = merged(
        &Config::default(),
        &json!({"tools": {"mcpServers": {
            "files": {"command": "npx"},
            "github": {"url": "https://mcp.github.test/mcp"},
        }}}),
    );
    let next = merged(&base, &json!({"tools": {"mcpServers": {"github": null}}}));
    assert_eq!(
        next.tools.mcp_servers.keys().collect::<Vec<_>>(),
        vec!["files"]
    );
}

#[test]
fn strips_a_null_that_says_unset_from_a_subtree_being_created() {
    // Deleting a key from nothing is a no-op; leaving the token that says so in
    // the result is not — it used to survive into the merged tree and fail the
    // re-parse as "expected object, received null".
    let next = merged(
        &Config::default(),
        &json!({"tools": {"mcpServers": {"files": {"command": "npx", "oauth": null}}}}),
    );
    assert!(next.tools.mcp_servers["files"].oauth.is_none());
}

#[test]
fn removes_an_oauth_block_an_existing_server_had() {
    let base = merged(
        &Config::default(),
        &json!({"tools": {"mcpServers": {"github": {
            "url": "https://mcp.github.test/mcp",
            "oauth": {
                "authUrl": "https://auth.test/a",
                "tokenUrl": "https://auth.test/t",
                "clientId": "x",
                "scopes": [],
                "callbackTimeoutMs": 0,
            },
        }}}}),
    );
    assert!(base.tools.mcp_servers["github"].oauth.is_some());
    let next = merged(
        &base,
        &json!({"tools": {"mcpServers": {"github": {"oauth": null}}}}),
    );
    assert!(next.tools.mcp_servers["github"].oauth.is_none());
    // The rest of the server is untouched: `oauth` is a leaf in the delete list,
    // not a record whose entry went away.
    assert_eq!(
        next.tools.mcp_servers["github"].url,
        "https://mcp.github.test/mcp"
    );
}

#[test]
fn replaces_an_mcp_server_env_rather_than_merging_it_key_by_key() {
    let base = merged(
        &Config::default(),
        &json!({"tools": {"mcpServers": {"files": {
            "command": "npx",
            "env": {"A": "1", "B": "2"},
            "headers": {"H": "1", "I": "2"},
        }}}}),
    );
    let next = merged(
        &base,
        &json!({"tools": {"mcpServers": {"files": {"env": {"A": "1"}, "headers": {"H": "1"}}}}}),
    );
    assert_eq!(next.tools.mcp_servers["files"].env.len(), 1);
    assert_eq!(next.tools.mcp_servers["files"].headers.len(), 1);
}

#[test]
fn merges_one_mcp_server_without_restating_its_siblings_or_its_fields() {
    let base = merged(
        &Config::default(),
        &json!({"tools": {"mcpServers": {"files": {"command": "npx", "args": ["-y", "srv"]}}}}),
    );
    let next = merged(
        &base,
        &json!({"tools": {"mcpServers": {"files": {"enabled": false}}}}),
    );
    assert!(!next.tools.mcp_servers["files"].enabled);
    assert_eq!(next.tools.mcp_servers["files"].args, vec!["-y", "srv"]);
}

#[test]
fn merges_the_agents_record_per_id_leaving_the_other_agents_alone() {
    let base = merged(
        &Config::default(),
        &json!({"agents": {"list": {
            "reviewer": {"label": "Reviewer", "model": "m1"},
            "writer": {"label": "Writer"},
        }}}),
    );
    let next = merged(
        &base,
        &json!({"agents": {"list": {"writer": {"label": "W"}}}}),
    );
    assert_eq!(next.agents.list["reviewer"].label, "Reviewer");
    assert_eq!(next.agents.list["writer"].label, "W");
    assert!(next.agents.list.contains_key("default"));
}

#[test]
fn replaces_one_agent_wholesale_so_clearing_an_override_is_expressible() {
    let base = merged(
        &Config::default(),
        &json!({"agents": {"list": {"default": {"temperature": 0.1, "reasoningEffort": "high"}}}}),
    );
    assert!(base.agents.list["default"].settings.temperature.is_some());
    // The patch *is* the agent: an empty temperature box means "send nothing",
    // and the re-parse fills what the patch did not name from the schema.
    let next = merged(
        &base,
        &json!({"agents": {"list": {"default": {"maxTokens": 4096}}}}),
    );
    assert!(next.agents.list["default"].settings.temperature.is_none());
    assert!(
        next.agents.list["default"]
            .settings
            .reasoning_effort
            .is_none()
    );
    assert_eq!(next.agents.list["default"].settings.max_tokens, 4096);
}

#[test]
fn replaces_an_agents_tool_map_wholesale_so_a_tool_can_be_removed() {
    let base = merged(
        &Config::default(),
        &json!({"agents": {"list": {"reviewer": {
            "tools": {"read_file": "allow", "write_file": "allow", "exec": "deny"},
        }}}}),
    );
    let next = merged(
        &base,
        &json!({"agents": {"list": {"reviewer": {"tools": {"read_file": "ask"}}}}}),
    );
    let tools = &next.agents.list["reviewer"].tools;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools["read_file"], ToolPermission::Ask);
}

#[test]
fn deletes_an_agent_on_an_explicit_null() {
    let base = merged(
        &Config::default(),
        &json!({"agents": {"list": {"reviewer": {}, "writer": {}}}}),
    );
    let next = merged(&base, &json!({"agents": {"list": {"writer": null}}}));
    assert!(next.agents.list.contains_key("reviewer"));
    assert!(!next.agents.list.contains_key("writer"));
}

#[test]
fn ignores_a_null_on_a_path_where_deletion_is_not_meaningful() {
    // `agents.list.default.model` is a leaf the schema rules on, so a `null`
    // there is a value it refuses rather than a key that goes away.
    let error = refused(
        &Config::default(),
        &json!({"agents": {"list": {"default": {"model": null}}}}),
    );
    assert_eq!(error.kind, ErrorKind::Config);
    // The path stops at the entry rather than the field: an agent's settings are
    // a flattened block, and a flattening deserialiser buffers its keys, so the
    // type failure is reported where the buffer was opened.
    assert!(
        error.message.contains("agents.list.default"),
        "{}",
        error.message
    );
    assert!(error.message.contains("null"), "{}", error.message);
}

#[test]
fn keeps_a_channel_extension_block_the_schema_does_not_name() {
    let base = merged(
        &Config::default(),
        &json!({"channels": {"telegram": {"token": "abc", "nested": {"a": 1}}}}),
    );
    let next = merged(
        &base,
        &json!({"channels": {"telegram": {"nested": {"b": 2}}}}),
    );
    let telegram = &next.channels.extra["telegram"];
    assert_eq!(telegram["token"], "abc");
    // A block the schema does not name is still a struct to the merge: it knows
    // nothing about it, so per-key merging is the rule that loses least.
    assert_eq!(telegram["nested"]["a"], 1);
    assert_eq!(telegram["nested"]["b"], 2);
}

#[test]
fn rejects_a_merge_that_produces_settings_the_schema_refuses_naming_the_path() {
    let error = refused(
        &Config::default(),
        &json!({"agents": {"list": {"default": {"temperature": 9}}}}),
    );
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(
        error
            .message
            .starts_with("Settings patch produces invalid settings:")
    );
    assert!(
        error.message.contains("agents.list.default.temperature"),
        "{}",
        error.message
    );
    // Every issue is carried structurally as well, so a form can mark fields.
    assert!(error.details["issues"].is_array());
}

#[test]
fn does_not_cascade_a_delete_into_another_agents_delegations() {
    let base = merged(
        &Config::default(),
        &json!({"agents": {"list": {
            "researcher": {"label": "Researcher"},
            "main": {"subagents": [{"id": "researcher", "prompt": "", "permission": "allow"}]},
        }}}),
    );
    // The merge is a generic tree walk and stays one: healing the orphan is the
    // reconfigure's job, so a *preview* of this patch changes only what it said.
    let next = merged(&base, &json!({"agents": {"list": {"researcher": null}}}));
    assert!(!next.agents.list.contains_key("researcher"));
    assert_eq!(next.agents.list["main"].subagents.len(), 1);
}

#[test]
fn replaces_one_extensions_settings_block_wholesale() {
    let base = merged(
        &Config::default(),
        &json!({"extensions": {"disabled": ["slack"], "settings": {"slack": {"channel": "#ops", "quiet": true}}}}),
    );
    let next = merged(
        &base,
        &json!({"extensions": {"settings": {"slack": {"channel": "#alerts"}}}}),
    );
    let slack = &next.extensions.settings["slack"];
    assert_eq!(slack["channel"], "#alerts");
    assert!(!slack.contains_key("quiet"));
    // The other keys of the block are untouched: only `settings.*` replaces.
    assert_eq!(next.extensions.disabled, vec!["slack".to_owned()]);
}

#[test]
fn deletes_an_extensions_settings_on_an_explicit_null() {
    let base = merged(
        &Config::default(),
        &json!({"extensions": {"settings": {"slack": {"channel": "#ops"}, "jira": {"url": "x"}}}}),
    );
    let next = merged(&base, &json!({"extensions": {"settings": {"slack": null}}}));
    assert_eq!(
        next.extensions.settings.keys().collect::<Vec<_>>(),
        vec!["jira"]
    );
}

#[test]
fn the_two_rule_tables_name_only_paths_a_patch_can_reach() {
    // A pattern with the wrong arity would silently never match, which is the
    // one failure mode of a dotted-path rule that no other test would show.
    for pattern in REPLACE_WHOLESALE.iter().chain(DELETE_BY_NULL.iter()) {
        let segments: Vec<&str> = pattern.split('.').collect();
        assert!(segments.len() >= 2, "{pattern}");
        assert!(
            segments.iter().all(|segment| !segment.is_empty()),
            "{pattern}"
        );
    }
    assert!(REPLACE_WHOLESALE.contains(&"agents.list.*"));
    assert!(DELETE_BY_NULL.contains(&"tools.mcpServers.*.oauth"));
}

#[test]
fn a_patch_that_is_not_an_object_is_refused_rather_than_read_positionally() {
    // A struct also deserialises from a JSON array, positionally, and every
    // field of a `Config` has a default — so without the guard this replaces the
    // whole tree and reads as a complete config.
    let error = refused(&Config::default(), &json!([]));
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(
        error.message.contains("expected an object, got an array"),
        "{}",
        error.message
    );
}
