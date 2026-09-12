//! Where each configured MCP server actually is.
//!
//! The settings tree says what should be connected; this says what is. They are
//! two different questions and only one of them belongs in `config.json` — an
//! operator's entry survives a laptop closing, and "unreachable since 12:04"
//! must not.
//!
//! It is also the only carrier for two things nothing else can express: the
//! *reason* a server is down, phrased for a person, and the URL an operator has
//! to visit when a server wants authorizing.
//!
//! Read-only, like `GET /api/toolboxes` next door. A server is created, edited
//! and deleted through `PATCH /api/settings`, because it is configuration; a
//! route here that could add one would be a second way to write the same file.

use axum::Json;
use axum::extract::State;
use ghostai_protocol::rest::McpStatusResponse;

use crate::errors::HttpError;
use crate::routes::AppState;

/// Every configured MCP server and its connection state.
///
/// An install whose build has no MCP client answers with an empty list rather
/// than a 501: it has no MCP servers, which is what the panel is asking.
/// Already sorted by id.
pub async fn list(State(state): State<AppState>) -> Result<Json<McpStatusResponse>, HttpError> {
    Ok(Json(McpStatusResponse {
        servers: state.runtime.mcp_servers(),
    }))
}
