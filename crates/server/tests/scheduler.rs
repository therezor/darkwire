//! The engine: schedule arithmetic, the drain, boot catch-up, and the
//! lifecycle of one run.
//!
//! Every decision reads the injected clock, so these tests move time by hand
//! and call [`Scheduler::tick`] rather than waiting for anything.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use darkwire_core::Database;
use darkwire_core::session_store::IdSource;
use darkwire_core::testkit::ManualClock;
use darkwire_protocol::automation::{
    AtKind, AtSchedule, AutomationPayload, AutomationSchedule, CronKind, CronSchedule, EveryKind,
    EverySchedule, HeartbeatKind, HeartbeatPayload, RunStatus, ScheduledKind, ScheduledPayload,
};
use darkwire_protocol::config::Config;
use darkwire_protocol::messages::{AssistantMessage, AssistantRole, StopReason, ToolCall, Usage};
use darkwire_protocol::rest::Notification;
use darkwire_protocol::ws::{
    AssistantDelta, AssistantDeltaTag, ClientMessage, ErrorCode, ErrorEvent, ErrorTag, MessageAck,
    MessageAckTag, Notice, NoticeKind, NoticeTag, NotificationLevel, Sequenced, ServerMessage,
    TurnEnd, TurnEndTag, TurnStart, TurnStartTag,
};
use darkwire_providers::{ChatResult, FinishReason};
use darkwire_server::automation_store::{
    AutomationStore, CreateJobInput, FinishRunInput, ListRuns,
};
use darkwire_server::heartbeat::MAX_TASK_FILE_BYTES;
use darkwire_server::notifications::{CreateNotificationInput, NotificationStore};
use darkwire_server::scheduler::{
    ChatFn, DirectChat, MAX_ARM_MS, NotificationBroadcast, ReadFileFn, ReadTaskFile, Scheduler,
    SchedulerConnectOptions, SchedulerConnection, SchedulerOptions, SchedulerPort, TurnCollector,
    first_run_at, next_run_after,
};
use indexmap::IndexMap;
use parking_lot::Mutex;

const NOW: i64 = 1_700_000_000_000;

fn ids(prefix: &'static str) -> IdSource {
    let n = AtomicU64::new(0);
    Box::new(move || format!("{prefix}{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

// Schedule arithmetic

fn at(ms: u64) -> AutomationSchedule {
    AutomationSchedule::At(AtSchedule {
        kind: AtKind,
        at_ms: ms,
    })
}

fn every(ms: u64) -> AutomationSchedule {
    AutomationSchedule::Every(EverySchedule {
        kind: EveryKind,
        every_ms: ms,
    })
}

fn cron(expr: &str) -> AutomationSchedule {
    AutomationSchedule::Cron(CronSchedule {
        kind: CronKind,
        expr: expr.to_owned(),
    })
}

#[test]
fn a_one_shot_fires_once_and_then_never_again() {
    let future = u64::try_from(NOW + 5_000).unwrap();
    assert_eq!(
        next_run_after(&at(future), NOW, "UTC").unwrap(),
        NOW + 5_000
    );
    // Zero rather than `None`: the column means "unscheduled", and a fired
    // one-shot, a disabled job and an unreachable cron are all the same to the
    // timer.
    assert_eq!(next_run_after(&at(future), NOW + 6_000, "UTC").unwrap(), 0);
    // Exactly now is already past.
    assert_eq!(next_run_after(&at(future), NOW + 5_000, "UTC").unwrap(), 0);
}

#[test]
fn an_interval_is_measured_from_the_instant_it_is_asked_about() {
    assert_eq!(
        next_run_after(&every(1_000), NOW, "UTC").unwrap(),
        NOW + 1_000
    );
    assert_eq!(
        next_run_after(&every(1_000), NOW + 10, "UTC").unwrap(),
        NOW + 1_010
    );
}

#[test]
fn a_cron_expression_is_read_in_the_installs_zone() {
    let utc = next_run_after(&cron("0 9 * * *"), NOW, "UTC").unwrap();
    let kyiv = next_run_after(&cron("0 9 * * *"), NOW, "Europe/Kyiv").unwrap();
    assert!(utc > NOW);
    assert!(kyiv > NOW);
    // Nine o'clock in two zones is two different instants, which is exactly why
    // the zone is one install-wide setting rather than a per-job field.
    assert_ne!(utc, kyiv);
}

#[test]
fn a_cron_expression_that_cannot_be_read_is_an_error_rather_than_a_silent_zero() {
    assert!(next_run_after(&cron("not a cron expression"), NOW, "UTC").is_err());
}

#[test]
fn a_disabled_job_has_no_first_run() {
    assert_eq!(first_run_at(&every(1_000), NOW, false, "UTC").unwrap(), 0);
    assert_eq!(
        first_run_at(&at(u64::try_from(NOW + 1).unwrap()), NOW, false, "UTC").unwrap(),
        0
    );
}

#[test]
fn a_one_shot_already_in_the_past_keeps_its_instant() {
    // The boot sweep is what decides whether a missed one-shot runs; moving it
    // forward here would take that decision away.
    let past = u64::try_from(NOW - 5_000).unwrap();
    assert_eq!(
        first_run_at(&at(past), NOW, true, "UTC").unwrap(),
        NOW - 5_000
    );
}

#[test]
fn an_interval_jobs_first_run_is_one_interval_away() {
    assert_eq!(
        first_run_at(&every(1_000), NOW, true, "UTC").unwrap(),
        NOW + 1_000
    );
}

// The collector

fn turn_start(turn_id: &str) -> ServerMessage {
    ServerMessage::TurnStart(Sequenced {
        seq: 1,
        event: TurnStart {
            tag: TurnStartTag,
            agent_id: "default".to_owned(),
            session_key: "session-1".to_owned(),
            turn_id: turn_id.to_owned(),
            first_seq: None,
            model: "test-model".to_owned(),
            provider: "test".to_owned(),
        },
    })
}

/// The run id every collector below is waiting on.
const RUN: &str = "run-1";

/// The hub accepting a message, and naming the turn it will run under.
fn ack(client_message_id: Option<&str>, turn_id: &str) -> ServerMessage {
    ServerMessage::MessageAck(Sequenced {
        seq: 0,
        event: MessageAck {
            tag: MessageAckTag,
            session_key: "session-1".to_owned(),
            message_id: turn_id.to_owned(),
            client_message_id: client_message_id.map(str::to_owned),
        },
    })
}

/// A collector that has already been told its turn is `turn-1`.
fn collecting() -> (
    Arc<TurnCollector>,
    tokio::sync::oneshot::Receiver<darkwire_server::scheduler::TurnOutcome>,
) {
    let (collector, outcome) = TurnCollector::new(RUN);
    collector.receive(&ack(Some(RUN), "turn-1"));
    (collector, outcome)
}

fn delta(text: &str) -> ServerMessage {
    delta_for("turn-1", text)
}

fn delta_for(turn_id: &str, text: &str) -> ServerMessage {
    ServerMessage::AssistantDelta(Sequenced {
        seq: 2,
        event: AssistantDelta {
            tag: AssistantDeltaTag,
            turn_id: turn_id.to_owned(),
            text: text.to_owned(),
        },
    })
}

fn failure(turn_id: Option<&str>, message: &str) -> ServerMessage {
    ServerMessage::Error(ErrorEvent {
        tag: ErrorTag,
        code: ErrorCode::NotConfigured,
        message: message.to_owned(),
        retryable: false,
        turn_id: turn_id.map(str::to_owned),
        call_id: None,
    })
}

fn turn_end(turn_id: &str, stop_reason: StopReason) -> ServerMessage {
    ServerMessage::TurnEnd(Sequenced {
        seq: 3,
        event: TurnEnd {
            tag: TurnEndTag,
            turn_id: turn_id.to_owned(),
            stop_reason,
            usage: None,
            iterations: 1,
            elapsed_ms: None,
            generation_ms: None,
            generation_tokens: None,
            first_token_ms: None,
            first_seq: None,
            last_seq: None,
        },
    })
}

#[tokio::test]
async fn the_collector_reassembles_an_answer_from_its_deltas() {
    let (collector, outcome) = collecting();
    collector.receive(&turn_start("turn-1"));
    collector.receive(&delta("  hello "));
    collector.receive(&delta("world  "));
    collector.receive(&turn_end("turn-1", StopReason::Complete));

    let settled = outcome.await.unwrap();
    assert_eq!(settled.text, "hello world");
    assert_eq!(settled.error, None);
    assert!(settled.warnings.is_empty());
}

#[tokio::test]
async fn a_turn_end_for_someone_elses_turn_is_ignored() {
    let (collector, outcome) = collecting();
    collector.receive(&turn_start("turn-1"));
    collector.receive(&delta("mine"));
    collector.receive(&turn_end("turn-2", StopReason::Complete));
    collector.receive(&turn_end("turn-1", StopReason::Complete));

    assert_eq!(outcome.await.unwrap().text, "mine");
}

#[tokio::test]
async fn the_iteration_cap_is_a_warning_rather_than_a_failure() {
    // The turn did work and produced an answer, it just ran out of tool budget
    // saying so.
    let (collector, outcome) = collecting();
    collector.receive(&turn_start("turn-1"));
    collector.receive(&delta("partial"));
    collector.receive(&turn_end("turn-1", StopReason::MaxIterations));

    let settled = outcome.await.unwrap();
    assert_eq!(settled.error, None);
    assert_eq!(settled.warnings.len(), 1);
    assert!(settled.warnings[0].contains("tool-iteration cap"));
}

#[tokio::test]
async fn any_other_early_end_is_a_failure_that_names_itself() {
    for reason in [
        StopReason::Aborted,
        StopReason::WallTimeout,
        StopReason::Error,
    ] {
        let (collector, outcome) = collecting();
        collector.receive(&turn_start("turn-1"));
        collector.receive(&turn_end("turn-1", reason));
        let settled = outcome.await.unwrap();
        assert!(
            settled
                .error
                .as_deref()
                .unwrap()
                .starts_with("The turn ended early"),
            "{reason:?}"
        );
    }
}

#[tokio::test]
async fn a_hub_refusal_settles_the_run_because_no_turn_end_will_follow() {
    // Before any ack: a full queue is refused in place of one.
    let (collector, outcome) = TurnCollector::new(RUN);
    collector.receive(&failure(None, "No model is configured."));

    let settled = outcome.await.unwrap();
    assert_eq!(settled.error.as_deref(), Some("No model is configured."));
}

fn notice(turn_id: Option<&str>, message: &str) -> ServerMessage {
    ServerMessage::Notice(Sequenced {
        seq: 4,
        event: Notice {
            tag: NoticeTag,
            kind: NoticeKind::Degraded,
            message: message.to_owned(),
            turn_id: turn_id.map(str::to_owned),
            call_id: None,
        },
    })
}

#[tokio::test]
async fn this_runs_own_failure_settles_it_without_a_turn_end() {
    // An install with no model: the hub names the turn and never opens it.
    let (collector, outcome) = collecting();
    collector.receive(&notice(None, "about the session"));
    collector.receive(&notice(Some("turn-1"), "about this turn"));
    collector.receive(&failure(Some("turn-1"), "No model is configured."));

    let settled = outcome.await.unwrap();
    assert_eq!(settled.error.as_deref(), Some("No model is configured."));
    assert_eq!(settled.warnings, ["about the session", "about this turn"]);
}

#[tokio::test]
async fn another_turn_on_the_same_session_is_not_this_runs_output() {
    // A job pinned to a session the operator is also typing in.
    let (collector, outcome) = TurnCollector::new(RUN);
    collector.receive(&ack(Some("tab-7"), "turn-0"));
    collector.receive(&ack(None, "turn-00"));
    collector.receive(&turn_start("turn-0"));
    collector.receive(&delta_for("turn-0", "the operator's answer"));
    collector.receive(&failure(Some("turn-0"), "the operator's turn failed"));
    collector.receive(&turn_end("turn-0", StopReason::Error));

    collector.receive(&ack(Some(RUN), "turn-1"));
    collector.receive(&notice(Some("turn-0"), "not mine"));
    collector.receive(&turn_start("turn-1"));
    collector.receive(&delta("mine"));
    collector.receive(&turn_end("turn-1", StopReason::Complete));

    let settled = outcome.await.unwrap();
    assert_eq!(settled.text, "mine");
    assert_eq!(settled.error, None);
    assert!(settled.warnings.is_empty(), "{:?}", settled.warnings);
}

#[tokio::test]
async fn finishing_by_hand_wins_and_is_idempotent() {
    let (collector, outcome) = collecting();
    collector.receive(&turn_start("turn-1"));
    collector.finish(Some("timed out".to_owned()));
    // Everything after is ignored.
    collector.receive(&delta("too late"));
    collector.finish(Some("also too late".to_owned()));

    let settled = outcome.await.unwrap();
    assert_eq!(settled.error.as_deref(), Some("timed out"));
    assert_eq!(settled.text, "");
}

// The engine

/// A connection that answers the turn it is given, on a script.
struct ScriptedConnection {
    send: Arc<dyn Fn(ServerMessage) + Send + Sync>,
    session_key: String,
    answer: String,
    /// When set, the connection says nothing at all — for the guards that only
    /// exist while a turn is in flight.
    silent: bool,
    frames: Arc<Mutex<Vec<ClientMessage>>>,
}

impl SchedulerConnection for ScriptedConnection {
    fn receive(&self, frame: ClientMessage) {
        self.frames.lock().push(frame.clone());
        if self.silent {
            return;
        }
        if let ClientMessage::UserMessage(message) = frame {
            (self.send)(ack(message.client_message_id.as_deref(), "turn-1"));
            (self.send)(ServerMessage::TurnStart(Sequenced {
                seq: 1,
                event: TurnStart {
                    tag: TurnStartTag,
                    agent_id: "default".to_owned(),
                    session_key: self.session_key.clone(),
                    turn_id: "turn-1".to_owned(),
                    first_seq: None,
                    model: "test-model".to_owned(),
                    provider: "test".to_owned(),
                },
            }));
            (self.send)(delta(&self.answer));
            (self.send)(turn_end("turn-1", StopReason::Complete));
        }
    }

    fn close(&self) {}
}

struct Harness {
    scheduler: Scheduler,
    jobs: Arc<AutomationStore>,
    notes: Arc<NotificationStore>,
    clock: Arc<ManualClock>,
    config: Arc<Mutex<Config>>,
    broadcasts: Arc<Mutex<Vec<NotificationBroadcast>>>,
    frames: Arc<Mutex<Vec<ClientMessage>>>,
    deleted_sessions: Arc<Mutex<Vec<String>>>,
    /// Every read of the settings tree, which every pass of the loop makes.
    config_reads: Arc<AtomicUsize>,
}

fn harness(silent: bool) -> Harness {
    harness_with(silent, None, None)
}

/// The same engine with a provider and a task file wired in, for the heartbeat.
fn harness_with(silent: bool, chat: Option<ChatFn>, read_file: Option<ReadFileFn>) -> Harness {
    let clock = Arc::new(ManualClock::at(NOW));
    let db = Database::in_memory().unwrap();
    let jobs =
        Arc::new(AutomationStore::new(db.clone(), Arc::clone(&clock) as Arc<_>, ids("j")).unwrap());
    let notes =
        Arc::new(NotificationStore::new(db, Arc::clone(&clock) as Arc<_>, ids("n")).unwrap());

    let config = Arc::new(Mutex::new(Config::default()));
    let broadcasts: Arc<Mutex<Vec<NotificationBroadcast>>> = Arc::new(Mutex::new(Vec::new()));
    let frames: Arc<Mutex<Vec<ClientMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let deleted: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let config_for_port = Arc::clone(&config);
    let config_reads = Arc::new(AtomicUsize::new(0));
    let reads_for_port = Arc::clone(&config_reads);
    let broadcasts_for_port = Arc::clone(&broadcasts);
    let frames_for_port = Arc::clone(&frames);
    let deleted_for_port = Arc::clone(&deleted);
    let notes_for_port = Arc::clone(&notes);

    let scheduler = Scheduler::new(SchedulerOptions {
        jobs: Arc::clone(&jobs),
        config: Arc::new(move || {
            reads_for_port.fetch_add(1, Ordering::SeqCst);
            config_for_port.lock().clone()
        }),
        connect: Arc::new(move |options: SchedulerConnectOptions| {
            Arc::new(ScriptedConnection {
                send: options.send,
                session_key: options.session_key,
                answer: "the answer".to_owned(),
                silent,
                frames: Arc::clone(&frames_for_port),
            }) as Arc<dyn SchedulerConnection>
        }),
        broadcast: Arc::new(move |event| broadcasts_for_port.lock().push(event)),
        raise: Arc::new(move |input: CreateNotificationInput| notes_for_port.create(input)),
        delete_session: Some(Arc::new(move |key: &str| {
            deleted_for_port.lock().push(key.to_owned());
        })),
        chat,
        read_file,
        clock: Arc::clone(&clock) as Arc<_>,
        new_id: ids("r"),
        // Short enough that a silent connection settles inside a test.
        run_timeout_ms: Some(50),
    });

    Harness {
        scheduler,
        jobs,
        notes,
        clock,
        config,
        broadcasts,
        frames,
        deleted_sessions: deleted,
        config_reads,
    }
}

fn message_payload(text: &str) -> AutomationPayload {
    AutomationPayload::Scheduled(ScheduledPayload {
        deliver: false,
        channel: None,
        to: None,
        session_key: None,
        workspace_id: None,
        agent_id: None,
        targets: IndexMap::default(),
        kind: ScheduledKind,
        message: text.to_owned(),
    })
}

fn job(name: &str, schedule: AutomationSchedule, next_run_at_ms: i64) -> CreateJobInput {
    CreateJobInput {
        name: name.to_owned(),
        schedule,
        payload: message_payload("do it"),
        enabled: true,
        delete_after_run: false,
        next_run_at_ms,
        created_by: None,
    }
}

/// Lets the spawned run task finish. The scripted connection answers inline, so
/// one yield past the await points is enough.
async fn settle(h: &Harness) {
    for _ in 0..50 {
        if h.scheduler.in_flight() == 0 {
            // One more pass so the `finally` bookkeeping lands.
            tokio::task::yield_now().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("a run never settled");
}

#[tokio::test]
async fn a_due_job_runs_and_records_what_the_turn_said() {
    let h = harness(false);
    let created = h
        .jobs
        .create_job(&job("watch", every(1_000), NOW - 1))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Ok);
    assert_eq!(runs[0].output.as_deref(), Some("the answer"));

    let after = h.jobs.get_job(&created.id).unwrap().unwrap();
    assert_eq!(after.state.last_status, RunStatus::Ok);
    assert_eq!(after.state.run_count, 1);
    // The authoritative next time, computed at completion.
    assert!(after.state.next_run_at_ms > u64::try_from(NOW).unwrap());
}

#[tokio::test]
async fn a_finished_scheduled_job_notifies_and_broadcasts() {
    let h = harness(false);
    h.jobs
        .create_job(&job("watch", every(1_000), NOW - 1))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let raised: Vec<Notification> = h
        .notes
        .list(&darkwire_server::notifications::ListNotifications::default())
        .unwrap();
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].title, "watch finished");
    assert_eq!(raised[0].body, "the answer");
    assert_eq!(raised[0].level, NotificationLevel::Info);

    let broadcast = h.broadcasts.lock();
    assert_eq!(broadcast.len(), 1);
    assert_eq!(broadcast[0].id, raised[0].id);
    assert_eq!(broadcast[0].job_id.as_deref(), Some("j1"));
}

#[tokio::test]
async fn the_turn_is_driven_through_the_hub_as_an_unattended_connection() {
    let h = harness(false);
    h.jobs
        .create_job(&job("watch", every(1_000), NOW - 1))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let run = h.jobs.list_runs("j1", &ListRuns::default()).unwrap();
    let frames = h.frames.lock();
    let ClientMessage::UserMessage(sent) = &frames[0] else {
        panic!("the first frame should be the user message");
    };
    assert_eq!(sent.content, "do it");
    // The run id, so a redelivery is acknowledged rather than run twice.
    assert_eq!(sent.client_message_id.as_deref(), Some(run[0].id.as_str()));
}

#[tokio::test]
async fn a_one_shot_is_unscheduled_the_instant_it_is_dispatched() {
    let h = harness(false);
    let created = h
        .jobs
        .create_job(&job("once", at(u64::try_from(NOW).unwrap()), NOW - 1))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let after = h.jobs.get_job(&created.id).unwrap().unwrap();
    assert_eq!(after.state.next_run_at_ms, 0);
}

#[tokio::test]
async fn a_self_destructing_one_shot_removes_itself() {
    let h = harness(false);
    let created = h
        .jobs
        .create_job(&CreateJobInput {
            delete_after_run: true,
            ..job("reminder", at(u64::try_from(NOW).unwrap()), NOW - 1)
        })
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    assert!(h.jobs.get_job(&created.id).unwrap().is_none());
}

#[tokio::test]
async fn a_disabled_scheduler_runs_nothing_but_still_reconciles() {
    let h = harness(false);
    let created = h
        .jobs
        .create_job(&job("watch", every(1_000), NOW - 1))
        .unwrap();
    let abandoned = h.jobs.start_run(&created.id, None).unwrap();
    h.config.lock().scheduler.enabled = false;

    h.scheduler.start().unwrap();
    settle(&h).await;

    assert!(!h.scheduler.enabled());
    // The dead process's row was closed out even though nothing will fire.
    let closed = h.jobs.get_run(&abandoned.id).unwrap().unwrap();
    assert_eq!(closed.status, RunStatus::Error);
    assert!(closed.error.as_deref().unwrap().contains("restart"));
    // And no new run happened.
    assert_eq!(h.jobs.count_runs(&created.id).unwrap(), 1);
}

#[tokio::test]
async fn the_concurrency_limit_leaves_the_rest_due() {
    let h = harness(true);
    h.config.lock().scheduler.concurrency = 1;
    // Scheduled ahead, so the boot sweep has nothing to catch up on: catch-up
    // dispatches every job whose time passed while the process was down, and
    // the limit is what governs an ordinary drain.
    h.jobs
        .create_job(&job("a", every(1_000), NOW + 1_000))
        .unwrap();
    h.jobs
        .create_job(&job("b", every(1_000), NOW + 1_001))
        .unwrap();

    h.scheduler.start().unwrap();
    assert_eq!(h.scheduler.in_flight(), 0, "nothing was due at boot");

    h.clock.advance(Duration::from_secs(2));
    h.scheduler.tick().unwrap();
    // The silent connection holds the first run open, so the second stays due.
    assert_eq!(h.scheduler.in_flight(), 1);
    h.scheduler.tick().unwrap();
    assert_eq!(h.scheduler.in_flight(), 1, "no slot for the second");

    h.scheduler.stop();
    settle(&h).await;
}

#[tokio::test]
async fn a_job_already_running_is_left_due_rather_than_started_twice() {
    let h = harness(true);
    h.config.lock().scheduler.concurrency = 4;
    let created = h.jobs.create_job(&job("slow", every(1), NOW - 1)).unwrap();

    h.scheduler.start().unwrap();
    assert_eq!(h.scheduler.in_flight(), 1);

    h.clock.advance(Duration::from_millis(10));
    h.scheduler.tick().unwrap();
    assert_eq!(h.scheduler.in_flight(), 1);
    assert_eq!(h.jobs.count_runs(&created.id).unwrap(), 1);

    h.scheduler.stop();
    settle(&h).await;
}

#[tokio::test]
async fn running_a_job_out_of_band_answers_the_pending_row() {
    let h = harness(true);
    let created = h
        .jobs
        .create_job(&job("manual", every(86_400_000), 0))
        .unwrap();

    h.scheduler.start().unwrap();
    let run = h.scheduler.run_now(&created.id).unwrap();
    assert_eq!(run.status, RunStatus::Pending);
    assert_eq!(run.job_id, created.id);

    // A second one while the first is in flight is a conflict.
    let again = h.scheduler.run_now(&created.id).unwrap_err();
    assert_eq!(again.kind, darkwire_core::ErrorKind::Conflict);

    h.scheduler.stop();
    settle(&h).await;
}

#[tokio::test]
async fn running_a_job_that_does_not_exist_is_a_not_found() {
    let h = harness(false);
    h.scheduler.start().unwrap();
    let error = h.scheduler.run_now("nope").unwrap_err();
    assert_eq!(error.kind, darkwire_core::ErrorKind::NotFound);
}

#[tokio::test]
async fn an_on_demand_run_of_a_disabled_job_does_not_put_it_back_on_the_timer() {
    // A row badged Disabled with a "Next run" beside it is a contradiction an
    // operator has to work out for themselves.
    let h = harness(false);
    let created = h
        .jobs
        .create_job(&CreateJobInput {
            enabled: false,
            ..job("off", every(1_000), 0)
        })
        .unwrap();

    h.scheduler.start().unwrap();
    h.scheduler.run_now(&created.id).unwrap();
    settle(&h).await;

    assert_eq!(
        h.jobs
            .get_job(&created.id)
            .unwrap()
            .unwrap()
            .state
            .next_run_at_ms,
        0
    );
}

#[tokio::test]
async fn boot_catch_up_coalesces_a_weekend_of_missed_occurrences_into_one_run() {
    let h = harness(false);
    let created = h
        .jobs
        .create_job(&job("nightly", every(60_000), NOW - 600_000))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    assert_eq!(h.jobs.count_runs(&created.id).unwrap(), 1);
    let run = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert!(
        run[0]
            .warnings
            .iter()
            .any(|w| w.contains("started at boot")),
        "{:?}",
        run[0].warnings
    );
}

#[tokio::test]
async fn without_catch_up_a_missed_one_shot_is_recorded_rather_than_hidden() {
    // A reminder that silently vanished is worse than one that says it was
    // missed.
    let h = harness(false);
    h.config.lock().scheduler.catch_up_on_boot = false;
    let created = h
        .jobs
        .create_job(&CreateJobInput {
            delete_after_run: true,
            ..job(
                "reminder",
                at(u64::try_from(NOW - 5_000).unwrap()),
                NOW - 5_000,
            )
        })
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Skipped);
    assert_eq!(
        runs[0].skip_reason.as_deref(),
        Some("Missed while the server was down.")
    );

    let after = h.jobs.get_job(&created.id).unwrap().unwrap();
    assert_eq!(after.state.next_run_at_ms, 0);
    assert_eq!(after.state.last_status, RunStatus::Skipped);
    // The self-destruct is deliberately not honoured, because it did not run.
    assert!(h.jobs.get_job(&created.id).unwrap().is_some());
}

#[tokio::test]
async fn without_catch_up_a_missed_recurring_job_is_simply_moved_forward() {
    let h = harness(false);
    h.config.lock().scheduler.catch_up_on_boot = false;
    let created = h
        .jobs
        .create_job(&job("nightly", every(60_000), NOW - 600_000))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    assert_eq!(h.jobs.count_runs(&created.id).unwrap(), 0);
    assert_eq!(
        h.jobs
            .get_job(&created.id)
            .unwrap()
            .unwrap()
            .state
            .next_run_at_ms,
        u64::try_from(NOW + 60_000).unwrap()
    );
}

#[tokio::test]
async fn changing_the_install_timezone_reschedules_cron_jobs_and_nothing_else() {
    let h = harness(false);
    // Nothing due, so start() arms and does not dispatch.
    let cron_job = h
        .jobs
        .create_job(&job("nightly", cron("0 9 * * *"), NOW + 1_000_000))
        .unwrap();
    let interval_job = h
        .jobs
        .create_job(&job("poll", every(60_000), NOW + 1_000_000))
        .unwrap();
    let one_shot = h
        .jobs
        .create_job(&job(
            "once",
            at(u64::try_from(NOW + 1_000_000).unwrap()),
            NOW + 1_000_000,
        ))
        .unwrap();

    h.scheduler.start().unwrap();
    // The first refresh after boot has nothing to recompute.
    h.scheduler.refresh_now().unwrap();
    assert_eq!(
        h.jobs
            .get_job(&cron_job.id)
            .unwrap()
            .unwrap()
            .state
            .next_run_at_ms,
        u64::try_from(NOW + 1_000_000).unwrap()
    );

    h.config.lock().ui.timezone = "Europe/Kyiv".to_owned();
    h.scheduler.refresh_now().unwrap();

    let moved = h.jobs.get_job(&cron_job.id).unwrap().unwrap();
    assert_ne!(
        moved.state.next_run_at_ms,
        u64::try_from(NOW + 1_000_000).unwrap()
    );
    // An interval has no wall clock in it and a one-shot was already resolved.
    assert_eq!(
        h.jobs
            .get_job(&interval_job.id)
            .unwrap()
            .unwrap()
            .state
            .next_run_at_ms,
        u64::try_from(NOW + 1_000_000).unwrap()
    );
    assert_eq!(
        h.jobs
            .get_job(&one_shot.id)
            .unwrap()
            .unwrap()
            .state
            .next_run_at_ms,
        u64::try_from(NOW + 1_000_000).unwrap()
    );
}

#[tokio::test]
async fn a_refresh_before_the_engine_starts_does_nothing() {
    let h = harness(false);
    h.config.lock().ui.timezone = "Europe/Kyiv".to_owned();
    let created = h
        .jobs
        .create_job(&job("nightly", cron("0 9 * * *"), NOW + 1_000_000))
        .unwrap();

    h.scheduler.refresh_now().unwrap();
    assert_eq!(
        h.jobs
            .get_job(&created.id)
            .unwrap()
            .unwrap()
            .state
            .next_run_at_ms,
        u64::try_from(NOW + 1_000_000).unwrap()
    );
}

#[tokio::test]
async fn the_next_delay_is_clamped_and_floored() {
    let h = harness(false);
    assert_eq!(h.scheduler.next_delay_ms(), None, "nothing scheduled");

    h.jobs
        .create_job(&job("far", every(1_000), NOW + MAX_ARM_MS * 4))
        .unwrap();
    assert_eq!(h.scheduler.next_delay_ms(), Some(MAX_ARM_MS));

    h.jobs
        .create_job(&job("due", every(1_000), NOW - 5_000))
        .unwrap();
    assert_eq!(h.scheduler.next_delay_ms(), Some(0), "already due");

    h.config.lock().scheduler.enabled = false;
    assert_eq!(h.scheduler.next_delay_ms(), None, "switched off");
}

#[tokio::test]
async fn a_failed_turn_always_notifies() {
    // A failure nobody was told about is a job that has quietly not worked for
    // a week, so the evaluate step never gets to veto it.
    let h = harness(true);
    let created = h
        .jobs
        .create_job(&job("watch", every(1_000), NOW - 1))
        .unwrap();

    h.scheduler.start().unwrap();
    // The silent connection never answers; the run timeout settles it.
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs[0].status, RunStatus::Error);

    let raised = h
        .notes
        .list(&darkwire_server::notifications::ListNotifications::default())
        .unwrap();
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].title, "watch failed");
    assert_eq!(raised[0].level, NotificationLevel::Error);
}

#[tokio::test]
async fn a_heartbeat_on_a_build_with_no_provider_access_fails_rather_than_running() {
    // Not a silent "always run": that would start an unbounded turn on whatever
    // the file happens to say, every interval, forever.
    let h = harness(false);
    let created = h
        .jobs
        .create_job(&CreateJobInput {
            payload: AutomationPayload::Heartbeat(HeartbeatPayload {
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
            ..job("pulse", every(1_000), NOW - 1)
        })
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs[0].status, RunStatus::Error);
    assert!(
        runs[0]
            .error
            .as_deref()
            .unwrap()
            .contains("no direct provider access")
    );
}

#[tokio::test]
async fn a_job_that_asks_for_delivery_with_no_channel_wired_says_so() {
    let h = harness(false);
    let mut payload = message_payload("do it");
    if let AutomationPayload::Scheduled(inner) = &mut payload {
        inner.deliver = true;
    }
    let created = h
        .jobs
        .create_job(&CreateJobInput {
            payload,
            ..job("watch", every(1_000), NOW - 1)
        })
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs[0].status, RunStatus::Ok, "a caveat is not a failure");
    assert!(
        runs[0]
            .warnings
            .iter()
            .any(|w| w.contains("no channel is wired"))
    );
}

#[tokio::test]
async fn history_is_trimmed_and_the_sessions_the_engine_minted_go_with_it() {
    let h = harness(false);
    h.config.lock().scheduler.run_retention = 1;
    let created = h
        .jobs
        .create_job(&job("watch", every(1_000), NOW - 1))
        .unwrap();
    // An older run this engine minted a session for.
    let old = h
        .jobs
        .start_run(&created.id, Some("minted-session".to_owned()))
        .unwrap();
    h.jobs
        .finish_run(&old.id, &FinishRunInput::default())
        .unwrap();
    // Genuinely older: two runs in the same millisecond tie, and the id breaks
    // the tie in the other direction.
    h.clock.advance(Duration::from_millis(10));

    h.scheduler.start().unwrap();
    settle(&h).await;

    assert_eq!(h.jobs.count_runs(&created.id).unwrap(), 1);
    assert_eq!(h.deleted_sessions.lock().as_slice(), ["minted-session"]);
}

#[tokio::test]
async fn a_session_the_operator_pinned_is_never_deleted_by_a_trim() {
    // Deleting it would take a conversation with it.
    let h = harness(false);
    h.config.lock().scheduler.run_retention = 1;
    let mut payload = message_payload("do it");
    if let AutomationPayload::Scheduled(inner) = &mut payload {
        inner.session_key = Some("shared-session".to_owned());
    }
    let created = h
        .jobs
        .create_job(&CreateJobInput {
            payload,
            ..job("watch", every(1_000), NOW - 1)
        })
        .unwrap();
    let old = h
        .jobs
        .start_run(&created.id, Some("shared-session".to_owned()))
        .unwrap();
    h.jobs
        .finish_run(&old.id, &FinishRunInput::default())
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    assert!(h.deleted_sessions.lock().is_empty());
}

#[tokio::test]
async fn starting_twice_is_a_no_op() {
    let h = harness(false);
    h.jobs
        .create_job(&job("watch", every(1_000), NOW - 1))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;
    h.scheduler.start().unwrap();
    settle(&h).await;

    assert_eq!(h.jobs.count_runs("j1").unwrap(), 1);
}

#[tokio::test]
async fn stopping_cancels_what_is_in_flight() {
    let h = harness(true);
    h.jobs
        .create_job(&job("slow", every(1_000), NOW - 1))
        .unwrap();

    h.scheduler.start().unwrap();
    assert_eq!(h.scheduler.in_flight(), 1);

    h.scheduler.stop();
    settle(&h).await;

    let runs = h.jobs.list_runs("j1", &ListRuns::default()).unwrap();
    assert_eq!(runs[0].status, RunStatus::Error);
    assert!(runs[0].error.as_deref().unwrap().contains("shutdown"));
    // And a tick after a stop does nothing.
    h.scheduler.tick().unwrap();
    assert_eq!(h.scheduler.in_flight(), 0);
}

// The heartbeat, with a provider and a task file wired in

fn chat_result(tool_calls: Vec<ToolCall>) -> ChatResult {
    ChatResult {
        message: AssistantMessage {
            role: AssistantRole,
            content: Vec::new(),
            tool_calls,
            reasoning: None,
            reasoning_ms: None,
        },
        finish_reason: FinishReason::Stop,
        usage: Usage::default(),
        model: "cheap".to_owned(),
        generation_ms: None,
        first_token_ms: None,
    }
}

fn tool_call(name: &str, arguments_json: &str) -> ToolCall {
    ToolCall {
        id: "call_1".to_owned(),
        name: name.to_owned(),
        arguments_json: arguments_json.to_owned(),
    }
}

/// Answers each request in turn, and records what it was asked.
fn scripted_chat(answers: Vec<ChatResult>) -> (ChatFn, Arc<Mutex<Vec<DirectChatSeen>>>) {
    let seen: Arc<Mutex<Vec<DirectChatSeen>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let queue = Arc::new(Mutex::new(answers.into_iter()));
    let chat: ChatFn = Arc::new(move |input: DirectChat| {
        recorder.lock().push(DirectChatSeen {
            model: input.model.clone(),
            agent_id: input.agent_id.clone(),
            tool: input.tools.first().map(|tool| tool.name.clone()),
            messages: input.messages.clone(),
        });
        let answer = queue.lock().next();
        Box::pin(async move {
            answer.ok_or_else(|| {
                darkwire_core::WireError::new(
                    darkwire_core::ErrorKind::Provider,
                    "the script ran out of answers",
                )
            })
        })
    });
    (chat, seen)
}

struct DirectChatSeen {
    model: Option<String>,
    agent_id: Option<String>,
    tool: Option<String>,
    messages: Vec<darkwire_protocol::messages::ChatMessage>,
}

fn file_returning(contents: Result<String, darkwire_core::WireError>) -> ReadFileFn {
    let contents = Arc::new(Mutex::new(Some(contents)));
    Arc::new(move |_request: ReadTaskFile| {
        let taken = contents.lock().take();
        Box::pin(async move {
            match taken {
                Some(Ok(text)) => Ok(text),
                Some(Err(error)) => Err(error),
                None => Ok(String::new()),
            }
        })
    })
}

fn heartbeat_job(name: &str, file: &str, model: Option<&str>) -> CreateJobInput {
    CreateJobInput {
        payload: AutomationPayload::Heartbeat(HeartbeatPayload {
            deliver: false,
            channel: None,
            to: None,
            session_key: None,
            workspace_id: None,
            agent_id: Some("watcher".to_owned()),
            targets: IndexMap::default(),
            kind: HeartbeatKind,
            file: file.to_owned(),
            model: model.map(str::to_owned),
        }),
        ..job(name, every(1_000), NOW - 1)
    }
}

#[tokio::test]
async fn a_heartbeat_that_decides_to_skip_never_starts_a_turn() {
    let (chat, seen) = scripted_chat(vec![chat_result(vec![tool_call(
        "heartbeat",
        r#"{"action":"skip","reason":"Nothing is due."}"#,
    )])]);
    let h = harness_with(
        false,
        Some(chat),
        Some(file_returning(Ok("Deploy on Friday.".to_owned()))),
    );
    let created = h
        .jobs
        .create_job(&heartbeat_job("pulse", "TASK.md", None))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs[0].status, RunStatus::Skipped);
    assert_eq!(runs[0].skip_reason.as_deref(), Some("Nothing is due."));
    // No turn was driven, and no second provider request was made.
    assert!(h.frames.lock().is_empty());
    assert_eq!(seen.lock().len(), 1);
    assert_eq!(seen.lock()[0].tool.as_deref(), Some("heartbeat"));
    // The decision carries the clock's instant, so "tomorrow" means something.
    let darkwire_protocol::messages::ChatMessage::System(system) = &seen.lock()[0].messages[0]
    else {
        panic!("the first message should be the system one");
    };
    assert!(
        system.content.contains("2023-11-14T22:13:20.000Z"),
        "{}",
        system.content
    );
}

#[tokio::test]
async fn a_heartbeat_that_decides_to_run_drives_a_turn_and_then_evaluates_it() {
    let (chat, seen) = scripted_chat(vec![
        chat_result(vec![tool_call(
            "heartbeat",
            r#"{"action":"run","reason":"The deploy is due.","instruction":"Deploy it."}"#,
        )]),
        chat_result(vec![tool_call(
            "heartbeat_result",
            r#"{"notify":true,"title":"Deployed","summary":"All green."}"#,
        )]),
    ]);
    let h = harness_with(
        false,
        Some(chat),
        Some(file_returning(Ok("Deploy now.".to_owned()))),
    );
    let created = h
        .jobs
        .create_job(&heartbeat_job("pulse", "TASK.md", Some("tiny-model")))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs[0].status, RunStatus::Ok);
    assert_eq!(runs[0].output.as_deref(), Some("the answer"));

    // The turn carried the model's own phrasing.
    let frames = h.frames.lock();
    let ClientMessage::UserMessage(sent) = &frames[0] else {
        panic!("the first frame should be the user message");
    };
    assert_eq!(sent.content, "Deploy it.");
    drop(frames);

    // Both provider requests used the job's cheap model and its agent.
    let seen = seen.lock();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].model.as_deref(), Some("tiny-model"));
    assert_eq!(seen[0].agent_id.as_deref(), Some("watcher"));
    assert_eq!(seen[1].tool.as_deref(), Some("heartbeat_result"));
    assert_eq!(seen[1].model.as_deref(), Some("tiny-model"));

    let raised = h
        .notes
        .list(&darkwire_server::notifications::ListNotifications::default())
        .unwrap();
    assert_eq!(raised[0].title, "Deployed");
    assert_eq!(raised[0].body, "All green.");
}

#[tokio::test]
async fn an_evaluation_that_says_no_raises_nothing() {
    let (chat, _seen) = scripted_chat(vec![
        chat_result(vec![tool_call(
            "heartbeat",
            r#"{"action":"run","reason":"Due.","instruction":"Look."}"#,
        )]),
        chat_result(vec![tool_call(
            "heartbeat_result",
            r#"{"notify":false,"title":"Routine"}"#,
        )]),
    ]);
    let h = harness_with(
        false,
        Some(chat),
        Some(file_returning(Ok("Look.".to_owned()))),
    );
    let created = h
        .jobs
        .create_job(&heartbeat_job("pulse", "TASK.md", None))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    assert_eq!(
        h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap()[0].status,
        RunStatus::Ok
    );
    assert!(
        h.notes
            .list(&darkwire_server::notifications::ListNotifications::default())
            .unwrap()
            .is_empty()
    );
    assert!(h.broadcasts.lock().is_empty());
}

#[tokio::test]
async fn a_heartbeat_with_no_task_file_is_a_normal_idle_state_and_costs_nothing() {
    // A fresh install has no TASK.md, and paying a provider to be told so
    // every interval would be the wrong answer.
    let (chat, seen) = scripted_chat(Vec::new());
    let h = harness_with(
        false,
        Some(chat),
        Some(file_returning(Err(darkwire_core::WireError::new(
            darkwire_core::ErrorKind::NotFound,
            "no such file",
        )))),
    );
    let created = h
        .jobs
        .create_job(&heartbeat_job("pulse", "TASK.md", None))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs[0].status, RunStatus::Skipped);
    assert_eq!(
        runs[0].skip_reason.as_deref(),
        Some("No TASK.md in the workspace.")
    );
    assert!(seen.lock().is_empty(), "no provider call was made");
}

#[tokio::test]
async fn an_empty_task_file_is_a_skip_with_no_provider_call_either() {
    let (chat, seen) = scripted_chat(Vec::new());
    let h = harness_with(
        false,
        Some(chat),
        Some(file_returning(Ok("   \n  ".to_owned()))),
    );
    let created = h
        .jobs
        .create_job(&heartbeat_job("pulse", "NOTES.md", None))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs[0].status, RunStatus::Skipped);
    assert_eq!(runs[0].skip_reason.as_deref(), Some("NOTES.md is empty."));
    assert!(seen.lock().is_empty());
}

#[tokio::test]
async fn a_task_file_past_the_cap_is_truncated_and_the_run_says_so() {
    let (chat, seen) = scripted_chat(vec![chat_result(vec![tool_call(
        "heartbeat",
        r#"{"action":"skip","reason":"Nothing."}"#,
    )])]);
    let huge = "x".repeat(MAX_TASK_FILE_BYTES + 500);
    let h = harness_with(false, Some(chat), Some(file_returning(Ok(huge))));
    let created = h
        .jobs
        .create_job(&heartbeat_job("pulse", "BIG.md", None))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert!(
        runs[0].warnings.iter().any(|w| w.contains("was truncated")),
        "{:?}",
        runs[0].warnings
    );
    // The model saw exactly the cap, not the whole file.
    let seen = seen.lock();
    let darkwire_protocol::messages::ChatMessage::User(user) = &seen[0].messages[1] else {
        panic!("the second message should be the user one");
    };
    let darkwire_protocol::messages::ContentPart::Text(text) = &user.content[0] else {
        panic!("the user message should be text");
    };
    assert_eq!(text.text.matches('x').count(), MAX_TASK_FILE_BYTES);
}

#[tokio::test]
async fn a_provider_failure_during_the_decision_fails_the_run() {
    let (chat, _seen) = scripted_chat(Vec::new());
    let h = harness_with(
        false,
        Some(chat),
        Some(file_returning(Ok("Do something.".to_owned()))),
    );
    let created = h
        .jobs
        .create_job(&heartbeat_job("pulse", "TASK.md", None))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs[0].status, RunStatus::Error);
    assert!(
        runs[0]
            .error
            .as_deref()
            .unwrap()
            .contains("ran out of answers")
    );
}

#[tokio::test]
async fn a_read_failure_that_is_not_a_missing_file_fails_the_run() {
    let (chat, _seen) = scripted_chat(Vec::new());
    let h = harness_with(
        false,
        Some(chat),
        Some(file_returning(Err(darkwire_core::WireError::new(
            darkwire_core::ErrorKind::JailEscape,
            "that path is outside the workspace",
        )))),
    );
    let created = h
        .jobs
        .create_job(&heartbeat_job("pulse", "../etc/passwd", None))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    let runs = h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap();
    assert_eq!(runs[0].status, RunStatus::Error);
    assert!(
        runs[0]
            .error
            .as_deref()
            .unwrap()
            .contains("outside the workspace")
    );
}

#[tokio::test]
async fn a_heartbeat_on_a_build_with_a_file_but_no_provider_still_refuses_to_run() {
    let h = harness_with(false, None, Some(file_returning(Ok("Do it.".to_owned()))));
    let created = h
        .jobs
        .create_job(&heartbeat_job("pulse", "TASK.md", None))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    assert_eq!(
        h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap()[0].status,
        RunStatus::Error
    );
}

#[tokio::test]
async fn a_heartbeat_whose_turn_fails_is_never_evaluated() {
    let (chat, seen) = scripted_chat(vec![chat_result(vec![tool_call(
        "heartbeat",
        r#"{"action":"run","reason":"Due.","instruction":"Go."}"#,
    )])]);
    // The silent connection never answers, so the run times out.
    let h = harness_with(true, Some(chat), Some(file_returning(Ok("Go.".to_owned()))));
    let created = h
        .jobs
        .create_job(&heartbeat_job("pulse", "TASK.md", None))
        .unwrap();

    h.scheduler.start().unwrap();
    settle(&h).await;

    assert_eq!(
        h.jobs.list_runs(&created.id, &ListRuns::default()).unwrap()[0].status,
        RunStatus::Error
    );
    assert_eq!(seen.lock().len(), 1, "the evaluation never happened");
}

#[tokio::test]
async fn the_wait_loop_fires_a_job_whose_time_arrives() {
    // The one part of the engine that uses a real timer. Everything it decides
    // still comes from the injected clock.
    let h = harness(false);
    let created = h
        .jobs
        .create_job(&job("soon", every(1_000), NOW + 5))
        .unwrap();

    h.scheduler.start().unwrap();
    let engine = h.scheduler.clone();
    let loop_task = tokio::spawn(async move { engine.run().await });

    h.clock.advance(Duration::from_millis(10));
    settle(&h).await;

    assert_eq!(h.jobs.count_runs(&created.id).unwrap(), 1);
    h.scheduler.stop();
    loop_task.abort();
}

#[tokio::test]
async fn a_tick_says_how_many_runs_it_started() {
    let h = harness(true);
    h.config.lock().scheduler.concurrency = 1;
    h.jobs
        .create_job(&job("a", every(1_000), NOW + 1_000))
        .unwrap();
    h.jobs
        .create_job(&job("b", every(1_000), NOW + 1_001))
        .unwrap();
    h.scheduler.start().unwrap();

    h.clock.advance(Duration::from_secs(2));
    assert_eq!(h.scheduler.tick().unwrap(), 1);
    assert_eq!(h.scheduler.tick().unwrap(), 0, "no slot for the second");

    h.scheduler.stop();
    settle(&h).await;
}

// The loop gets a thread and a runtime of its own, so a loop that never
// yields fails the count below instead of starving the test.
#[test]
fn the_wait_loop_backs_off_while_what_is_due_cannot_start() {
    let h = harness(true);
    h.config.lock().scheduler.concurrency = 1;
    h.jobs
        .create_job(&job("a", every(1_000), NOW + 1_000))
        .unwrap();
    h.jobs
        .create_job(&job("b", every(1_000), NOW + 1_001))
        .unwrap();
    h.scheduler.start().unwrap();
    h.clock.advance(Duration::from_secs(2));

    // The silent connection holds "a" open, so "b" stays due with no slot.
    let engine = h.scheduler.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(engine.run());
        let _ = done_tx.send(());
    });
    for _ in 0..500 {
        if h.scheduler.in_flight() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(h.scheduler.in_flight(), 1);
    let before = h.config_reads.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(20));
    let passes = h.config_reads.load(Ordering::SeqCst) - before;

    h.scheduler.stop();
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("stop makes the loop return");
    assert!(
        passes < 50,
        "the loop spun: {passes} settings reads in 20 ms"
    );
}
