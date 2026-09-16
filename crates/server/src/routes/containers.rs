//! The container definitions installed on this machine.
//!
//! Read from disk on every request, for the same reason the container list is:
//! an edited definition must report its new digest, its new capabilities and
//! its new gateway verdict the moment it changes.
//!
//! Read-only for now. A definition names an image, a capability set and a uid,
//! and writing one is what bounds every command an agent runs.

use axum::Json;
use axum::extract::State;
use ghostai_protocol::rest::{ContainerListResponse, ContainerSummary};
use ghostai_security::{assert_gateway_compatible, weakened_in};

use crate::errors::HttpError;
use crate::routes::AppState;

/// Container definitions installed on this machine.
pub async fn list_containers(
    State(state): State<AppState>,
) -> Result<Json<ContainerListResponse>, HttpError> {
    let containers = state
        .runtime
        .containers()
        .into_iter()
        .map(|listing| {
            let definition = listing.value;
            ContainerSummary {
                name: listing.name,
                image: definition
                    .as_ref()
                    .map(|d| d.image.clone())
                    .unwrap_or_default(),
                shared: definition.as_ref().is_some_and(|d| d.shared),
                runtime: definition.as_ref().map(|d| d.runtime).unwrap_or_default(),
                workdir: definition
                    .as_ref()
                    .map(|d| d.workdir.clone())
                    .unwrap_or_default(),
                user: definition
                    .as_ref()
                    .map(|d| d.user.clone())
                    .unwrap_or_default(),
                limits: definition
                    .as_ref()
                    .map(|d| d.limits.clone())
                    .unwrap_or_default(),
                caps_added: definition
                    .as_ref()
                    .map(|d| d.caps.add.clone())
                    .unwrap_or_default(),
                // The same function the CLI's review prints, so the two cannot
                // describe one definition differently.
                weakened: definition.as_ref().map(weakened_in).unwrap_or_default(),
                // Resolved here rather than on save, so the editor can warn
                // while an operator is still choosing the network rather than
                // after they press save.
                gateway_problem: definition
                    .as_ref()
                    .and_then(|d| assert_gateway_compatible(d).err())
                    .map(|error| error.message),
                problem: listing.problem,
            }
        })
        .collect();
    Ok(Json(ContainerListResponse { containers }))
}
