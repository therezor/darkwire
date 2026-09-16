//! What each installed extension actually is, and the two buttons on its row.
//!
//! The listing is `GET /api/mcp`'s sibling and reads the same way: the settings
//! tree says which extensions an operator disabled, and this says what happened
//! when the install tried to load them. "Never approved", "changed since
//! approval" and "failed on activation" are three different things to do next,
//! and none of them belongs in `config.yaml`.
//!
//! Approve and revoke are **not** settings patches, and that is the design
//! rather than an omission. An approval is a statement about the exact bytes on
//! disk at one moment — recorded as a digest in the database, next to the
//! container approvals it copies. Writing it into `config.yaml` would make it
//! survive an edit to the very files it was about, which is the one thing the
//! whole gate exists to prevent.
//!
//! `POST` for both, and neither is idempotent in the way a `PUT` promises:
//! approving records *what is on disk now*, so approving twice across an edit
//! approves two different things. That is the intended behaviour and the verb
//! should say so.

use axum::Json;
use axum::extract::{Path, State};
use ghostai_protocol::rest::ExtensionListResponse;

use crate::errors::HttpError;
use crate::queries::IdParams;
use crate::routes::AppState;

/// Every installed extension and the state it is in.
///
/// A build with no extension host answers `[]` rather than refusing: "which
/// extensions do you have" has a true answer here, and it is "none".
pub async fn list(State(state): State<AppState>) -> Result<Json<ExtensionListResponse>, HttpError> {
    Ok(Json(ExtensionListResponse {
        extensions: state.runtime.extension_statuses(),
    }))
}

/// Approve the files an extension currently holds, and load it.
///
/// Answers with the whole list rather than the one row: approving loads the
/// extension, which can move another row — an id it shadows, a tool name it
/// takes. One request, one truthful picture.
pub async fn approve(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
) -> Result<Json<ExtensionListResponse>, HttpError> {
    let pending = state
        .runtime
        .approve_extension(&params.id)
        .ok_or_else(no_host)?;
    pending.await?;
    Ok(Json(ExtensionListResponse {
        extensions: state.runtime.extension_statuses(),
    }))
}

/// Forget an extension's approval and unload it.
pub async fn revoke(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
) -> Result<Json<ExtensionListResponse>, HttpError> {
    let pending = state
        .runtime
        .revoke_extension(&params.id)
        .ok_or_else(no_host)?;
    pending.await?;
    Ok(Json(ExtensionListResponse {
        extensions: state.runtime.extension_statuses(),
    }))
}

/// The one refusal both writes share.
///
/// A build with no extension host has nothing to approve anything with. The
/// listing takes the opposite line and answers `[]`, because "which extensions
/// do you have" has a true answer there and this does not.
fn no_host() -> HttpError {
    HttpError::not_found("This installation was built without the extension host.")
}
