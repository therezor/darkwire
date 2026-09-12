//! The three refusals, and the stamping that makes a job run as its author.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ghostai_core::Database;
use ghostai_core::session_store::{CreateSession, IdSource, SessionStore};
use ghostai_core::testkit::ManualClock;
use ghostai_protocol::automation::{
    AUTOMATION_ORIGIN, AutomationPayload, AutomationSchedule, CreateAutomationJob, CronKind,
    CronSchedule, EveryKind, EverySchedule, HeartbeatKind, HeartbeatPayload, ScheduledKind,
    ScheduledPayload,
};
use ghostai_protocol::config::AgentToolboxNetwork;
use ghostai_server::automation_port::{MAX_AGENT_JOBS, ServerAutomationResolver};
use ghostai_server::automation_store::AutomationStore;
use ghostai_tools::automation::{AutomationRefusal, AutomationResolver};
use ghostai_tools::runner::ToolboxRequest;
use indexmap::IndexMap;

const NOW: i64 = 1_700_000_000_000;

fn ids(prefix: &'static str) -> IdSource {
    let n = AtomicU64::new(0);
    Box::new(move || format!("{prefix}{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

struct Harness {
    resolver: ServerAutomationResolver,
    jobs: Arc<AutomationStore>,
    sessions: Arc<SessionStore>,
    refreshes: Arc<AtomicUsize>,
}

fn harness() -> Harness {
    let clock: Arc<ManualClock> = Arc::new(ManualClock::at(NOW));
    let db = Database::in_memory().unwrap();
    let sessions =
        Arc::new(SessionStore::new(db.clone(), Arc::clone(&clock) as Arc<_>, ids("s")).unwrap());
    let jobs = Arc::new(AutomationStore::new(db, Arc::clone(&clock) as Arc<_>, ids("j")).unwrap());
    let refreshes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&refreshes);

    let resolver = ServerAutomationResolver::new(
        Arc::clone(&jobs),
        Arc::clone(&sessions),
        Arc::new(|| "UTC".to_owned()),
        clock,
    )
    .with_refresh(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    Harness {
        resolver,
        jobs,
        sessions,
        refreshes,
    }
}

fn request(agent_id: &str, session_key: &str, workspace_id: &str) -> ToolboxRequest {
    ToolboxRequest {
        agent_id: agent_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
        session_key: session_key.to_owned(),
        toolbox: String::new(),
        network: AgentToolboxNetwork::default(),
        workspace_root: "/tmp".to_owned(),
    }
}

fn every(ms: u64) -> AutomationSchedule {
    AutomationSchedule::Every(EverySchedule {
        kind: EveryKind,
        every_ms: ms,
    })
}

fn scheduled_payload() -> AutomationPayload {
    AutomationPayload::Scheduled(ScheduledPayload {
        deliver: false,
        channel: None,
        to: None,
        session_key: None,
        workspace_id: None,
        agent_id: None,
        targets: IndexMap::default(),
        kind: ScheduledKind,
        message: "check the feed".to_owned(),
    })
}

fn create(schedule: AutomationSchedule, payload: AutomationPayload) -> CreateAutomationJob {
    CreateAutomationJob {
        name: "watch".to_owned(),
        schedule,
        payload,
        enabled: true,
        delete_after_run: false,
    }
}

#[test]
fn a_created_job_is_stamped_with_its_author_and_workspace() {
    let h = harness();
    let port = h
        .resolver
        .for_turn(&request("researcher", "session-1", "papers"))
        .unwrap();

    let job = port
        .create(create(every(60_000), scheduled_payload()))
        .unwrap();

    let creator = job.created_by.as_ref().unwrap();
    assert_eq!(creator.agent_id, "researcher");
    assert_eq!(creator.session_key, "session-1");

    let AutomationPayload::Scheduled(payload) = &job.payload else {
        panic!("the payload should still be a scheduled message");
    };
    // The turn's agent and workspace, not anything the model wrote.
    assert_eq!(payload.agent_id.as_deref(), Some("researcher"));
    assert_eq!(payload.workspace_id.as_deref(), Some("papers"));
    assert_eq!(h.refreshes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_heartbeat_payload_is_stamped_the_same_way() {
    let h = harness();
    let port = h
        .resolver
        .for_turn(&request("watcher", "session-1", "notes"))
        .unwrap();

    let job = port
        .create(create(
            every(60_000),
            AutomationPayload::Heartbeat(HeartbeatPayload {
                deliver: false,
                channel: None,
                to: None,
                session_key: None,
                workspace_id: None,
                agent_id: None,
                targets: IndexMap::default(),
                kind: HeartbeatKind,
                file: "TASK.md".to_owned(),
                model: None,
            }),
        ))
        .unwrap();

    let AutomationPayload::Heartbeat(payload) = &job.payload else {
        panic!("the payload should still be a heartbeat");
    };
    assert_eq!(payload.agent_id.as_deref(), Some("watcher"));
    assert_eq!(payload.workspace_id.as_deref(), Some("notes"));
}

#[test]
fn a_model_naming_its_own_agent_or_workspace_is_overwritten() {
    // A model naming a workspace would be a way out of the jail it is working
    // in, and naming an agent would be scheduling a turn as somebody else.
    let h = harness();
    let port = h
        .resolver
        .for_turn(&request("researcher", "session-1", "papers"))
        .unwrap();

    let mut payload = scheduled_payload();
    if let AutomationPayload::Scheduled(inner) = &mut payload {
        inner.agent_id = Some("admin".to_owned());
        inner.workspace_id = Some("secrets".to_owned());
    }

    let job = port.create(create(every(60_000), payload)).unwrap();
    let AutomationPayload::Scheduled(stored) = &job.payload else {
        panic!("scheduled");
    };
    assert_eq!(stored.agent_id.as_deref(), Some("researcher"));
    assert_eq!(stored.workspace_id.as_deref(), Some("papers"));
}

#[test]
fn a_scheduled_run_may_not_schedule() {
    // Without this a job that asks the agent to "keep an eye on things" can
    // create another job that does the same, and the install grows jobs
    // geometrically with nobody watching.
    let h = harness();
    h.sessions
        .ensure_session(
            "automation-session",
            CreateSession {
                origin: Some(AUTOMATION_ORIGIN.to_owned()),
                ..CreateSession::default()
            },
        )
        .unwrap();

    let port = h
        .resolver
        .for_turn(&request("researcher", "automation-session", "default"))
        .unwrap();

    assert_eq!(
        port.create(create(every(60_000), scheduled_payload())),
        Err(AutomationRefusal::Nested)
    );
    assert_eq!(h.jobs.list_jobs().unwrap().len(), 0);
}

#[test]
fn a_scheduled_run_may_still_read_what_it_has_scheduled() {
    // Reading tells a job something useful and creates nothing.
    let h = harness();
    h.sessions
        .ensure_session(
            "automation-session",
            CreateSession {
                origin: Some(AUTOMATION_ORIGIN.to_owned()),
                ..CreateSession::default()
            },
        )
        .unwrap();
    let port = h
        .resolver
        .for_turn(&request("researcher", "automation-session", "default"))
        .unwrap();

    assert_eq!(port.list().unwrap().len(), 0);
}

#[test]
fn an_agent_meets_a_wall_rather_than_filling_the_table() {
    let h = harness();
    let port = h
        .resolver
        .for_turn(&request("looper", "session-1", "default"))
        .unwrap();

    for _ in 0..MAX_AGENT_JOBS {
        port.create(create(every(60_000), scheduled_payload()))
            .unwrap();
    }
    assert_eq!(
        port.create(create(every(60_000), scheduled_payload())),
        Err(AutomationRefusal::AtCapacity)
    );
    assert_eq!(
        h.jobs.count_jobs_by("looper").unwrap(),
        MAX_AGENT_JOBS,
        "nothing past the cap was written"
    );
}

#[test]
fn a_schedule_the_timer_could_not_honour_is_refused_with_the_parsers_sentence() {
    let h = harness();
    let port = h
        .resolver
        .for_turn(&request("researcher", "session-1", "default"))
        .unwrap();

    let refusal = port
        .create(create(
            AutomationSchedule::Cron(CronSchedule {
                kind: CronKind,
                expr: "not a cron expression".to_owned(),
            }),
            scheduled_payload(),
        ))
        .unwrap_err();

    let AutomationRefusal::Unschedulable(detail) = refusal else {
        panic!("expected an unschedulable refusal, got {refusal:?}");
    };
    assert!(!detail.is_empty(), "the model gets the parser's own words");
    assert_eq!(h.jobs.list_jobs().unwrap().len(), 0);
}

#[test]
fn an_agent_sees_only_its_own_jobs() {
    let h = harness();
    let mine = h
        .resolver
        .for_turn(&request("researcher", "session-1", "default"))
        .unwrap();
    let theirs = h
        .resolver
        .for_turn(&request("other", "session-2", "default"))
        .unwrap();

    mine.create(create(every(60_000), scheduled_payload()))
        .unwrap();
    theirs
        .create(create(every(60_000), scheduled_payload()))
        .unwrap();
    // And one the operator made through the panel.
    h.jobs
        .create_job(&ghostai_server::automation_store::CreateJobInput {
            name: "operator's".to_owned(),
            schedule: every(60_000),
            payload: scheduled_payload(),
            enabled: true,
            delete_after_run: false,
            next_run_at_ms: NOW,
            created_by: None,
        })
        .unwrap();

    assert_eq!(mine.list().unwrap().len(), 1);
    assert_eq!(theirs.list().unwrap().len(), 1);
    assert_eq!(h.jobs.list_jobs().unwrap().len(), 3);
}

#[test]
fn deleting_someone_elses_job_and_deleting_nothing_give_the_same_answer() {
    // So an agent cannot map the operator's jobs by probing ids for the
    // difference.
    let h = harness();
    let mine = h
        .resolver
        .for_turn(&request("researcher", "session-1", "default"))
        .unwrap();
    let theirs = h
        .resolver
        .for_turn(&request("other", "session-2", "default"))
        .unwrap();

    let ours = mine
        .create(create(every(60_000), scheduled_payload()))
        .unwrap();

    assert_eq!(theirs.delete(&ours.id), Err(AutomationRefusal::NotYours));
    assert_eq!(
        theirs.delete("no-such-id"),
        Err(AutomationRefusal::NotYours)
    );
    assert!(h.jobs.get_job(&ours.id).unwrap().is_some());

    let before = h.refreshes.load(Ordering::SeqCst);
    assert_eq!(mine.delete(&ours.id), Ok(()));
    assert!(h.jobs.get_job(&ours.id).unwrap().is_none());
    assert_eq!(h.refreshes.load(Ordering::SeqCst), before + 1);
}

#[test]
fn an_operator_made_job_is_invisible_to_every_agent() {
    let h = harness();
    h.jobs
        .create_job(&ghostai_server::automation_store::CreateJobInput {
            name: "operator's".to_owned(),
            schedule: every(60_000),
            payload: scheduled_payload(),
            enabled: true,
            delete_after_run: false,
            next_run_at_ms: NOW,
            created_by: None,
        })
        .unwrap();

    let port = h
        .resolver
        .for_turn(&request("researcher", "session-1", "default"))
        .unwrap();
    assert!(port.list().unwrap().is_empty());
    assert_eq!(port.delete("j1"), Err(AutomationRefusal::NotYours));
}

#[test]
fn a_disabled_job_is_created_unscheduled() {
    let h = harness();
    let port = h
        .resolver
        .for_turn(&request("researcher", "session-1", "default"))
        .unwrap();

    let job = port
        .create(CreateAutomationJob {
            enabled: false,
            ..create(every(60_000), scheduled_payload())
        })
        .unwrap();
    assert!(!job.enabled);
    assert_eq!(job.state.next_run_at_ms, 0);
}

#[test]
fn a_resolver_with_no_refresh_hook_still_writes() {
    let clock: Arc<ManualClock> = Arc::new(ManualClock::at(NOW));
    let db = Database::in_memory().unwrap();
    let sessions =
        Arc::new(SessionStore::new(db.clone(), Arc::clone(&clock) as Arc<_>, ids("s")).unwrap());
    let jobs = Arc::new(AutomationStore::new(db, Arc::clone(&clock) as Arc<_>, ids("j")).unwrap());
    let resolver = ServerAutomationResolver::new(
        Arc::clone(&jobs),
        sessions,
        Arc::new(|| "UTC".to_owned()),
        clock,
    );

    let port = resolver
        .for_turn(&request("researcher", "session-1", "default"))
        .unwrap();
    port.create(create(every(60_000), scheduled_payload()))
        .unwrap();
    assert_eq!(jobs.list_jobs().unwrap().len(), 1);
}
