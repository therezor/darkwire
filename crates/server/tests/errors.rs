//! One error shape for every non-2xx response, and one direction of mapping.
//!
//! The test that matters most here is the completeness one: every kind in the
//! core taxonomy maps to a status and a code. The mapping is a `match`, so the
//! compiler already refuses a missing arm — what this adds is the assertion
//! that the arms are the *right* ones, which no compiler can make.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use axum::http::StatusCode;
use ghostai_core::{ErrorKind, GhostError};
use ghostai_protocol::ws::ErrorCode;
use ghostai_server::errors::{
    HttpError, OPAQUE_500, code_str, error_body, resolve_error, status_and_code,
};
use indexmap::IndexMap;

#[test]
fn every_kind_in_the_taxonomy_is_mapped() {
    for kind in ErrorKind::ALL {
        let (status, _) = status_and_code(kind);
        assert!(
            status.as_u16() >= 400,
            "{kind} mapped to a success status: {status}"
        );
    }
}

#[test]
fn the_mapping_is_the_one_the_clients_were_written_against() {
    let expected = [
        (ErrorKind::Config, 500, ErrorCode::ConfigInvalid),
        (ErrorKind::InvalidInput, 422, ErrorCode::BadRequest),
        (ErrorKind::NotFound, 404, ErrorCode::NotFound),
        (ErrorKind::Conflict, 409, ErrorCode::BadRequest),
        (ErrorKind::PermissionDenied, 403, ErrorCode::Unauthorized),
        (ErrorKind::JailEscape, 403, ErrorCode::Unauthorized),
        (ErrorKind::Network, 502, ErrorCode::ProviderError),
        (ErrorKind::Provider, 502, ErrorCode::ProviderError),
        (ErrorKind::Tool, 500, ErrorCode::ToolError),
        (ErrorKind::Timeout, 504, ErrorCode::Internal),
        (ErrorKind::Aborted, 499, ErrorCode::Internal),
        (ErrorKind::RateLimited, 429, ErrorCode::RateLimited),
        (ErrorKind::Storage, 500, ErrorCode::Internal),
        (ErrorKind::Extension, 500, ErrorCode::Internal),
        (ErrorKind::Internal, 500, ErrorCode::Internal),
    ];
    assert_eq!(expected.len(), ErrorKind::ALL.len());
    for (kind, status, code) in expected {
        let (actual_status, actual_code) = status_and_code(kind);
        assert_eq!(actual_status.as_u16(), status, "{kind}");
        assert_eq!(actual_code, code, "{kind}");
    }
}

#[test]
fn a_jail_escape_is_a_refusal_rather_than_a_not_found() {
    // Saying "not found" would let a caller map the filesystem by probing for
    // the difference between the two answers.
    let (status, _) = status_and_code(ErrorKind::JailEscape);
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_ne!(status, StatusCode::NOT_FOUND);
}

#[test]
fn an_abort_keeps_its_own_status_so_the_access_log_can_tell_it_apart() {
    let (status, _) = status_and_code(ErrorKind::Aborted);
    assert_eq!(status.as_u16(), 499);
}

#[test]
fn a_ghost_error_at_five_hundred_keeps_its_message_when_it_was_expected() {
    let error = GhostError::new(ErrorKind::Storage, "the database file is read-only");
    let http = resolve_error(error, true);
    assert_eq!(http.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(http.message, "the database file is read-only");
}

#[test]
fn anything_unexpected_at_five_hundred_is_opaque() {
    // A message written for a developer reading a backtrace — a file path, a
    // SQL fragment, a stringified row — is not a thing to hand to whoever made
    // the request.
    let error = GhostError::new(ErrorKind::Internal, "no such column: sessions.wrkspace_id");
    let http = resolve_error(error, false);
    assert_eq!(http.message, OPAQUE_500);
}

#[test]
fn a_four_hundred_keeps_its_message_whether_it_was_expected_or_not() {
    let error = GhostError::new(ErrorKind::NotFound, "No session named \"abc\"");
    assert_eq!(
        resolve_error(error, false).message,
        "No session named \"abc\""
    );
}

#[test]
fn the_envelope_omits_details_when_there_are_none() {
    let body = error_body(ErrorCode::NotFound, "gone", IndexMap::new());
    assert_eq!(body.error.code, "not_found");
    assert_eq!(body.error.message, "gone");
    assert!(body.error.details.is_none());
}

#[test]
fn the_envelope_carries_details_keyed_by_json_pointer() {
    let error = HttpError::unprocessable("Invalid body")
        .with_detail("/title", "must not be empty")
        .with_detail("/agents/list/reviewer/model", "unknown model");
    let body = error.body();
    let details = body.error.details.expect("a 422 carries details");
    assert_eq!(details["/title"], "must not be empty");
    assert_eq!(details["/agents/list/reviewer/model"], "unknown model");
}

#[test]
fn a_missing_credential_is_always_a_four_hundred_and_one() {
    let error = HttpError::unauthorized("No credential");
    assert_eq!(error.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error.code, ErrorCode::Unauthorized);
    // 401 rather than 403, which is what the kind alone would have produced:
    // the core taxonomy has no concept of a missing credential, and this is the
    // case HTTP itself defines.
    assert_eq!(error.kind, ErrorKind::PermissionDenied);
}

#[test]
fn a_conflict_is_look_again_rather_than_fix_the_request() {
    let error = HttpError::conflict("The file changed since it was read");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.kind, ErrorKind::Conflict);
}

#[test]
fn an_unconfigured_install_says_so_rather_than_reporting_a_broken_config() {
    // Nothing is wrong with the settings; they are merely incomplete, and the
    // client's response is to offer setup rather than to report a fault.
    let error = HttpError::not_configured("No provider and model are configured");
    assert_eq!(error.code, ErrorCode::NotConfigured);
    assert_ne!(error.code, ErrorCode::ConfigInvalid);
}

#[test]
fn the_ghost_error_conversion_runs_only_from_kind_to_status() {
    let error = GhostError::new(ErrorKind::RateLimited, "slow down");
    let http = HttpError::from(error);
    assert_eq!(http.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(http.code, ErrorCode::RateLimited);
}

#[test]
fn every_wire_code_has_a_spelling_and_they_are_all_distinct() {
    let codes = [
        ErrorCode::Unauthorized,
        ErrorCode::BadRequest,
        ErrorCode::NotFound,
        ErrorCode::RateLimited,
        ErrorCode::ProviderError,
        ErrorCode::ToolError,
        ErrorCode::ConfigInvalid,
        ErrorCode::NotConfigured,
        ErrorCode::SessionBusy,
        ErrorCode::Internal,
    ];
    let mut spellings: Vec<&str> = codes.iter().map(|code| code_str(*code)).collect();
    let count = spellings.len();
    spellings.sort_unstable();
    spellings.dedup();
    assert_eq!(spellings.len(), count);
    assert_eq!(code_str(ErrorCode::SessionBusy), "session_busy");
    assert_eq!(code_str(ErrorCode::ProviderError), "provider_error");
}

#[test]
fn details_can_be_replaced_wholesale() {
    let mut details = IndexMap::new();
    details.insert(
        "/".to_owned(),
        serde_json::Value::from("expected an object"),
    );
    let error = HttpError::unprocessable("Invalid body").with_details(details);
    let body = error.body();
    assert_eq!(
        body.error.details.expect("details")["/"],
        "expected an object"
    );
}
