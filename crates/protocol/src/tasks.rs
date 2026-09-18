//! The task list a session carries, and how it is spelled in a prompt.
//!
//! One tool replaces the whole list on each call, so there is no patch format
//! and no per-task id: the model sends what the list should now be, and the
//! last call wins. That is the whole of the write contract, and it is why a
//! task is two fields.
//!
//! It lives here rather than beside the tool for the reason
//! [`subagent`](crate::subagent) does: six layers read it and none of them can
//! reach the others. The tool writes it, the store holds it, the prompt renders
//! it, the terminal draws it, a chat command prints it and the browser fetches
//! it over REST. A constant duplicated across that span eventually differs in
//! one of them.
//!
//! **The list is in the session's metadata bag, not a column.** It costs no
//! schema, no index and no query surface, and nothing searches by it. A
//! subagent opens its own session, so keying by session is what gives a
//! delegated run its own list for free.
//!
//! ## Why the list carries a sequence number
//!
//! A plan is written *during* a turn, about the work that turn is doing. Re-run
//! that turn — `/regenerate`, or `/edit` on the message that started it — and
//! the answers it produced are dropped, so a plan describing them is describing
//! messages that are no longer there. The next turn would then open reading a
//! half-ticked list for work the transcript has no record of.
//!
//! So the list records `seq`: the sequence number the *next* message would have
//! taken when it was written, which places it immediately before everything the
//! turn went on to append. A truncation to `cut` drops the list when
//! `seq > cut`, and keeps it otherwise, which is the same comparison the
//! messages themselves are cut by.

use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::json::Object;

/// Where a session records its task list.
pub const TASKS_METADATA_KEY: &str = "tasks";

/// The list, and the point in the conversation it was written at.
///
/// One key holding both rather than a list beside a number, because the two are
/// only meaningful together: a list with no position cannot be cut, and a
/// position with no list is a row nothing reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskList {
    /// The seq the next message would have taken when this was written. Zero on
    /// a list written before this was recorded, which no truncation drops.
    pub seq: i64,
    /// The tasks, in the order the model wrote them.
    pub items: Vec<TaskItem>,
}

/// How many tasks a list may hold.
///
/// Ten, because the list is re-read on every iteration of every turn and a plan
/// nobody can hold in their head is not a plan. Work that needs more of them
/// needs a smaller first task.
pub const MAX_TASKS: usize = 10;

/// How long one task's text may be, in UTF-16 code units.
pub const MAX_TASK_CHARS: usize = 100;

/// Where one task has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Not started.
    #[default]
    Todo,
    /// In hand. At most one task may be here.
    Doing,
    /// Finished.
    Done,
}

impl TaskStatus {
    /// The marker a rendered list puts in front of the text.
    pub fn marker(self) -> &'static str {
        match self {
            TaskStatus::Todo => "[ ]",
            TaskStatus::Doing => "[>]",
            TaskStatus::Done => "[x]",
        }
    }

    /// The wire spelling, for a reader that has a string and not a value.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Todo => "todo",
            TaskStatus::Doing => "doing",
            TaskStatus::Done => "done",
        }
    }

    /// The value behind a wire spelling. `None` for anything else.
    pub fn parse(value: &str) -> Option<TaskStatus> {
        match value {
            "todo" => Some(TaskStatus::Todo),
            "doing" => Some(TaskStatus::Doing),
            "done" => Some(TaskStatus::Done),
            _ => None,
        }
    }
}

/// One line of the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct TaskItem {
    /// What the task is, in the model's own words.
    #[garde(length(utf16, min = 1, max = 100))]
    pub text: String,
    /// Where it has got to.
    pub status: TaskStatus,
}

/// The list and its position, read out of a session's metadata.
///
/// Tolerant of anything that is not the expected shape, exactly as
/// [`subagent_runs_of`](crate::subagent::subagent_runs_of) is: the bag is
/// untyped storage that other things also write, and one malformed entry must
/// not stop a panel rendering. An entry with no usable text is dropped; an
/// unknown status reads as `todo`, because a task nobody can classify is still
/// a task somebody wrote.
///
/// The cap is applied on the way out as well as on the way in. A bag written by
/// hand is not bound by the tool's schema, and a prompt section is not the place
/// to discover that.
pub fn task_list_of(metadata: &Object) -> TaskList {
    let Some(Value::Object(raw)) = metadata.get(TASKS_METADATA_KEY) else {
        return TaskList::default();
    };
    let seq = raw.get("seq").and_then(Value::as_i64).unwrap_or(0).max(0);
    let Some(Value::Array(items)) = raw.get("items") else {
        return TaskList {
            seq,
            items: Vec::new(),
        };
    };
    TaskList {
        seq,
        items: read_items(items),
    }
}

/// Just the tasks, for the readers that have no use for the position.
pub fn tasks_of(metadata: &Object) -> Vec<TaskItem> {
    task_list_of(metadata).items
}

fn read_items(raw: &[Value]) -> Vec<TaskItem> {
    raw.iter()
        .filter_map(|value| {
            let entry = value.as_object()?;
            let text = entry.get("text").and_then(Value::as_str)?;
            if text.is_empty() {
                return None;
            }
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .and_then(TaskStatus::parse)
                .unwrap_or_default();
            Some(TaskItem {
                text: text.to_owned(),
                status,
            })
        })
        .take(MAX_TASKS)
        .collect()
}

/// The whole metadata bag, with this list in place of whatever was there.
///
/// An empty list removes the key rather than storing an empty one, so a cleared
/// list and a session that never had one are the same row.
pub fn with_tasks(metadata: &Object, seq: i64, tasks: &[TaskItem]) -> Object {
    let mut next = metadata.clone();
    if tasks.is_empty() {
        next.shift_remove(TASKS_METADATA_KEY);
        return next;
    }
    let items = tasks
        .iter()
        .take(MAX_TASKS)
        .map(|task| serde_json::to_value(task).unwrap_or(Value::Null))
        .collect::<Vec<Value>>();
    next.insert(
        TASKS_METADATA_KEY.to_owned(),
        json!({ "seq": seq.max(0), "items": items }),
    );
    next
}

/// The list as the model reads it back, one task per line.
///
/// No heading: the section that places it decides how it is introduced, and a
/// heading baked in here would be one the terminal and a chat command have to
/// strip. Empty renders as the empty string, never as a blank line.
pub fn render_tasks(tasks: &[TaskItem]) -> String {
    tasks
        .iter()
        .map(|task| format!("{} {}", task.status.marker(), task.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// How many tasks are in each state, in list order: done, doing, to do.
///
/// Here rather than at the one call site because both the tool's result line and
/// a chat command's summary count the same three things, and a second count that
/// bucketed them differently would be a second answer to one question.
pub fn task_counts(tasks: &[TaskItem]) -> (usize, usize, usize) {
    let mut counts = (0, 0, 0);
    for task in tasks {
        match task.status {
            TaskStatus::Done => counts.0 += 1,
            TaskStatus::Doing => counts.1 += 1,
            TaskStatus::Todo => counts.2 += 1,
        }
    }
    counts
}
