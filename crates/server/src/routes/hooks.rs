//! Seams only the end-to-end suite uses.
//!
//! In the manifest like every other route, so the auth-matrix test covers them,
//! and `Required` like the routes they stand in for: a hook that skipped
//! authentication would be a hole the moment a build shipped with the feature
//! on. Excluded from the OpenAPI document, because they are not part of the API
//! a client may rely on.
//!
//! **Two independent switches, and both are needed.** The cargo feature decides
//! whether this code is compiled at all, and `GHOSTAI_TEST_HOOKS=1` decides
//! whether it answers. A release artefact is built without the feature, so the
//! environment variable reaches nothing; a binary that *was* built with it —
//! the one CI hands to Playwright — still refuses until the harness says so.
//! Either switch alone is a no.
//!
//! What they exist for is the rule about never asserting a transient state. A
//! browser test that wanted a finished automation run had to start a real turn
//! and wait for it; these let the suite put the durable state in place and then
//! assert on the page that renders it.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use garde::Validate;
use ghostai_core::messages::{AssistantOptions, assistant_message, user_message};
use ghostai_core::session_store::{AppendOptions, CreateSession};
use ghostai_protocol::automation::{AutomationRun, RunStatus};
use ghostai_protocol::messages::ChatMessage;
use ghostai_protocol::rest::Notification;
use ghostai_protocol::ws::NotificationLevel;
use serde::{Deserialize, Serialize};

use crate::automation_store::FinishRunInput;
use crate::errors::HttpError;
use crate::notifications::CreateNotificationInput;
use crate::queries::IdParams;
use crate::routes::AppState;
use crate::schema::parse_body;

/// The environment variable that arms the hooks at run time.
pub const TEST_HOOKS_ENV: &str = "GHOSTAI_TEST_HOOKS";

/// Whether the hooks are armed.
///
/// Read per request rather than cached at boot: the cost is a single
/// environment lookup, and a value read once is a value that cannot be turned
/// off again — which is the wrong shape for a switch whose whole job is to stay
/// off.
fn armed() -> bool {
    std::env::var(TEST_HOOKS_ENV).is_ok_and(|value| value == "1")
}

/// Refuses unless the environment armed the hooks.
///
/// A 404 rather than a 403, and deliberately: a build that is not running the
/// suite should look exactly like a build that has no such route, so probing
/// for one tells an attacker nothing about how the binary was compiled.
fn require_armed() -> Result<(), HttpError> {
    if armed() {
        Ok(())
    } else {
        Err(HttpError::not_found("No such route"))
    }
}

/// Reads a JSON body into one of the shapes below.
fn read_json<T: serde::de::DeserializeOwned + Validate<Context = ()>>(
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<T, HttpError> {
    let Json(raw) = body.map_err(|error| HttpError::bad_request(error.body_text()))?;
    parse_body("body", raw)
}

/// Who said one of a seeded conversation's lines.
///
/// Two roles and not four: a seed is a conversation somebody could have had,
/// and a `tool` message with no call to answer or a second `system` prompt is
/// not one. A spec that wants either is asserting on the loop, which is the
/// loop's own suite's to prove.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedRole {
    /// The person.
    User,
    /// The model.
    Assistant,
}

/// One line of a conversation to put in place without running a turn.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SeedMessage {
    /// Who said it.
    pub role: SeedRole,
    /// What they said. Plain text; a seeded turn needs no parts.
    #[garde(length(min = 1))]
    pub text: String,
}

/// A conversation to put in place without running a turn.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct CreateSessionHook {
    /// The session key. The suite names it so a spec can navigate straight to
    /// it.
    #[garde(length(min = 1))]
    pub key: String,
    /// The title the sidebar shows.
    #[serde(default)]
    pub title: Option<String>,
    /// Where it came from. Defaults to `web`.
    #[serde(default)]
    pub origin: Option<String>,
    /// Which workspace it lives in.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Which agent it is bound to.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// The history it already has. Empty is a conversation with no messages.
    ///
    /// Written through the store's own `append_many`, so a seeded transcript is
    /// sequenced and validated exactly as a turn's would be — the point of
    /// seeding is to reach the state a finished turn leaves behind, and a row
    /// written any other way would be a different state that merely looks
    /// similar on screen.
    #[serde(default)]
    #[garde(dive)]
    pub messages: Vec<SeedMessage>,
}

/// What the session hook answers with.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedSession {
    /// The key the row was stored under.
    pub key: String,
}

/// Creates a conversation without running a turn.
pub async fn sessions(
    State(state): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<CreatedSession>), HttpError> {
    require_armed()?;
    let request: CreateSessionHook = read_json(body)?;
    let store = state.runtime.store();
    let record = store.ensure_session(
        &request.key,
        CreateSession {
            title: request.title,
            origin: request.origin,
            workspace_id: request.workspace_id,
            agent_id: request.agent_id,
            metadata: None,
        },
    )?;
    if !request.messages.is_empty() {
        let messages = request
            .messages
            .into_iter()
            .map(|seed| match seed.role {
                SeedRole::User => ChatMessage::User(user_message(seed.text)),
                SeedRole::Assistant => ChatMessage::Assistant(assistant_message(
                    seed.text,
                    AssistantOptions::default(),
                )),
            })
            .collect();
        store.append_many(&record.key, messages, &AppendOptions::default())?;
    }
    Ok((
        StatusCode::CREATED,
        Json(CreatedSession { key: record.key }),
    ))
}

/// A notification to raise without waiting for something to raise it.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct CreateNotificationHook {
    /// The headline.
    #[garde(length(min = 1))]
    pub title: String,
    /// The detail. Empty is normal.
    #[serde(default)]
    pub body: String,
    /// How loud.
    #[serde(default)]
    pub level: NotificationLevel,
    /// The conversation it is about, if any.
    #[serde(default)]
    pub session_key: Option<String>,
    /// The automation job that raised it, if any.
    #[serde(default)]
    pub job_id: Option<String>,
}

/// Raises a notification without waiting for something to raise it.
pub async fn notifications(
    State(state): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Notification>), HttpError> {
    require_armed()?;
    let request: CreateNotificationHook = read_json(body)?;
    let raised = state.notifications.create(CreateNotificationInput {
        title: request.title,
        body: request.body,
        level: request.level,
        session_key: request.session_key,
        job_id: request.job_id,
    })?;
    Ok((StatusCode::CREATED, Json(raised)))
}

/// A run to start without a schedule firing.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct StartRunHook {
    /// The job the run belongs to. A real row: the foreign key is enforced.
    #[garde(length(min = 1))]
    pub job_id: String,
    /// The conversation the turn would have run in, if the spec wants one.
    #[serde(default)]
    pub session_key: Option<String>,
}

/// Starts an automation run without a schedule firing.
pub async fn automation_run(
    State(state): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<AutomationRun>), HttpError> {
    require_armed()?;
    let request: StartRunHook = read_json(body)?;
    let started = state
        .automation
        .start_run(&request.job_id, request.session_key)?;
    Ok((StatusCode::CREATED, Json(started)))
}

/// How a run the suite started ended.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct FinishRunHook {
    /// How it ended.
    #[serde(default)]
    pub status: RunStatus,
    /// What the agent answered.
    #[serde(default)]
    pub output: Option<String>,
    /// Why it failed.
    #[serde(default)]
    pub error: Option<String>,
    /// Set when the heartbeat model chose `skip`.
    #[serde(default)]
    pub skip_reason: Option<String>,
    /// Things worth saying about a run that did not fail.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Finishes an automation run the suite started.
pub async fn automation_run_finish(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<AutomationRun>, HttpError> {
    require_armed()?;
    let request: FinishRunHook = read_json(body)?;
    let finished = state.automation.finish_run(
        &params.id,
        &FinishRunInput {
            status: request.status,
            output: request.output,
            error: request.error,
            skip_reason: request.skip_reason,
            warnings: request.warnings,
        },
    )?;
    finished
        .map(Json)
        .ok_or_else(|| HttpError::not_found(format!("No automation run \"{}\"", params.id)))
}

/// Moves the scheduler on by hand.
///
/// The engine reads what is due when it is told to, so a tick is a refresh: the
/// suite advances whatever drives the clock and then asks the engine to look
/// again, rather than waiting out a real interval.
pub async fn scheduler_tick(State(state): State<AppState>) -> Result<StatusCode, HttpError> {
    require_armed()?;
    let Some(scheduler) = state.scheduler.as_deref() else {
        return Err(HttpError::not_found(
            "This build has no scheduler, so there is nothing to tick.",
        ));
    };
    scheduler.refresh();
    Ok(StatusCode::NO_CONTENT)
}
