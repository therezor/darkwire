//! The toolboxes installed on this machine.
//!
//! Read from disk on every request rather than from a cached list, and that is
//! the point rather than laziness: an edited manifest must report its new
//! digest and its new operations the moment it changes, and a list built at
//! boot would keep saying it was fine until a restart.
//!
//! Read-only. Installing a toolbox is an operator action with a terminal behind
//! it, and the curated operation surface is what bounds what an agent can ask
//! for, so it is not something a browser session composes.

use axum::Json;
use axum::extract::State;
use ghostai_protocol::rest::{ToolboxListResponse, ToolboxSummary, ToolboxToolSummary};

use crate::errors::HttpError;
use crate::routes::AppState;

/// Toolboxes installed on this machine.
pub async fn list_toolboxes(
    State(state): State<AppState>,
) -> Result<Json<ToolboxListResponse>, HttpError> {
    let toolboxes = state
        .runtime
        .toolboxes()
        .into_iter()
        .map(|listing| {
            let resolved = listing.value;
            ToolboxSummary {
                name: listing.name,
                label: resolved
                    .as_ref()
                    .map(|r| r.toolbox.label.clone())
                    .unwrap_or_default(),
                version: resolved
                    .as_ref()
                    .map(|r| r.toolbox.version.clone())
                    .unwrap_or_default(),
                notes: resolved
                    .as_ref()
                    .map(|r| r.toolbox.notes.clone())
                    .unwrap_or_default(),
                tools: resolved
                    .as_ref()
                    .map(|r| {
                        r.toolbox
                            .tools
                            .iter()
                            .map(|grant| ToolboxToolSummary {
                                name: grant.name.clone(),
                                description: r
                                    .operations
                                    .get(&grant.name)
                                    .map(|operation| operation.description.clone())
                                    .unwrap_or_default(),
                                permission: grant.permission,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                problem: listing.problem,
            }
        })
        .collect();
    Ok(Json(ToolboxListResponse { toolboxes }))
}
