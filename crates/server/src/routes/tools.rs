//! Every tool that exists, for the screen that decides who may call them.
//!
//! Read from the live registry rather than derived from the settings tree, and
//! the difference is the point: `scheduler.enabled: false` removes a tool, an
//! MCP server connecting adds several, and an extension can add its own.
//!
//! The registry, and deliberately not one agent's subset. The only caller is
//! the agent editor, which draws one permission row per entry, so answering
//! with the *default* agent's list breaks both ways: a tool that agent does not
//! hold would have no row on any agent and could never be granted to one
//! (`automation` is absent from its tools on purpose), while its subagent
//! delegation tools would appear as grantable rows on agents that have none.
//!
//! What one agent is actually offered is [`AgentView::tools`], which the
//! context inspector reads per session and this route has no business
//! restating.
//!
//! [`AgentView::tools`]: crate::runtime::AgentView::tools

use axum::Json;
use axum::extract::State;
use darkwire_protocol::rest::ToolListResponse;

use crate::errors::HttpError;
use crate::routes::AppState;

/// Every tool the registry holds, whoever may call it.
///
/// Already sorted by name: the registry keeps that order so a reconnecting MCP
/// server cannot rewrite the cached prompt prefix.
pub async fn list(State(state): State<AppState>) -> Result<Json<ToolListResponse>, HttpError> {
    Ok(Json(ToolListResponse {
        tools: state.runtime.registered_tools(),
    }))
}
