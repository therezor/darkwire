//! Typed provider failures: reasons, status classification, `Retry-After` and transport wording.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::io;

use darkwire_core::{ErrorKind, WireError};
use darkwire_providers::{
    ProviderError, ProviderErrorReason, TransportContext, WireErrorBody, classify_status,
    parse_retry_after, transport_error,
};
use serde_json::{Value, json};

#[test]
fn a_reason_maps_onto_the_core_taxonomy() {
    let kind = |reason: ProviderErrorReason| ProviderError::new(reason, "x").into_wire().kind;
    assert_eq!(kind(ProviderErrorReason::RateLimit), ErrorKind::RateLimited);
    assert_eq!(kind(ProviderErrorReason::Transport), ErrorKind::Network);
    assert_eq!(kind(ProviderErrorReason::Auth), ErrorKind::PermissionDenied);
    assert_eq!(kind(ProviderErrorReason::Aborted), ErrorKind::Aborted);
    assert_eq!(
        kind(ProviderErrorReason::UnsupportedParam),
        ErrorKind::Provider
    );
    assert_eq!(
        kind(ProviderErrorReason::ModelNotFound),
        ErrorKind::NotFound
    );
    assert_eq!(kind(ProviderErrorReason::Timeout), ErrorKind::Timeout);
}

#[test]
fn retryability_comes_from_the_reason_unless_overridden() {
    assert!(ProviderError::new(ProviderErrorReason::Server, "x").retryable);
    assert!(!ProviderError::new(ProviderErrorReason::InvalidRequest, "x").retryable);
    let overridden = ProviderError::new(ProviderErrorReason::InvalidRequest, "x")
        .with_retryable(true)
        .into_wire();
    assert!(overridden.retryable);
}

#[test]
fn the_diagnosis_lives_in_structured_details() {
    let error = ProviderError::new(ProviderErrorReason::UnsupportedParam, "nope")
        .with_provider("openai")
        .with_status(400)
        .with_code(Some("unsupported_parameter".into()))
        .with_param(Some("reasoning_effort".into()))
        .into_wire();
    assert_eq!(
        Value::Object(error.details.clone()),
        json!({
            "reason": "unsupported_param",
            "providerId": "openai",
            "status": 400,
            "code": "unsupported_parameter",
            "param": "reasoning_effort",
        })
    );
    // Redaction and log filtering work by path, so absent fields must be
    // absent rather than present-and-null.
    let bare = ProviderError::new(ProviderErrorReason::Server, "x").into_wire();
    let keys: Vec<&String> = bare.details.keys().collect();
    assert_eq!(keys, vec!["reason"]);
    // Empty code and param are absent too.
    let empty = ProviderError::new(ProviderErrorReason::Server, "x")
        .with_code(Some(String::new()))
        .with_param(Some(String::new()));
    assert_eq!(empty.code, None);
    assert_eq!(empty.param, None);
}

#[test]
fn a_provider_error_round_trips_through_the_core_error() {
    let original = ProviderError::new(ProviderErrorReason::RateLimit, "slow down")
        .with_provider("groq")
        .with_status(429)
        .with_retry_after_ms(Some(2000))
        .with_detail("url", "http://x/v1/chat/completions");
    let ghost: WireError = original.clone().into();
    assert!(ProviderError::is_provider_error(&ghost));
    assert_eq!(ghost.kind, ErrorKind::RateLimited);
    assert_eq!(ProviderError::of(&ghost), original);
    assert_eq!(
        ProviderError::reason_of(&ghost),
        ProviderErrorReason::RateLimit
    );
    assert_eq!(original.to_string(), "slow down");
}

#[test]
fn a_bare_core_error_is_classified_by_kind() {
    assert_eq!(
        ProviderError::of(&WireError::aborted("Request")).reason,
        ProviderErrorReason::Aborted
    );
    assert_eq!(
        ProviderError::of(&WireError::new(ErrorKind::Timeout, "slow")).reason,
        ProviderErrorReason::Timeout
    );
    // Everything else on the request path is a failed connection.
    let plain = WireError::new(ErrorKind::Internal, "x");
    assert!(!ProviderError::is_provider_error(&plain));
    assert_eq!(
        ProviderError::of(&plain).reason,
        ProviderErrorReason::Transport
    );
    // A detail that spells a reason nothing recognises is not one.
    let foreign = WireError::new(ErrorKind::Provider, "x").with_detail("reason", "teapot");
    assert!(!ProviderError::is_provider_error(&foreign));
}

#[test]
fn every_declared_reason_has_a_spelling_and_a_kind() {
    for reason in ProviderErrorReason::ALL {
        assert_eq!(ProviderErrorReason::parse(reason.as_str()), Some(reason));
        assert_eq!(reason.to_string(), reason.as_str());
        let _ = reason.kind();
        let _ = reason.default_retryable();
    }
    assert_eq!(ProviderErrorReason::parse("nope"), None);
    let json = serde_json::to_string(&ProviderErrorReason::StreamParse).unwrap();
    assert_eq!(json, "\"stream_parse\"");
}

#[test]
fn classify_status_reads_the_status_where_it_is_enough() {
    let by = |status| classify_status(status, None);
    assert_eq!(by(401), ProviderErrorReason::Auth);
    assert_eq!(by(403), ProviderErrorReason::Auth);
    assert_eq!(by(404), ProviderErrorReason::ModelNotFound);
    assert_eq!(by(408), ProviderErrorReason::Timeout);
    assert_eq!(by(429), ProviderErrorReason::RateLimit);
    assert_eq!(by(500), ProviderErrorReason::Server);
    assert_eq!(by(502), ProviderErrorReason::Server);
    assert_eq!(by(503), ProviderErrorReason::Overloaded);
    // Anthropic's non-standard overload status, which no client special-cases.
    assert_eq!(by(529), ProviderErrorReason::Overloaded);
    assert_eq!(by(200), ProviderErrorReason::Unknown);
    assert_eq!(by(302), ProviderErrorReason::Unknown);
}

fn body(code: Option<&str>, param: Option<&str>, message: Option<&str>) -> WireErrorBody {
    WireErrorBody {
        message: message.map(str::to_owned),
        kind: None,
        code: code.map(str::to_owned),
        param: param.map(str::to_owned),
    }
}

#[test]
fn classify_status_reads_the_code_on_a_400() {
    let by = |code| classify_status(400, Some(&body(Some(code), None, None)));
    assert_eq!(
        by("context_length_exceeded"),
        ProviderErrorReason::ContextLength
    );
    assert_eq!(
        by("unsupported_parameter"),
        ProviderErrorReason::UnsupportedParam
    );
    assert_eq!(by("invalid_model"), ProviderErrorReason::ModelNotFound);
    assert_eq!(by("content_filter"), ProviderErrorReason::ContentFilter);
    assert_eq!(by("insufficient_quota"), ProviderErrorReason::RateLimit);
    assert_eq!(by("something_else"), ProviderErrorReason::InvalidRequest);
}

#[test]
fn classify_status_treats_a_named_param_as_the_provider_pointing() {
    assert_eq!(
        classify_status(400, Some(&body(None, Some("reasoning_effort"), None))),
        ProviderErrorReason::UnsupportedParam
    );
    assert_eq!(
        classify_status(400, Some(&body(None, Some(""), None))),
        ProviderErrorReason::InvalidRequest
    );
}

#[test]
fn classify_status_never_reads_the_message_text() {
    // A body whose prose says "context length exceeded" but carries no code
    // is an ordinary bad request.
    assert_eq!(
        classify_status(
            400,
            Some(&body(
                None,
                None,
                Some("context length exceeded, rate limit, overloaded")
            ))
        ),
        ProviderErrorReason::InvalidRequest
    );
}

#[test]
fn wire_error_body_reads_the_four_fields() {
    let parsed = WireErrorBody::from_value(&json!({
        "message": "m", "type": "t", "code": "c", "param": "p", "extra": 1
    }));
    assert_eq!(
        parsed,
        body(Some("c"), Some("p"), Some("m")).clone_with_kind("t")
    );
}

trait WithKind {
    fn clone_with_kind(self, kind: &str) -> WireErrorBody;
}

impl WithKind for WireErrorBody {
    fn clone_with_kind(mut self, kind: &str) -> WireErrorBody {
        self.kind = Some(kind.to_owned());
        self
    }
}

const NOW: i64 = 1_785_153_600_000; // 2026-07-27T12:00:00Z

#[test]
fn retry_after_reads_delta_seconds() {
    assert_eq!(parse_retry_after(Some("2"), NOW), Some(2000));
    assert_eq!(parse_retry_after(Some("  30 "), NOW), Some(30_000));
}

#[test]
fn retry_after_reads_an_http_date() {
    assert_eq!(
        parse_retry_after(Some("Mon, 27 Jul 2026 12:00:05 GMT"), NOW),
        Some(5000)
    );
    // The two obsolete forms the RFC still requires recipients to accept.
    assert_eq!(
        parse_retry_after(Some("Monday, 27-Jul-26 12:00:05 GMT"), NOW),
        Some(5000)
    );
    assert_eq!(
        parse_retry_after(Some("Mon Jul 27 12:00:05 2026"), NOW),
        Some(5000)
    );
    // Never negative for a date already past.
    assert_eq!(
        parse_retry_after(Some("Mon, 27 Jul 2026 11:59:00 GMT"), NOW),
        Some(0)
    );
}

#[test]
fn retry_after_is_none_for_absent_or_malformed_headers() {
    assert_eq!(parse_retry_after(None, NOW), None);
    assert_eq!(parse_retry_after(Some(""), NOW), None);
    assert_eq!(parse_retry_after(Some("soon"), NOW), None);
    assert_eq!(parse_retry_after(Some("-5"), NOW), None);
    assert_eq!(parse_retry_after(Some("Mon, 99 Foo 2026"), NOW), None);
}

#[derive(Debug)]
struct Wrapper(io::Error);

impl std::fmt::Display for Wrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("error sending request")
    }
}

impl std::error::Error for Wrapper {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// The client's generic error with the socket's reason nested, as it is
/// thrown.
fn wrapped(kind: io::ErrorKind, detail: &str) -> Wrapper {
    Wrapper(io::Error::new(kind, detail.to_owned()))
}

fn context() -> TransportContext {
    TransportContext {
        url: Some("http://127.0.0.1:11434/v1/chat/completions".into()),
        label: Some("Ollama".into()),
    }
}

#[test]
fn a_refused_connection_names_the_endpoint_and_is_not_retryable() {
    let error = transport_error(
        &wrapped(io::ErrorKind::ConnectionRefused, "connect ECONNREFUSED"),
        "ollama",
        &context(),
    );
    // The origin, not the path: which server is down is the question.
    assert_eq!(
        error.message,
        "Could not reach Ollama at http://127.0.0.1:11434 — nothing is listening there."
    );
    assert!(!error.retryable);
    assert_eq!(error.reason, ProviderErrorReason::Transport);
    assert_eq!(error.details.get("code"), Some(&json!("ECONNREFUSED")));
    assert_eq!(
        error.details.get("url"),
        Some(&json!("http://127.0.0.1:11434/v1/chat/completions"))
    );
}

#[test]
fn a_timeout_and_a_reset_stay_retryable() {
    let timed_out = transport_error(&wrapped(io::ErrorKind::TimedOut, "x"), "ollama", &context());
    assert!(timed_out.message.ends_with("the connection timed out."));
    assert!(timed_out.retryable);
    let reset = transport_error(
        &wrapped(io::ErrorKind::ConnectionReset, "x"),
        "ollama",
        &TransportContext::default(),
    );
    assert_eq!(
        reset.message,
        "Could not reach ollama — it closed the connection before answering."
    );
    let unreachable = transport_error(
        &wrapped(io::ErrorKind::HostUnreachable, "x"),
        "custom",
        &TransportContext {
            url: Some("http://rzr-ai:8080/v1/chat/completions".into()),
            label: Some("Custom".into()),
        },
    );
    assert_eq!(
        unreachable.message,
        "Could not reach Custom at http://rzr-ai:8080 — there is no route to that host."
    );
    assert!(
        transport_error(
            &wrapped(io::ErrorKind::NetworkUnreachable, "x"),
            "c",
            &context()
        )
        .message
        .contains("that network is unreachable")
    );
}

#[test]
fn a_fault_without_wording_falls_back_to_the_innermost_message() {
    let error = transport_error(
        &wrapped(io::ErrorKind::Other, "failed to lookup address information"),
        "ollama",
        &context(),
    );
    assert_eq!(
        error.message,
        "Could not reach Ollama at http://127.0.0.1:11434 — failed to lookup address information."
    );
    assert!(error.retryable);
    assert!(error.details.get("code").is_none());
    // A URL that does not parse is reported as it was given.
    let odd = transport_error(
        &wrapped(io::ErrorKind::Other, "boom"),
        "x",
        &TransportContext {
            url: Some("not a url".into()),
            label: None,
        },
    );
    assert_eq!(odd.message, "Could not reach x at not a url — boom.");
}
