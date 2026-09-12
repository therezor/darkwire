//! Where a scheduled job actually gets written.
//!
//! The store lives in the server, four layers above this crate, so the tool
//! cannot depend on it. The established answer is the one `runner` gives for
//! `exec`: **the interface is declared down here, and the composition root
//! supplies the implementation.** `JailResolver` in `ghostai-security` is the
//! same shape for the same reason.
//!
//! Worth stating, because the subagent design deliberately went the other way:
//! delegation is *not* a tool, because starting a turn would invert the layer
//! graph and because [`ToolContext`](crate::ToolContext) has no event sink.
//! Neither objection applies here. Creating a job writes a row and returns a
//! string — the turn it causes happens later, on the scheduler's own timer,
//! with nothing to stream and nothing reaching back into a loop.
//!
//! **The port `for_turn` returns is bound to its caller.** It closes over the
//! agent and the session that asked, so the tool cannot name a different
//! owner, cannot see another agent's jobs and cannot delete one it did not
//! make. Every guard lives on that side rather than in the tool, because the
//! tool runs on arguments a model wrote and cannot be trusted to say who it is.

use std::sync::Arc;

use ghostai_protocol::{AutomationJob, CreateAutomationJob};

use crate::runner::ToolboxRequest;

/// Why a job could not be created, listed or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationRefusal {
    /// The calling turn is itself a scheduled run.
    Nested,
    /// This agent already holds as many jobs as it may.
    AtCapacity,
    /// No job with that id, or one this agent did not create.
    NotYours,
    /// The schedule cannot be honoured — an unreachable cron, a bad zone. The
    /// detail is the validator's own sentence, or empty.
    Unschedulable(String),
}

/// What a port answers: the value, or why not.
///
/// Answers rather than fails, for the reason the tool exists: a model told its
/// request was refused can carry on without it, and a turn that dies instead
/// leaves the operator with an error where an answer was possible.
pub type AutomationOutcome<T> = std::result::Result<T, AutomationRefusal>;

/// One turn's access to the scheduler, already scoped to the caller.
pub trait AutomationPort: Send + Sync {
    /// Creates a job owned by the calling agent and session.
    fn create(&self, input: CreateAutomationJob) -> AutomationOutcome<AutomationJob>;
    /// Only this agent's own jobs.
    fn list(&self) -> AutomationOutcome<Vec<AutomationJob>>;
    /// Removes one of this agent's own jobs.
    fn delete(&self, job_id: &str) -> AutomationOutcome<()>;
}

/// Supplies the port a turn's `automation` tool uses.
///
/// `None` means this build has no scheduler — a headless install, or a route
/// test — and the tool says so rather than pretending. Keyed by
/// [`ToolboxRequest`] because that is already the per-turn identity the loop
/// computes for `exec`, and it carries exactly what is needed: the agent, the
/// workspace and the session.
pub trait AutomationResolver: Send + Sync {
    /// The port for one turn, or `None` when nothing can be scheduled.
    fn for_turn(&self, request: &ToolboxRequest) -> Option<Arc<dyn AutomationPort>>;
}
