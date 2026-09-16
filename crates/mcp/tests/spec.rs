//! Resolving one config entry into a connectable spec, and the two fingerprints.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_core::ErrorKind;
use darkwire_mcp::{
    McpConnectionSpec, McpTransportSpec, exposure_fingerprint, resolve_spec, transport_fingerprint,
};
use darkwire_protocol::{McpServerConfig, McpTransport};
use serde_json::{Value, json};

/// The schema's defaults, so a case states only what it is about.
fn config(overrides: Value) -> McpServerConfig {
    serde_json::from_value(overrides).unwrap()
}

#[test]
fn infers_stdio_from_a_command() {
    let spec = resolve_spec("files", &config(json!({ "command": "npx" }))).unwrap();
    assert_eq!(spec.kind(), McpTransport::Stdio);
}

#[test]
fn infers_streamable_http_from_a_url_never_the_deprecated_sse() {
    let spec = resolve_spec(
        "remote",
        &config(json!({ "url": "https://example.test/mcp" })),
    )
    .unwrap();
    assert_eq!(spec.kind(), McpTransport::StreamableHttp);
}

#[test]
fn reaches_sse_only_when_the_entry_asks_for_it_by_name() {
    let spec = resolve_spec(
        "legacy",
        &config(json!({ "type": "sse", "url": "https://example.test/sse" })),
    )
    .unwrap();
    assert_eq!(spec.kind(), McpTransport::Sse);
}

#[test]
fn trims_so_a_pasted_command_with_a_trailing_space_still_resolves() {
    let spec = resolve_spec("files", &config(json!({ "command": "  npx  " }))).unwrap();
    match spec.transport {
        McpTransportSpec::Stdio { command, .. } => assert_eq!(command, "npx"),
        McpTransportSpec::Http { .. } => panic!("expected stdio"),
    }
}

#[test]
fn carries_the_arguments_environment_and_exposure_through() {
    let spec = resolve_spec(
        "files",
        &config(json!({
            "command": "npx",
            "args": ["-y", "server"],
            "env": { "TOKEN": "x" },
            "enabledTools": ["read"],
            "toolTimeoutMs": 5000
        })),
    )
    .unwrap();
    let McpTransportSpec::Stdio { args, env, .. } = &spec.transport else {
        panic!("expected stdio");
    };
    assert_eq!(args, &["-y", "server"]);
    assert_eq!(env.get("TOKEN").map(String::as_str), Some("x"));
    assert_eq!(spec.enabled_tools, ["read"]);
    assert_eq!(spec.tool_timeout_ms, 5_000);
    assert!(spec.url().is_none());
    assert!(spec.oauth().is_none());
}

#[test]
fn refuses_an_entry_that_names_neither_a_command_nor_a_url() {
    let error = resolve_spec("empty", &config(json!({}))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("neither a command nor a url"));
}

#[test]
fn refuses_an_entry_that_names_both() {
    let error = resolve_spec(
        "both",
        &config(json!({ "command": "npx", "url": "https://example.test/mcp" })),
    )
    .unwrap_err();
    assert!(error.message.contains("both"));
}

#[test]
fn refuses_an_explicit_type_whose_own_field_is_missing() {
    // Caught here rather than as an unexplained spawn failure a minute later.
    assert!(resolve_spec("a", &config(json!({ "type": "stdio" }))).is_err());
    assert!(resolve_spec("b", &config(json!({ "type": "streamableHttp" }))).is_err());
    let sse = resolve_spec("c", &config(json!({ "type": "sse" }))).unwrap_err();
    assert!(sse.message.contains("sse server with no url"));
}

#[test]
fn refuses_a_url_that_is_not_one() {
    let error = resolve_spec("remote", &config(json!({ "url": "not a url" }))).unwrap_err();
    assert!(error.message.contains("is not a URL"));
}

#[test]
fn refuses_a_scheme_this_client_does_not_speak() {
    let error =
        resolve_spec("remote", &config(json!({ "url": "file:///etc/passwd" }))).unwrap_err();
    assert!(error.message.contains("only http and https"));
}

#[test]
fn accepts_a_loopback_url_which_the_ssrf_guard_would_have_refused() {
    // The single most common MCP deployment. See the module docs.
    let spec = resolve_spec(
        "local",
        &config(json!({ "url": "http://127.0.0.1:3001/mcp" })),
    )
    .unwrap();
    assert_eq!(spec.url(), Some("http://127.0.0.1:3001/mcp"));
}

#[test]
fn carries_headers_and_oauth_for_an_http_server() {
    let spec = resolve_spec(
        "remote",
        &config(json!({
            "url": "https://example.test/mcp",
            "headers": { "X-Key": "k" },
            "oauth": { "authUrl": "https://a.test/a", "tokenUrl": "https://a.test/t", "clientId": "c" }
        })),
    )
    .unwrap();
    assert_eq!(spec.oauth().map(|o| o.client_id.as_str()), Some("c"));
    let McpTransportSpec::Http { headers, .. } = &spec.transport else {
        panic!("expected http");
    };
    assert_eq!(headers.get("X-Key").map(String::as_str), Some("k"));
}

fn spec_of(overrides: Value) -> McpConnectionSpec {
    let mut base = json!({ "command": "npx", "args": ["-y", "server"] });
    if let (Value::Object(base), Value::Object(overrides)) = (&mut base, overrides) {
        base.extend(overrides);
    }
    resolve_spec("files", &config(base)).unwrap()
}

#[test]
fn moves_the_transport_fingerprint_when_the_process_would_change() {
    assert_ne!(
        transport_fingerprint(&spec_of(json!({}))),
        transport_fingerprint(&spec_of(json!({ "args": ["-y", "other"] })))
    );
    assert_ne!(
        transport_fingerprint(&spec_of(json!({}))),
        transport_fingerprint(&spec_of(json!({ "env": { "TOKEN": "x" } })))
    );
}

#[test]
fn holds_the_transport_fingerprint_still_when_only_exposure_changed() {
    // The whole reason there are two: narrowing `enabledTools` must not kill
    // and respawn a subprocess to re-filter a list already in memory.
    assert_eq!(
        transport_fingerprint(&spec_of(json!({}))),
        transport_fingerprint(&spec_of(json!({ "enabledTools": ["read"] })))
    );
    assert_ne!(
        exposure_fingerprint(&spec_of(json!({}))),
        exposure_fingerprint(&spec_of(json!({ "enabledTools": ["read"] })))
    );
    assert_ne!(
        exposure_fingerprint(&spec_of(json!({}))),
        exposure_fingerprint(&spec_of(json!({ "toolTimeoutMs": 1000 })))
    );
}

#[test]
fn fingerprints_an_http_server_by_its_endpoint_headers_and_oauth() {
    let base = json!({ "url": "https://example.test/mcp" });
    let plain = resolve_spec("r", &config(base.clone())).unwrap();
    let with_header = resolve_spec(
        "r",
        &config(json!({ "url": "https://example.test/mcp", "headers": { "A": "b" } })),
    )
    .unwrap();
    assert_ne!(
        transport_fingerprint(&plain),
        transport_fingerprint(&with_header)
    );
}
