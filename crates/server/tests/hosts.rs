//! The `Host` names a server answers to, which is what stops DNS rebinding.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use darkwire_protocol::config::{Config, ServerConfig};
use darkwire_server::hosts::{Authority, HostPolicy};
use darkwire_server::testkit::{TestServer, TestServerOptions, start_test_server};
use serde_json::Value;
use tower::ServiceExt as _;

fn policy(bind: &str, allowed: &[&str], hostname: Option<&str>) -> HostPolicy {
    let server = ServerConfig {
        host: bind.to_owned(),
        allowed_hosts: allowed.iter().map(|entry| (*entry).to_owned()).collect(),
        ..ServerConfig::default()
    };
    HostPolicy::new(&server, hostname)
}

// Reading a host

#[test]
fn a_host_is_read_with_or_without_its_port_and_in_any_case() {
    let parsed = |raw: &str| Authority::parse(raw).map(|a| (a.name, a.port));
    assert_eq!(parsed("LocalHost"), Some(("localhost".to_owned(), None)));
    assert_eq!(
        parsed("Example.COM.:8080"),
        Some(("example.com".to_owned(), Some(8080)))
    );
    assert_eq!(parsed("[::1]:3000"), Some(("::1".to_owned(), Some(3000))));
    assert_eq!(parsed("[::1]"), Some(("::1".to_owned(), None)));
    assert_eq!(parsed("::1"), Some(("::1".to_owned(), None)));
    for nonsense in ["", ":80", "host:port", "[::1", "[::1]x"] {
        assert_eq!(parsed(nonsense), None, "{nonsense:?}");
    }
}

// The names every server answers to

#[test]
fn loopback_names_and_ip_literals_are_always_accepted() {
    let hosts = policy("127.0.0.1", &[], None);
    for host in [
        "localhost",
        "LOCALHOST:3000",
        "localhost:5173",
        "app.localhost",
        "127.0.0.1",
        "127.0.0.1:3000",
        "[::1]:3000",
        "192.168.1.20:3000",
    ] {
        assert!(hosts.allows(host), "{host}");
    }
}

#[test]
fn a_name_nobody_configured_is_refused() {
    let hosts = policy("127.0.0.1", &[], Some("studio"));
    for host in [
        "evil.example",
        "evil.example:3000",
        "localhost.evil.example",
        "",
    ] {
        assert!(!hosts.allows(host), "{host}");
    }
}

#[test]
fn the_bind_host_and_the_machines_names_are_accepted() {
    let hosts = policy("box.lan", &[], Some("Studio"));
    assert!(hosts.allows("box.lan:3000"));
    assert!(hosts.allows("studio"));
    assert!(hosts.allows("studio.local:3000"));

    // A hostname that already carries `.local` answers to its bare form too.
    let local = policy("0.0.0.0", &[], Some("studio.local"));
    assert!(local.allows("studio"));
    assert!(local.allows("STUDIO.LOCAL"));
}

#[test]
fn an_allowed_host_matches_on_any_port_unless_it_names_one() {
    let hosts = policy(
        "127.0.0.1",
        &["darkwire.example.com", "Proxy.Example.com:8443"],
        None,
    );
    assert!(hosts.allows("darkwire.example.com"));
    assert!(hosts.allows("DARKWIRE.example.com:443"));
    assert!(hosts.allows("proxy.example.com:8443"));
    assert!(!hosts.allows("proxy.example.com"));
    assert!(!hosts.allows("proxy.example.com:9000"));
}

#[test]
fn only_a_configured_name_is_listed() {
    // What the socket's `Origin` check asks. The built-in names describe the
    // listener reached directly, where `Origin` already equals `Host`.
    let hosts = policy("127.0.0.1", &["darkwire.example.com"], Some("studio"));
    let listed = |raw: &str| hosts.lists(&Authority::parse(raw).unwrap());
    assert!(listed("darkwire.example.com"));
    assert!(!listed("localhost"));
    assert!(!listed("studio"));
}

// In front of every route

fn server(allowed: &[&str]) -> TestServer {
    let mut config = Config::default();
    config.server.allowed_hosts = allowed.iter().map(|entry| (*entry).to_owned()).collect();
    start_test_server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    })
    .expect("a test server")
}

async fn get(test: &TestServer, path: &str, host: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {}", test.token));
    if let Some(host) = host {
        request = request.header(header::HOST, host);
    }
    let response = test
        .router
        .clone()
        .oneshot(request.body(Body::empty()).expect("a well-formed request"))
        .await
        .expect("the router answered");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("a body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_rebound_name_is_refused_on_the_api_the_socket_and_the_ui() {
    let test = server(&[]);
    for path in ["/api/health", "/api/sessions", "/ws", "/chat/some-session"] {
        let (status, body) = get(&test, path, Some("evil.example:3000")).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "{path}");
        assert_eq!(body["error"]["code"], "bad_request", "{path}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("server.allowedHosts"),
            "{body}"
        );
        assert_eq!(body["error"]["details"]["host"], "evil.example:3000");
    }
}

#[tokio::test]
async fn a_known_name_reaches_the_route() {
    let test = server(&["darkwire.example.com"]);
    for host in [
        Some("localhost:3000"),
        Some("127.0.0.1:49152"),
        Some("darkwire.example.com"),
        // No `Host` at all is not a browser, and not what this defends against.
        None,
    ] {
        let (status, _) = get(&test, "/api/health", host).await;
        assert_eq!(status, StatusCode::OK, "{host:?}");
    }
}
