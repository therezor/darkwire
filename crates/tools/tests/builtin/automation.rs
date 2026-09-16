//! The `automation` tool, over a stub port.
//!
//! What is asserted here is argument handling and the wording a model reads
//! back — the guards themselves live on the port and are tested where they are
//! enforced. The split matters: a tool that decided any of this would be
//! deciding it from arguments a model wrote.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_core::ErrorKind;
use darkwire_protocol::{
    AutomationJob, AutomationJobState, AutomationPayload, AutomationSchedule, CreateAutomationJob,
    CronKind, CronSchedule, ScheduledKind, ScheduledPayload, ToolRisk,
};
use darkwire_tools::testkit::TestWorkspace;
use darkwire_tools::{
    AutomationOutcome, AutomationPort, AutomationRefusal, ToolContext, ToolExecution,
    automation_tool,
};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Value, json};

fn job() -> AutomationJob {
    AutomationJob {
        id: "job-1".to_owned(),
        name: "Nightly build".to_owned(),
        schedule: AutomationSchedule::Cron(CronSchedule {
            kind: CronKind,
            expr: "0 9 * * *".to_owned(),
        }),
        payload: AutomationPayload::Scheduled(ScheduledPayload {
            deliver: false,
            channel: None,
            to: None,
            session_key: None,
            workspace_id: None,
            agent_id: None,
            targets: IndexMap::new(),
            kind: ScheduledKind,
            message: "check".to_owned(),
        }),
        state: AutomationJobState::default(),
        enabled: true,
        created_at_ms: 0,
        updated_at_ms: 0,
        delete_after_run: false,
        created_by: None,
    }
}

#[derive(Default)]
struct Stub {
    created: Mutex<Vec<CreateAutomationJob>>,
    deleted: Mutex<Vec<String>>,
    listed: Mutex<Option<AutomationOutcome<Vec<AutomationJob>>>>,
    refusal: Option<AutomationRefusal>,
}

impl AutomationPort for Stub {
    fn create(&self, input: CreateAutomationJob) -> AutomationOutcome<AutomationJob> {
        if let Some(refusal) = &self.refusal {
            return Err(refusal.clone());
        }
        let mut created = job();
        created.name.clone_from(&input.name);
        self.created.lock().push(input);
        Ok(created)
    }

    fn list(&self) -> AutomationOutcome<Vec<AutomationJob>> {
        if let Some(refusal) = &self.refusal {
            return Err(refusal.clone());
        }
        self.listed
            .lock()
            .clone()
            .unwrap_or_else(|| Ok(vec![job()]))
    }

    fn delete(&self, job_id: &str) -> AutomationOutcome<()> {
        if let Some(refusal) = &self.refusal {
            return Err(refusal.clone());
        }
        self.deleted.lock().push(job_id.to_owned());
        Ok(())
    }
}

fn stub() -> Arc<Stub> {
    Arc::new(Stub::default())
}

fn refusing(refusal: AutomationRefusal) -> Arc<Stub> {
    Arc::new(Stub {
        refusal: Some(refusal),
        ..Stub::default()
    })
}

fn context_with(ws: &TestWorkspace, port: Option<Arc<Stub>>) -> ToolContext {
    let mut ctx = ws.context().clone();
    ctx.automation = port.map(|port| port as Arc<dyn AutomationPort>);
    ctx
}

async fn run(args: Value, port: Option<Arc<Stub>>) -> ToolExecution {
    let ws = TestWorkspace::new();
    automation_tool()
        .execute(args, &context_with(&ws, port))
        .await
}

fn create(extra: Value) -> Value {
    let mut args = json!({"action": "create", "name": "x", "message": "go"});
    if let (Value::Object(args), Value::Object(extra)) = (&mut args, extra) {
        for (key, value) in extra {
            args.insert(key, value);
        }
    }
    args
}

#[test]
fn advertises_the_things_a_model_gets_wrong() {
    let tool = automation_tool();
    let description = tool.definition().description.to_lowercase();
    assert!(description.contains("current time is in your system prompt"));
    assert!(description.contains("install timezone"));
    assert!(description.contains("do not convert it"));
    assert!(description.contains("fresh conversation that cannot see this one"));
    let properties = tool.definition().parameters["properties"].to_string();
    assert!(properties.contains("no history"));
}

#[test]
fn is_in_the_exec_band_because_of_what_it_causes_rather_than_what_it_does() {
    assert_eq!(automation_tool().risk(), ToolRisk::Exec);
}

#[tokio::test]
async fn refuses_unknown_arguments_rather_than_stripping_them() {
    let result = run(json!({"action": "list", "wat": 1}), Some(stub())).await;
    assert_eq!(result.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn maps_every_minutes_onto_the_interval_schedule() {
    let port = stub();
    let result = run(
        create(json!({"every_minutes": 15})),
        Some(Arc::clone(&port)),
    )
    .await;
    assert!(!result.is_error, "{}", result.content);
    let created = port.created.lock();
    assert_eq!(
        serde_json::to_value(&created[0].schedule).unwrap(),
        json!({"kind": "every", "everyMs": 900_000})
    );
    assert!(!created[0].delete_after_run);
    assert!(result.content.contains("Scheduled \"x\" (job-1)"));
}

#[tokio::test]
async fn maps_cron_onto_a_schedule_with_no_zone_of_its_own() {
    let port = stub();
    run(
        create(json!({"cron": " 0 9 * * * "})),
        Some(Arc::clone(&port)),
    )
    .await;
    let created = port.created.lock();
    assert_eq!(
        serde_json::to_value(&created[0].schedule).unwrap(),
        json!({"kind": "cron", "expr": "0 9 * * *"})
    );
}

#[tokio::test]
async fn refuses_a_tz_argument_rather_than_accepting_one_it_would_ignore() {
    let result = run(
        create(json!({"cron": "0 9 * * *", "tz": "UTC"})),
        Some(stub()),
    )
    .await;
    assert_eq!(result.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn maps_an_iso_instant_onto_a_one_shot_which_self_destructs_by_default() {
    let port = stub();
    run(
        create(json!({"at": "2026-08-01T09:00:00Z"})),
        Some(Arc::clone(&port)),
    )
    .await;
    let created = port.created.lock();
    assert_eq!(
        serde_json::to_value(&created[0].schedule).unwrap(),
        json!({"kind": "at", "atMs": 1_785_574_800_000_u64})
    );
    assert!(created[0].delete_after_run);
}

#[tokio::test]
async fn accepts_a_naive_instant_and_a_date_as_utc_and_honours_delete_after_run() {
    let port = stub();
    run(
        create(json!({"at": "2026-08-01T09:00:00", "delete_after_run": false})),
        Some(Arc::clone(&port)),
    )
    .await;
    run(create(json!({"at": "2026-08-01"})), Some(Arc::clone(&port))).await;
    let created = port.created.lock();
    assert_eq!(
        serde_json::to_value(&created[0].schedule).unwrap()["atMs"],
        json!(1_785_574_800_000_u64)
    );
    assert!(!created[0].delete_after_run);
    assert_eq!(
        serde_json::to_value(&created[1].schedule).unwrap()["atMs"],
        json!(1_785_542_400_000_u64)
    );
}

#[tokio::test]
async fn refuses_two_schedules_rather_than_picking_one() {
    let port = stub();
    let result = run(
        create(json!({"every_minutes": 5, "cron": "0 9 * * *"})),
        Some(Arc::clone(&port)),
    )
    .await;
    assert!(result.is_error);
    assert!(result.content.contains("only one"));
    assert!(port.created.lock().is_empty());
}

#[tokio::test]
async fn refuses_none_at_all_and_an_at_that_is_not_an_instant() {
    let port = stub();
    let none = run(create(json!({})), Some(Arc::clone(&port))).await;
    assert!(none.is_error);
    assert!(none.content.contains("exactly one"));
    let bad = run(create(json!({"at": "tomorrow"})), Some(Arc::clone(&port))).await;
    assert!(bad.is_error);
    assert!(bad.content.contains("ISO instant"));
    let huge = run(
        create(json!({"every_minutes": u64::MAX})),
        Some(Arc::clone(&port)),
    )
    .await;
    assert!(huge.is_error);
    assert!(port.created.lock().is_empty());
}

#[tokio::test]
async fn requires_a_name_and_a_message() {
    let port = stub();
    let nameless = run(
        json!({"action": "create", "message": "go", "every_minutes": 5}),
        Some(Arc::clone(&port)),
    )
    .await;
    assert!(nameless.is_error);
    let mute = run(
        json!({"action": "create", "name": "x", "every_minutes": 5}),
        Some(Arc::clone(&port)),
    )
    .await;
    assert!(mute.is_error);
    let blank = run(
        json!({"action": "create", "name": "  ", "message": "go", "every_minutes": 5}),
        Some(Arc::clone(&port)),
    )
    .await;
    assert!(blank.is_error);
    assert!(port.created.lock().is_empty());
}

#[tokio::test]
async fn coerces_a_stringified_number_because_models_emit_them() {
    let port = stub();
    run(
        create(json!({"every_minutes": "15"})),
        Some(Arc::clone(&port)),
    )
    .await;
    assert_eq!(
        serde_json::to_value(&port.created.lock()[0].schedule).unwrap()["everyMs"],
        json!(900_000)
    );
}

#[tokio::test]
async fn tells_a_scheduled_run_to_do_the_work_rather_than_schedule_it() {
    let result = run(
        create(json!({"every_minutes": 5})),
        Some(refusing(AutomationRefusal::Nested)),
    )
    .await;
    assert!(result.is_error);
    assert_eq!(result.kind, None);
    assert!(result.content.contains("Do the work now instead"));
}

#[tokio::test]
async fn tells_an_agent_at_capacity_to_delete_one_first() {
    let result = run(
        create(json!({"every_minutes": 5})),
        Some(refusing(AutomationRefusal::AtCapacity)),
    )
    .await;
    assert!(
        result
            .content
            .contains("Delete one before creating another")
    );
}

#[tokio::test]
async fn passes_the_validators_own_sentence_through() {
    let result = run(
        create(json!({"cron": "99 * * * *"})),
        Some(refusing(AutomationRefusal::Unschedulable(
            "minute must be between 0 and 59.".to_owned(),
        ))),
    )
    .await;
    assert!(result.content.contains("cannot be honoured"));
    assert!(result.content.contains("minute must be between 0 and 59"));
    let bare = run(
        create(json!({"cron": "99 * * * *"})),
        Some(refusing(AutomationRefusal::Unschedulable(String::new()))),
    )
    .await;
    assert_eq!(bare.content, "Refused: that schedule cannot be honoured.");
}

#[tokio::test]
async fn says_so_when_the_install_has_no_scheduler_at_all() {
    let result = run(create(json!({"every_minutes": 5})), None).await;
    assert!(result.is_error);
    assert!(result.content.contains("no scheduler"));
}

#[tokio::test]
async fn reads_back_jobs_in_a_form_a_model_can_then_delete_by_id() {
    let result = run(json!({"action": "list"}), Some(stub())).await;
    assert!(result.content.contains("job-1"));
    assert!(result.content.contains("Nightly build"));
    assert!(result.content.contains("cron \"0 9 * * *\""));
    assert!(result.content.contains("not scheduled"));
    assert!(!result.content.contains("1970"));
    assert!(!result.content.contains("disabled"));
}

#[tokio::test]
async fn says_plainly_when_there_are_none_and_refuses_when_told_to() {
    let port = stub();
    *port.listed.lock() = Some(Ok(Vec::new()));
    let result = run(json!({"action": "list"}), Some(port)).await;
    assert_eq!(result.content, "You have no scheduled jobs.");
    assert!(!result.is_error);

    let refused = run(
        json!({"action": "list"}),
        Some(refusing(AutomationRefusal::NotYours)),
    )
    .await;
    assert!(refused.is_error);
    assert!(refused.content.contains("not one you created"));
}

#[tokio::test]
async fn marks_a_disabled_job_and_says_when_each_job_runs() {
    let port = stub();
    let mut disabled = job();
    disabled.enabled = false;
    disabled.state.next_run_at_ms = 1_785_661_200_000;
    *port.listed.lock() = Some(Ok(vec![disabled]));
    let result = run(json!({"action": "list"}), Some(port)).await;
    assert!(result.content.contains("disabled"));
    assert!(result.content.contains("next 2026-08-02T09:00:00Z"));
}

#[tokio::test]
async fn renders_every_and_at_schedules_in_the_models_own_words() {
    let port = stub();
    let mut every = job();
    every.schedule = serde_json::from_value(json!({"kind": "every", "everyMs": 900_000})).unwrap();
    let mut at = job();
    at.schedule =
        serde_json::from_value(json!({"kind": "at", "atMs": 1_785_574_800_000_u64})).unwrap();
    *port.listed.lock() = Some(Ok(vec![every, at]));
    let result = run(json!({"action": "list"}), Some(port)).await;
    assert!(result.content.contains("every 15 min"));
    assert!(result.content.contains("once at 2026-08-01T09:00:00Z"));
}

#[tokio::test]
async fn deletes_by_id_and_requires_one() {
    let port = stub();
    let result = run(
        json!({"action": "delete", "job_id": "job-1"}),
        Some(Arc::clone(&port)),
    )
    .await;
    assert_eq!(*port.deleted.lock(), vec!["job-1"]);
    assert!(!result.is_error);
    assert_eq!(result.content, "Deleted job-1.");

    let missing = run(json!({"action": "delete"}), Some(Arc::clone(&port))).await;
    assert!(missing.is_error);
    assert_eq!(port.deleted.lock().len(), 1);

    let refused = run(
        json!({"action": "delete", "job_id": "other"}),
        Some(refusing(AutomationRefusal::NotYours)),
    )
    .await;
    assert!(refused.is_error);
}

#[tokio::test]
async fn honours_a_cancelled_token() {
    let ws = TestWorkspace::new();
    ws.token().cancel();
    let result = automation_tool()
        .execute(json!({"action": "list"}), &context_with(&ws, Some(stub())))
        .await;
    assert_eq!(result.kind, Some(ErrorKind::Aborted));
}
