//! `kill`: stopping a background job through the session's port.

use std::sync::Arc;

use darkwire_tools::testkit::TestWorkspace;
use darkwire_tools::{BackgroundPort, BackgroundRefusal, StartJob, ToolContext, kill_tool};
use parking_lot::Mutex;
use serde_json::json;

#[derive(Default)]
struct Jobs {
    killed: Mutex<Vec<String>>,
}

impl BackgroundPort for Jobs {
    fn start(&self, _job: StartJob) -> Result<String, BackgroundRefusal> {
        Err(BackgroundRefusal::Off)
    }

    fn kill(&self, id: &str) -> Result<(), BackgroundRefusal> {
        if id != "1" {
            return Err(BackgroundRefusal::Unknown);
        }
        self.killed.lock().push(id.to_owned());
        Ok(())
    }
}

fn context(ws: &TestWorkspace, jobs: &Arc<Jobs>) -> ToolContext {
    let mut ctx = ws.context().clone();
    ctx.background = Some(Arc::clone(jobs) as Arc<dyn BackgroundPort>);
    ctx
}

#[tokio::test]
async fn stops_a_running_job_by_its_id() {
    let ws = TestWorkspace::new();
    let jobs = Arc::new(Jobs::default());

    let result = kill_tool()
        .execute(json!({"id": " 1 "}), &context(&ws, &jobs))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert_eq!(*jobs.killed.lock(), vec!["1".to_owned()]);
}

#[tokio::test]
async fn an_unknown_id_is_an_answer_not_a_failure() {
    let ws = TestWorkspace::new();
    let jobs = Arc::new(Jobs::default());

    let result = kill_tool()
        .execute(json!({"id": "9"}), &context(&ws, &jobs))
        .await;

    assert!(result.is_error);
    assert!(result.kind.is_none());
    assert_eq!(result.content, "No running job with that id.");
}

#[tokio::test]
async fn with_nowhere_to_reach_jobs_it_says_so() {
    let ws = TestWorkspace::new();

    let result = kill_tool().execute(json!({"id": "1"}), ws.context()).await;

    assert!(result.is_error);
    assert!(
        result.content.contains("not available"),
        "{}",
        result.content
    );
}
