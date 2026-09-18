//! Where a session's task list actually gets written.
//!
//! The store lives in `darkwire-core`, and the session key the list belongs to
//! is not something a tool may name: a model writing its own plan must not be
//! able to write it onto somebody else's conversation. So this follows the
//! answer [`automation`](crate::automation) gives — **the interface is declared
//! down here, and the composition root supplies the implementation** — with the
//! same guard on the same reasoning: the port is bound to the turn that got it,
//! and closes over the session key rather than taking one.
//!
//! That binding is also what gives a subagent its own list for nothing. A
//! delegated run opens its own session, so the port its turn receives points
//! somewhere else, and neither run can see or clear the other's plan.
//!
//! One method, because the write contract is one call: the model sends the list
//! it wants, and the last call wins. There is no add, no complete and no
//! reorder, so there is nothing here to keep consistent with a patch format.

use darkwire_core::Result;
use darkwire_protocol::tasks::TaskItem;

/// One turn's access to its session's task list, already scoped to it.
pub trait TaskPort: Send + Sync {
    /// Replaces the whole list. An empty slice clears it.
    fn replace(&self, tasks: &[TaskItem]) -> Result<()>;
}
