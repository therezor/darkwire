//! Commands that outlive the call that started them.
//!
//! Declared here and implemented by the server, for the reason
//! [`crate::automation`] gives: a finished job's result has to reach a
//! conversation, and the hub that owns conversations is layers above this
//! crate.
//!
//! **The port `for_turn` returns is bound to its caller's session.** A job it
//! starts reports back there and nowhere else, and `kill` can reach only that
//! session's jobs.

use std::sync::Arc;

use darkwire_core::Clock;
use darkwire_security::ExecPlan;

use crate::runner::{CommandRunner, PlacementRequest};

/// A command to run in the background, already guarded and approved.
pub struct StartJob {
    /// What the model asked for, for the label and the result.
    pub argv: Vec<String>,
    /// What runs.
    pub plan: ExecPlan,
    /// `0` means no limit.
    pub timeout_ms: u64,
    /// Where it runs: the turn's own runner, so a sandboxed agent stays
    /// sandboxed.
    pub runner: Arc<dyn CommandRunner>,
    /// Measures the run.
    pub clock: Arc<dyn Clock>,
    /// The agent's `backgroundJobs`: how many this session may run at once.
    pub limit: u64,
    /// The agent's result budget, which the posted result is cut to.
    pub max_output_chars: u64,
}

impl std::fmt::Debug for StartJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartJob")
            .field("argv", &self.argv)
            .field("timeout_ms", &self.timeout_ms)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

/// Why a job could not be started or stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundRefusal {
    /// The agent's `backgroundJobs` is `0`.
    Off,
    /// This session already runs as many as the agent allows.
    AtCapacity,
    /// No running job with that id in this session.
    Unknown,
    /// A subagent or scheduled run: nobody owns the turn a result would wake.
    NotHere,
}

/// One turn's access to background jobs, already scoped to its session.
pub trait BackgroundPort: Send + Sync {
    /// Starts a job and returns its id.
    fn start(&self, job: StartJob) -> Result<String, BackgroundRefusal>;
    /// Stops a running job. Its result is still posted, as stopped.
    fn kill(&self, id: &str) -> Result<(), BackgroundRefusal>;
}

/// Supplies the port a turn's `exec` and `kill` use. `None` is a build with
/// nowhere to post a result, where both refuse.
pub trait BackgroundResolver: Send + Sync {
    /// The port for one turn.
    fn for_turn(&self, request: &PlacementRequest) -> Option<Arc<dyn BackgroundPort>>;
}
