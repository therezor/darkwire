//! `todo` — the whole list replaced on each call, and the caps that bound it.

use std::sync::Arc;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::tasks::TaskItem;
use darkwire_tools::testkit::TestWorkspace;
use darkwire_tools::{TaskPort, ToolContext, ToolExecution, todo_tool};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// A port that remembers the last list it was handed, or refuses.
#[derive(Default)]
struct Stub {
    written: Mutex<Option<Vec<TaskItem>>>,
    fails: bool,
}

impl TaskPort for Stub {
    fn replace(&self, tasks: &[TaskItem]) -> Result<()> {
        if self.fails {
            return Err(WireError::new(ErrorKind::Internal, "the store said no"));
        }
        *self.written.lock() = Some(tasks.to_vec());
        Ok(())
    }
}

fn context_with(ws: &TestWorkspace, port: Option<Arc<Stub>>) -> ToolContext {
    let mut ctx = ws.context().clone();
    ctx.tasks = port.map(|port| port as Arc<dyn TaskPort>);
    ctx
}

async fn run(args: Value, port: Option<Arc<Stub>>) -> ToolExecution {
    let ws = TestWorkspace::new();
    todo_tool().execute(args, &context_with(&ws, port)).await
}

fn three() -> Value {
    json!({"tasks": [
        {"text": "Inspect auth", "status": "done"},
        {"text": "Update sessions", "status": "doing"},
        {"text": "Add tests", "status": "todo"},
    ]})
}

fn many(count: usize) -> Value {
    let tasks: Vec<Value> = (0..count)
        .map(|index| json!({"text": format!("task {index}"), "status": "todo"}))
        .collect();
    json!({ "tasks": tasks })
}

#[tokio::test]
async fn writes_the_whole_list_and_counts_it_back() {
    let port = Arc::new(Stub::default());
    let execution = run(three(), Some(Arc::clone(&port))).await;

    assert!(!execution.is_error, "{}", execution.content);
    assert_eq!(execution.content, "3 tasks: 1 done, 1 doing, 1 to do.");

    let written = port.written.lock().clone().expect("the port was written");
    assert_eq!(written.len(), 3);
    assert_eq!(written[1].text, "Update sessions");
}

/// The result is a sentence, not the list. A tool result is replayed into every
/// later request, and the list is already in the prompt on every iteration.
#[tokio::test]
async fn does_not_echo_the_list_back_to_the_model() {
    let execution = run(three(), Some(Arc::new(Stub::default()))).await;
    assert!(!execution.content.contains("Inspect auth"));
}

#[tokio::test]
async fn an_empty_list_clears_it() {
    let port = Arc::new(Stub::default());
    let execution = run(json!({"tasks": []}), Some(Arc::clone(&port))).await;

    assert!(!execution.is_error);
    assert_eq!(execution.content, "Task list cleared.");
    assert_eq!(port.written.lock().clone(), Some(Vec::new()));
}

#[tokio::test]
async fn refuses_a_second_task_in_hand() {
    let port = Arc::new(Stub::default());
    let execution = run(
        json!({"tasks": [
            {"text": "one", "status": "doing"},
            {"text": "two", "status": "doing"},
        ]}),
        Some(Arc::clone(&port)),
    )
    .await;

    assert!(execution.is_error);
    assert!(execution.content.contains("At most one"));
    // Refused rather than corrected, so nothing reached the store.
    assert_eq!(*port.written.lock(), None);
}

#[tokio::test]
async fn takes_the_cap_and_refuses_one_over_it() {
    assert!(
        !run(many(10), Some(Arc::new(Stub::default())))
            .await
            .is_error
    );

    let execution = run(many(11), Some(Arc::new(Stub::default()))).await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn refuses_a_task_longer_than_the_cap() {
    let execution = run(
        json!({"tasks": [{"text": "x".repeat(101), "status": "todo"}]}),
        Some(Arc::new(Stub::default())),
    )
    .await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::InvalidInput));

    let ok = run(
        json!({"tasks": [{"text": "x".repeat(100), "status": "todo"}]}),
        Some(Arc::new(Stub::default())),
    )
    .await;
    assert!(!ok.is_error, "{}", ok.content);
}

#[tokio::test]
async fn refuses_an_empty_task() {
    let execution = run(
        json!({"tasks": [{"text": "", "status": "todo"}]}),
        Some(Arc::new(Stub::default())),
    )
    .await;
    assert!(execution.is_error);
}

#[tokio::test]
async fn refuses_a_status_it_does_not_know() {
    let execution = run(
        json!({"tasks": [{"text": "one", "status": "blocked"}]}),
        Some(Arc::new(Stub::default())),
    )
    .await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn refuses_an_unknown_argument() {
    let execution = run(
        json!({"tasks": [], "replace": true}),
        Some(Arc::new(Stub::default())),
    )
    .await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::InvalidInput));
}

/// A call with nothing behind it says so rather than reporting a write that
/// went nowhere.
#[tokio::test]
async fn says_so_when_there_is_no_session_to_write_to() {
    let execution = run(three(), None).await;
    assert!(execution.is_error);
    assert!(execution.content.contains("no conversation"));
}

#[tokio::test]
async fn reports_a_store_that_refused() {
    let port = Arc::new(Stub {
        fails: true,
        ..Stub::default()
    });
    let execution = run(three(), Some(port)).await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::Internal));
}

#[tokio::test]
async fn notices_a_cancelled_turn_before_writing() {
    let ws = TestWorkspace::new();
    let port = Arc::new(Stub::default());
    ws.token().cancel();

    let execution = todo_tool()
        .execute(three(), &context_with(&ws, Some(Arc::clone(&port))))
        .await;

    assert_eq!(execution.kind, Some(ErrorKind::Aborted));
    assert_eq!(*port.written.lock(), None);
}
