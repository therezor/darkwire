//! The task list, as a section of the prompt.
//!
//! **The runtime half, not the static one.** A plan changes while a turn runs —
//! that is the entire point of it — so a section read once per turn would show
//! the model a list it had already moved past by iteration three. It is the one
//! contributor whose content is supposed to be volatile, and the runtime half is
//! where volatile content is affordable: it sits after the history, so rewriting
//! it costs its own tokens and not the conversation's.
//!
//! The read is one row by primary key, per iteration. `runtime_section` is
//! synchronous and the store is too, so there is nothing to hold: a contributor
//! that cached the list would serve one session's plan to a concurrent turn on
//! another, which is the same reason `memory_contributor` re-reads its folder.
//!
//! **No operator template.** Every other section describes a mechanism an
//! operator might want worded differently; this one is a list they did not
//! write, under a heading naming what it is. An agent that does not want it
//! denies the `todo` tool, which removes the tool and this section together.

use std::sync::Arc;

use darkwire_core::SessionStore;
use darkwire_protocol::tasks::{TaskItem, render_tasks};

use crate::prompt::{ContextContributor, RuntimePromptContext};

/// What the model reads, given a list.
///
/// Pure, and separate from the contributor for the reason `render_skills` is:
/// the empty case and the exact shape are what is worth testing, and neither
/// needs a database.
///
/// An empty list renders as the empty string, never as a bare heading. The
/// runtime block drops a section that trims to nothing, so this is how "no plan"
/// becomes "no section".
pub fn render_task_section(tasks: &[TaskItem]) -> String {
    if tasks.is_empty() {
        return String::new();
    }
    format!("## Tasks\n\n{}", render_tasks(tasks))
}

/// Reads the session's task list and places it in the runtime prompt.
#[derive(Clone)]
pub struct TasksContributor {
    store: Arc<SessionStore>,
}

impl std::fmt::Debug for TasksContributor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TasksContributor").finish_non_exhaustive()
    }
}

impl TasksContributor {
    /// A contributor reading from `store`.
    pub fn new(store: Arc<SessionStore>) -> TasksContributor {
        TasksContributor { store }
    }
}

impl ContextContributor for TasksContributor {
    fn name(&self) -> &'static str {
        "tasks"
    }

    fn runtime_section(&self, context: &RuntimePromptContext) -> Option<String> {
        // A store that cannot answer is not a reason to fail a turn: the plan is
        // an aid, and a request that drops it is better than one that never
        // leaves. The failure is visible in the logs the store writes.
        let tasks = self
            .store
            .tasks(&context.static_context.session_key)
            .unwrap_or_default();
        let section = render_task_section(&tasks);
        if section.is_empty() {
            None
        } else {
            Some(section)
        }
    }
}
