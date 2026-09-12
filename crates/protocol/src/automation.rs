//! Automation jobs.
//!
//! Schedules and payloads are unions discriminated on `kind` rather than one
//! struct with every variant's fields defaulted to null. The flat shape makes
//! `{kind: "cron", atMs: 5}` representable and forces a null check on `expr`
//! even where `kind` already proves it is set; here each variant carries
//! exactly its own fields and an impossible combination fails to parse.
//!
//! The variants **refuse unknown keys** rather than stripping them, which is
//! what makes that last clause true: a key-stripping object would silently
//! drop the stray `atMs` from a cron schedule, turning an importer bug or a
//! hand-edited job into a job that quietly runs on the wrong trigger. These
//! are our own persisted shapes, so an unknown key is always a defect worth
//! surfacing.

use garde::Validate;
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::json::{MAX_SAFE_INTEGER, literal, positive, prefault, tagged_union};

/// The origin a scheduled run's session is recorded under.
///
/// Load-bearing, exactly as the subagent origin is: the unscoped session
/// listing excludes it. A job on a five-minute interval writes about 105,000
/// sessions a year, every one a single machine-started turn, and a sidebar
/// that listed them would bury the conversations a person actually had. They
/// stay reachable by key and by asking for this origin by name.
pub const AUTOMATION_ORIGIN: &str = "automation";

literal! {
    /// The `kind` of an [`AtSchedule`].
    pub struct AtKind = "at";
}
literal! {
    /// The `kind` of an [`EverySchedule`].
    pub struct EveryKind = "every";
}
literal! {
    /// The `kind` of a [`CronSchedule`].
    pub struct CronKind = "cron";
}

/// One-shot, at an absolute epoch-ms instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct AtSchedule {
    /// Always `at`.
    pub kind: AtKind,
    /// When.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub at_ms: u64,
}

/// Fixed interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct EverySchedule {
    /// Always `every`.
    pub kind: EveryKind,
    /// The interval. Zero would spin the timer, so it is refused.
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub every_ms: u64,
}

/// Standard 5-field cron, evaluated in the install's `ui.timezone`.
///
/// **No per-job zone**, and its absence is deliberate. A per-job `tz` makes
/// "when does this fire" a question with three inputs — the job's zone, the
/// scheduler's default, and the zone the reader's browser rendered the answer
/// in. One install-wide zone means the expression is read
/// against the same clock the next-run line is printed against. A job stored
/// by an older build may still carry `tz`; the store strips it on open, because
/// this shape refuses unknown keys rather than ignoring them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct CronSchedule {
    /// Always `cron`.
    pub kind: CronKind,
    /// The cron expression.
    #[garde(length(utf16, min = 1))]
    pub expr: String,
}

tagged_union! {
    /// When a job fires.
    pub enum AutomationSchedule by "kind" {
        /// Once, at an instant.
        At(AtSchedule) = "at",
        /// On a fixed interval.
        Every(EverySchedule) = "every",
        /// On a cron expression.
        Cron(CronSchedule) = "cron",
    }
}

/// Where a job's output goes. Shared by both payload kinds.
///
/// `deliver: false` still runs the turn — the result lands in the job's run
/// history and the notification centre without interrupting anyone.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AutomationDelivery {
    /// Whether to send the result anywhere.
    #[serde(default)]
    pub deliver: bool,
    /// Channel id, e.g. `telegram`. Required in practice when `deliver` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Channel-specific destination (chat id, user id, room).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Overrides the isolated `automation:{jobId}:{runId}` session. Setting
    /// this is what makes a nightly job grow an unbounded context window, so
    /// the default of a fresh session per run is deliberate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// The workspace a run's session is created in. Absent means the default.
    ///
    /// Only ever *creates*: a job pinned to a `session_key` that already exists
    /// runs in whatever workspace that session is bound to. A job an agent
    /// schedules for itself is stamped with the workspace that agent was
    /// working in; the model does not get to name one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: Option<String>,
    /// The agent to run as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Additional channel id → address fan-out.
    #[serde(default)]
    pub targets: IndexMap<String, String>,
}

literal! {
    /// The `kind` of a [`ScheduledPayload`].
    pub struct ScheduledKind = "scheduled";
}
literal! {
    /// The `kind` of a [`HeartbeatPayload`].
    pub struct HeartbeatKind = "heartbeat";
}

/// Sends a fixed message to the agent.
///
/// The delivery fields are restated rather than nested, because this shape
/// refuses unknown keys and the point is to reject a `file` on a scheduled
/// payload instead of dropping it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct ScheduledPayload {
    /// Whether to send the result anywhere.
    #[serde(default)]
    pub deliver: bool,
    /// Channel id, e.g. `telegram`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Channel-specific destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Overrides the isolated per-run session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// The workspace a run's session is created in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: Option<String>,
    /// The agent to run as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Additional channel id → address fan-out.
    #[serde(default)]
    pub targets: IndexMap<String, String>,
    /// Always `scheduled`.
    pub kind: ScheduledKind,
    /// What to send the agent.
    #[garde(length(utf16, min = 1))]
    pub message: String,
}

/// Reads a markdown file and lets a cheap model decide whether to act.
///
/// The decide/run/evaluate triad is why heartbeats are not annoying: a forced
/// `heartbeat` tool call returns `skip` or `run`, and after a run the output is
/// evaluated again for whether it is worth interrupting the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct HeartbeatPayload {
    /// Whether to send the result anywhere.
    #[serde(default)]
    pub deliver: bool,
    /// Channel id, e.g. `telegram`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Channel-specific destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Overrides the isolated per-run session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// The workspace a run's session is created in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: Option<String>,
    /// The agent to run as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Additional channel id → address fan-out.
    #[serde(default)]
    pub targets: IndexMap<String, String>,
    /// Always `heartbeat`.
    pub kind: HeartbeatKind,
    /// Path relative to the workspace.
    #[serde(default = "default_heartbeat_file")]
    #[garde(length(utf16, min = 1))]
    pub file: String,
    /// The model that decides. Absent means the agent's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

fn default_heartbeat_file() -> String {
    "TASK.md".to_owned()
}

tagged_union! {
    /// What a job does when it fires.
    pub enum AutomationPayload by "kind" {
        /// A fixed message.
        Scheduled(ScheduledPayload) = "scheduled",
        /// A heartbeat file.
        Heartbeat(HeartbeatPayload) = "heartbeat",
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Not yet run.
    #[default]
    Pending,
    /// Ran.
    Ok,
    /// Failed.
    Error,
    /// The heartbeat model chose `skip`.
    Skipped,
}

/// The scheduler's bookkeeping for one job.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AutomationJobState {
    /// 0 when unscheduled (disabled, or a fired one-shot).
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub next_run_at_ms: u64,
    /// When it last fired, or 0.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub last_run_at_ms: u64,
    /// How the last run ended.
    #[serde(default)]
    pub last_status: RunStatus,
    /// The last run's error, or empty.
    #[serde(default)]
    pub last_error: String,
    /// How many times it has run.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub run_count: u64,
}

/// Who made a job, when it was not a person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AutomationJobCreator {
    /// The agent that asked for it.
    #[garde(length(utf16, min = 1))]
    pub agent_id: String,
    /// The conversation the tool call came from.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
}

/// A job, as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AutomationJob {
    /// The row id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// What the operator called it.
    #[garde(length(utf16, min = 1))]
    pub name: String,
    /// When it fires.
    #[garde(dive)]
    pub schedule: AutomationSchedule,
    /// What it does.
    #[garde(dive)]
    pub payload: AutomationPayload,
    /// The scheduler's bookkeeping.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub state: AutomationJobState,
    /// Whether it is scheduled at all.
    #[serde(default = "crate::json::yes")]
    pub enabled: bool,
    /// When it was created.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub created_at_ms: u64,
    /// When it was last edited.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub updated_at_ms: u64,
    /// Self-destruct after firing — how a one-shot reminder cleans up.
    #[serde(default)]
    pub delete_after_run: bool,
    /// Who made this, when it was not a person.
    ///
    /// Absent means the operator, through the panel. Present means an agent
    /// asked for it during a turn, and names both the agent and the
    /// conversation — so a job list that has grown mysterious can be traced
    /// back to the sentence that caused it, and one agent's jobs can be found
    /// and removed together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub created_by: Option<AutomationJobCreator>,
}

/// A single execution. Persisted as real rows rather than collapsing into
/// last-run-only state, so the UI can show a history and a nightly job that
/// failed three days ago is still diagnosable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AutomationRun {
    /// The row id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// The job it belongs to.
    #[garde(length(utf16, min = 1))]
    pub job_id: String,
    /// When it started.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub started_at_ms: u64,
    /// When it finished, once it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub finished_at_ms: Option<u64>,
    /// How it ended.
    pub status: RunStatus,
    /// Set when the heartbeat model chose `skip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What the agent answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// The session the turn ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// Things worth saying about a run that did not fail.
    ///
    /// A separate list rather than folding into `error`, because `status` is
    /// what the panel colours and a run that succeeded with a caveat must not
    /// read as broken: delivery requested on an install with no channel wired,
    /// a boot catch-up that coalesced several missed occurrences into one run,
    /// output truncated at the cap.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Job creation over REST. The server assigns `id`, timestamps and `state`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct CreateAutomationJob {
    /// What to call it.
    #[garde(length(utf16, min = 1))]
    pub name: String,
    /// When it fires.
    #[garde(dive)]
    pub schedule: AutomationSchedule,
    /// What it does.
    #[garde(dive)]
    pub payload: AutomationPayload,
    /// Whether it is scheduled at all.
    #[serde(default = "crate::json::yes")]
    pub enabled: bool,
    /// Self-destruct after firing.
    #[serde(default)]
    pub delete_after_run: bool,
}

/// A job edit over REST. Every field optional; absent means unchanged.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct UpdateAutomationJob {
    /// A new name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub name: Option<String>,
    /// A new schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub schedule: Option<AutomationSchedule>,
    /// A new payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub payload: Option<AutomationPayload>,
    /// Switch it on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Whether to self-destruct after firing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_after_run: Option<bool>,
}
