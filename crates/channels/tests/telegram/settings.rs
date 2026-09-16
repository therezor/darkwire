//! `channels.telegram`, and what it refuses.

use darkwire_channels::telegram::settings::{TelegramSettings, parse_telegram_settings};
use darkwire_core::ErrorKind;
use serde_json::{Map, Value, json};

fn block(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().expect("an object")
}

#[test]
fn fills_in_every_default() {
    // Asserted whole, so a field the schema stops producing is a failing test
    // rather than a silently absent setting.
    let parsed = parse_telegram_settings(&Map::new()).expect("an empty block is usable");

    assert_eq!(
        parsed,
        TelegramSettings {
            enabled: true,
            allowlist: Vec::new(),
            admins: Vec::new(),
            agent_id: None,
            workspace_id: None,
            poll_timeout_sec: 30,
            edit_interval_ms: 2000,
            api_base: "https://api.telegram.org".to_owned(),
        }
    );
}

#[test]
fn reads_every_field_an_operator_may_set() {
    let parsed = parse_telegram_settings(&block(&json!({
        "enabled": false,
        "allowlist": ["4471", "-100123|the group"],
        "admins": ["4471"],
        "agentId": "researcher",
        "workspaceId": "notes",
        "pollTimeoutSec": 5,
        "editIntervalMs": 500,
        "apiBase": "http://localhost:8081",
    })))
    .expect("a full block is usable");

    assert!(!parsed.enabled);
    assert_eq!(parsed.allowlist, vec!["4471", "-100123|the group"]);
    assert_eq!(parsed.admins, vec!["4471"]);
    assert_eq!(parsed.agent_id.as_deref(), Some("researcher"));
    assert_eq!(parsed.workspace_id.as_deref(), Some("notes"));
    assert_eq!(parsed.poll_timeout_sec, 5);
    assert_eq!(parsed.edit_interval_ms, 500);
    assert_eq!(parsed.api_base, "http://localhost:8081");
}

#[test]
fn ignores_a_key_it_does_not_know() {
    // The channels config is a loose object, so a key from a newer build gets a
    // running bot rather than a refusal.
    let parsed = parse_telegram_settings(&block(&json!({ "somethingNew": 1 })))
        .expect("an unknown key is not a refusal");

    assert!(parsed.enabled);
}

#[test]
fn names_the_field_that_is_the_wrong_shape() {
    let error = parse_telegram_settings(&block(&json!({ "allowlist": "4471" })))
        .expect_err("a string is not a list");

    assert_eq!(error.kind, ErrorKind::Config);
    assert!(
        error.message.contains("channels.telegram is not usable"),
        "{}",
        error.message
    );
    assert!(error.message.contains("allowlist"), "{}", error.message);
}

#[test]
fn refuses_a_blank_allowlist_entry() {
    let error = parse_telegram_settings(&block(&json!({ "allowlist": ["4471", ""] })))
        .expect_err("a blank entry is refused");

    assert!(error.message.contains("allowlist"), "{}", error.message);
}

#[test]
fn refuses_a_blank_admin_entry() {
    let error = parse_telegram_settings(&block(&json!({ "admins": [""] })))
        .expect_err("a blank entry is refused");

    assert!(error.message.contains("admins"), "{}", error.message);
}

#[test]
fn refuses_a_named_but_empty_agent_or_workspace() {
    for field in ["agentId", "workspaceId"] {
        let error = parse_telegram_settings(&block(&json!({ field: "" })))
            .err()
            .unwrap_or_else(|| panic!("{field} must not be empty"));
        assert!(error.message.contains(field), "{}", error.message);
    }
}

#[test]
fn holds_the_poll_timeout_to_what_telegram_will_accept() {
    for timeout in [0, 51, 1000] {
        let error = parse_telegram_settings(&block(&json!({ "pollTimeoutSec": timeout })))
            .err()
            .unwrap_or_else(|| panic!("{timeout} is out of range"));
        assert!(
            error.message.contains("pollTimeoutSec"),
            "{}",
            error.message
        );
    }

    for timeout in [1, 30, 50] {
        assert!(parse_telegram_settings(&block(&json!({ "pollTimeoutSec": timeout }))).is_ok());
    }
}

#[test]
fn refuses_an_empty_api_base() {
    let error = parse_telegram_settings(&block(&json!({ "apiBase": "" })))
        .expect_err("an empty base is refused");

    assert!(error.message.contains("apiBase"), "{}", error.message);
}

#[test]
fn the_token_is_not_a_setting() {
    // It is resolved by whoever builds the factory, from the vault first: a
    // context has no vault by design, and a token should not be sitting in a
    // world-readable JSON file.
    let parsed = parse_telegram_settings(&block(&json!({ "token": "123:abc" })))
        .expect("an unknown key is ignored");

    let rendered = format!("{parsed:?}");
    assert!(!rendered.contains("123:abc"), "{rendered}");
}
