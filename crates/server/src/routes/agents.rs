//! The agents a turn can be run by.
//!
//! Read-only, and that is the whole design. Creating, editing, deleting and
//! renaming an agent is a settings edit — `PATCH /api/settings` — because an
//! agent *is* a subtree of the settings tree, and a second CRUD surface over
//! the same state would need its own merge rules, its own validation and its
//! own answer to what a partial write means. `providers` made the same call for
//! the same reason.
//!
//! A rename is the interesting case, because it is the one operation a config
//! patch cannot fully describe on its own: `{"reviewer": null, "code-review":
//! {…}}` lands the right tree, but says equally well "rename reviewer" and
//! "delete reviewer, create code-review", which are opposites for the
//! conversations bound to the old id and for its standing tool approvals. That
//! missing word is carried by `renameAgents` on the settings body rather than
//! by a route here. Keeping it in the same request is what lets one Save move
//! an agent's id *and* its model without a window in which the first has landed
//! and the second has not.
//!
//! What this route adds that `GET /api/settings` cannot: the model each agent
//! would actually use, after any process-wide model pin. A picker rendering the
//! raw config would show what the file says rather than what a turn would send,
//! and those differ whenever a pin is in play.

use axum::Json;
use axum::extract::State;
use ghostai_protocol::rest::{AgentListResponse, AgentSummary};

use crate::errors::HttpError;
use crate::routes::AppState;

/// Every agent that can run a turn.
///
/// Already ordered — the default first, then the operator's own order — so a
/// picker never has to sort and never renders a different order twice.
pub async fn list(State(state): State<AppState>) -> Result<Json<AgentListResponse>, HttpError> {
    let agents = state
        .runtime
        .agents()
        .into_iter()
        .map(|summary| AgentSummary {
            id: summary.id,
            label: summary.label,
            model: summary.model,
            provider: summary.provider,
            reasoning_effort: summary.reasoning_effort,
        })
        .collect();
    Ok(Json(AgentListResponse { agents }))
}
