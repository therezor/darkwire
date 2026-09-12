//! Scheduled jobs over REST.
//!
//! Two things here are not the obvious choice.
//!
//! **[`run`] answers 202 with a `pending` run**, not 200 with the result. A
//! turn takes minutes; a handler that awaited one would hold a browser request
//! open past every timeout between them. The client watches the run list, which
//! the `notification` frame tells it to refresh — and which is also what keeps
//! the end-to-end suite inside the rule about never asserting a transient
//! state.
//!
//! **A bad cron expression is a 422 naming the field.** Parsing one fails with
//! [`ErrorKind::Config`], whose default mapping is a 500, because a config
//! failure normally means *this install* is broken. Here it means the operator
//! typed something into a form, so it is caught and re-raised as the validation
//! failure it actually is.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use ghostai_core::ErrorKind;
use ghostai_protocol::automation::{
    AutomationJob, AutomationPayload, AutomationRun, AutomationSchedule, CreateAutomationJob,
    UpdateAutomationJob,
};
use ghostai_protocol::rest::{AutomationJobListResponse, AutomationRunListResponse};

use crate::automation_store::{CreateJobInput, ListRuns, RunAfter, UpdateJobInput};
use crate::cursor::{
    AutomationRunCursor, assert_one_paging_mode, decode_automation_run_cursor,
    encode_automation_run_cursor, paginate,
};
use crate::errors::HttpError;
use crate::queries::{IdParams, PageQuery};
use crate::routes::AppState;
use crate::runtime::ServerRuntime;
use crate::scheduler::{SchedulerPort, first_run_at};
use crate::schema::{parse_body, validated};

/// A stored count as the wire carries it.
///
/// Storage counts upwards from zero and the wire is unsigned; a negative here
/// would mean a corrupt row, and zero is the answer that cannot mislead.
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

/// Reads a query string, reporting a failure in the one error envelope.
fn read_query<T: garde::Validate<Context = ()>>(
    query: Result<Query<T>, QueryRejection>,
) -> Result<T, HttpError> {
    let Query(value) = query.map_err(|error| HttpError::unprocessable(error.body_text()))?;
    validated("query", value)
}

/// Validates a schedule by asking it when it would next fire.
///
/// Cheaper than a second validator and impossible to drift from one: whatever
/// the engine will do with this schedule is exactly what runs here, so an
/// expression the timer could not honour cannot be saved. An unusable install
/// timezone surfaces the same way, on the save rather than at the first fire —
/// the alternative is a job that looks scheduled and silently never runs.
fn next_run_or_refuse(
    schedule: &AutomationSchedule,
    now_ms: i64,
    enabled: bool,
    tz: &str,
) -> Result<i64, HttpError> {
    first_run_at(schedule, now_ms, enabled, tz).map_err(|error| {
        if error.kind == ErrorKind::Config {
            HttpError::unprocessable(error.message.clone()).with_detail("/schedule", error.message)
        } else {
            HttpError::from(error)
        }
    })
}

/// The workspace a payload names, whichever kind it is.
fn payload_workspace(payload: &AutomationPayload) -> Option<&str> {
    match payload {
        AutomationPayload::Scheduled(scheduled) => scheduled.workspace_id.as_deref(),
        AutomationPayload::Heartbeat(heartbeat) => heartbeat.workspace_id.as_deref(),
    }
}

/// Refuses a payload naming a workspace nothing can list.
///
/// Checked at authoring time because the alternative surfaces much later and
/// much worse: resolving a jail refuses an id that is not a legal slug, so a
/// typo here becomes a heartbeat that fails every interval forever, reported as
/// a run failure rather than as the mistake it is.
fn require_workspace(
    runtime: &dyn ServerRuntime,
    workspace_id: Option<&str>,
) -> Result<(), HttpError> {
    let Some(id) = workspace_id else {
        return Ok(());
    };
    if runtime.workspaces().get(id)?.is_none() {
        return Err(HttpError::not_found(format!("No such workspace: {id}")));
    }
    Ok(())
}

/// The engine, or a refusal naming which of the two reasons it is missing.
fn require_scheduler(state: &AppState) -> Result<&dyn SchedulerPort, HttpError> {
    let Some(scheduler) = state.scheduler.as_deref() else {
        return Err(HttpError::not_found(
            "This build has no scheduler, so a job cannot be run on demand.",
        ));
    };
    if !scheduler.enabled() {
        // Not a 404: the route exists and the job exists. The operator turned
        // the engine off, and saying so is what lets them turn it back on.
        const MESSAGE: &str = "The scheduler is disabled in settings.";
        return Err(HttpError::unprocessable(MESSAGE).with_detail("/scheduler/enabled", MESSAGE));
    }
    Ok(scheduler)
}

/// Tells the engine to re-read what is due.
///
/// After every write, without exception: a job created in the panel does not
/// run until a restart otherwise.
fn refresh(state: &AppState) {
    if let Some(scheduler) = state.scheduler.as_deref() {
        scheduler.refresh();
    }
}

/// The job, or a 404 — never a silently empty answer standing in for one.
fn require_job(state: &AppState, id: &str) -> Result<AutomationJob, HttpError> {
    state
        .automation
        .get_job(id)?
        .ok_or_else(|| HttpError::not_found(format!("No automation job \"{id}\"")))
}

/// Every scheduled job.
pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<AutomationJobListResponse>, HttpError> {
    Ok(Json(AutomationJobListResponse {
        jobs: state.automation.list_jobs()?,
    }))
}

/// Creates a scheduled job.
pub async fn create(
    State(state): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<AutomationJob>), HttpError> {
    let request: CreateAutomationJob = read_json("body", body)?;
    require_workspace(state.runtime.as_ref(), payload_workspace(&request.payload))?;

    // Live rather than the boot config, so a zone changed in the appearance
    // panel applies to the next job saved rather than to the next restart.
    let timezone = state.runtime.config().ui.timezone;
    let next_run_at_ms = next_run_or_refuse(
        &request.schedule,
        state.clock.now_ms(),
        request.enabled,
        &timezone,
    )?;

    let created = state.automation.create_job(&CreateJobInput {
        name: request.name,
        schedule: request.schedule,
        payload: request.payload,
        enabled: request.enabled,
        delete_after_run: request.delete_after_run,
        next_run_at_ms,
        // Absent: the operator made this one through the panel.
        created_by: None,
    })?;
    refresh(&state);
    Ok((StatusCode::CREATED, Json(created)))
}

/// One scheduled job.
pub async fn get(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
) -> Result<Json<AutomationJob>, HttpError> {
    Ok(Json(require_job(&state, &params.id)?))
}

/// Changes a scheduled job.
pub async fn update(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<AutomationJob>, HttpError> {
    let request: UpdateAutomationJob = read_json("body", body)?;
    let existing = require_job(&state, &params.id)?;
    require_workspace(
        state.runtime.as_ref(),
        request.payload.as_ref().and_then(payload_workspace),
    )?;

    // Recomputed whenever either half of "when does this fire" moves. Leaving
    // the old instant behind is how a job edited to run at 9am keeps firing at
    // 3am until it happens to be restarted.
    let rescheduled = request.schedule.is_some() || request.enabled.is_some();
    let next_run_at_ms = if rescheduled {
        let schedule = request.schedule.clone().unwrap_or(existing.schedule);
        let enabled = request.enabled.unwrap_or(existing.enabled);
        let timezone = state.runtime.config().ui.timezone;
        Some(next_run_or_refuse(
            &schedule,
            state.clock.now_ms(),
            enabled,
            &timezone,
        )?)
    } else {
        None
    };

    let updated = state.automation.update_job(
        &params.id,
        &UpdateJobInput {
            name: request.name,
            schedule: request.schedule,
            payload: request.payload,
            enabled: request.enabled,
            delete_after_run: request.delete_after_run,
            next_run_at_ms,
        },
    )?;
    let job = updated
        .ok_or_else(|| HttpError::not_found(format!("No automation job \"{}\"", params.id)))?;
    refresh(&state);
    Ok(Json(job))
}

/// Deletes a scheduled job and its history.
pub async fn delete(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
) -> Result<StatusCode, HttpError> {
    if !state.automation.delete_job(&params.id)? {
        return Err(HttpError::not_found(format!(
            "No automation job \"{}\"",
            params.id
        )));
    }
    refresh(&state);
    Ok(StatusCode::NO_CONTENT)
}

/// Runs a scheduled job now.
pub async fn run(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
) -> Result<(StatusCode, Json<AutomationRun>), HttpError> {
    require_job(&state, &params.id)?;
    let scheduler = require_scheduler(&state)?;
    // A job that is already running fails with `conflict`, which the one error
    // mapping already turns into a 409; there is nothing to re-raise here.
    let started = scheduler.run_now(&params.id)?;
    // 202: the run has started, and its answer is minutes away. The client
    // follows the run list rather than holding a socket open.
    Ok((StatusCode::ACCEPTED, Json(started)))
}

/// One job's run history, newest first.
pub async fn runs(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<AutomationRunListResponse>, HttpError> {
    require_job(&state, &params.id)?;
    let query = read_query(query)?;
    assert_one_paging_mode(query.cursor.as_deref(), query.offset)?;

    let after = match query.cursor.as_deref() {
        Some(cursor) => {
            let decoded = decode_automation_run_cursor(cursor)?;
            Some(RunAfter {
                started_at_ms: decoded.started_at_ms,
                id: decoded.id,
            })
        }
        None => None,
    };

    let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
    // One more than asked for, so "is there another page" is answered by what
    // came back rather than by a second count query.
    let rows = state.automation.list_runs(
        &params.id,
        &ListRuns {
            limit: Some(i64::from(query.limit) + 1),
            offset: query.offset.map(i64::from),
            after,
        },
    )?;

    let page = paginate(
        rows,
        limit,
        |last| {
            encode_automation_run_cursor(&AutomationRunCursor {
                started_at_ms: i64::try_from(last.started_at_ms).unwrap_or(i64::MAX),
                id: last.id.clone(),
            })
        },
        true,
    );

    Ok(Json(AutomationRunListResponse {
        runs: page.rows,
        next_cursor: page.next_cursor,
        // The panel this feeds is a numbered pager: a job on a five-minute
        // schedule produces a few hundred runs a day, and "of 288" is the part
        // a page of rows cannot say. Unlike sessions there is one ordering, so
        // the cursor stays valid whatever the caller asked for.
        total: as_u64(state.automation.count_runs(&params.id)?),
    }))
}
