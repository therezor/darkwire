//! Liveness, what is running, and the generated document.
//!
//! `GET /api/status` answers "what would a turn do right now" rather than "what
//! does the config file say": the model and provider come from the agent that
//! would run it, so a settings save that failed to take effect is visible here
//! instead of being invisible until the next answer comes back from the wrong
//! model.

use axum::Json;
use axum::extract::State;
use ghostai_core::Database;
use ghostai_core::ids::DEFAULT_WORKSPACE_ID;
use ghostai_protocol::rest::{
    HealthCheck, HealthCheckStatus, HealthResponse, HealthStatus, StatusResponse,
};
use ghostai_protocol::ws::PROTOCOL_VERSION;
use serde_json::Value;

use crate::errors::HttpError;
use crate::routes::AppState;
use crate::version::SERVER_VERSION;

/// `SELECT 1` — the cheapest statement that proves the file is still readable.
fn database_healthy(database: &Database) -> bool {
    database
        .lock()
        .query_row("SELECT 1", [], |_| Ok(()))
        .is_ok()
}

/// Liveness, and the checks behind it.
pub async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, HttpError> {
    let storage = database_healthy(&state.database);
    Ok(Json(HealthResponse {
        // `Fail` rather than `Degraded`: with no database there is no session
        // to write a turn into, so nothing the server does still works.
        status: if storage {
            HealthStatus::Ok
        } else {
            HealthStatus::Fail
        },
        checks: vec![HealthCheck {
            name: "database".to_owned(),
            status: if storage {
                HealthCheckStatus::Ok
            } else {
                HealthCheckStatus::Fail
            },
            detail: if storage {
                String::new()
            } else {
                "the session database is not readable".to_owned()
            },
        }],
    }))
}

/// Version, uptime, and what a turn would use right now.
pub async fn status(State(state): State<AppState>) -> Result<Json<StatusResponse>, HttpError> {
    let agent = state.runtime.agent(None)?;
    let extensions = state.runtime.extensions();
    Ok(Json(StatusResponse {
        version: SERVER_VERSION.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        // Monotonic: an NTP correction must not make a process look like it
        // started in the future.
        uptime_ms: u64::try_from(
            state
                .clock
                .monotonic()
                .saturating_sub(state.started_at)
                .as_millis(),
        )
        .unwrap_or(u64::MAX),
        model: agent.model().to_owned(),
        provider: agent.provider().to_owned(),
        configured: agent.configured(),
        // The id, never the path. An absolute host path handed to every
        // authenticated client names the operator's account and directory
        // layout, which is the one string that turns a blind traversal attempt
        // into a targeted one. The banner on the terminal still prints it; a
        // terminal on the host is not a network boundary.
        workspace_id: DEFAULT_WORKSPACE_ID.to_owned(),
        workspace_count: u64::try_from(state.runtime.workspaces().list()?.len())
            .unwrap_or(u64::MAX),
        // From the boot config, not the live one: this reports whether the
        // running listener authenticates, and that is not something a settings
        // save can change under an already-authenticated session.
        auth_enabled: state.config.server.auth.enabled,
        tool_count: u64::try_from(agent.tools().len()).unwrap_or(u64::MAX),
        mcp_servers_connected: u64::from(extensions.mcp_servers_connected),
        extensions_loaded: u64::from(extensions.extensions_loaded),
    }))
}

/// The generated OpenAPI 3.1 document.
pub async fn openapi() -> Result<Json<Value>, HttpError> {
    Ok(Json(crate::openapi::openapi_document()))
}
