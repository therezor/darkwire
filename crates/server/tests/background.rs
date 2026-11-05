//! Background jobs: started from a turn, reported back into its session.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::messages::Content;
use darkwire_core::session_store::CreateSession;
use darkwire_core::{ErrorKind, Result, SystemClock, WireError};
use darkwire_protocol::automation::AUTOMATION_ORIGIN;
use darkwire_protocol::config::EnvironmentNetwork;
use darkwire_protocol::messages::DeliveredStatus;
use darkwire_protocol::subagent::SUBAGENT_ORIGIN;
use darkwire_protocol::ws::ServerMessage;
use darkwire_security::ExecPlan;
use darkwire_tools::BoxFuture;
use darkwire_tools::background::{BackgroundPort, BackgroundRefusal, BackgroundResolver, StartJob};
use darkwire_tools::runner::{CommandRunner, PlacementRequest, RunOutcome, RunRequest};
use indexmap::IndexMap;
use serde_json::json;
use tokio::sync::Notify;

use crate::hub::{Harness, HarnessOptions, SESSION, harness, until_turns};

/// Holds every command until released, and honours a stop.
struct HeldRunner {
    release: Notify,
    outcome: RunOutcome,
}

impl HeldRunner {
    fn printing(stdout: &str) -> Arc<HeldRunner> {
        Arc::new(HeldRunner {
            release: Notify::new(),
            outcome: RunOutcome {
                stdout: stdout.to_owned(),
                code: Some(0),
                ..RunOutcome::default()
            },
        })
    }
}

impl CommandRunner for HeldRunner {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        Box::pin(async move {
            tokio::select! {
                () = request.token.cancelled() => Err(WireError::new(ErrorKind::Aborted, "stopped")),
                () = self.release.notified() => Ok(self.outcome.clone()),
            }
        })
    }
}

fn with_jobs() -> Harness {
    let h = harness(&HarnessOptions {
        background: true,
        ..HarnessOptions::default()
    });
    h.store
        .ensure_session(SESSION, CreateSession::default())
        .unwrap();
    h
}

fn port(h: &Harness, session_key: &str) -> Arc<dyn BackgroundPort> {
    h.jobs
        .as_ref()
        .unwrap()
        .for_turn(&PlacementRequest {
            agent_id: "default".to_owned(),
            workspace_id: "default".to_owned(),
            session_key: session_key.to_owned(),
            environment: String::new(),
            network: EnvironmentNetwork::default(),
            workspace_root: "/tmp".to_owned(),
        })
        .unwrap()
}

fn job(runner: &Arc<HeldRunner>, limit: u64) -> StartJob {
    StartJob {
        argv: vec!["make".to_owned(), "test".to_owned()],
        plan: ExecPlan {
            file: "make".to_owned(),
            args: vec!["test".to_owned()],
            cwd: PathBuf::from("/tmp"),
            env: IndexMap::new(),
            timeout_ms: 0,
            max_output_bytes: 1024,
        },
        timeout_ms: 0,
        runner: Arc::clone(runner) as Arc<dyn CommandRunner>,
        clock: Arc::new(SystemClock),
        limit,
        max_output_chars: 8192,
    }
}

fn running(h: &Harness) -> usize {
    h.jobs.as_ref().unwrap().running(SESSION).len()
}

/// What the woken turn was asked.
fn posted(h: &Harness, index: usize) -> (String, DeliveredStatus) {
    match h.runner.turn(index).input.content.clone() {
        Content::Delivered(text, from) => (text, from.status),
        other => panic!("expected a posted result, got {other:?}"),
    }
}

async fn until_idle(h: &Harness) {
    for _ in 0..500 {
        if running(h) == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("a job never ended");
}

#[tokio::test]
async fn a_finished_job_posts_its_output_and_wakes_the_session() {
    let h = with_jobs();
    let runner = HeldRunner::printing("built");

    let id = port(&h, SESSION).start(job(&runner, 4)).unwrap();
    assert_eq!(id, "1");
    assert_eq!(running(&h), 1);

    runner.release.notify_one();
    until_turns(&h, 1).await;

    let (text, status) = posted(&h, 0);
    assert_eq!(status, DeliveredStatus::Ok);
    assert!(
        text.starts_with("[Background job 1 finished: make test]\n\nbuilt"),
        "{text}"
    );
    assert_eq!(running(&h), 0);
}

#[tokio::test]
async fn a_job_past_the_agent_cap_is_refused() {
    let h = with_jobs();
    let runner = HeldRunner::printing("");
    let port = port(&h, SESSION);

    port.start(job(&runner, 1)).unwrap();

    assert_eq!(
        port.start(job(&runner, 1)),
        Err(BackgroundRefusal::AtCapacity)
    );
    assert_eq!(port.start(job(&runner, 0)), Err(BackgroundRefusal::Off));
}

#[tokio::test]
async fn killing_a_job_posts_that_it_was_stopped() {
    let h = with_jobs();
    let runner = HeldRunner::printing("never seen");
    let port = port(&h, SESSION);
    port.start(job(&runner, 4)).unwrap();

    assert_eq!(port.kill("9"), Err(BackgroundRefusal::Unknown));
    port.kill("1").unwrap();
    until_turns(&h, 1).await;

    let (text, status) = posted(&h, 0);
    assert_eq!(status, DeliveredStatus::Killed);
    assert_eq!(text, "[Background job 1 was stopped: make test]");
}

#[tokio::test]
async fn a_subagent_or_a_scheduled_run_may_not_start_one() {
    let h = with_jobs();
    let runner = HeldRunner::printing("");
    for (key, origin) in [("child", SUBAGENT_ORIGIN), ("run", AUTOMATION_ORIGIN)] {
        h.store
            .ensure_session(
                key,
                CreateSession {
                    origin: Some(origin.to_owned()),
                    ..CreateSession::default()
                },
            )
            .unwrap();
        assert_eq!(
            port(&h, key).start(job(&runner, 4)),
            Err(BackgroundRefusal::NotHere),
            "{origin}"
        );
    }
}

#[tokio::test]
async fn closing_the_server_stops_jobs_without_waking_anyone() {
    let h = with_jobs();
    let runner = HeldRunner::printing("");
    port(&h, SESSION).start(job(&runner, 4)).unwrap();

    h.hub.close();
    until_idle(&h).await;

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(h.runner.count(), 0);
}

#[tokio::test]
async fn deleting_a_session_stops_its_jobs_and_their_results_go_nowhere() {
    let h = with_jobs();
    let runner = HeldRunner::printing("");
    port(&h, SESSION).start(job(&runner, 4)).unwrap();

    h.store.delete_session(SESSION).unwrap();
    h.hub.session_deleted(SESSION);
    until_idle(&h).await;

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(h.runner.count(), 0);
}

#[tokio::test]
async fn the_status_frame_lists_running_jobs_and_a_stop_frame_ends_one() {
    let h = with_jobs();
    let client = h.plain();
    client.until_seen("connected").await;
    let runner = HeldRunner::printing("");

    port(&h, SESSION).start(job(&runner, 4)).unwrap();
    client
        .until("a status listing the job", |frames| {
            frames.iter().any(|frame| {
                matches!(frame, ServerMessage::SessionStatus(status)
                    if status.event.jobs.iter().any(|job| job.id == "1" && job.label == "make test"))
            })
        })
        .await;

    client.send(json!({ "type": "job.stop", "sessionKey": SESSION, "id": "1" }));
    until_turns(&h, 1).await;
    assert_eq!(posted(&h, 0).1, DeliveredStatus::Killed);
}

#[tokio::test]
async fn a_reconnecting_tab_is_told_about_running_jobs() {
    let h = with_jobs();
    let runner = HeldRunner::printing("");
    port(&h, SESSION).start(job(&runner, 4)).unwrap();

    let client = h.plain();
    client
        .until("a status listing the job", |frames| {
            frames.iter().any(|frame| {
                matches!(frame, ServerMessage::SessionStatus(status) if status.event.jobs.len() == 1)
            })
        })
        .await;
}
