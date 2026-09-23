//! The `automation` tool's reach into the scheduler, scoped to one turn.
//!
//! Here rather than in `darkwire-tools` because this is where the stores are —
//! and because every guard below needs to read something the tool has no
//! business being told. The tool passes arguments a model wrote; it does not
//! get to claim which agent it is, which session it is in, or whether it is
//! allowed to schedule at all. [`AutomationResolver::for_turn`] binds all
//! three, so the port a tool receives can only ever act as its actual caller.
//!
//! Three refusals, and one of them is the reason this file exists:
//!
//!  - **`Nested`** — a scheduled run may not schedule. Without it a job whose
//!    payload asks the agent to "keep an eye on things" can create another job
//!    that does the same, and the install grows jobs geometrically with nobody
//!    watching. The check reads the stored session's origin rather than
//!    trusting anything passed down, because the row is the only thing that
//!    cannot be argued with.
//!  - **`AtCapacity`** — a cap per agent, so a model in a loop meets a wall
//!    instead of filling the table.
//!  - **`NotYours`** — an agent lists and deletes only what it created. The
//!    operator's own jobs are invisible to it.
//!
//! The delegation refusal in `darkwire-agent` is the nearest existing guard and
//! does **not** cover the first one: it works off the turn's delegation chain,
//! which is empty for a turn a person started — and the scheduler starts turns
//! exactly the same way. Origin is the only honest signal.

use std::sync::Arc;

use darkwire_core::clock::Clock;
use darkwire_core::errors::ErrorKind;
use darkwire_core::session_store::SessionStore;
use darkwire_protocol::automation::{
    AUTOMATION_ORIGIN, AutomationJob, AutomationJobCreator, AutomationPayload, CreateAutomationJob,
};
use darkwire_protocol::subagent::{SUBAGENT_METADATA_KEY, SUBAGENT_ORIGIN, SubagentLineage};
use darkwire_tools::automation::{
    AutomationOutcome, AutomationPort, AutomationRefusal, AutomationResolver,
};
use darkwire_tools::runner::PlacementRequest;

use crate::automation_store::{AutomationStore, CreateJobInput};
use crate::scheduler::first_run_at;

/// How many jobs one agent may hold.
///
/// A backstop rather than a budget, in the spirit of the subagent depth cap:
/// the thing that actually bounds this is that an operator has to grant the
/// tool at all, and each create prompts. This is what stops a model that has
/// misread its own instructions from turning that one grant into a thousand
/// rows.
pub const MAX_AGENT_JOBS: i64 = 25;

/// How far up a delegation chain the nested check will follow.
///
/// Well past any depth the delegation cap allows, and only there so that a
/// lineage which loops back on itself ends in a refusal rather than a hang.
const MAX_LINEAGE_HOPS: usize = 32;

/// Re-arms the timer after a write, so a job created mid-turn actually fires.
pub type RefreshTimer = Arc<dyn Fn() + Send + Sync>;

/// Reads the install's `ui.timezone` — the one zone a cron expression is read
/// in. A function rather than a value, because a settings save moves it.
pub type TimezoneSource = Arc<dyn Fn() -> String + Send + Sync>;

/// Builds the per-turn [`AutomationPort`] the `automation` tool calls.
pub struct ServerAutomationResolver {
    jobs: Arc<AutomationStore>,
    sessions: Arc<SessionStore>,
    timezone: TimezoneSource,
    clock: Arc<dyn Clock>,
    refresh: Option<RefreshTimer>,
}

impl std::fmt::Debug for ServerAutomationResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerAutomationResolver")
    }
}

impl ServerAutomationResolver {
    /// A resolver over the job store and the session store the guards read.
    pub fn new(
        jobs: Arc<AutomationStore>,
        sessions: Arc<SessionStore>,
        timezone: TimezoneSource,
        clock: Arc<dyn Clock>,
    ) -> ServerAutomationResolver {
        ServerAutomationResolver {
            jobs,
            sessions,
            timezone,
            clock,
            refresh: None,
        }
    }

    /// Re-arms the scheduler's timer after every write this port makes.
    ///
    /// Separate from the constructor because the scheduler is built after the
    /// resolver: the loop needs the tool, and the scheduler needs the loop.
    #[must_use]
    pub fn with_refresh(mut self, refresh: RefreshTimer) -> ServerAutomationResolver {
        self.refresh = Some(refresh);
        self
    }
}

impl AutomationResolver for ServerAutomationResolver {
    fn for_turn(&self, request: &PlacementRequest) -> Option<Arc<dyn AutomationPort>> {
        Some(Arc::new(TurnPort {
            jobs: Arc::clone(&self.jobs),
            sessions: Arc::clone(&self.sessions),
            timezone: Arc::clone(&self.timezone),
            clock: Arc::clone(&self.clock),
            refresh: self.refresh.clone(),
            agent_id: request.agent_id.clone(),
            session_key: request.session_key.clone(),
            workspace_id: request.workspace_id.clone(),
        }))
    }
}

/// One turn's port, closed over the caller it may act as.
struct TurnPort {
    jobs: Arc<AutomationStore>,
    sessions: Arc<SessionStore>,
    timezone: TimezoneSource,
    clock: Arc<dyn Clock>,
    refresh: Option<RefreshTimer>,
    agent_id: String,
    session_key: String,
    workspace_id: String,
}

impl TurnPort {
    /// Whether this turn is itself a scheduled run, or works for one.
    ///
    /// A subagent's session has an origin of its own, so the question is asked
    /// of the conversation at the top of its lineage. A lineage that cannot be
    /// followed to the top is refused: it is exactly the case this guard
    /// cannot vouch for.
    fn nested(&self) -> bool {
        let mut key = self.session_key.clone();
        for _ in 0..=MAX_LINEAGE_HOPS {
            let Ok(Some(session)) = self.sessions.get_session(&key) else {
                // No row yet is a conversation on its first turn, which only a
                // person or a channel starts.
                return key != self.session_key;
            };
            if session.origin == AUTOMATION_ORIGIN {
                return true;
            }
            if session.origin != SUBAGENT_ORIGIN {
                return false;
            }
            let parent = session
                .metadata
                .get(SUBAGENT_METADATA_KEY)
                .cloned()
                .and_then(|value| serde_json::from_value::<SubagentLineage>(value).ok());
            let Some(parent) = parent else {
                return true;
            };
            key = parent.parent_session_key;
        }
        true
    }

    /// This agent's own jobs.
    fn mine(&self) -> Vec<AutomationJob> {
        self.jobs
            .list_jobs()
            .unwrap_or_default()
            .into_iter()
            .filter(|job| {
                job.created_by
                    .as_ref()
                    .is_some_and(|creator| creator.agent_id == self.agent_id)
            })
            .collect()
    }

    /// Stamps the calling agent and workspace onto whichever payload variant
    /// this is.
    ///
    /// Set here and not in the tool, for the same reason the creator is: the
    /// tool runs on arguments a model wrote, so letting it name an agent would
    /// be letting it schedule a turn as somebody else.
    ///
    /// Without this the payload carried no agent, the scheduler read that as
    /// "the default agent", and every job any agent made ran on a different
    /// prompt and a different tool grant than the one that wrote it — which
    /// reads, from the outside, as the agent not understanding the tool.
    ///
    /// The workspace answers the same failure: a job scheduled during a turn in
    /// a named workspace would otherwise run in the default one, so the
    /// follow-up work could not see the files that prompted it. The turn's workspace is also
    /// the only one this agent has any claim to — a model naming its own would
    /// be a way out of the jail it is working in.
    fn stamp(&self, payload: AutomationPayload) -> AutomationPayload {
        match payload {
            AutomationPayload::Scheduled(mut scheduled) => {
                scheduled.agent_id = Some(self.agent_id.clone());
                scheduled.workspace_id = Some(self.workspace_id.clone());
                AutomationPayload::Scheduled(scheduled)
            }
            AutomationPayload::Heartbeat(mut heartbeat) => {
                heartbeat.agent_id = Some(self.agent_id.clone());
                heartbeat.workspace_id = Some(self.workspace_id.clone());
                AutomationPayload::Heartbeat(heartbeat)
            }
        }
    }
}

impl AutomationPort for TurnPort {
    fn create(&self, input: CreateAutomationJob) -> AutomationOutcome<AutomationJob> {
        if self.nested() {
            return Err(AutomationRefusal::Nested);
        }
        if self
            .jobs
            .count_jobs_by(&self.agent_id)
            .unwrap_or(MAX_AGENT_JOBS)
            >= MAX_AGENT_JOBS
        {
            return Err(AutomationRefusal::AtCapacity);
        }

        // The same validator the REST route uses, so a schedule the timer could
        // not honour cannot be created here either — and the model gets the
        // parser's own sentence rather than a generic refusal.
        let next_run_at_ms = first_run_at(
            &input.schedule,
            self.clock.now_ms(),
            input.enabled,
            &(self.timezone)(),
        )
        .map_err(|error| match error.kind {
            ErrorKind::Config => AutomationRefusal::Unschedulable(error.message),
            _ => AutomationRefusal::Unschedulable(String::new()),
        })?;

        let created = self
            .jobs
            .create_job(&CreateJobInput {
                name: input.name,
                schedule: input.schedule,
                payload: self.stamp(input.payload),
                enabled: input.enabled,
                delete_after_run: input.delete_after_run,
                next_run_at_ms,
                created_by: Some(AutomationJobCreator {
                    agent_id: self.agent_id.clone(),
                    session_key: self.session_key.clone(),
                }),
            })
            .map_err(|error| AutomationRefusal::Unschedulable(error.message))?;

        if let Some(refresh) = &self.refresh {
            refresh();
        }
        Ok(created)
    }

    fn list(&self) -> AutomationOutcome<Vec<AutomationJob>> {
        // Deliberately allowed inside a scheduled run: reading what it has
        // scheduled tells a job something useful and creates nothing.
        Ok(self.mine())
    }

    fn delete(&self, job_id: &str) -> AutomationOutcome<()> {
        let job = self.jobs.get_job(job_id).unwrap_or(None);
        // One answer for "no such job" and "not yours", so an agent cannot map
        // the operator's jobs by probing ids for the difference.
        let owned = job.as_ref().is_some_and(|job| {
            job.created_by
                .as_ref()
                .is_some_and(|creator| creator.agent_id == self.agent_id)
        });
        if !owned {
            return Err(AutomationRefusal::NotYours);
        }

        let _ = self.jobs.delete_job(job_id);
        if let Some(refresh) = &self.refresh {
            refresh();
        }
        Ok(())
    }
}
