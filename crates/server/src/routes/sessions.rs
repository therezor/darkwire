//! Sessions, their messages, and what the model would actually be sent.
//!
//! Cursor-paged, both listings, for the reason the cursor module states: these
//! are the two tables that move under a reader. A page is fetched one row
//! longer than asked for and trimmed, so `nextCursor` is present exactly when
//! there is another row — a client never follows a cursor into an empty page,
//! and never stops one row early.
//!
//! The session listing *also* answers an offset, because it has a second kind
//! of reader: the sessions management screen is a numbered pager that searches
//! and jumps rather than walking the list, and page 7 is a position it has no
//! cursor for. The two modes are alternatives — a request naming both is
//! refused — and a cursor is issued only while the listing is in the ordering a
//! cursor addresses. `total` is returned either way, because a pager cannot
//! derive "of 12" from the rows in front of it.
//!
//! `GET /api/sessions/:key/context` is the panel that makes the token budget
//! legible instead of a mystery. Two properties make it worth having rather
//! than misleading: the system prompt comes from the loop that would send it,
//! and the message window is produced by the same windowing code the loop
//! calls rather than by a second implementation of the rules.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use darkwire_core::session_store::{
    CreateSession, ForkSession, ListSessions, ReadMessages, SessionCursor, SessionOrderBy,
    SessionRecord, TurnStatsRecord, UpdateSession,
};
use darkwire_core::to_stored_message;
use darkwire_protocol::json::Object;
use darkwire_protocol::messages::Usage;
use darkwire_protocol::rest::{
    BranchSessionRequest, ContextResponse, CreateSessionRequest, SessionListResponse,
    SessionMessagesResponse, SessionSummary, TasksResponse, TurnStats, TurnStatsResponse,
    UpdateSessionRequest,
};
use darkwire_protocol::subagent::subagent_runs_of;
use darkwire_protocol::uuid::new_uuid;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::context::build_context_response;
use crate::cursor::{
    MessageCursor, SessionListCursor, assert_one_paging_mode, decode_message_cursor,
    decode_session_cursor, encode_message_cursor, encode_session_cursor, paginate,
};
use crate::errors::HttpError;
use crate::queries::{PageQuery, SessionListQuery, SessionParams, SessionSort, TurnsQuery};
use crate::routes::AppState;
use crate::runtime::ServerRuntime;
use crate::schema::{parse_body, validated};

/// A stored count or timestamp as the wire carries it.
///
/// Storage counts upwards from zero in an `i64` and the wire is unsigned; a
/// negative here would mean a corrupt row, and reporting zero for one is the
/// answer that cannot mislead a client into rendering a date before the epoch.
pub(crate) fn as_u64<T: TryInto<u64>>(value: T) -> u64 {
    value.try_into().unwrap_or(0)
}

/// Reads a JSON body into a protocol type.
///
/// Two failures, two statuses, and the difference is what the caller does next.
/// A body that is not JSON at all never reached a schema, so it is a 400; a
/// body that parsed and then failed its rules is a 422 whose `details` name the
/// fields.
pub(crate) fn read_json<T: DeserializeOwned + garde::Validate<Context = ()>>(
    what: &str,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<T, HttpError> {
    let Json(raw) = body.map_err(|error| HttpError::bad_request(error.body_text()))?;
    parse_body(what, raw)
}

/// Reads a query string into one of the query shapes.
///
/// A value that will not parse into its field's type and a value that parses
/// and then breaks a bound are the same mistake to whoever sent it — a
/// parameter they have to fix — so both answer 422 rather than one of them
/// arriving as a bare 400 with no field named.
fn read_query<T: garde::Validate<Context = ()>>(
    query: Result<Query<T>, QueryRejection>,
) -> Result<T, HttpError> {
    let Query(value) = query.map_err(|error| HttpError::unprocessable(error.body_text()))?;
    validated("query", value)
}

/// `total_usage` is omitted rather than zeroed when there is none.
///
/// A conversation whose turns predate the turn-stats table has no total, and
/// reporting `0` would claim it cost nothing rather than that nobody counted.
fn to_summary(
    record: &SessionRecord,
    message_count: usize,
    total_usage: Option<Usage>,
) -> SessionSummary {
    SessionSummary {
        key: record.key.clone(),
        title: record.title.clone(),
        message_count: as_u64(message_count),
        created_at_ms: as_u64(record.created_at_ms),
        updated_at_ms: as_u64(record.updated_at_ms),
        origin: record.origin.clone(),
        workspace_id: record.workspace_id.clone(),
        agent_id: record.agent_id.clone(),
        total_usage,
    }
}

/// Storage's untyped bag as the protocol's, which preserves insertion order.
fn as_object(metadata: &serde_json::Map<String, Value>) -> Object {
    metadata
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// One recorded turn as the wire carries it.
fn to_turn_stats(record: TurnStatsRecord) -> TurnStats {
    TurnStats {
        turn_id: record.turn_id,
        session_key: record.session_key,
        agent_id: record.agent_id,
        workspace_id: record.workspace_id,
        provider: record.provider,
        model: record.model,
        started_at_ms: as_u64(record.started_at_ms),
        ended_at_ms: as_u64(record.ended_at_ms),
        iterations: as_u64(record.iterations),
        stop_reason: record.stop_reason,
        usage: record.usage,
        generation_ms: record.generation_ms.map(as_u64),
        generation_tokens: record.generation_tokens.map(as_u64),
        first_token_ms: record.first_token_ms.map(as_u64),
        error: record.error,
    }
}

/// The session, or a 404 — never an empty listing standing in for one.
fn require_session(runtime: &dyn ServerRuntime, key: &str) -> Result<SessionRecord, HttpError> {
    runtime
        .store()
        .get_session(key)?
        .ok_or_else(|| HttpError::not_found(format!("No session \"{key}\"")))
}

/// Refuses a binding to an agent that cannot run, the way the workspace guard
/// does.
///
/// A conversation bound to an agent nobody can resolve is one that can never
/// take a turn under the agent it names. This is where almost every dangling
/// binding came from: the adjacent workspace id was checked and this was not.
///
/// Deliberately about the *incoming* id and never the stored one — moving a
/// session off an agent that has since been deleted has to keep working, and
/// that is the recovery this route exists to provide.
///
/// A disabled agent is refused too: it is absent from every listing an operator
/// could have picked from, so accepting it here would bind a conversation to
/// something the UI cannot show them.
fn require_agent(runtime: &dyn ServerRuntime, agent_id: Option<&str>) -> Result<(), HttpError> {
    let Some(agent_id) = agent_id.filter(|id| !id.is_empty()) else {
        return Ok(());
    };
    if runtime.agents().iter().any(|agent| agent.id == agent_id) {
        return Ok(());
    }
    Err(HttpError::not_found(format!("No such agent: {agent_id}")))
}

/// Refuses a workspace the manager cannot list, for create and for update.
///
/// A conversation bound to a workspace nothing can list is one the UI can never
/// show the files for. Shared by the two routes that accept an id so there is a
/// single spelling of the sentence — they had drifted once already, with create
/// checking and update not existing yet.
fn require_workspace(
    runtime: &dyn ServerRuntime,
    workspace_id: Option<&str>,
) -> Result<(), HttpError> {
    let Some(workspace_id) = workspace_id else {
        return Ok(());
    };
    if runtime.workspaces().get(workspace_id)?.is_some() {
        return Ok(());
    }
    Err(HttpError::not_found(format!(
        "No such workspace: {workspace_id}"
    )))
}

/// A fresh conversation key, for a client that did not choose one.
///
/// The browser mints its own before its first message is sent, so this is the
/// path for everything that does not — and the layout is the protocol's, which
/// is what keeps the two spellings from drifting.
fn mint_key(state: &AppState) -> String {
    let mut random = [0u8; 10];
    state.random.fill(&mut random);
    new_uuid(as_u64(state.clock.now_ms()), &random)
}

/// Sessions, newest activity first.
pub async fn list(
    State(state): State<AppState>,
    query: Result<Query<SessionListQuery>, QueryRejection>,
) -> Result<Json<SessionListResponse>, HttpError> {
    let query = read_query(query)?;
    assert_one_paging_mode(query.cursor.as_deref(), query.offset)?;
    let store = state.runtime.store();

    // The predicate, named once: the page runs under it and so does the count,
    // and a total describing a different set than the rows beneath it is a
    // "Page 4 of 3" that only appears once someone searches.
    let filter = ListSessions {
        origin: query.origin.clone(),
        exclude_origin: query.exclude_origin.clone(),
        workspace_id: query.workspace.clone(),
        query: query.q.clone(),
        ..ListSessions::default()
    };

    let after = match query.cursor.as_deref() {
        Some(cursor) => {
            let decoded = decode_session_cursor(cursor)?;
            Some(SessionCursor {
                updated_at_ms: decoded.updated_at_ms,
                key: decoded.key,
            })
        }
        None => None,
    };

    let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
    let rows = store.list_sessions(&ListSessions {
        // One more than asked for: the extra row is what decides whether a
        // cursor is issued, and it is dropped rather than returned.
        limit: Some(limit + 1),
        offset: query
            .offset
            .map(|offset| usize::try_from(offset).unwrap_or(usize::MAX)),
        order_by: query.sort.map(order_by),
        descending: query.desc,
        after,
        ..filter.clone()
    })?;

    // A cursor encodes `(updated_at_ms, key)`, which is a position in the
    // default ordering and in no other. Issuing one while the caller has sorted
    // by title would hand back a cursor that cannot be followed — the store
    // refuses the combination — so under any other ordering there simply is no
    // next cursor, and the pager uses `total` instead.
    let default_order = query.sort.unwrap_or(SessionSort::Updated) == SessionSort::Updated
        && query.desc.unwrap_or(true);

    let page = paginate(
        rows,
        limit,
        |last| {
            encode_session_cursor(&SessionListCursor {
                updated_at_ms: last.session.updated_at_ms,
                key: last.session.key.clone(),
            })
        },
        default_order,
    );

    // One statement for the whole page. A per-row lookup here is the difference
    // between a listing and fifty of them.
    let keys: Vec<&str> = page
        .rows
        .iter()
        .map(|record| record.session.key.as_str())
        .collect();
    let usage = store.session_usage(&keys)?;

    Ok(Json(SessionListResponse {
        sessions: page
            .rows
            .iter()
            .map(|record| {
                to_summary(
                    &record.session,
                    record.message_count,
                    usage.get(&record.session.key).copied(),
                )
            })
            .collect(),
        total: as_u64(store.count_sessions(&filter)?),
        next_cursor: page.next_cursor,
    }))
}

/// Which column the store orders by.
fn order_by(sort: SessionSort) -> SessionOrderBy {
    match sort {
        SessionSort::Updated => SessionOrderBy::Updated,
        SessionSort::Created => SessionOrderBy::Created,
        SessionSort::Title => SessionOrderBy::Title,
    }
}

/// Creates a session, or returns the existing one for a key.
pub async fn create(
    State(state): State<AppState>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<SessionSummary>), HttpError> {
    let request: CreateSessionRequest = read_json("body", body)?;
    let runtime = state.runtime.as_ref();

    require_workspace(runtime, request.workspace_id.as_deref())?;
    require_agent(runtime, request.agent_id.as_deref())?;

    // Creating is idempotent, so a client that retries a create it never saw
    // the response to gets its session rather than a 409 about a session it
    // already owns.
    let store = runtime.store();
    let key = request.key.unwrap_or_else(|| mint_key(&state));
    let record = store.ensure_session(
        &key,
        CreateSession {
            title: request.title,
            origin: Some("web".to_owned()),
            workspace_id: request.workspace_id,
            agent_id: request.agent_id,
            metadata: None,
        },
    )?;
    let count = store.message_count(&record.key)?;
    Ok((StatusCode::CREATED, Json(to_summary(&record, count, None))))
}

/// One session.
pub async fn get(
    State(state): State<AppState>,
    Path(params): Path<SessionParams>,
) -> Result<Json<SessionSummary>, HttpError> {
    let runtime = state.runtime.as_ref();
    let record = require_session(runtime, &params.key)?;
    let store = runtime.store();
    let usage = store.session_usage(&[record.key.as_str()])?;
    let count = store.message_count(&record.key)?;
    let total = usage.get(&record.key).copied();
    Ok(Json(to_summary(&record, count, total)))
}

/// Renames a session, or moves it to another agent or workspace.
pub async fn update(
    State(state): State<AppState>,
    Path(params): Path<SessionParams>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<SessionSummary>, HttpError> {
    let request: UpdateSessionRequest = read_json("body", body)?;
    let runtime = state.runtime.as_ref();
    require_session(runtime, &params.key)?;
    require_workspace(runtime, request.workspace_id.as_deref())?;
    require_agent(runtime, request.agent_id.as_deref())?;

    let store = runtime.store();
    let moved = request.workspace_id.is_some();
    let updated = store.update_session(
        &params.key,
        UpdateSession {
            title: request.title,
            agent_id: request.agent_id.map(Some),
            workspace_id: request.workspace_id,
            metadata: None,
        },
    )?;

    // Deliberately not refused while a turn is running. The loop captures its
    // jail once, when the turn starts, so the turn in flight finishes in the
    // workspace it began in and the next one picks up the move — there is no
    // state for a guard here to protect. Announced, though, so a second tab
    // does not keep showing the workspace it moved out of.
    if moved {
        state.hub.session_moved(&params.key);
    }

    let count = store.message_count(&params.key)?;
    Ok(Json(to_summary(&updated, count, None)))
}

/// Deletes a session and its messages.
pub async fn delete(
    State(state): State<AppState>,
    Path(params): Path<SessionParams>,
) -> Result<StatusCode, HttpError> {
    if state.runtime.store().delete_session(&params.key)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(HttpError::not_found(format!(
            "No session \"{}\"",
            params.key
        )))
    }
}

/// A session transcript, oldest first.
pub async fn messages(
    State(state): State<AppState>,
    Path(params): Path<SessionParams>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<SessionMessagesResponse>, HttpError> {
    let query = read_query(query)?;
    let runtime = state.runtime.as_ref();
    let record = require_session(runtime, &params.key)?;
    let store = runtime.store();

    let after_seq = match query.cursor.as_deref() {
        Some(cursor) => Some(decode_message_cursor(cursor)?.seq),
        None => None,
    };

    let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
    let rows = store.messages(
        &params.key,
        &ReadMessages {
            after_seq,
            limit: Some(limit + 1),
            ..ReadMessages::default()
        },
    )?;

    let page = paginate(
        rows,
        limit,
        |last| encode_message_cursor(&MessageCursor { seq: last.seq }),
        true,
    );

    Ok(Json(SessionMessagesResponse {
        session_key: params.key.clone(),
        messages: page.rows.iter().map(to_stored_message).collect(),
        next_cursor: page.next_cursor,
        // The one thing in a transcript these rows cannot describe: a
        // subagent's steps live in the subagent's own session. Sent whole
        // rather than paged with the messages — it is a handful of entries, and
        // a page that carried only its own share would leave a card on the next
        // page unable to say it had a run.
        subagent_runs: subagent_runs_of(&as_object(&record.metadata)),
        // Beside the runs, sent whole for the same reason. A failed turn
        // appends nothing, so without this a rebuilt transcript shows the
        // question, no answer, and no sign that anything went wrong.
        failures: store
            .turn_stats(&params.key, None)?
            .into_iter()
            .filter_map(|turn| turn.error.map(|error| (turn.turn_id, error)))
            .collect(),
    }))
}

/// Drops a session transcript, keeping the session.
pub async fn clear(
    State(state): State<AppState>,
    Path(params): Path<SessionParams>,
) -> Result<StatusCode, HttpError> {
    let runtime = state.runtime.as_ref();
    require_session(runtime, &params.key)?;
    runtime.store().clear_messages(&params.key)?;
    // The same courtesy the update route pays: a tab attached to this
    // conversation is still rendering the history that has just been deleted.
    state.hub.session_cleared(&params.key);
    Ok(StatusCode::NO_CONTENT)
}

/// What the agent would send to the model for this session.
pub async fn context(
    State(state): State<AppState>,
    Path(params): Path<SessionParams>,
) -> Result<Json<ContextResponse>, HttpError> {
    let runtime = state.runtime.as_ref();
    require_session(runtime, &params.key)?;

    // The agent-resolution policy lives in the context module, so the chat
    // channels measure against the same agent this panel does.
    build_context_response(runtime, &params.key)
        .await?
        .map(Json)
        .ok_or_else(|| HttpError::not_found(format!("No session \"{}\"", params.key)))
}

/// Forks a session at a point into a new one.
pub async fn branch(
    State(state): State<AppState>,
    Path(params): Path<SessionParams>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<SessionSummary>), HttpError> {
    let request: BranchSessionRequest = read_json("body", body)?;
    let runtime = state.runtime.as_ref();
    require_session(runtime, &params.key)?;

    // Forking mid-turn would copy a question whose answer has not been written
    // yet: the loop appends an assistant turn and all of its tool traffic in
    // one transaction at the end, so a branch taken now starts with an
    // unanswered question and no way to tell that it did.
    if state.hub.busy(&params.key) {
        return Err(HttpError::conflict(
            "A turn is running on this session. Stop it, then branch.",
        ));
    }

    let store = runtime.store();
    let fork = store.fork_session(
        &params.key,
        i64::try_from(request.seq).unwrap_or(i64::MAX),
        ForkSession {
            key: request.key,
            title: request.title,
            ..ForkSession::default()
        },
    )?;

    let usage = store.session_usage(&[fork.session.key.as_str()])?;
    let total = usage.get(&fork.session.key).copied();
    Ok((
        StatusCode::CREATED,
        Json(to_summary(&fork.session, fork.copied, total)),
    ))
}

/// `GET /api/sessions/:key/tasks` — the plan this conversation is running on.
///
/// Its own route rather than a field on the session body, because the two are
/// read on different cadences: the list moves several times inside one turn,
/// and the session carries a workspace, an agent and a subagent map that do
/// not. A 404 for a session with no row is the same answer `context` gives, and
/// for the same reason: a conversation nobody has started has no plan, and
/// inventing an empty one would report a list for a session that does not
/// exist.
pub async fn tasks(
    State(state): State<AppState>,
    Path(params): Path<SessionParams>,
) -> Result<Json<TasksResponse>, HttpError> {
    let runtime = state.runtime.as_ref();
    require_session(runtime, &params.key)?;

    Ok(Json(TasksResponse {
        tasks: runtime.store().tasks(&params.key)?,
    }))
}

/// `DELETE /api/sessions/:key/tasks` — empties the plan by hand.
///
/// The browser's half of `/tasks clear`. It is a route rather than a `PATCH` on
/// the session, because a plan is not one of the fields that body carries and
/// emptying one must not be expressible as a side effect of renaming a session.
pub async fn clear_tasks(
    State(state): State<AppState>,
    Path(params): Path<SessionParams>,
) -> Result<StatusCode, HttpError> {
    let runtime = state.runtime.as_ref();
    require_session(runtime, &params.key)?;
    runtime.store().set_tasks(&params.key, &[])?;
    Ok(StatusCode::NO_CONTENT)
}

/// What each turn in this session cost.
pub async fn turns(
    State(state): State<AppState>,
    Path(params): Path<SessionParams>,
    query: Result<Query<TurnsQuery>, QueryRejection>,
) -> Result<Json<TurnStatsResponse>, HttpError> {
    let query = read_query(query)?;
    let runtime = state.runtime.as_ref();
    require_session(runtime, &params.key)?;

    Ok(Json(TurnStatsResponse {
        session_key: params.key.clone(),
        turns: runtime
            .store()
            .turn_stats(
                &params.key,
                Some(usize::try_from(query.limit).unwrap_or(usize::MAX)),
            )?
            .into_iter()
            .map(to_turn_stats)
            .collect(),
    }))
}
