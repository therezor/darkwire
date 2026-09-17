//! The workspace manager over HTTP.
//!
//! A workspace is a named folder the user works in. Every one of them,
//! `default` included, is a folder under the workspaces directory, and none of
//! them can see another.
//!
//! **No path ever crosses this boundary.** A workspace is created by name, gets
//! a derived slug, and lives at `<workspaces>/<slug>`. Accepting a directory
//! from a client would turn "managed directories only" from a fact into a
//! convention, and the first request that sent `/` would hand an authenticated
//! caller the entire filesystem.
//!
//! **Deleting detaches; it does not remove.** The registry row goes and the
//! directory stays. A delete in a web UI is one click away from a misclick and
//! there is no undo for a recursive remove of a tree someone has been working
//! in — whereas a detached directory is re-adopted by creating a workspace with
//! the same name. Sessions are the one thing that blocks it: a workspace whose
//! conversations still name it answers 409 with the count, and the move route
//! is the way through.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use darkwire_core::WorkspaceRecord;
use darkwire_core::workspace_store::CreateWorkspace;
use darkwire_protocol::rest::{
    CreateWorkspaceRequest, MoveSessionsRequest, MoveSessionsResponse, UpdateWorkspaceRequest,
    WorkspaceListResponse, WorkspaceSummary,
};

use crate::errors::HttpError;
use crate::queries::IdParams;
use crate::routes::AppState;
use crate::runtime::ServerRuntime;
use crate::schema::parse_body;

/// A stored count or timestamp as the wire carries it.
///
/// Storage counts upwards from zero and the wire is unsigned; a negative here
/// would mean a corrupt row, and reporting zero for one is the answer that
/// cannot mislead a client into rendering a date before the epoch.
fn as_u64<T: TryInto<u64>>(value: T) -> u64 {
    value.try_into().unwrap_or(0)
}

/// Reads a JSON body into a protocol type.
///
/// Two failures, two statuses, and the difference is what the caller does next.
/// A body that is not JSON at all never reached a schema, so it is a 400; a
/// body that parsed and then failed its rules is a 422 whose `details` name the
/// fields.
fn read_json<T: serde::de::DeserializeOwned + garde::Validate<Context = ()>>(
    what: &str,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<T, HttpError> {
    let Json(raw) = body.map_err(|error| HttpError::bad_request(error.body_text()))?;
    parse_body(what, raw)
}

/// One registry row as the wire carries it, with the count a delete would have
/// to move first.
fn summarise(
    runtime: &dyn ServerRuntime,
    record: &WorkspaceRecord,
) -> Result<WorkspaceSummary, HttpError> {
    Ok(WorkspaceSummary {
        id: record.id.clone(),
        name: record.name.clone(),
        is_default: record.is_default,
        created_at_ms: as_u64(record.created_at_ms),
        updated_at_ms: as_u64(record.updated_at_ms),
        session_count: as_u64(runtime.store().count_by_workspace(&record.id)?),
    })
}

/// The row, or a 404 — never a silently empty answer standing in for one.
fn require(runtime: &dyn ServerRuntime, id: &str) -> Result<WorkspaceRecord, HttpError> {
    runtime
        .workspaces()
        .get(id)?
        .ok_or_else(|| HttpError::not_found(format!("No such workspace: {id}")))
}

/// Every workspace, the default first.
pub async fn list(State(state): State<AppState>) -> Result<Json<WorkspaceListResponse>, HttpError> {
    let runtime = state.runtime.as_ref();
    let rows = runtime.workspaces().list()?;
    let mut workspaces = Vec::with_capacity(rows.len());
    for record in &rows {
        workspaces.push(summarise(runtime, record)?);
    }
    Ok(Json(WorkspaceListResponse { workspaces }))
}

/// Creates a workspace and its folder.
pub async fn create(
    State(state): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<WorkspaceSummary>), HttpError> {
    let request: CreateWorkspaceRequest = read_json("body", body)?;
    let runtime = state.runtime.as_ref();

    // The store owns slug derivation, uniqueness, the reserved names and the
    // refusal to sit on top of an existing file. Its failures already carry the
    // kind the error mapping turns into a status, so there is nothing to
    // re-validate here.
    let created = runtime.workspaces().create(CreateWorkspace {
        name: request.name,
        id: request.id,
        metadata: None,
    })?;
    Ok((StatusCode::CREATED, Json(summarise(runtime, &created)?)))
}

/// Renames a workspace, moves its folder, or both.
pub async fn update(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<WorkspaceSummary>, HttpError> {
    let request: UpdateWorkspaceRequest = read_json("body", body)?;
    let runtime = state.runtime.as_ref();
    let mut record = require(runtime, &params.id)?;

    // The name first, and against the *old* id, so a body carrying both does
    // not have to guess which one the row is keyed on mid-request.
    if let Some(name) = request.name {
        record = runtime.workspaces().rename(&record.id, &name)?;
    }

    if let Some(folder) = request.id.filter(|folder| *folder != record.id) {
        let from = record.id.clone();
        // The store moves the directory and the row together, and refuses the
        // default, whose folder is the root every other workspace sits in.
        record = runtime.workspaces().relocate(&from, &folder)?;
        // Then everything that resolved through the old id follows it. Not in
        // the store: `sessions` is another store's table, and the jail cache is
        // not a store at all — a jail canonicalises its root once, so an entry
        // keyed on a directory that has just been renamed away holds a path
        // that is no longer there, and would be handed to the *next* workspace
        // created on that freed folder name.
        runtime.store().reassign_workspace(&from, &record.id)?;
        runtime.release_workspace(&from);
    }

    Ok(Json(summarise(runtime, &record)?))
}

/// Detaches a workspace, keeping its files.
pub async fn delete(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
) -> Result<StatusCode, HttpError> {
    let runtime = state.runtime.as_ref();
    let record = require(runtime, &params.id)?;

    // Counted before anything is removed. The count is what the UI renders in
    // its "move them to Default first" affordance, so it belongs in the error
    // rather than only in the message.
    let session_count = runtime.store().count_by_workspace(&record.id)?;
    if session_count > 0 {
        let plural = if session_count == 1 { "" } else { "s" };
        return Err(HttpError::conflict(format!(
            "{session_count} session{plural} still use this workspace"
        ))
        .with_detail("sessionCount", as_u64(session_count))
        .with_detail("workspaceId", record.id.clone()));
    }

    runtime.workspaces().delete(&record.id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Moves every session in one workspace to another.
pub async fn move_sessions(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<MoveSessionsResponse>, HttpError> {
    let request: MoveSessionsRequest = read_json("body", body)?;
    let runtime = state.runtime.as_ref();

    // Both ends must exist. Moving *into* a workspace nobody can name would
    // strand the conversations somewhere the UI cannot show them, which is
    // worse than the delete this was meant to unblock.
    let from = require(runtime, &params.id)?;
    require(runtime, &request.to)?;
    if from.id == request.to {
        return Err(
            HttpError::conflict("A workspace cannot be moved into itself")
                .with_detail("workspaceId", from.id.clone()),
        );
    }

    let moved = runtime.store().reassign_workspace(&from.id, &request.to)?;
    Ok(Json(MoveSessionsResponse {
        moved: as_u64(moved),
    }))
}
