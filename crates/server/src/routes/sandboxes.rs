//! The containers the sandbox service is holding open.
//!
//! Unlike the two lists next door, this one is *not* read from disk: an
//! instance is live state owned by another process, so both routes forward to
//! it over its socket and return what it says.
//!
//! Lifecycle is writable where approval is not, and the asymmetry is
//! deliberate. Starting, stopping and restarting an instance changes nothing
//! about what an agent is permitted to run — an operator who stops a container
//! gets a new one on the next command, under the same approved definition. Only
//! `execute` is refused here, because a model's tool call reaches the service
//! through the agent loop, which supplies the approval hash it resolved the
//! toolbox at; an `execute` arriving as a management call has no such
//! provenance, whatever it claims.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use ghostai_core::{ErrorKind, GhostError};
use ghostai_protocol::rest::{SandboxListResponse, SandboxRequest};

use crate::errors::HttpError;
use crate::routes::AppState;
use crate::schema::parse_body;

/// Live managed containers, including busy and shared state.
pub async fn list_sandboxes(
    State(state): State<AppState>,
) -> Result<Json<SandboxListResponse>, HttpError> {
    let value = state
        .runtime
        .sandbox_request(
            serde_json::to_value(SandboxRequest::List)
                .map_err(|error| GhostError::new(ErrorKind::Internal, error.to_string()))?,
        )
        .await?;
    let response: SandboxListResponse = serde_json::from_value(value)
        .map_err(|error| GhostError::new(ErrorKind::Internal, error.to_string()))?;
    Ok(Json(response))
}

/// Only lifecycle actions reach the service through HTTP.
///
/// The body is read as JSON and deserialised here rather than extracted as a
/// `Json<SandboxRequest>`, because axum's own rejection is `text/plain` and
/// every other error this server returns is the JSON envelope. A client that
/// has to parse two error formats from one API parses neither reliably.
pub async fn manage(
    State(state): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let Json(raw) = body.map_err(|error| HttpError::bad_request(error.body_text()))?;
    let request: SandboxRequest = parse_body("sandbox request", raw)?;
    if matches!(request, SandboxRequest::Execute { .. }) {
        return Err(GhostError::new(
            ErrorKind::PermissionDenied,
            "Tool execution is not a management operation",
        )
        .into());
    }
    let request = serde_json::to_value(request)
        .map_err(|error| GhostError::new(ErrorKind::Internal, error.to_string()))?;
    Ok(Json(state.runtime.sandbox_request(request).await?))
}
