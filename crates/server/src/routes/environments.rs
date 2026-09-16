//! The environment definitions installed on this machine.
//!
//! Read from disk on every request, for the same reason the container list is:
//! an edited definition must report its new digest, its new capabilities and
//! its new gateway verdict the moment it changes.
//!
//! **Writable, and the invariant that makes that safe is where the files sit
//! rather than who may ask.** The policy root is beside the workspace, never
//! inside it, so `write_file` and anything reached by prompt injection cannot
//! touch a definition. An authenticated operator pressing Save in Settings is a
//! different actor, and the same one who already edits `agents.list` through
//! `PATCH /api/settings`. What does not change is the validation: every save
//! clears exactly the checks a hand-written file clears, so the API cannot
//! install a tag-pinned image or a `NET_ADMIN` capability either.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use ghostai_core::{ErrorKind, GhostError};
use ghostai_protocol::environment::EnvironmentDefinition;
use ghostai_protocol::rest::{EnvironmentListResponse, EnvironmentSummary};
use ghostai_security::{assert_gateway_compatible, weakened_in};

use crate::errors::HttpError;
use crate::routes::AppState;
use crate::schema::parse_body;

/// Environment definitions installed on this machine.
pub async fn list_environments(
    State(state): State<AppState>,
) -> Result<Json<EnvironmentListResponse>, HttpError> {
    Ok(Json(EnvironmentListResponse {
        environments: state
            .runtime
            .environments()
            .into_iter()
            .map(summary)
            .collect(),
    }))
}

/// The definition plus the three facts that are not in the file.
fn summary(listing: ghostai_security::policy_store::EnvironmentListing) -> EnvironmentSummary {
    EnvironmentSummary {
        name: listing.name,
        // The same functions the CLI's review prints, so the two cannot
        // describe one definition differently.
        weakened: listing.value.as_ref().map(weakened_in).unwrap_or_default(),
        // Resolved here rather than on save, so the editor can warn while an
        // operator is still choosing the network rather than after they press
        // save.
        gateway_problem: listing
            .value
            .as_ref()
            .and_then(|definition| assert_gateway_compatible(definition).err())
            .map(|error| error.message),
        definition: listing.value,
        problem: listing.problem,
    }
}

/// Installs or replaces one definition.
///
/// An upsert rather than a create and an update, because the name is the
/// filename and there is nothing a second verb would decide. The browser stops
/// a create from landing on a name already in the list; a race between two
/// operators is not what this route is defending against.
pub async fn save_environment(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<EnvironmentListResponse>, HttpError> {
    let Json(raw) = body.map_err(|error| HttpError::bad_request(error.body_text()))?;
    let definition: EnvironmentDefinition = parse_body("environment definition", raw)?;
    if definition.name != name {
        return Err(GhostError::new(
            ErrorKind::InvalidInput,
            format!(
                "This definition names itself \"{}\", but it is being saved as \"{name}\".\n  A definition's name is its filename.",
                definition.name
            ),
        )
        .into());
    }
    assert_no_agent_is_broken_by(&state, &name, Some(&definition))?;
    state
        .runtime
        .save_environment(&definition)
        .map_err(rejected_body)?;
    list_environments(State(state)).await
}

/// A policy refusal is about the body, not about this server.
///
/// The store raises `Config` for both, because from its side "these bytes are
/// not a valid policy" is one condition however they arrived. Over HTTP the two
/// are different: a definition on disk that will not parse is the server's
/// problem and a 500, while a definition somebody just submitted is theirs and a
/// 422. Without this an operator pasting a tag-pinned image is told the server
/// broke, and the sentence explaining what they did wrong arrives under it.
fn rejected_body(error: GhostError) -> GhostError {
    if error.kind == ErrorKind::Config {
        return GhostError::new(ErrorKind::InvalidInput, error.message);
    }
    error
}

/// Uninstalls one definition.
pub async fn remove_environment(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<EnvironmentListResponse>, HttpError> {
    assert_no_agent_is_broken_by(&state, &name, None)?;
    // A name that is not installed is a 404 rather than the store's `Config`,
    // which would report the server as broken for a stale link.
    state.runtime.remove_environment(&name).map_err(|error| {
        if error.kind == ErrorKind::Config {
            return GhostError::new(ErrorKind::NotFound, error.message);
        }
        error
    })?;
    list_environments(State(state)).await
}

/// Refuses a write that the next start would reject.
///
/// `Runtime::resolve_policies` resolves every enabled agent's environment on
/// every build, and propagates. On a reconfigure that is a rollback; on a cold
/// start it is a server that will not open a socket. So deleting an environment
/// an agent names, or saving one that stops being able to host an allow-list an
/// agent already asked for, would let the UI write a config nothing can boot.
///
/// Checked here rather than left to the build for one reason: the operator is
/// on the screen, and the agents are nameable. A rollback three settings later
/// is a mystery; "the reviewer uses this" is an instruction.
///
/// `next` is the definition about to be written, or `None` for a delete.
fn assert_no_agent_is_broken_by(
    state: &AppState,
    name: &str,
    next: Option<&EnvironmentDefinition>,
) -> Result<(), HttpError> {
    let config = state.runtime.config();
    let blocked: Vec<String> = config
        .agents
        .list
        .iter()
        .filter(|(_, agent)| agent.enabled && agent.environment.name == name)
        .filter(|(_, agent)| match next {
            // A delete breaks every agent that names it.
            None => true,
            // A save breaks only the ones that asked for an allow-list this
            // definition can no longer host. Everything else about a definition
            // is the operator's to weaken, and is surfaced rather than refused.
            Some(definition) => {
                agent.environment.network.mode == ghostai_protocol::NetworkMode::Allowlist
                    && assert_gateway_compatible(definition).is_err()
            }
        })
        .map(|(id, agent)| {
            if agent.label.is_empty() {
                id.clone()
            } else {
                format!("{} ({id})", agent.label)
            }
        })
        .collect();
    if blocked.is_empty() {
        return Ok(());
    }

    let what = if next.is_none() {
        format!("Cannot remove \"{name}\"")
    } else {
        format!("Cannot save \"{name}\" with these settings")
    };
    let why = if next.is_none() {
        "still uses it"
    } else {
        "asks it for an allow-list it could no longer enforce"
    };
    Err(GhostError::new(
        ErrorKind::InvalidInput,
        format!(
            "{what}: {} {why}.\n  Change those agents first, or switch them off.",
            blocked.join(", ")
        ),
    )
    .with_detail("agents", blocked.join(", "))
    .into())
}
