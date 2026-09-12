//! The refusal that runs before a listener exists.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_core::ErrorKind;
use ghostai_protocol::config::Config;
use ghostai_server::boot::assert_boot_policy;

/// A settings tree with `server.host`, `server.port` and `server.auth.enabled`
/// set, and every other field at its default.
fn config(host: &str, port: u16, auth_enabled: bool) -> Config {
    let mut config = Config::default();
    host.clone_into(&mut config.server.host);
    config.server.port = port;
    config.server.auth.enabled = auth_enabled;
    config
}

#[test]
fn the_default_loopback_bind_with_authentication_on_is_allowed() {
    assert!(assert_boot_policy(&Config::default()).is_ok());
}

#[test]
fn authentication_off_on_loopback_is_allowed() {
    assert!(assert_boot_policy(&config("127.0.0.1", 7071, false)).is_ok());
}

/// The single most important check in the crate. A warning here scrolls past
/// and leaves a shell-capable agent answering to the whole network.
#[test]
fn a_non_loopback_bind_without_authentication_is_refused() {
    for host in ["0.0.0.0", "::", "192.168.1.10", "ghost.local", "[::]"] {
        let error = assert_boot_policy(&config(host, 7071, false))
            .expect_err(&format!("{host} should have been refused"));
        assert!(
            error.message.contains("Refusing to start"),
            "{host}: {}",
            error.message
        );
    }
}

#[test]
fn every_loopback_spelling_is_recognised() {
    for host in ["127.0.0.1", "127.0.0.99", "localhost", "::1", "[::1]"] {
        assert!(
            assert_boot_policy(&config(host, 7071, false)).is_ok(),
            "{host} should be loopback"
        );
    }
}

#[test]
fn the_refusal_names_the_host_the_port_and_what_to_change() {
    let error = assert_boot_policy(&config("0.0.0.0", 8080, false)).unwrap_err();
    assert!(error.message.contains("0.0.0.0"));
    assert!(error.message.contains("8080"));
    assert!(error.message.contains("server.auth.enabled"));
}

#[test]
fn the_host_and_port_travel_as_structured_detail_not_only_in_the_message() {
    let error = assert_boot_policy(&config("0.0.0.0", 8080, false)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert_eq!(error.details["host"], "0.0.0.0");
    assert_eq!(error.details["port"], 8080);
    assert_eq!(error.details.len(), 2);
}

/// This used to be a refusal, and removing it is what the one-time setup code
/// exists for: the interface that sets a password was the one thing an install
/// without a password could not reach. Startup now mints a single-use code
/// instead, so an unclaimed install is serveable and still not open.
#[test]
fn authentication_on_with_no_password_starts_and_leaves_the_claim_to_the_setup_code() {
    assert!(assert_boot_policy(&config("127.0.0.1", 7071, true)).is_ok());
}
