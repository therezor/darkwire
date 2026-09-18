//! The error taxonomy.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::error::Error as _;

use darkwire_core::{ErrorKind, WireError};
use serde_json::{Map, Value, json};

#[test]
fn carries_its_kind_and_message() {
    let error = WireError::new(ErrorKind::Tool, "read failed");
    assert_eq!(error.kind, ErrorKind::Tool);
    assert_eq!(error.message, "read failed");
    assert_eq!(error.to_string(), "read failed");
}

#[test]
fn defaults_retryable_from_the_kind() {
    assert!(WireError::new(ErrorKind::Network, "boom").retryable);
    assert!(WireError::new(ErrorKind::RateLimited, "slow down").retryable);
    assert!(WireError::new(ErrorKind::Timeout, "too slow").retryable);
    assert!(!WireError::new(ErrorKind::InvalidInput, "nope").retryable);
    // A provider 400 is the common case, and retrying it burns quota to reach
    // the same answer; the adapter overrides this when it knows the status.
    assert!(!WireError::new(ErrorKind::Provider, "bad request").retryable);
}

#[test]
fn lets_the_caller_override_retryable() {
    assert!(
        WireError::new(ErrorKind::Provider, "overloaded")
            .with_retryable(true)
            .retryable
    );
}

#[test]
fn defaults_details_to_an_empty_object() {
    assert!(WireError::new(ErrorKind::Internal, "x").details.is_empty());
}

#[test]
fn keeps_structured_details() {
    let error = WireError::new(ErrorKind::Storage, "x")
        .with_detail("sessionKey", "s")
        .with_detail("seq", 3);
    assert_eq!(error.details["sessionKey"], "s");
    assert_eq!(error.details["seq"], 3);

    let mut replaced = Map::new();
    replaced.insert("only".to_owned(), json!(true));
    let error = error.with_details(replaced.clone());
    assert_eq!(error.details, replaced);
}

#[test]
fn preserves_a_source() {
    let cause = std::io::Error::other("underlying");
    let error = WireError::new(ErrorKind::Storage, "wrapper").with_source(cause);
    assert_eq!(error.source().unwrap().to_string(), "underlying");
    assert!(
        WireError::new(ErrorKind::Storage, "bare")
            .source()
            .is_none()
    );
}

#[test]
fn has_a_retryable_default_for_every_declared_kind() {
    assert_eq!(ErrorKind::ALL.len(), 15);
    let retryable: Vec<ErrorKind> = ErrorKind::ALL
        .into_iter()
        .filter(|kind| kind.default_retryable())
        .collect();
    assert_eq!(
        retryable,
        [
            ErrorKind::Network,
            ErrorKind::Timeout,
            ErrorKind::RateLimited
        ]
    );
}

#[test]
fn spells_every_kind_in_snake_case_and_reads_it_back() {
    for kind in ErrorKind::ALL {
        assert_eq!(ErrorKind::parse(kind.as_str()), Some(kind));
        assert_eq!(kind.to_string(), kind.as_str());
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            Value::from(kind.as_str())
        );
        assert_eq!(
            serde_json::from_value::<ErrorKind>(Value::from(kind.as_str())).unwrap(),
            kind
        );
    }
    assert_eq!(ErrorKind::InvalidInput.as_str(), "invalid_input");
    assert_eq!(ErrorKind::parse("invented"), None);
}

#[test]
fn builds_a_non_retryable_aborted_error() {
    let error = WireError::aborted("Turn");
    assert_eq!(error.kind, ErrorKind::Aborted);
    assert!(!error.retryable);
    assert_eq!(error.message, "Turn aborted");
    assert!(error.is_aborted());
    assert!(!WireError::new(ErrorKind::Timeout, "x").is_aborted());
}

#[test]
fn maps_io_errors_by_kind() {
    let missing: WireError = std::io::Error::from(std::io::ErrorKind::NotFound).into();
    assert_eq!(missing.kind, ErrorKind::NotFound);
    let denied: WireError = std::io::Error::from(std::io::ErrorKind::PermissionDenied).into();
    assert_eq!(denied.kind, ErrorKind::PermissionDenied);
    let other: WireError = std::io::Error::other("disk on fire").into();
    assert_eq!(other.kind, ErrorKind::Storage);
    assert_eq!(other.message, "disk on fire");
    assert!(other.source().is_some());
}

#[test]
fn maps_sqlite_errors_to_storage() {
    let error: WireError = rusqlite::Error::InvalidQuery.into();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert!(error.source().is_some());
}

#[test]
fn has_a_debug_form_naming_the_kind() {
    let text = format!("{:?}", WireError::new(ErrorKind::JailEscape, "outside"));
    assert!(text.contains("JailEscape"));
    assert!(text.contains("outside"));
}
