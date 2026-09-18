//! `todo` — the plan for a long piece of work, as the model keeps it.
//!
//! One call replaces the whole list. There is no add, no complete and no
//! reorder, and that is the design rather than a first version of it: a patch
//! format needs stable ids, and stable ids need the model to remember what it
//! called each task three iterations ago. Sending the list back is one thing to
//! get right instead of two, and it makes the tool idempotent — the same call
//! twice is the same list.
//!
//! **The list is not the point; reading it back is.** It is placed in the
//! runtime half of the prompt, so it arrives on every iteration of every turn,
//! which is what stops a forty-step turn drifting off the plan it wrote at step
//! three. That is also why the caps are small: ten tasks of a hundred characters
//! is about four hundred tokens re-read on every request, and a plan longer than
//! that is one nobody, model or operator, is holding in their head.
//!
//! The caps live in the schema, not in this file's prose, so an oversized call
//! is refused by the validator the registry compiled before `execute` is
//! reached. The one rule a schema cannot state — at most one task in `doing` —
//! is checked here.
//!
//! Where the list is written is not this tool's business. [`TaskPort`] arrives
//! already bound to the turn's session, so a model cannot write its plan onto
//! another conversation, and a subagent writes to its own.

use darkwire_core::Result;
use darkwire_protocol::tasks::{TaskItem, TaskStatus, task_counts};
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::builtin::built;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

/// Where one task has got to, as the schema spells it.
///
/// A local enum rather than the protocol's [`TaskStatus`], and the reason is
/// the *emitted* schema rather than the type. Deriving from the protocol type
/// would carry its variant doc comments into the argument schema as three
/// per-variant descriptions, which is a `oneOf` of three objects where this is
/// a flat `enum` of three words — paid on every request of every turn, to say
/// what the field's own one-line description already says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Status {
    Todo,
    Doing,
    Done,
}

impl From<Status> for TaskStatus {
    fn from(status: Status) -> TaskStatus {
        match status {
            Status::Todo => TaskStatus::Todo,
            Status::Doing => TaskStatus::Doing,
            Status::Done => TaskStatus::Done,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Task {
    #[schemars(
        length(min = 1, max = 100),
        description = "One step, in a few words. \"Add the migration\", not a paragraph."
    )]
    text: String,
    #[schemars(description = "todo, doing or done. At most one task may be doing.")]
    status: Status,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TodoArgs {
    #[schemars(
        length(max = 10),
        description = "The whole list, in order. It replaces what is there, so send every \
                       task each time, not only the ones that changed. An empty list clears it."
    )]
    tasks: Vec<Task>,
}

struct Todo;

impl ToolHandler for Todo {
    type Args = TodoArgs;

    fn execute<'a>(
        &'a self,
        args: TodoArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "todo")?;

            let Some(port) = ctx.tasks.as_ref() else {
                return Ok(ToolOutput::error(
                    "There is no conversation to track tasks on.",
                ));
            };

            let tasks: Vec<TaskItem> = args
                .tasks
                .into_iter()
                .map(|task| TaskItem {
                    text: task.text,
                    status: task.status.into(),
                })
                .collect();

            // The one rule the schema cannot state. Refused rather than
            // corrected: which of the two the model meant is exactly what it
            // failed to say, and picking one silently would show an operator a
            // plan nobody wrote.
            let (done, doing, todo) = task_counts(&tasks);
            if doing > 1 {
                return Ok(ToolOutput::error(format!(
                    "{doing} tasks are doing. At most one may be; the rest are todo or done."
                )));
            }

            port.replace(&tasks)?;

            // One line, not the list. A tool result sits in the history for the
            // life of the session and is re-sent on every request after it,
            // while the list itself is already in the prompt on every
            // iteration — echoing it here would pay for the same text twice.
            let text = if tasks.is_empty() {
                "Task list cleared.".to_owned()
            } else {
                format!(
                    "{} tasks: {done} done, {doing} doing, {todo} to do.",
                    tasks.len()
                )
            };
            Ok(ToolOutput::text(text)
                .with_detail("count", tasks.len() as u64)
                .with_detail("done", done as u64)
                .with_detail("doing", doing as u64))
        })
    }
}

/// The `todo` tool.
pub fn todo_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "todo",
            "For tasks requiring 3+ steps, first create a `todo` list.
Send the complete list on each update. Update after major progress.
Only one task may be doing.",
        )
        // It writes no workspace file and runs nothing. `tool_search` is the
        // precedent: a tool that moves session state without touching the disk
        // the jail guards is still safe to run unattended.
        .risk(ToolRisk::Safe)
        .annotations(ToolAnnotations {
            title: Some("Track tasks".to_owned()),
            // Deliberately not `read_only_hint`. Dispatch runs adjacent
            // read-only calls concurrently, and two `todo` calls in one batch
            // must land in the order the model wrote them or the later list
            // loses.
            read_only_hint: Some(false),
            // The same list twice is the same state.
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        Todo,
    ))
}
