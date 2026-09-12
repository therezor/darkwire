//! Scheduled jobs and their run history.
//!
//! Here rather than in `ghostai-core` for the reason the auth tables are:
//! nothing below the transport schedules anything. The agent loop has no
//! opinion about when it is called, and the one thing that does — the
//! scheduler — sits at this level beside the hub it drives turns through.
//!
//! Three decisions carry the file.
//!
//!  - **`schedule` and `payload` are JSON columns, not decomposed.** They are
//!    discriminated unions, and the union exists so `{kind: "cron", atMs: 5}`
//!    is not representable. Spreading them into `kind, at_ms, every_ms, expr`
//!    rebuilds exactly the nullable flat shape it refuses, and nothing would
//!    then stop a hand-edited row from running on the wrong trigger. `state`
//!    *is* decomposed, because `next_run_at_ms` has to be indexable for the
//!    timer's due query.
//!
//!  - **A row whose JSON does not parse is disabled, not skipped.** Listing
//!    tolerates it — one bad row must not blank the panel — but
//!    [`AutomationStore::due_jobs`] cannot: a schedule nobody can read is a
//!    schedule nobody can honour, and quietly passing over it produces a job
//!    that shows in the UI and never fires. It is switched off with the parse
//!    failure in `last_error`, so it is visible and inert rather than a silent
//!    hole.
//!
//!  - **Run history is trimmed per job, not globally.** A job on a five-minute
//!    interval writes about 105,000 rows a year. One shared ceiling would let
//!    that job's afternoon evict a nightly job's entire year, which is
//!    backwards: the sparse history is the one worth keeping.

use std::collections::BTreeSet;
use std::sync::Arc;

use ghostai_core::clock::Clock;
use ghostai_core::db::Database;
use ghostai_core::errors::{ErrorKind, GhostError, Result};
use ghostai_core::session_store::IdSource;
use ghostai_core::sqlite_row::RowReader;
use ghostai_protocol::automation::{
    AutomationJob, AutomationJobCreator, AutomationJobState, AutomationPayload, AutomationRun,
    AutomationSchedule, RunStatus,
};
use rusqlite::{Row, params};
use serde_json::Value;

/// The `automation_jobs` table.
///
/// The comment inside the column list is deliberate and is the one place in
/// this repository where one appears: SQLite stored this text verbatim when
/// the TypeScript build created the table, so a byte-identical schema — which
/// is what lets an existing install upgrade in place — has to carry it too.
/// Nothing here ever runs `ALTER TABLE DROP COLUMN`, which is the operation the
/// no-comments rule exists to protect.
pub const AUTOMATION_JOBS_TABLE: &str = "CREATE TABLE IF NOT EXISTS automation_jobs (
  id               TEXT    PRIMARY KEY,
  name             TEXT    NOT NULL,
  schedule_json    TEXT    NOT NULL,
  payload_json     TEXT    NOT NULL,
  enabled          INTEGER NOT NULL DEFAULT 1,
  delete_after_run INTEGER NOT NULL DEFAULT 0,
  next_run_at_ms   INTEGER NOT NULL DEFAULT 0,
  last_run_at_ms   INTEGER NOT NULL DEFAULT 0,
  last_status      TEXT    NOT NULL DEFAULT 'pending',
  last_error       TEXT    NOT NULL DEFAULT '',
  run_count        INTEGER NOT NULL DEFAULT 0,
  created_at_ms    INTEGER NOT NULL,
  updated_at_ms    INTEGER NOT NULL,
  -- Who asked for this, when it was not a person. Empty is the operator, which
  -- is the common case, so these default rather than being nullable: a job the
  -- panel made and a job whose agent has been forgotten are the same row shape.
  created_by_agent   TEXT NOT NULL DEFAULT '',
  created_by_session TEXT NOT NULL DEFAULT ''
) STRICT;";

/// The only index the timer needs, and partial on purpose: `next_run_at_ms = 0`
/// means unscheduled, which in a mature table is the majority of rows — every
/// fired one-shot and every disabled job.
pub const AUTOMATION_JOBS_DUE_INDEX: &str = "CREATE INDEX IF NOT EXISTS automation_jobs_due
  ON automation_jobs(next_run_at_ms ASC, id ASC)
  WHERE enabled = 1 AND next_run_at_ms > 0;";

/// The `automation_runs` table.
///
/// A real foreign key, which `sessions`/`workspaces` could not express: those
/// are created by two different stores in an order nothing guarantees, and
/// these two are created together, here. `PRAGMA foreign_keys` is already ON
/// for the shared connection.
pub const AUTOMATION_RUNS_TABLE: &str = "CREATE TABLE IF NOT EXISTS automation_runs (
  id             TEXT    PRIMARY KEY,
  job_id         TEXT    NOT NULL REFERENCES automation_jobs(id) ON DELETE CASCADE,
  started_at_ms  INTEGER NOT NULL,
  finished_at_ms INTEGER,
  status         TEXT    NOT NULL,
  skip_reason    TEXT,
  error          TEXT,
  output         TEXT,
  warnings_json  TEXT    NOT NULL DEFAULT '[]',
  session_key    TEXT
) STRICT;";

/// One job's runs, newest first — the order the run listing walks.
pub const AUTOMATION_RUNS_JOB_INDEX: &str = "CREATE INDEX IF NOT EXISTS automation_runs_job
  ON automation_runs(job_id, started_at_ms DESC, id ASC);";

/// Every DDL statement the store runs on construction, in order.
pub const SCHEMA: &[&str] = &[
    AUTOMATION_JOBS_TABLE,
    AUTOMATION_JOBS_DUE_INDEX,
    AUTOMATION_RUNS_TABLE,
    AUTOMATION_RUNS_JOB_INDEX,
];

/// Columns added to a table an older build already created, in order.
///
/// `CREATE TABLE IF NOT EXISTS` does nothing to a table that exists, so a
/// column added after the fact is invisible to anyone whose database predates
/// it — and every read here would then fail with `storage`.
///
/// Deliberately not a migration framework. This repository has never needed
/// one: every other table has been created whole, and the ledger is only for
/// altering tables that already exist. This is that case, once, for two
/// columns.
pub const CREATED_BY_LEDGER: &[(&str, &str)] = &[
    (
        "created_by_agent",
        "ALTER TABLE automation_jobs ADD COLUMN created_by_agent TEXT NOT NULL DEFAULT ''",
    ),
    (
        "created_by_session",
        "ALTER TABLE automation_jobs ADD COLUMN created_by_session TEXT NOT NULL DEFAULT ''",
    ),
];

const READ: RowReader = RowReader::new("automation");

/// What the scheduler knows at creation time that the REST body does not.
#[derive(Debug, Clone)]
pub struct CreateJobInput {
    /// What the operator called it.
    pub name: String,
    /// When it fires.
    pub schedule: AutomationSchedule,
    /// What it does.
    pub payload: AutomationPayload,
    /// Whether it is scheduled at all.
    pub enabled: bool,
    /// Self-destruct after firing.
    pub delete_after_run: bool,
    /// 0 when the job is disabled or its schedule has no next occurrence.
    pub next_run_at_ms: i64,
    /// Absent means the operator made it through the panel.
    pub created_by: Option<AutomationJobCreator>,
}

/// A job edit. Every field absent means unchanged.
#[derive(Debug, Clone, Default)]
pub struct UpdateJobInput {
    /// A new name.
    pub name: Option<String>,
    /// A new schedule.
    pub schedule: Option<AutomationSchedule>,
    /// A new payload.
    pub payload: Option<AutomationPayload>,
    /// Switch it on or off.
    pub enabled: Option<bool>,
    /// Whether to self-destruct after firing.
    pub delete_after_run: Option<bool>,
    /// A new next-run instant, which the caller computes from the schedule.
    pub next_run_at_ms: Option<i64>,
}

/// One position in the `(started_at_ms DESC, id ASC)` order of a job's runs.
///
/// The store takes the decoded position rather than an opaque cursor string:
/// encoding is a transport concern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAfter {
    /// The `started_at_ms` of the last row of the previous page.
    pub started_at_ms: i64,
    /// Its id, which breaks a tie inside one millisecond.
    pub id: String,
}

/// How a caller asks for one page of a job's runs.
#[derive(Debug, Clone, Default)]
pub struct ListRuns {
    /// Rows per page.
    pub limit: Option<i64>,
    /// For a numbered pager, where `after` is for a sequential reader. The two
    /// are alternatives and the route refuses the pair.
    pub offset: Option<i64>,
    /// Where the previous page ended.
    pub after: Option<RunAfter>,
}

/// What [`AutomationStore::finish_run`] records.
#[derive(Debug, Clone, Default)]
pub struct FinishRunInput {
    /// How it ended.
    pub status: RunStatus,
    /// What the agent answered.
    pub output: Option<String>,
    /// Why it failed.
    pub error: Option<String>,
    /// Set when the heartbeat model chose `skip`.
    pub skip_reason: Option<String>,
    /// Things worth saying about a run that did not fail.
    pub warnings: Vec<String>,
}

/// One finished run folded into the job's `state`.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// When the run started.
    pub ran_at_ms: i64,
    /// How it ended.
    pub status: RunStatus,
    /// The failure, when there was one.
    pub error: Option<String>,
}

/// What a trimmed run took with it, so its session can be cleaned up too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimmedRun {
    /// The run id that went.
    pub id: String,
    /// The session it had been the only reference to, if it named one.
    pub session_key: Option<String>,
}

/// The wire spelling of a run status.
fn status_str(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Ok => "ok",
        RunStatus::Error => "error",
        RunStatus::Skipped => "skipped",
    }
}

/// A status written by an older build becomes `pending` rather than failing the
/// read — the same trade the notification level makes, for the same reason.
fn read_status(row: &Row<'_>, column: &str) -> Result<RunStatus> {
    Ok(match READ.string(row, column)?.as_str() {
        "ok" => RunStatus::Ok,
        "error" => RunStatus::Error,
        "skipped" => RunStatus::Skipped,
        _ => RunStatus::Pending,
    })
}

fn read_warnings(row: &Row<'_>) -> Vec<String> {
    let Some(raw) = READ.optional_string(row, "warnings_json") else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Milliseconds as the wire carries them.
fn as_wire_ms(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn row_to_run(row: &Row<'_>) -> Result<AutomationRun> {
    Ok(AutomationRun {
        id: READ.string(row, "id")?,
        job_id: READ.string(row, "job_id")?,
        started_at_ms: as_wire_ms(READ.int(row, "started_at_ms")?),
        finished_at_ms: READ.optional_int(row, "finished_at_ms").map(as_wire_ms),
        status: read_status(row, "status")?,
        skip_reason: READ.optional_string(row, "skip_reason"),
        error: READ.optional_string(row, "error"),
        output: READ.optional_string(row, "output"),
        session_key: READ.optional_string(row, "session_key"),
        warnings: read_warnings(row),
    })
}

/// The parse failure a bad row carries, or `None` when it is fine.
fn job_parse_error(row: &Row<'_>) -> Result<Option<String>> {
    let schedule = READ.string(row, "schedule_json")?;
    if let Err(error) = serde_json::from_str::<AutomationSchedule>(&schedule) {
        return Ok(Some(format!("schedule: {error}")));
    }
    let payload = READ.string(row, "payload_json")?;
    if let Err(error) = serde_json::from_str::<AutomationPayload>(&payload) {
        return Ok(Some(format!("payload: {error}")));
    }
    Ok(None)
}

/// The attribution, or nothing at all when the operator made it.
fn created_by_of(row: &Row<'_>) -> Option<AutomationJobCreator> {
    let agent_id = READ
        .optional_string(row, "created_by_agent")
        .unwrap_or_default();
    if agent_id.is_empty() {
        return None;
    }
    Some(AutomationJobCreator {
        agent_id,
        session_key: READ
            .optional_string(row, "created_by_session")
            .unwrap_or_default(),
    })
}

/// What one row of the due query turned out to be.
///
/// Read under the connection lock and acted on after it is released, because
/// disabling a broken row is itself a write.
enum Verdict {
    /// A job that parsed and is due.
    Due(Box<AutomationJob>),
    /// An id and the reason its stored shape could not be read.
    Broken(String, String),
}

/// Parses a row, or logs and returns `None`. Never fails on bad JSON.
fn tolerate(row: &Row<'_>) -> Result<Option<AutomationJob>> {
    match job_parse_error(row)? {
        None => Ok(Some(row_to_job(row)?)),
        Some(problem) => {
            tracing::warn!(
                job_id = READ.optional_string(row, "id"),
                problem = problem,
                "skipping an automation job whose stored shape does not parse",
            );
            Ok(None)
        }
    }
}

/// A row whose JSON has already been checked by [`job_parse_error`].
fn row_to_job(row: &Row<'_>) -> Result<AutomationJob> {
    let schedule = serde_json::from_str::<AutomationSchedule>(&READ.string(row, "schedule_json")?)
        .map_err(|error| {
            GhostError::new(ErrorKind::Storage, format!("Unreadable schedule: {error}"))
        })?;
    let payload = serde_json::from_str::<AutomationPayload>(&READ.string(row, "payload_json")?)
        .map_err(|error| {
            GhostError::new(ErrorKind::Storage, format!("Unreadable payload: {error}"))
        })?;

    Ok(AutomationJob {
        id: READ.string(row, "id")?,
        name: READ.string(row, "name")?,
        schedule,
        payload,
        enabled: READ.int(row, "enabled")? == 1,
        delete_after_run: READ.int(row, "delete_after_run")? == 1,
        created_by: created_by_of(row),
        created_at_ms: as_wire_ms(READ.int(row, "created_at_ms")?),
        updated_at_ms: as_wire_ms(READ.int(row, "updated_at_ms")?),
        state: AutomationJobState {
            next_run_at_ms: as_wire_ms(READ.int(row, "next_run_at_ms")?),
            last_run_at_ms: as_wire_ms(READ.int(row, "last_run_at_ms")?),
            last_status: read_status(row, "last_status")?,
            last_error: READ.string(row, "last_error")?,
            run_count: as_wire_ms(READ.int(row, "run_count")?),
        },
    })
}

/// Jobs and runs, over the shared connection.
pub struct AutomationStore {
    db: Database,
    clock: Arc<dyn Clock>,
    new_id: IdSource,
}

impl std::fmt::Debug for AutomationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AutomationStore")
    }
}

impl AutomationStore {
    /// Creates the tables if they are not there, applies the column ledger, and
    /// strips any legacy per-job timezone.
    pub fn new(db: Database, clock: Arc<dyn Clock>, new_id: IdSource) -> Result<AutomationStore> {
        // Connection-level and idempotent. `SessionStore` already sets it on
        // the shared connection, but this store's cascade is the only thing
        // standing between deleting a job and orphaning its entire run history
        // — so it does not inherit that guarantee from a construction order
        // nothing enforces.
        db.execute_batch("PRAGMA foreign_keys = ON")?;
        for statement in SCHEMA {
            db.execute_batch(statement)?;
        }

        let store = AutomationStore { db, clock, new_id };
        store.add_missing_columns()?;
        store.drop_legacy_schedule_tz()?;
        Ok(store)
    }

    fn add_missing_columns(&self) -> Result<()> {
        let present = self.db.column_names("automation_jobs")?;
        for (column, ddl) in CREATED_BY_LEDGER {
            if !present.iter().any(|name| name == column) {
                self.db.execute_batch(ddl)?;
            }
        }
        Ok(())
    }

    /// Strips the per-job `tz` an older build wrote into `schedule_json`.
    ///
    /// Not cosmetic, and not deferrable. A cron schedule refuses unknown keys,
    /// so a row still carrying `tz` does not parse with the key ignored — it
    /// fails outright, and every job in the listing is dropped because one of
    /// them is old. The panel would show an empty automation page on an install
    /// that has jobs.
    ///
    /// Rewriting rather than tolerating on read, because a blob nobody rewrites
    /// keeps the stale field until someone edits that job by hand — and the
    /// next reader of the row has to know a rule that is written down nowhere
    /// in it.
    ///
    /// The log line is the point of collecting the zones. A job written
    /// `0 9 * * *` in `Europe/Kyiv` now fires at 09:00 in the install's zone,
    /// which is a different instant; an operator who is told which zones were
    /// dropped can go and check the jobs that moved. Silence here would be the
    /// same change made invisibly.
    fn drop_legacy_schedule_tz(&self) -> Result<()> {
        let candidates: Vec<(String, String)> = {
            let guard = self.db.lock();
            let mut statement = guard.prepare(
                r#"SELECT id, schedule_json FROM automation_jobs WHERE schedule_json LIKE '%"tz"%'"#,
            )?;
            let mut rows = statement.query([])?;
            let mut found = Vec::new();
            while let Some(row) = rows.next()? {
                found.push((READ.string(row, "id")?, READ.string(row, "schedule_json")?));
            }
            found
        };
        if candidates.is_empty() {
            return Ok(());
        }

        let mut zones = BTreeSet::new();
        let mut changed = 0_u64;
        for (id, raw) in candidates {
            // Unreadable JSON is a different defect and not one this pass
            // invented. Leaving it is what lets the schema report it against
            // the job it is on.
            let Ok(Value::Object(mut object)) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let Some(tz) = object.shift_remove("tz") else {
                continue;
            };
            if let Value::String(zone) = &tz
                && !zone.is_empty()
            {
                zones.insert(zone.clone());
            }
            let rewritten = serde_json::to_string(&Value::Object(object)).map_err(|error| {
                GhostError::new(ErrorKind::Storage, format!("Unwritable schedule: {error}"))
            })?;
            self.db.lock().execute(
                "UPDATE automation_jobs SET schedule_json = ? WHERE id = ?",
                params![&rewritten, &id],
            )?;
            changed += 1;
        }

        if changed > 0 {
            let zones: Vec<&str> = zones.iter().map(String::as_str).collect();
            tracing::warn!(
                jobs = changed,
                zones = ?zones,
                "dropped per-job automation timezones; these jobs now use the install timezone (ui.timezone) and may fire at a different instant",
            );
        }
        Ok(())
    }

    // Jobs

    /// Writes a new job and returns it as stored.
    pub fn create_job(&self, input: &CreateJobInput) -> Result<AutomationJob> {
        let now = self.clock.now_ms();
        let id = (self.new_id)();
        let schedule =
            serde_json::to_string(&input.schedule).map_err(|error| unwritable(&error))?;
        let payload = serde_json::to_string(&input.payload).map_err(|error| unwritable(&error))?;
        let (agent, session) = match &input.created_by {
            Some(creator) => (creator.agent_id.clone(), creator.session_key.clone()),
            None => (String::new(), String::new()),
        };

        self.db.lock().execute(
            "INSERT INTO automation_jobs
               (id, name, schedule_json, payload_json, enabled, delete_after_run,
                next_run_at_ms, last_run_at_ms, last_status, last_error, run_count,
                created_at_ms, updated_at_ms, created_by_agent, created_by_session)
             VALUES (?, ?, ?, ?, ?, ?, ?, 0, 'pending', '', 0, ?, ?, ?, ?)",
            params![
                &id,
                &input.name,
                &schedule,
                &payload,
                i64::from(input.enabled),
                i64::from(input.delete_after_run),
                input.next_run_at_ms,
                now,
                now,
                &agent,
                &session,
            ],
        )?;

        self.get_job(&id)?.ok_or_else(|| {
            GhostError::new(
                ErrorKind::Storage,
                "Automation job vanished immediately after insert",
            )
            .with_detail("id", id.as_str())
        })
    }

    /// One job by id, or `None` when there is none or its shape does not parse.
    pub fn get_job(&self, id: &str) -> Result<Option<AutomationJob>> {
        let guard = self.db.lock();
        let mut statement = guard.prepare("SELECT * FROM automation_jobs WHERE id = ?")?;
        let mut rows = statement.query(params![id])?;
        match rows.next()? {
            Some(row) => tolerate(row),
            None => Ok(None),
        }
    }

    /// Every job, newest first.
    ///
    /// Unpaged, which is what the list response describes: the panel edits a
    /// handful of jobs and a cursor the protocol does not mention would be a
    /// second contract.
    pub fn list_jobs(&self) -> Result<Vec<AutomationJob>> {
        let guard = self.db.lock();
        let mut statement =
            guard.prepare("SELECT * FROM automation_jobs ORDER BY created_at_ms DESC, id ASC")?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            if let Some(job) = tolerate(row)? {
                out.push(job);
            }
        }
        Ok(out)
    }

    /// Applies a patch. `None` when there is no such row.
    pub fn update_job(&self, id: &str, patch: &UpdateJobInput) -> Result<Option<AutomationJob>> {
        {
            let guard = self.db.lock();
            let mut statement = guard.prepare("SELECT id FROM automation_jobs WHERE id = ?")?;
            if !statement.exists(params![id])? {
                return Ok(None);
            }
        }

        let schedule = patch
            .schedule
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| unwritable(&error))?;
        let payload = patch
            .payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| unwritable(&error))?;

        self.db.lock().execute(
            "UPDATE automation_jobs
                SET name             = COALESCE(?, name),
                    schedule_json    = COALESCE(?, schedule_json),
                    payload_json     = COALESCE(?, payload_json),
                    enabled          = COALESCE(?, enabled),
                    delete_after_run = COALESCE(?, delete_after_run),
                    next_run_at_ms   = COALESCE(?, next_run_at_ms),
                    updated_at_ms    = ?
              WHERE id = ?",
            params![
                patch.name.as_deref(),
                schedule.as_deref(),
                payload.as_deref(),
                patch.enabled.map(i64::from),
                patch.delete_after_run.map(i64::from),
                patch.next_run_at_ms,
                self.clock.now_ms(),
                id,
            ],
        )?;

        self.get_job(id)
    }

    /// Removes one job and, through the cascade, its whole run history.
    pub fn delete_job(&self, id: &str) -> Result<bool> {
        let changed = self
            .db
            .lock()
            .execute("DELETE FROM automation_jobs WHERE id = ?", params![id])?;
        Ok(changed > 0)
    }

    /// How many jobs one agent has made.
    ///
    /// A count rather than the length of a listing, because the only caller is
    /// a cap check on a path a model drives — and a model in a loop would
    /// otherwise parse and discard every job in the table on every attempt.
    pub fn count_jobs_by(&self, agent_id: &str) -> Result<i64> {
        let guard = self.db.lock();
        let n = guard.query_row(
            "SELECT COUNT(*) AS n FROM automation_jobs WHERE created_by_agent = ?",
            params![agent_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(n)
    }

    // Scheduling

    /// When the timer should next wake, or `None` when nothing is scheduled.
    pub fn earliest_due_ms(&self) -> Result<Option<i64>> {
        let guard = self.db.lock();
        let at = guard.query_row(
            "SELECT MIN(next_run_at_ms) AS at FROM automation_jobs
              WHERE enabled = 1 AND next_run_at_ms > 0",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(at)
    }

    /// Jobs due at or before `now_ms`, soonest first.
    ///
    /// A row that does not parse is switched off here rather than passed over —
    /// see the module header. It is the one read path where tolerating a bad
    /// row would produce something worse than an error: a job that exists,
    /// shows in the panel, and silently never runs.
    pub fn due_jobs(&self, now_ms: i64, limit: i64) -> Result<Vec<AutomationJob>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }

        let verdicts: Vec<Verdict> = {
            let guard = self.db.lock();
            let mut statement = guard.prepare(
                "SELECT * FROM automation_jobs
                  WHERE enabled = 1 AND next_run_at_ms > 0 AND next_run_at_ms <= ?
                  ORDER BY next_run_at_ms ASC, id ASC
                  LIMIT ?",
            )?;
            let mut rows = statement.query(params![now_ms, limit])?;
            let mut found = Vec::new();
            while let Some(row) = rows.next()? {
                found.push(match job_parse_error(row)? {
                    None => Verdict::Due(Box::new(row_to_job(row)?)),
                    Some(problem) => Verdict::Broken(READ.string(row, "id")?, problem),
                });
            }
            found
        };

        let mut due = Vec::new();
        for verdict in verdicts {
            match verdict {
                Verdict::Due(job) => due.push(*job),
                Verdict::Broken(id, problem) => {
                    tracing::error!(
                        job_id = id,
                        problem = problem,
                        "disabling an automation job that does not parse",
                    );
                    self.db.lock().execute(
                        "UPDATE automation_jobs
                            SET enabled = 0, next_run_at_ms = 0, last_status = 'error',
                                last_error = ?, updated_at_ms = ?
                          WHERE id = ?",
                        params![
                            format!("This job's stored shape does not parse ({problem})."),
                            self.clock.now_ms(),
                            &id,
                        ],
                    )?;
                }
            }
        }
        Ok(due)
    }

    /// Every enabled job whose time passed while the process was down.
    pub fn missed_jobs(&self, now_ms: i64) -> Result<Vec<AutomationJob>> {
        self.due_jobs(now_ms, i64::MAX)
    }

    /// Moves one job's next fire time.
    pub fn set_next_run(&self, id: &str, next_run_at_ms: i64) -> Result<()> {
        self.db.lock().execute(
            "UPDATE automation_jobs SET next_run_at_ms = ?, updated_at_ms = ? WHERE id = ?",
            params![next_run_at_ms, self.clock.now_ms(), id],
        )?;
        Ok(())
    }

    /// Folds one finished run into the job's `state`.
    pub fn record_outcome(&self, id: &str, outcome: &RunOutcome) -> Result<()> {
        self.db.lock().execute(
            "UPDATE automation_jobs
                SET last_run_at_ms = ?, last_status = ?, last_error = ?,
                    run_count = run_count + 1, updated_at_ms = ?
              WHERE id = ?",
            params![
                outcome.ran_at_ms,
                status_str(outcome.status),
                outcome.error.as_deref().unwrap_or(""),
                self.clock.now_ms(),
                id,
            ],
        )?;
        Ok(())
    }

    // Runs

    /// Writes a `pending` run row before anything else happens.
    pub fn start_run(&self, job_id: &str, session_key: Option<String>) -> Result<AutomationRun> {
        let id = (self.new_id)();
        let started_at_ms = self.clock.now_ms();
        self.db.lock().execute(
            "INSERT INTO automation_runs
               (id, job_id, started_at_ms, finished_at_ms, status, skip_reason, error, output, warnings_json, session_key)
             VALUES (?, ?, ?, NULL, 'pending', NULL, NULL, NULL, '[]', ?)",
            params![&id, job_id, started_at_ms, session_key.as_deref()],
        )?;

        Ok(AutomationRun {
            id,
            job_id: job_id.to_owned(),
            started_at_ms: as_wire_ms(started_at_ms),
            finished_at_ms: None,
            status: RunStatus::Pending,
            skip_reason: None,
            error: None,
            output: None,
            session_key,
            warnings: Vec::new(),
        })
    }

    /// Closes one run out and returns it as it now stands.
    pub fn finish_run(
        &self,
        run_id: &str,
        input: &FinishRunInput,
    ) -> Result<Option<AutomationRun>> {
        let warnings =
            serde_json::to_string(&input.warnings).map_err(|error| unwritable(&error))?;
        self.db.lock().execute(
            "UPDATE automation_runs
                SET finished_at_ms = ?, status = ?, skip_reason = ?, error = ?,
                    output = ?, warnings_json = ?
              WHERE id = ?",
            params![
                self.clock.now_ms(),
                status_str(input.status),
                input.skip_reason.as_deref(),
                input.error.as_deref(),
                input.output.as_deref(),
                &warnings,
                run_id,
            ],
        )?;
        self.get_run(run_id)
    }

    /// One run by id.
    pub fn get_run(&self, id: &str) -> Result<Option<AutomationRun>> {
        let guard = self.db.lock();
        let mut statement = guard.prepare("SELECT * FROM automation_runs WHERE id = ?")?;
        let mut rows = statement.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_run(row)?)),
            None => Ok(None),
        }
    }

    /// A page of one job's runs, newest first.
    ///
    /// The predicate is the sort order written as a comparison. Two runs
    /// starting in one millisecond is the normal case when the boot sweep
    /// dispatches a backlog.
    pub fn list_runs(&self, job_id: &str, options: &ListRuns) -> Result<Vec<AutomationRun>> {
        let limit = options.limit.unwrap_or(50);
        let offset = options.offset.unwrap_or(0);
        let after_at = options.after.as_ref().map(|after| after.started_at_ms);
        let after_id = options.after.as_ref().map(|after| after.id.as_str());

        let guard = self.db.lock();
        let mut statement = guard.prepare(
            "SELECT * FROM automation_runs
              WHERE job_id = ?
                AND (? IS NULL
                     OR started_at_ms < ?
                     OR (started_at_ms = ? AND id > ?))
              ORDER BY started_at_ms DESC, id ASC
              LIMIT ? OFFSET ?",
        )?;
        let mut rows = statement.query(params![
            job_id, after_at, after_at, after_at, after_id, limit, offset
        ])?;

        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_run(row)?);
        }
        Ok(out)
    }

    /// How many runs one job has kept.
    ///
    /// What a numbered pager needs and a cursor does not — "Page 3 of 12"
    /// cannot be derived from a page of rows. The total is bounded rather than
    /// open-ended: [`AutomationStore::trim_runs`] holds each job to its
    /// retention knob, so this counts a capped table however long the job has
    /// been running.
    pub fn count_runs(&self, job_id: &str) -> Result<i64> {
        let guard = self.db.lock();
        let n = guard.query_row(
            "SELECT COUNT(*) AS n FROM automation_runs WHERE job_id = ?",
            params![job_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(n)
    }

    /// Drops everything past the newest `keep` runs of one job.
    ///
    /// Returns what went, because a trimmed run's session is now unreachable —
    /// the run row was the only thing naming it — and the caller deletes it so
    /// the sessions table stays bounded by the same knob.
    pub fn trim_runs(&self, job_id: &str, keep: i64) -> Result<Vec<TrimmedRun>> {
        let doomed: Vec<TrimmedRun> = {
            let guard = self.db.lock();
            let mut statement = guard.prepare(
                "SELECT id, session_key FROM automation_runs
                  WHERE job_id = ?
                    AND id NOT IN (SELECT id FROM automation_runs
                                    WHERE job_id = ?
                                    ORDER BY started_at_ms DESC, id ASC
                                    LIMIT ?)",
            )?;
            let mut rows = statement.query(params![job_id, job_id, keep])?;
            let mut found = Vec::new();
            while let Some(row) = rows.next()? {
                found.push(TrimmedRun {
                    id: READ.string(row, "id")?,
                    session_key: READ.optional_string(row, "session_key"),
                });
            }
            found
        };

        for run in &doomed {
            self.db
                .lock()
                .execute("DELETE FROM automation_runs WHERE id = ?", params![&run.id])?;
        }
        Ok(doomed)
    }

    /// Closes out runs left `pending` by a process that died mid-turn.
    ///
    /// Called once at boot. Without it a hard kill leaves a row that the panel
    /// renders as still running, forever — and the run it describes has no
    /// process behind it.
    pub fn reconcile_pending(&self, message: &str) -> Result<usize> {
        let changed = self.db.lock().execute(
            "UPDATE automation_runs
                SET status = 'error', error = ?, finished_at_ms = ?
              WHERE status = 'pending'",
            params![message, self.clock.now_ms()],
        )?;
        Ok(changed)
    }
}

fn unwritable(error: &serde_json::Error) -> GhostError {
    GhostError::new(
        ErrorKind::Storage,
        format!("An automation field could not be written as JSON: {error}"),
    )
}
