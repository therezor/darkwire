//! Where an agent's commands run.
//!
//! [`CommandRunner`] answers *how* to run one command. An environment is the
//! place itself, and it adds exactly one thing a caller has to know: whether
//! that place is confined. Everything else a backend needs — the image, the
//! socket, the pool — is the backend's own business and has no representation
//! here, which is what keeps an agent from being written against one.
//!
//! **One method, deliberately.** A richer capability record was considered and
//! dropped: every field would have had exactly one reader, and a boolean nobody
//! branches on is a claim nobody checks. `confined` has two readers that
//! already exist — the exec guard's host-shaped refusals, and the prompt.
//!
//! [`EnvironmentResolver`] is the same shape as
//! [`AutomationResolver`](crate::AutomationResolver), for the same reason: the
//! container backend lives in a crate above this one, so the interface is
//! declared down here and the composition root supplies the implementation.
//! Resolution is per turn because placement is a property of (agent, workspace,
//! session), and re-deriving it per call would let a mid-turn config change
//! move a command that is already running.

use std::sync::Arc;

use ghostai_core::Result;

use crate::runner::{CommandRunner, LocalRunner, PlacementRequest, RunOutcome, RunRequest};
use crate::tool::BoxFuture;

/// A place commands run.
pub trait Environment: CommandRunner {
    /// Whether commands run away from this machine's filesystem.
    ///
    /// Read by the exec guard, which refuses shells and absolute paths on the
    /// host precisely because a command there can reach anything the agent's
    /// own process can. Inside a container neither refusal buys anything, and
    /// both cost an operator the shell their image exists to provide.
    ///
    /// A backend answers `true` only when *it* runs the command elsewhere.
    /// Answering `true` while the process still starts on this machine would
    /// lift both refusals for a command that then runs unconfined.
    fn confined(&self) -> bool;
}

/// Where a turn's commands run, and what the model is told about it.
///
/// The prompt rides along rather than hanging off [`Environment`] because it is
/// not a property of the *place*. It is operator wording read from the
/// definition on disk, and the trait above is deliberately one method. Reading
/// it here also keeps the disk access in the composition root, where the rest
/// of the policy I/O already lives; a loop cannot open a definition and should
/// not learn how.
#[derive(Clone)]
pub struct Placed {
    /// Where commands run.
    pub environment: Arc<dyn Environment>,
}

impl Placed {
    /// This machine, with nothing to say about itself.
    #[must_use]
    pub fn host() -> Placed {
        Placed {
            environment: Arc::new(HostEnvironment::new()),
        }
    }
}

/// The environment for one turn.
pub trait EnvironmentResolver: Send + Sync {
    /// Where this turn's commands run.
    fn for_turn(&self, request: &PlacementRequest) -> Placed;
}

/// Commands run as child processes of this one.
///
/// The default, and the only environment that needs nothing installed. It is a
/// thin wrapper rather than a `CommandRunner` used directly, so that every
/// caller goes through one seam and `confined` is answered in one place.
#[derive(Debug, Default)]
pub struct HostEnvironment {
    runner: LocalRunner,
}

impl HostEnvironment {
    /// A host environment with the default kill grace.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl CommandRunner for HostEnvironment {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        self.runner.run(request)
    }
}

impl Environment for HostEnvironment {
    fn confined(&self) -> bool {
        false
    }
}
