//! One error shape for every non-2xx response.
//!
//! A client has one branch to write, and the code it switches on comes from the
//! `ErrorCode` vocabulary the WebSocket already uses — never from a substring of
//! a message. Deriving a code by searching text for "429" or "not found" is how
//! a model that legitimately writes about rate limiting ends up triggering a
//! retry in the client rendering its answer.
//!
//! The mapping runs in one direction only: a [`GhostError`]'s `kind` decides
//! the status and the code. Nothing here inspects a message, and nothing
//! constructs a response body outside [`error_body`].

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ghostai_core::{ErrorKind, GhostError};
use ghostai_protocol::rest::{ErrorBody, ErrorResponse};
use ghostai_protocol::ws::ErrorCode;
use indexmap::IndexMap;
use serde_json::Value;

/// A generic message for anything that reached 500 without being a
/// [`GhostError`].
///
/// An unexpected failure carries a message written for a developer reading a
/// backtrace — a file path, a SQL fragment, a stringified row — and that is not
/// a thing to hand to whoever made the request. The real message goes to the
/// log line beside it.
pub const OPAQUE_500: &str = "Internal server error";

/// Status and wire code for one kind in the core taxonomy.
///
/// A `match` rather than a lookup table: exhaustiveness is the proof that every
/// kind is mapped, so a sixteenth variant added to `ghostai-core` fails to
/// compile here rather than falling through to a 500 nobody chose.
pub fn status_and_code(kind: ErrorKind) -> (StatusCode, ErrorCode) {
    match kind {
        ErrorKind::Config => (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::ConfigInvalid),
        ErrorKind::InvalidInput => (StatusCode::UNPROCESSABLE_ENTITY, ErrorCode::BadRequest),
        ErrorKind::NotFound => (StatusCode::NOT_FOUND, ErrorCode::NotFound),
        ErrorKind::Conflict => (StatusCode::CONFLICT, ErrorCode::BadRequest),
        // A path that resolved outside the workspace is a refusal, not a 404:
        // saying "not found" would let a caller map the filesystem by probing
        // for the difference between the two answers, which is why it shares
        // the 403 a refused approval gets.
        ErrorKind::PermissionDenied | ErrorKind::JailEscape => {
            (StatusCode::FORBIDDEN, ErrorCode::Unauthorized)
        }
        ErrorKind::Network | ErrorKind::Provider => {
            (StatusCode::BAD_GATEWAY, ErrorCode::ProviderError)
        }
        ErrorKind::Tool => (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::ToolError),
        ErrorKind::Timeout => (StatusCode::GATEWAY_TIMEOUT, ErrorCode::Internal),
        // The client hung up or the turn was stopped. Nothing is listening for
        // this body; the status exists so the access log tells the two cases
        // apart. 499 is outside the IANA registry, hence the fallible
        // construction with the same meaning as its fallback.
        ErrorKind::Aborted => (
            StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST),
            ErrorCode::Internal,
        ),
        ErrorKind::RateLimited => (StatusCode::TOO_MANY_REQUESTS, ErrorCode::RateLimited),
        ErrorKind::Storage | ErrorKind::Extension | ErrorKind::Internal => {
            (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal)
        }
    }
}

/// A [`GhostError`] that also names its HTTP status.
///
/// The kind mapping above covers everything raised from below the transport,
/// where HTTP does not exist. This covers the cases HTTP itself defines — a
/// missing credential is a 401 and nothing in the core taxonomy is — without
/// adding transport concepts to a crate that must not know about them.
#[derive(Debug)]
pub struct HttpError {
    /// The status to answer with.
    pub status: StatusCode,
    /// The wire code, from the `ErrorCode` vocabulary.
    pub code: ErrorCode,
    /// The core kind, so a caller that logs it sees the same taxonomy.
    pub kind: ErrorKind,
    /// For a person.
    pub message: String,
    /// Field-level detail for a 422, keyed by JSON pointer.
    pub details: IndexMap<String, Value>,
}

impl HttpError {
    /// An error naming its own status and code.
    pub fn new(
        status: StatusCode,
        code: ErrorCode,
        kind: ErrorKind,
        message: impl Into<String>,
    ) -> HttpError {
        HttpError {
            status,
            code,
            kind,
            message: message.into(),
            details: IndexMap::new(),
        }
    }

    /// Adds one field-level detail, keyed by JSON pointer.
    #[must_use]
    pub fn with_detail(mut self, pointer: impl Into<String>, value: impl Into<Value>) -> HttpError {
        self.details.insert(pointer.into(), value.into());
        self
    }

    /// Replaces the whole detail map.
    #[must_use]
    pub fn with_details(mut self, details: IndexMap<String, Value>) -> HttpError {
        self.details = details;
        self
    }

    /// No credential, or one that does not check out. Always 401, never 403.
    pub fn unauthorized(message: impl Into<String>) -> HttpError {
        HttpError::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
            ErrorKind::PermissionDenied,
            message,
        )
    }

    /// A request this server will not act on as written.
    pub fn bad_request(message: impl Into<String>) -> HttpError {
        HttpError::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::BadRequest,
            ErrorKind::InvalidInput,
            message,
        )
    }

    /// A request body, query or param that failed its schema.
    pub fn unprocessable(message: impl Into<String>) -> HttpError {
        HttpError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::BadRequest,
            ErrorKind::InvalidInput,
            message,
        )
    }

    /// No such thing.
    pub fn not_found(message: impl Into<String>) -> HttpError {
        HttpError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            ErrorKind::NotFound,
            message,
        )
    }

    /// Too many attempts at a credential.
    ///
    /// The same status the rate-limit layer produces, from a different
    /// mechanism: that layer counts requests per address in a window, and this
    /// is the login throttle deciding a caller has to wait. A client cannot
    /// tell them apart and should not have to — both mean "come back later".
    pub fn too_many_requests(message: impl Into<String>) -> HttpError {
        HttpError::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RateLimited,
            ErrorKind::RateLimited,
            message,
        )
    }

    /// The request was legal and the current state refuses it.
    ///
    /// Distinct from [`HttpError::bad_request`], and the distinction is what a
    /// client does next: a 400 means "fix the request", a 409 means "look again
    /// and decide". Saving a file the agent rewrote since it was loaded is the
    /// second, not the first.
    pub fn conflict(message: impl Into<String>) -> HttpError {
        HttpError::new(
            StatusCode::CONFLICT,
            ErrorCode::BadRequest,
            ErrorKind::Conflict,
            message,
        )
    }

    /// No provider and model are configured, so no turn can run.
    pub fn not_configured(message: impl Into<String>) -> HttpError {
        HttpError::new(
            StatusCode::CONFLICT,
            ErrorCode::NotConfigured,
            ErrorKind::Config,
            message,
        )
    }

    /// The response body this error becomes.
    pub fn body(&self) -> ErrorResponse {
        error_body(self.code, &self.message, self.details.clone())
    }
}

impl From<GhostError> for HttpError {
    /// The one direction the mapping runs: a kind decides the status.
    ///
    /// A `GhostError` is written for an operator, so its message survives even
    /// at 5xx; anything that is not one is opaque by the time it reaches here.
    fn from(error: GhostError) -> HttpError {
        let (status, code) = status_and_code(error.kind);
        HttpError {
            status,
            code,
            kind: error.kind,
            message: error.message,
            details: IndexMap::new(),
        }
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HttpError {}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        // Structured, not interpolated: the logger redacts by path, and a
        // message built by formatting is past the point redaction can reach.
        if self.status.as_u16() >= 500 {
            tracing::error!(status = self.status.as_u16(), code = ?self.code, "request failed");
        } else {
            tracing::warn!(status = self.status.as_u16(), code = ?self.code, "request rejected");
        }
        (self.status, Json(self.body())).into_response()
    }
}

/// Normalises a `GhostError` into the status, code and body to answer with.
///
/// The 5xx opacity rule lives here rather than in [`From<GhostError>`]: a
/// `GhostError` that reached the transport was written by this codebase and
/// names something an operator can act on, so it is kept; `expected` is false
/// for anything that arrived some other way.
pub fn resolve_error(error: GhostError, expected: bool) -> HttpError {
    let mut http = HttpError::from(error);
    if http.status.as_u16() >= 500 && !expected {
        OPAQUE_500.clone_into(&mut http.message);
    }
    http
}

/// The one place a non-2xx body is constructed, so no route can invent a
/// second shape.
pub fn error_body(
    code: ErrorCode,
    message: &str,
    details: IndexMap<String, Value>,
) -> ErrorResponse {
    ErrorResponse {
        error: ErrorBody {
            code: code_str(code).to_owned(),
            message: message.to_owned(),
            details: if details.is_empty() {
                None
            } else {
                Some(details)
            },
        },
    }
}

/// The wire spelling of an [`ErrorCode`].
///
/// `ErrorBody.code` is a `String` because the response schema types it that
/// way — a document generated from it must not pin clients to today's list —
/// while the WebSocket's `error` event carries the union. Both go through here,
/// so the two spellings cannot drift.
pub fn code_str(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unauthorized => "unauthorized",
        ErrorCode::BadRequest => "bad_request",
        ErrorCode::NotFound => "not_found",
        ErrorCode::RateLimited => "rate_limited",
        ErrorCode::ProviderError => "provider_error",
        ErrorCode::ToolError => "tool_error",
        ErrorCode::ConfigInvalid => "config_invalid",
        ErrorCode::NotConfigured => "not_configured",
        ErrorCode::SessionBusy => "session_busy",
        ErrorCode::Internal => "internal",
    }
}
