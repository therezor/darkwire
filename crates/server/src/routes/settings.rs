//! Settings, and the credentials that are deliberately not part of them.
//!
//! Two rules this module exists to hold:
//!
//!  - **`GET /api/settings` never returns a credential.** The vault is
//!    write-only over HTTP: a key goes in through `PUT
//!    /api/settings/credentials` and never comes back out, and the panel gets a
//!    per-provider boolean instead. Nothing relies on response serialisation to
//!    enforce that — it is enforced by the response simply not containing one.
//!    (What the settings tree *does* carry is `providers.<id>.extraHeaders` and
//!    an MCP server's `headers`: those are operator-typed config that lives in
//!    `config.yaml` in the clear, and the panel showing them is the panel they
//!    were typed into.)
//!
//!  - **A patch that could not be served is refused at save time.** Saving
//!    `auth.enabled: false` against a LAN bind would leave an operator with a
//!    config file whose next boot is a refusal — the failure would surface on
//!    restart, long after the change that caused it.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use ghostai_protocol::config::{
    AgentEntry, AgentEntryPatch, AgentsConfigPatch, Config, ConfigPatch,
};
use ghostai_protocol::ids::{DEFAULT_AGENT_ID, RESERVED_AGENT_IDS, is_agent_id};
use ghostai_protocol::rest::{
    AgentRename, SetCredentialRequest, SettingsPatchRequest, SettingsResponse,
};
use indexmap::IndexMap;

use crate::boot::assert_boot_policy;
use crate::errors::HttpError;
use crate::routes::AppState;
use crate::schema::parse_body;

/// The settings tree, with credentials replaced by presence flags.
pub async fn get(State(state): State<AppState>) -> Result<Json<SettingsResponse>, HttpError> {
    Ok(Json(settings_response(&state)))
}

/// Apply a settings patch and rebuild what depends on it.
pub async fn patch(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<SettingsResponse>, HttpError> {
    let raw = serde_json::from_slice(&body)
        .map_err(|error| HttpError::bad_request(format!("Invalid JSON body: {error}")))?;
    // The deep-partial patch, plus `renameAgents`: a shallow partial would
    // leave each field's default in place, so saving one panel would rewrite
    // every untouched field in the tree back to its default.
    let request: SettingsPatchRequest = parse_body("body", raw)?;
    let (mut patch, renames) = request.into_parts();

    let current = state.runtime.config();
    assert_servable(&current, &patch)?;

    // The renames and the patch go in as **one** merge, which is the whole
    // reason they travel together: as two writes, the first can land and the
    // second fail, leaving an agent under its new name holding its old
    // settings. Renames first so the caller's patch addresses the new ids — and
    // last-writer-wins on `agents.list` is what the caller wants there, because
    // their entry is the edited one and the rename's is the copy.
    if !renames.is_empty() {
        let moves = rename_patch(&current, &renames)?;
        let mut list = moves;
        if let Some(theirs) = patch.agents.and_then(|agents| agents.list) {
            list.extend(theirs);
        }
        patch.agents = Some(AgentsConfigPatch { list: Some(list) });
    }
    state.runtime.apply_settings(patch)?;

    // The stores the settings tree does not reach, each all-or-nothing and in
    // this order for a reason.
    //
    // *After* the config, because a save refused above — an unbuildable agent,
    // an unservable server block — must not have already moved conversations
    // onto an id the config never took. Config first means a failure here
    // leaves them pointing at an id that no longer resolves, which is the case
    // the default-agent fallback exists for and which re-running the save
    // repairs. The other order would move the conversations and then fail to
    // record why.
    //
    // One transaction rather than one per rename, so a save carrying two cannot
    // land half of them.
    let moves: Vec<(&str, &str)> = renames
        .iter()
        .map(|rename| (rename.from.as_str(), rename.to.as_str()))
        .collect();
    state.runtime.store().reassign_agents(&moves)?;
    // In memory and last, because it cannot fail in a way worth ordering
    // around. The same agent, so its standing tool approvals follow it; a
    // delete-and-recreate is a *different* agent and deliberately does not —
    // see `forget_departed_agents`.
    for rename in &renames {
        if rename.from != rename.to {
            state.hub.rename_agent(&rename.from, &rename.to);
        }
    }

    forget_departed_agents(&state);
    // On both writers, for the reason `forget_departed_agents` is on both: a
    // patch and a reload can each move `scheduler.*`. The engine reads
    // `enabled`, `concurrency` and `runRetention` live, but its *timer* is armed
    // from what was due when it last looked — so without this, switching the
    // scheduler on does nothing until a restart.
    if let Some(scheduler) = &state.scheduler {
        scheduler.refresh();
    }
    Ok(Json(settings_response(&state)))
}

/// Re-read `config.yaml` from disk and rebuild what depends on it.
///
/// The settings tree on the way out, because that is the question the caller is
/// really asking: not "did it work" but "what is it running now". A body of
/// `{"ok": true}` would send every caller straight back for the answer, and
/// would be a second shape to keep in step with the one the read already
/// publishes.
pub async fn reload(State(state): State<AppState>) -> Result<Json<SettingsResponse>, HttpError> {
    state.runtime.reload()?;
    forget_departed_agents(&state);
    if let Some(scheduler) = &state.scheduler {
        scheduler.refresh();
    }
    Ok(Json(settings_response(&state)))
}

/// Store or clear one credential.
///
/// No body on the way out. A route that echoed what it stored would be a read
/// path for a store that has none.
pub async fn credential(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<StatusCode, HttpError> {
    let raw = serde_json::from_slice(&body)
        .map_err(|error| HttpError::bad_request(format!("Invalid JSON body: {error}")))?;
    let request: SetCredentialRequest = parse_body("body", raw)?;
    state.runtime.set_credential(&request)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The whole answer both the read and the two writers give.
fn settings_response(state: &AppState) -> SettingsResponse {
    SettingsResponse {
        config: state.runtime.config(),
        credentials_present: state.runtime.credentials_present(),
        // Empty for a runtime that has no channels to report — a route test
        // standing in for one, and any build that ships none.
        channels: state.runtime.channels(),
        load_error: state.runtime.load_error(),
        // Read fresh on every response rather than only after a write: a
        // warning most often comes from the file as it was found at boot, and
        // the first request for the settings tree is where anyone would look
        // for it.
        warnings: state.runtime.config_warnings(),
    }
}

/// Whether the settings this patch produces could be served on the next boot.
///
/// Only the `server` subtree is merged, and only shallowly, because that is all
/// the boot policy reads. Reaching for the general deep merge would mean either
/// depending on the composition root or reimplementing it — and a second merge
/// implementation that disagrees with the real one in some corner is worse than
/// a narrow one that cannot.
fn assert_servable(current: &Config, patch: &ConfigPatch) -> Result<(), HttpError> {
    let mut merged = current.clone();
    if let Some(server) = &patch.server {
        if let Some(host) = &server.host {
            merged.server.host.clone_from(host);
        }
        if let Some(port) = server.port {
            merged.server.port = port;
        }
        if let Some(auth) = &server.auth
            && let Some(enabled) = auth.enabled
        {
            merged.server.auth.enabled = enabled;
        }
    }
    // A `config` failure is a 500 through the kind table, which is the right
    // answer for a config file the operator wrote and the wrong one for a body
    // this request just sent.
    assert_boot_policy(&merged).map_err(|error| HttpError::bad_request(error.message))
}

/// The key moves a rename asks for, as the patch that performs them.
///
/// Three edits at once, and they have to be one patch rather than three: each
/// intermediate state has either a dangling delegation or two agents holding
/// the same entry, and the merge validates — and prunes — every state it is
/// given.
///
/// Refused here rather than left to the merge, because the merge would happily
/// write any of them and the operator would find out from the next turn:
///
///  - the source has to exist, and must not be the default agent, which is
///    resolvable whether or not it has an entry and which nothing downstream
///    can do without
///  - the target has to be a usable id, not reserved, and not already taken
///
/// `agents.list.*` is replaced wholesale rather than merged, so every entry
/// built here is a complete agent expressed as a patch and not a diff of one.
fn rename_patch(
    current: &Config,
    renames: &[AgentRename],
) -> Result<IndexMap<String, Option<AgentEntryPatch>>, HttpError> {
    let mut list: IndexMap<String, Option<AgentEntryPatch>> = IndexMap::new();
    // Applied against a copy so a second rename in the same request sees the
    // first one's result — renaming a to b and b to c in one save is odd but
    // expressible, and silently letting the second land on the *old* b would be
    // worse.
    let mut pending: IndexMap<String, AgentEntry> = current.agents.list.clone();

    for AgentRename { from, to } in renames {
        if from == DEFAULT_AGENT_ID {
            return Err(
                HttpError::unprocessable("The default agent cannot be renamed.")
                    .with_detail("/renameAgents/from", from.clone()),
            );
        }
        let Some(entry) = pending.get(from).cloned() else {
            return Err(HttpError::not_found(format!("No such agent: {from}")));
        };
        if to == from {
            continue;
        }
        if !is_agent_id(to) || RESERVED_AGENT_IDS.contains(&to.as_str()) {
            return Err(HttpError::unprocessable(format!(
                "\"{to}\" cannot be used as an agent id.\n  \
                 Ids are lower-case letters, digits and hyphens, up to 40 characters,\n  \
                 and cannot be a reserved device name."
            ))
            .with_detail("/renameAgents/to", to.clone()));
        }
        if pending.contains_key(to) {
            return Err(HttpError::conflict(format!(
                "There is already an agent called \"{to}\"."
            )));
        }

        pending.shift_remove(from);
        pending.insert(to.clone(), entry.clone());
        list.insert(from.clone(), None);
        list.insert(to.clone(), Some(entry.into()));

        // Every *other* agent that delegates to the old id follows it. Read off
        // `pending` so a delegation rewritten by an earlier rename in this same
        // request is rewritten again rather than reverted.
        let followers: Vec<String> = pending
            .iter()
            .filter(|(id, other)| {
                id.as_str() != to
                    && other
                        .subagents
                        .iter()
                        .any(|reference| &reference.id == from)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in followers {
            let Some(other) = pending.get_mut(&id) else {
                continue;
            };
            for reference in &mut other.subagents {
                if &reference.id == from {
                    reference.id.clone_from(to);
                }
            }
            list.insert(id, Some(other.clone().into()));
        }
    }

    Ok(list)
}

/// Drops standing tool approvals for agents this write removed.
///
/// On both writers, because both can remove an agent: a patch does it directly,
/// and a reload does it by re-reading a file someone edited by hand.
///
/// The thing it prevents is not a stale cache but a privilege carried across an
/// identity boundary. An agent id is user-authored and re-creatable, so deleting
/// `reviewer` and creating a new `reviewer` produces two different agents that
/// share a key — and without this the second inherits every standing "always
/// allow" the first was ever granted.
fn forget_departed_agents(state: &AppState) {
    let live: Vec<String> = state
        .runtime
        .agents()
        .into_iter()
        .map(|agent| agent.id)
        .collect();
    state.hub.retain_agents(&live);
}
