//! The automation routes, over a real store and a stand-in engine.
//!
//! No timer runs here. `SchedulerPort` is narrow enough that a test supplies a
//! three-method double, which is the whole reason the route bag holds the port
//! rather than the engine.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use darkwire_core::{Clock, ErrorKind, Result, WireError};
use darkwire_protocol::automation::{AutomationRun, RunStatus};
use darkwire_protocol::config::Config;
use darkwire_server::automation_store::{AutomationStore, FinishRunInput};
use darkwire_server::scheduler::SchedulerPort;
use darkwire_server::testkit::{TestServer, TestServerOptions, start_test_server};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt as _;

// Fixtures

/// A job with no timezone of its own: the install's `ui.timezone` is the one
/// clock every expression is read against.
fn cron_body() -> Value {
    json!({
        "name": "Morning",
        "schedule": {"kind": "cron", "expr": "0 9 * * *"},
        "payload": {"kind": "scheduled", "message": "check the build"},
    })
}

/// An engine that records what it was asked, and answers instantly.
struct FakeScheduler {
    enabled: bool,
    /// Every job id a run was forced for, in order.
    ran: Mutex<Vec<String>>,
    /// How many times the routes told it to re-read.
    refreshes: Mutex<usize>,
    /// When set, every forced run fails with this kind and message.
    fails: Option<(ErrorKind, String)>,
}

impl FakeScheduler {
    fn new() -> Arc<FakeScheduler> {
        Arc::new(FakeScheduler {
            enabled: true,
            ran: Mutex::new(Vec::new()),
            refreshes: Mutex::new(0),
            fails: None,
        })
    }

    fn disabled() -> Arc<FakeScheduler> {
        Arc::new(FakeScheduler {
            enabled: false,
            ran: Mutex::new(Vec::new()),
            refreshes: Mutex::new(0),
            fails: None,
        })
    }

    fn failing(kind: ErrorKind, message: &str) -> Arc<FakeScheduler> {
        Arc::new(FakeScheduler {
            enabled: true,
            ran: Mutex::new(Vec::new()),
            refreshes: Mutex::new(0),
            fails: Some((kind, message.to_owned())),
        })
    }
}

impl SchedulerPort for FakeScheduler {
    fn run_now(&self, job_id: &str) -> Result<AutomationRun> {
        self.ran.lock().push(job_id.to_owned());
        if let Some((kind, message)) = &self.fails {
            return Err(WireError::new(*kind, message.clone()));
        }
        Ok(AutomationRun {
            id: format!("run-{job_id}"),
            job_id: job_id.to_owned(),
            started_at_ms: 1,
            finished_at_ms: None,
            status: RunStatus::Pending,
            skip_reason: None,
            error: None,
            output: None,
            session_key: None,
            warnings: Vec::new(),
        })
    }

    fn refresh(&self) {
        *self.refreshes.lock() += 1;
    }

    fn enabled(&self) -> bool {
        self.enabled
    }
}

/// A server with no engine at all, which is the default this build ships.
fn server() -> TestServer {
    start_test_server(TestServerOptions::default()).expect("a test server")
}

/// A server with `config` and no engine.
fn server_with(config: Config) -> TestServer {
    start_test_server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    })
    .expect("a test server")
}

/// A server over one of the engine doubles above.
fn server_with_engine(scheduler: Arc<FakeScheduler>) -> TestServer {
    start_test_server(TestServerOptions {
        scheduler: Some(scheduler as Arc<dyn SchedulerPort>),
        ..TestServerOptions::default()
    })
    .expect("a test server")
}

// Driving the router

struct Answer {
    status: StatusCode,
    body: Value,
}

impl Answer {
    fn json(&self) -> &Value {
        &self.body
    }
}

async fn send(test: &TestServer, method: Method, uri: &str, body: Option<Value>) -> Answer {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {}", test.token))
        .header("content-type", "application/json")
        .body(match &body {
            Some(value) => Body::from(value.to_string()),
            None => Body::empty(),
        })
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("a body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer { status, body }
}

async fn create_job(test: &TestServer) -> Value {
    let answer = send(
        test,
        Method::POST,
        "/api/automation/jobs",
        Some(cron_body()),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CREATED, "{:?}", answer.body);
    answer.body
}

// CRUD

#[tokio::test]
async fn creating_a_job_answers_201_with_the_row_and_its_computed_next_run() {
    let test = server();
    let job = create_job(&test).await;

    assert_eq!(job["name"], "Morning");
    assert_eq!(job["enabled"], true);
    // The next fire is computed at save time by asking the same arithmetic the
    // engine uses, so an expression the timer could not honour cannot be saved.
    assert!(
        job["state"]["nextRunAtMs"].as_u64().unwrap_or(0) > 0,
        "{job}"
    );
}

#[tokio::test]
async fn the_listing_carries_what_was_created() {
    let test = server();
    let job = create_job(&test).await;

    let answer = send(&test, Method::GET, "/api/automation/jobs", None).await;
    assert_eq!(answer.status, StatusCode::OK);
    let jobs = answer.json()["jobs"].as_array().expect("an array");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["id"], job["id"]);
}

#[tokio::test]
async fn one_job_reads_back_by_id_so_a_deep_link_resolves() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::GET,
        &format!("/api/automation/jobs/{id}"),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["name"], "Morning");
}

#[tokio::test]
async fn a_patch_changes_only_what_the_body_names() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::PATCH,
        &format!("/api/automation/jobs/{id}"),
        Some(json!({"name": "Evening"})),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["name"], "Evening");
    assert_eq!(answer.json()["schedule"], job["schedule"]);
    // Untouched: neither half of "when does this fire" moved.
    assert_eq!(
        answer.json()["state"]["nextRunAtMs"],
        job["state"]["nextRunAtMs"]
    );
}

#[tokio::test]
async fn the_next_run_is_recomputed_when_the_schedule_changes() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    // Leaving the old instant behind is how a job edited to run at 9am keeps
    // firing at 3am until it happens to be restarted.
    let answer = send(
        &test,
        Method::PATCH,
        &format!("/api/automation/jobs/{id}"),
        Some(json!({"schedule": {"kind": "cron", "expr": "0 21 * * *"}})),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_ne!(
        answer.json()["state"]["nextRunAtMs"],
        job["state"]["nextRunAtMs"]
    );
}

#[tokio::test]
async fn switching_a_job_off_unschedules_it_and_switching_it_on_reschedules_it() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let off = send(
        &test,
        Method::PATCH,
        &format!("/api/automation/jobs/{id}"),
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(off.status, StatusCode::OK);
    // Zero means unscheduled, which is what the partial index is keyed on.
    assert_eq!(off.json()["state"]["nextRunAtMs"], 0);

    let on = send(
        &test,
        Method::PATCH,
        &format!("/api/automation/jobs/{id}"),
        Some(json!({"enabled": true})),
    )
    .await;
    assert!(on.json()["state"]["nextRunAtMs"].as_u64().unwrap_or(0) > 0);
}

#[tokio::test]
async fn deleting_a_job_answers_204_and_the_job_is_gone() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::DELETE,
        &format!("/api/automation/jobs/{id}"),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);

    let after = send(
        &test,
        Method::GET,
        &format!("/api/automation/jobs/{id}"),
        None,
    )
    .await;
    assert_eq!(after.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn every_route_naming_a_job_that_is_not_there_answers_404() {
    let test = server();
    for (method, uri, body) in [
        (Method::GET, "/api/automation/jobs/ghost", None),
        (
            Method::PATCH,
            "/api/automation/jobs/ghost",
            Some(json!({"name": "x"})),
        ),
        (Method::DELETE, "/api/automation/jobs/ghost", None),
        (Method::POST, "/api/automation/jobs/ghost/run", None),
        (Method::GET, "/api/automation/jobs/ghost/runs", None),
    ] {
        let answer = send(&test, method.clone(), uri, body).await;
        assert_eq!(answer.status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
}

// Workspaces

#[tokio::test]
async fn a_payload_naming_a_workspace_that_exists_round_trips() {
    let test = server();
    let answer = send(
        &test,
        Method::POST,
        "/api/automation/jobs",
        Some(json!({
            "name": "Morning",
            "schedule": {"kind": "cron", "expr": "0 9 * * *"},
            "payload": {
                "kind": "scheduled",
                "message": "check the build",
                "workspaceId": "default",
            },
        })),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CREATED, "{:?}", answer.body);
    assert_eq!(answer.json()["payload"]["workspaceId"], "default");
}

#[tokio::test]
async fn a_create_naming_a_workspace_nothing_can_list_is_a_404() {
    // Checked at authoring time: the alternative is a heartbeat that fails
    // every interval forever, reported as a run failure rather than as the
    // typo it is.
    let test = server();
    let answer = send(
        &test,
        Method::POST,
        "/api/automation/jobs",
        Some(json!({
            "name": "Morning",
            "schedule": {"kind": "cron", "expr": "0 9 * * *"},
            "payload": {
                "kind": "scheduled",
                "message": "check the build",
                "workspaceId": "not-a-workspace",
            },
        })),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_update_naming_a_workspace_nothing_can_list_is_a_404() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::PATCH,
        &format!("/api/automation/jobs/{id}"),
        Some(json!({
            "payload": {
                "kind": "scheduled",
                "message": "check the build",
                "workspaceId": "not-a-workspace",
            },
        })),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

// Validation

#[tokio::test]
async fn a_cron_expression_it_cannot_honour_is_a_422_naming_the_field() {
    // Parsing one fails with `config`, whose default mapping is a 500 — right
    // for "this install is broken", wrong for "the operator typed it".
    let test = server();
    let answer = send(
        &test,
        Method::POST,
        "/api/automation/jobs",
        Some(json!({
            "name": "Morning",
            "schedule": {"kind": "cron", "expr": "not a cron"},
            "payload": {"kind": "scheduled", "message": "check the build"},
        })),
    )
    .await;

    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        answer.json()["error"]["details"]["/schedule"].is_string(),
        "{:?}",
        answer.body
    );
}

#[tokio::test]
async fn an_unknown_install_timezone_is_a_422_at_save_rather_than_at_first_fire() {
    // The zone is a bare string — an enum would have to enumerate the IANA
    // database — so an unusable one gets as far as the cron parser. Surfacing
    // it on the save is the point: the alternative is a job that looks
    // scheduled and silently never fires.
    let mut config = Config::default();
    config.ui.timezone = "Mars/Base".to_owned();
    let test = server_with(config);

    let answer = send(
        &test,
        Method::POST,
        "/api/automation/jobs",
        Some(cron_body()),
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_per_job_timezone_is_refused_rather_than_quietly_dropped() {
    // A client written against the old shape gets a validation failure instead
    // of a job scheduled on a clock it did not ask for and cannot see.
    let test = server();
    let answer = send(
        &test,
        Method::POST,
        "/api/automation/jobs",
        Some(json!({
            "name": "Morning",
            "schedule": {"kind": "cron", "expr": "0 9 * * *", "tz": "Europe/Kyiv"},
            "payload": {"kind": "scheduled", "message": "check the build"},
        })),
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_schedule_carrying_a_field_from_a_different_kind_is_refused() {
    let test = server();
    let answer = send(
        &test,
        Method::POST,
        "/api/automation/jobs",
        Some(json!({
            "name": "Morning",
            "schedule": {"kind": "cron", "expr": "0 9 * * *", "atMs": 5},
            "payload": {"kind": "scheduled", "message": "check the build"},
        })),
    )
    .await;
    // 422 rather than 400: the body was well-formed JSON that failed its
    // schema.
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_patch_whose_new_schedule_does_not_parse_leaves_the_job_alone() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::PATCH,
        &format!("/api/automation/jobs/{id}"),
        Some(json!({"schedule": {"kind": "cron", "expr": "99 * * * *"}})),
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);

    let after = send(
        &test,
        Method::GET,
        &format!("/api/automation/jobs/{id}"),
        None,
    )
    .await;
    assert_eq!(after.json()["schedule"], job["schedule"]);
}

#[tokio::test]
async fn a_body_that_is_not_json_at_all_is_a_400() {
    let test = server();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/automation/jobs")
        .header("authorization", format!("Bearer {}", test.token))
        .header("content-type", "application/json")
        .body(Body::from("{ not json"))
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// Running on demand

#[tokio::test]
async fn a_forced_run_answers_202_with_the_pending_row_rather_than_waiting_out_the_turn() {
    let scheduler = FakeScheduler::new();
    let test = server_with_engine(Arc::clone(&scheduler));
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id").to_owned();

    let answer = send(
        &test,
        Method::POST,
        &format!("/api/automation/jobs/{id}/run"),
        None,
    )
    .await;

    assert_eq!(answer.status, StatusCode::ACCEPTED);
    assert_eq!(answer.json()["status"], "pending");
    assert_eq!(*scheduler.ran.lock(), vec![id]);
}

#[tokio::test]
async fn a_build_with_no_engine_at_all_refuses_a_forced_run() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::POST,
        &format!("/api/automation/jobs/{id}/run"),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_engine_switched_off_refuses_with_422_rather_than_404() {
    // The route exists and so does the job. The operator turned the engine off,
    // and saying which is what lets them turn it back on.
    let test = server_with_engine(FakeScheduler::disabled());
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::POST,
        &format!("/api/automation/jobs/{id}/run"),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("disabled"),
        "{:?}",
        answer.body
    );
}

#[tokio::test]
async fn a_job_that_is_already_running_answers_409() {
    let test = server_with_engine(FakeScheduler::failing(
        ErrorKind::Conflict,
        "\"Morning\" is already running.",
    ));
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::POST,
        &format!("/api/automation/jobs/{id}/run"),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
}

// Run history

/// The store behind the routes, reached over the same shared connection.
///
/// A second handle rather than a borrowed one: the routes hold theirs privately,
/// and `Database` is one re-entrant connection, so both see the same rows.
fn automation_store(test: &TestServer) -> AutomationStore {
    let counter = Arc::new(Mutex::new(0u64));
    AutomationStore::new(
        test.database.clone(),
        Arc::clone(&test.clock) as Arc<dyn Clock>,
        Box::new(move || {
            let mut next = counter.lock();
            *next += 1;
            format!("seeded-{next}")
        }),
    )
    .expect("the store")
}

/// Runs a millisecond apart, so the keyset order is not a tie.
fn seed_runs(
    test: &TestServer,
    store: &AutomationStore,
    job_id: &str,
    count: usize,
) -> Vec<String> {
    let mut ids = Vec::new();
    for _ in 0..count {
        test.clock.advance(Duration::from_millis(1));
        ids.push(store.start_run(job_id, None).expect("a run").id);
    }
    ids
}

#[tokio::test]
async fn run_history_pages_newest_first_over_a_keyset_cursor() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id").to_owned();
    let seeded = seed_runs(&test, &automation_store(&test), &id, 3);
    assert_eq!(seeded.len(), 3);

    let first = send(
        &test,
        Method::GET,
        &format!("/api/automation/jobs/{id}/runs?limit=2"),
        None,
    )
    .await;
    assert_eq!(first.status, StatusCode::OK);
    let runs = first.json()["runs"].as_array().expect("an array");
    assert_eq!(runs.len(), 2);
    assert_eq!(first.json()["total"], 3);
    let cursor = first.json()["nextCursor"]
        .as_str()
        .expect("a cursor for the next page")
        .to_owned();

    let second = send(
        &test,
        Method::GET,
        &format!("/api/automation/jobs/{id}/runs?limit=2&cursor={cursor}"),
        None,
    )
    .await;
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(second.json()["runs"].as_array().expect("an array").len(), 1);
    // The last page issues none: there is nothing after it to address.
    assert!(second.json()["nextCursor"].is_null());
}

#[tokio::test]
async fn a_cursor_this_server_did_not_issue_is_a_400() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::GET,
        &format!("/api/automation/jobs/{id}/runs?cursor=not-a-cursor"),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn run_history_pages_over_an_offset_and_reports_the_whole_history() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id").to_owned();
    seed_runs(&test, &automation_store(&test), &id, 3);

    let answer = send(
        &test,
        Method::GET,
        &format!("/api/automation/jobs/{id}/runs?limit=1&offset=1"),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["runs"].as_array().expect("an array").len(), 1);
    assert_eq!(answer.json()["total"], 3);
}

#[tokio::test]
async fn a_run_listing_naming_both_paging_modes_is_refused() {
    // A page relative to a page is not a thing to ask for, and a precedence
    // rule would mean one parameter is silently ignored — which looks exactly
    // like a server that paged wrongly.
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::GET,
        &format!("/api/automation/jobs/{id}/runs?cursor=abc&offset=1"),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_limit_past_the_cap_is_a_422() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id");

    let answer = send(
        &test,
        Method::GET,
        &format!("/api/automation/jobs/{id}/runs?limit=500"),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_runs_warnings_come_through_so_a_caveat_is_visible_without_being_a_failure() {
    let test = server();
    let job = create_job(&test).await;
    let id = job["id"].as_str().expect("an id").to_owned();

    let store = automation_store(&test);
    let started = store.start_run(&id, None).expect("a run");
    store
        .finish_run(
            &started.id,
            &FinishRunInput {
                status: RunStatus::Ok,
                output: Some("done".to_owned()),
                warnings: vec!["no channel is wired yet".to_owned()],
                ..FinishRunInput::default()
            },
        )
        .expect("a finished run");

    let answer = send(
        &test,
        Method::GET,
        &format!("/api/automation/jobs/{id}/runs"),
        None,
    )
    .await;
    let runs = answer.json()["runs"].as_array().expect("an array");
    assert_eq!(runs[0]["status"], "ok");
    assert_eq!(runs[0]["warnings"][0], "no channel is wired yet");
}

// The engine seam

#[tokio::test]
async fn every_write_tells_the_engine_to_re_read() {
    // Without this a job created in the panel does not run until a restart.
    let scheduler = FakeScheduler::new();
    let test = server_with_engine(Arc::clone(&scheduler));

    let job = create_job(&test).await;
    assert_eq!(*scheduler.refreshes.lock(), 1);
    let id = job["id"].as_str().expect("an id").to_owned();

    send(
        &test,
        Method::PATCH,
        &format!("/api/automation/jobs/{id}"),
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(*scheduler.refreshes.lock(), 2);

    send(
        &test,
        Method::DELETE,
        &format!("/api/automation/jobs/{id}"),
        None,
    )
    .await;
    assert_eq!(*scheduler.refreshes.lock(), 3);
}

#[tokio::test]
async fn a_refused_write_does_not_tell_the_engine_anything() {
    let scheduler = FakeScheduler::new();
    let test = server_with_engine(Arc::clone(&scheduler));

    let answer = send(
        &test,
        Method::POST,
        "/api/automation/jobs",
        Some(json!({
            "name": "Morning",
            "schedule": {"kind": "cron", "expr": "not a cron"},
            "payload": {"kind": "scheduled", "message": "x"},
        })),
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(*scheduler.refreshes.lock(), 0);
}

#[tokio::test]
async fn the_crud_surface_still_works_with_no_engine_so_jobs_can_be_authored_first() {
    let test = server();
    let answer = send(
        &test,
        Method::POST,
        "/api/automation/jobs",
        Some(cron_body()),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CREATED);
}

// The end-to-end seams

/// Whether the environment has armed the hooks.
///
/// Read rather than set: `unsafe_code` is forbidden workspace-wide, and
/// `std::env::set_var` is unsafe in this edition, so a test cannot arm them from
/// the inside. The disarmed branch is the one that matters here anyway — it is
/// the security-relevant half, and it is the state every build but the
/// end-to-end suite's runs in.
fn hooks_armed() -> bool {
    std::env::var("DARKWIRE_TEST_HOOKS").is_ok_and(|value| value == "1")
}

#[tokio::test]
async fn every_hook_route_is_closed_unless_the_environment_arms_it() {
    let test = server();
    let calls = [
        (
            Method::POST,
            "/api/_test/sessions",
            // With a history, so an armed run exercises the seeding path as
            // well as the row: a session hook that created the row and dropped
            // the messages would look identical from out here.
            json!({
                "key": "s-1",
                "messages": [
                    {"role": "user", "text": "What did we decide?"},
                    {"role": "assistant", "text": "To ship the gate first."},
                ],
            }),
        ),
        (
            Method::POST,
            "/api/_test/notifications",
            json!({"title": "hello"}),
        ),
        (
            Method::POST,
            "/api/_test/automation/runs",
            json!({"jobId": "j-1"}),
        ),
        (
            Method::POST,
            "/api/_test/automation/runs/r-1/finish",
            json!({"status": "ok"}),
        ),
        (Method::POST, "/api/_test/scheduler/tick", json!({})),
    ];

    for (method, uri, body) in calls {
        let answer = send(&test, method, uri, Some(body)).await;
        if hooks_armed() {
            // Armed, the hooks answer about the request rather than about
            // themselves: a run naming a job that is not there is a storage
            // refusal, not a missing route.
            assert_ne!(answer.status, StatusCode::UNAUTHORIZED, "{uri}");
        } else {
            // A 404 rather than a 403, deliberately: a build that is not
            // running the suite looks exactly like one that has no such route,
            // so probing tells an attacker nothing about how it was compiled.
            assert_eq!(answer.status, StatusCode::NOT_FOUND, "{uri}");
        }
    }
}

#[tokio::test]
async fn a_hook_route_still_needs_a_credential_before_it_needs_the_environment() {
    // The manifest calls them `Required`, and the order matters: an
    // unauthenticated caller must not be able to tell an armed build from a
    // disarmed one.
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/_test/notifications")
        .header("content-type", "application/json")
        .body(Body::from(json!({"title": "hello"}).to_string()))
        .expect("a well-formed request");
    let test = server();
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
