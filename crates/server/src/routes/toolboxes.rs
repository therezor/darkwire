//! The toolboxes installed on this machine.
//!
//! Read from disk on every request rather than from a cached list, and that is
//! the point rather than laziness: a manifest edited after approval must stop
//! reporting as approved the moment it changes, and a list built at boot would
//! keep saying it was fine until a restart.
//!
//! Read-only. Installing and approving are operator actions with a terminal
//! behind them, and exposing approval over HTTP would put the one decision that
//! makes a container policy mean something behind whatever session happens to
//! be open in a browser tab.

use axum::Json;
use axum::extract::State;
use ghostai_protocol::rest::{ToolboxListResponse, ToolboxSummary, ToolboxToolSummary};
use ghostai_protocol::toolbox::{ToolboxExposure, ToolboxNetworkMode};
use ghostai_security::toolbox::weakened_in;

use crate::errors::HttpError;
use crate::routes::AppState;

/// Toolboxes installed on this machine.
pub async fn list(State(state): State<AppState>) -> Result<Json<ToolboxListResponse>, HttpError> {
    let toolboxes = state
        .runtime
        .toolboxes()
        .into_iter()
        .map(|entry| {
            let manifest = entry.toolbox;
            ToolboxSummary {
                name: entry.name,
                label: manifest
                    .as_ref()
                    .map(|toolbox| toolbox.label.clone())
                    .unwrap_or_default(),
                version: manifest
                    .as_ref()
                    .map(|toolbox| toolbox.version.clone())
                    .unwrap_or_default(),
                image: manifest
                    .as_ref()
                    .map(|toolbox| toolbox.image.clone())
                    .unwrap_or_default(),
                tools: manifest
                    .as_ref()
                    .map(|toolbox| {
                        toolbox
                            .tools
                            .iter()
                            .map(|tool| ToolboxToolSummary {
                                name: tool.name.clone(),
                                r#use: tool.r#use.clone(),
                                permission: tool.permission,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                // Whether those names are callables the agent editor can
                // permission one by one, or a prompt section reached through
                // `exec`.
                exposes_tools: manifest
                    .as_ref()
                    .is_some_and(|toolbox| toolbox.expose == ToolboxExposure::Tools),
                max_network: manifest
                    .as_ref()
                    .map_or(ToolboxNetworkMode::None, |toolbox| toolbox.network.max_mode),
                caps_added: manifest
                    .as_ref()
                    .map(|toolbox| toolbox.caps.add.clone())
                    .unwrap_or_default(),
                // The same list the terminal's review prints, so a browser and
                // a terminal cannot disagree about what a toolbox is asking
                // for.
                weakened: manifest.as_ref().map(weakened_in).unwrap_or_default(),
                approved: entry.approved,
                problem: entry.problem,
            }
        })
        .collect();

    Ok(Json(ToolboxListResponse { toolboxes }))
}
