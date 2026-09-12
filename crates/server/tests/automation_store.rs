//! Jobs, runs, and the reads the timer makes.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ghostai_core::Database;
use ghostai_core::session_store::IdSource;
use ghostai_core::testkit::ManualClock;
use ghostai_protocol::automation::{
    AtKind, AtSchedule, AutomationJobCreator, AutomationPayload, AutomationSchedule, CronKind,
    CronSchedule, EveryKind, EverySchedule, RunStatus, ScheduledKind, ScheduledPayload,
};
use ghostai_server::automation_store::{
    AutomationStore, CreateJobInput, FinishRunInput, ListRuns, RunAfter, RunOutcome, UpdateJobInput,
};
use indexmap::IndexMap;

const NOW: i64 = 1_700_000_000_000;

fn ids(prefix: &'static str) -> IdSource {
    let n = AtomicU64::new(0);
    Box::new(move || format!("{prefix}{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

fn store() -> (AutomationStore, Arc<ManualClock>, Database) {
    let clock = Arc::new(ManualClock::at(NOW));
    let db = Database::in_memory().unwrap();
    let store = AutomationStore::new(db.clone(), Arc::clone(&clock) as Arc<_>, ids("j")).unwrap();
    (store, clock, db)
}

fn every(ms: u64) -> AutomationSchedule {
    AutomationSchedule::Every(EverySchedule {
        kind: EveryKind,
        every_ms: ms,
    })
}

fn at(ms: u64) -> AutomationSchedule {
    AutomationSchedule::At(AtSchedule {
        kind: AtKind,
        at_ms: ms,
    })
}

fn cron(expr: &str) -> AutomationSchedule {
    AutomationSchedule::Cron(CronSchedule {
        kind: CronKind,
        expr: expr.to_owned(),
    })
}

fn message(text: &str) -> AutomationPayload {
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
        payload: message("do the thing"),
        enabled: true,
        delete_after_run: false,
        next_run_at_ms,
        created_by: None,
    }
}

// Jobs

#[test]
fn a_created_job_reads_back_whole() {
    let (store, _clock, _db) = store();
    let created = store
        .create_job(&job("nightly", cron("0 9 * * *"), NOW + 1000))
        .unwrap();

    assert_eq!(created.id, "j1");
    assert_eq!(created.name, "nightly");
    assert_eq!(created.schedule, cron("0 9 * * *"));
    assert!(created.enabled);
    assert!(!created.delete_after_run);
    assert_eq!(created.created_at_ms, u64::try_from(NOW).unwrap());
    assert_eq!(created.updated_at_ms, u64::try_from(NOW).unwrap());
    assert_eq!(
        created.state.next_run_at_ms,
        u64::try_from(NOW + 1000).unwrap()
    );
    assert_eq!(created.state.last_status, RunStatus::Pending);
    assert_eq!(created.state.run_count, 0);
    assert_eq!(created.created_by, None);

    assert_eq!(store.get_job("j1").unwrap().unwrap(), created);
}

#[test]
fn attribution_is_absent_when_the_operator_made_it_and_present_when_an_agent_did() {
    let (store, _clock, _db) = store();
    let mine = store
        .create_job(&CreateJobInput {
            created_by: Some(AutomationJobCreator {
                agent_id: "researcher".to_owned(),
                session_key: "session-1".to_owned(),
            }),
            ..job("agent job", every(60_000), NOW)
        })
        .unwrap();
    let theirs = store
        .create_job(&job("panel job", every(60_000), NOW))
        .unwrap();

    assert_eq!(
        mine.created_by,
        Some(AutomationJobCreator {
            agent_id: "researcher".to_owned(),
            session_key: "session-1".to_owned(),
        })
    );
    assert_eq!(theirs.created_by, None);
    assert_eq!(store.count_jobs_by("researcher").unwrap(), 1);
    assert_eq!(store.count_jobs_by("").unwrap(), 1);
    assert_eq!(store.count_jobs_by("nobody").unwrap(), 0);
}

#[test]
fn the_listing_is_newest_first() {
    let (store, clock, _db) = store();
    store.create_job(&job("first", every(1000), NOW)).unwrap();
    clock.advance(Duration::from_millis(5));
    store.create_job(&job("second", every(1000), NOW)).unwrap();

    let names: Vec<String> = store
        .list_jobs()
        .unwrap()
        .into_iter()
        .map(|job| job.name)
        .collect();
    assert_eq!(names, ["second", "first"]);
}

#[test]
fn an_update_touches_only_what_it_names() {
    let (store, clock, _db) = store();
    let created = store.create_job(&job("before", every(1000), NOW)).unwrap();

    clock.advance(Duration::from_millis(7));
    let updated = store
        .update_job(
            &created.id,
            &UpdateJobInput {
                name: Some("after".to_owned()),
                enabled: Some(false),
                ..UpdateJobInput::default()
            },
        )
        .unwrap()
        .unwrap();

    assert_eq!(updated.name, "after");
    assert!(!updated.enabled);
    // Untouched.
    assert_eq!(updated.schedule, every(1000));
    assert_eq!(updated.created_at_ms, created.created_at_ms);
    assert_eq!(updated.updated_at_ms, u64::try_from(NOW + 7).unwrap());
}

#[test]
fn updating_a_job_that_is_not_there_answers_none() {
    let (store, _clock, _db) = store();
    assert!(
        store
            .update_job("nope", &UpdateJobInput::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_schedule_and_payload_swap_round_trip_as_their_own_variants() {
    let (store, _clock, _db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();

    let updated = store
        .update_job(
            &created.id,
            &UpdateJobInput {
                schedule: Some(at(NOW as u64 + 5_000)),
                payload: Some(message("something else")),
                ..UpdateJobInput::default()
            },
        )
        .unwrap()
        .unwrap();

    assert_eq!(updated.schedule, at(NOW as u64 + 5_000));
    let AutomationPayload::Scheduled(payload) = &updated.payload else {
        panic!("the payload should still be a scheduled message");
    };
    assert_eq!(payload.message, "something else");
}

#[test]
fn deleting_a_job_reports_whether_there_was_one_and_takes_its_runs() {
    let (store, _clock, _db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();
    store.start_run(&created.id, None).unwrap();
    assert_eq!(store.count_runs(&created.id).unwrap(), 1);

    assert!(store.delete_job(&created.id).unwrap());
    assert!(!store.delete_job(&created.id).unwrap());
    // The cascade, which is why the store sets the pragma itself.
    assert_eq!(store.count_runs(&created.id).unwrap(), 0);
}

// Scheduling reads

#[test]
fn the_earliest_due_instant_ignores_disabled_and_unscheduled_rows() {
    let (store, _clock, _db) = store();
    assert_eq!(store.earliest_due_ms().unwrap(), None);

    store
        .create_job(&job("late", every(1000), NOW + 5_000))
        .unwrap();
    store
        .create_job(&job("soon", every(1000), NOW + 1_000))
        .unwrap();
    // Unscheduled.
    store.create_job(&job("never", every(1000), 0)).unwrap();
    // Disabled.
    store
        .create_job(&CreateJobInput {
            enabled: false,
            ..job("off", every(1000), NOW + 1)
        })
        .unwrap();

    assert_eq!(store.earliest_due_ms().unwrap(), Some(NOW + 1_000));
}

#[test]
fn due_jobs_are_soonest_first_and_bounded_by_the_limit() {
    let (store, _clock, _db) = store();
    store.create_job(&job("c", every(1000), NOW - 1)).unwrap();
    store.create_job(&job("a", every(1000), NOW - 3)).unwrap();
    store.create_job(&job("b", every(1000), NOW - 2)).unwrap();
    // Not yet.
    store
        .create_job(&job("future", every(1000), NOW + 10))
        .unwrap();

    let due: Vec<String> = store
        .due_jobs(NOW, 2)
        .unwrap()
        .into_iter()
        .map(|job| job.name)
        .collect();
    assert_eq!(due, ["a", "b"]);

    assert!(store.due_jobs(NOW, 0).unwrap().is_empty());
    assert!(store.due_jobs(NOW, -1).unwrap().is_empty());
}

#[test]
fn a_job_whose_stored_shape_does_not_parse_is_disabled_rather_than_skipped() {
    // A schedule nobody can read is a schedule nobody can honour; quietly
    // passing over it produces a job that shows in the panel and never fires.
    let (store, _clock, db) = store();
    let created = store
        .create_job(&job("broken", every(1000), NOW - 1))
        .unwrap();
    db.lock()
        .execute(
            "UPDATE automation_jobs SET schedule_json = '{\"kind\":\"cron\"}' WHERE id = ?",
            [&created.id],
        )
        .unwrap();

    assert!(store.due_jobs(NOW, 10).unwrap().is_empty());

    let row: (i64, i64, String, String) = db
        .lock()
        .query_row(
            "SELECT enabled, next_run_at_ms, last_status, last_error FROM automation_jobs WHERE id = ?",
            [&created.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(row.0, 0, "switched off");
    assert_eq!(row.1, 0, "unscheduled");
    assert_eq!(row.2, "error");
    assert!(row.3.contains("does not parse"), "{}", row.3);
}

#[test]
fn a_job_whose_shape_does_not_parse_is_dropped_from_the_listing_not_fatal_to_it() {
    // One bad row must not blank the panel.
    let (store, _clock, db) = store();
    let broken = store.create_job(&job("broken", every(1000), NOW)).unwrap();
    store.create_job(&job("fine", every(1000), NOW)).unwrap();
    db.lock()
        .execute(
            "UPDATE automation_jobs SET payload_json = 'not json' WHERE id = ?",
            [&broken.id],
        )
        .unwrap();

    let names: Vec<String> = store
        .list_jobs()
        .unwrap()
        .into_iter()
        .map(|job| job.name)
        .collect();
    assert_eq!(names, ["fine"]);
    assert!(store.get_job(&broken.id).unwrap().is_none());
}

#[test]
fn missed_jobs_are_every_due_row_with_no_ceiling() {
    let (store, _clock, _db) = store();
    for index in 0..5 {
        store
            .create_job(&job(&format!("j{index}"), every(1000), NOW - 10 - index))
            .unwrap();
    }
    assert_eq!(store.missed_jobs(NOW).unwrap().len(), 5);
}

#[test]
fn setting_a_next_run_moves_the_row_and_its_updated_stamp() {
    let (store, clock, _db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();

    clock.advance(Duration::from_millis(3));
    store.set_next_run(&created.id, NOW + 99).unwrap();

    let reread = store.get_job(&created.id).unwrap().unwrap();
    assert_eq!(
        reread.state.next_run_at_ms,
        u64::try_from(NOW + 99).unwrap()
    );
    assert_eq!(reread.updated_at_ms, u64::try_from(NOW + 3).unwrap());
}

#[test]
fn recording_an_outcome_folds_it_into_the_jobs_state() {
    let (store, _clock, _db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();

    store
        .record_outcome(
            &created.id,
            &RunOutcome {
                ran_at_ms: NOW + 5,
                status: RunStatus::Error,
                error: Some("it broke".to_owned()),
            },
        )
        .unwrap();

    let after = store.get_job(&created.id).unwrap().unwrap();
    assert_eq!(after.state.last_run_at_ms, u64::try_from(NOW + 5).unwrap());
    assert_eq!(after.state.last_status, RunStatus::Error);
    assert_eq!(after.state.last_error, "it broke");
    assert_eq!(after.state.run_count, 1);

    store
        .record_outcome(
            &created.id,
            &RunOutcome {
                ran_at_ms: NOW + 9,
                status: RunStatus::Ok,
                error: None,
            },
        )
        .unwrap();
    let again = store.get_job(&created.id).unwrap().unwrap();
    assert_eq!(again.state.run_count, 2);
    assert_eq!(again.state.last_error, "", "a clean run clears the message");
}

// Runs

#[test]
fn a_run_starts_pending_and_finishes_with_what_it_produced() {
    let (store, clock, _db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();

    let run = store
        .start_run(&created.id, Some("session-9".to_owned()))
        .unwrap();
    assert_eq!(run.status, RunStatus::Pending);
    assert_eq!(run.session_key.as_deref(), Some("session-9"));
    assert_eq!(run.finished_at_ms, None);
    assert!(run.warnings.is_empty());

    clock.advance(Duration::from_millis(20));
    let finished = store
        .finish_run(
            &run.id,
            &FinishRunInput {
                status: RunStatus::Ok,
                output: Some("done".to_owned()),
                warnings: vec!["a caveat".to_owned()],
                ..FinishRunInput::default()
            },
        )
        .unwrap()
        .unwrap();

    assert_eq!(finished.status, RunStatus::Ok);
    assert_eq!(finished.output.as_deref(), Some("done"));
    assert_eq!(finished.warnings, ["a caveat"]);
    assert_eq!(
        finished.finished_at_ms,
        Some(u64::try_from(NOW + 20).unwrap())
    );
    assert_eq!(store.get_run(&run.id).unwrap().unwrap(), finished);
}

#[test]
fn a_skipped_run_carries_its_reason_and_a_failed_one_its_error() {
    let (store, _clock, _db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();

    let skipped = store.start_run(&created.id, None).unwrap();
    let skipped = store
        .finish_run(
            &skipped.id,
            &FinishRunInput {
                status: RunStatus::Skipped,
                skip_reason: Some("Nothing due.".to_owned()),
                ..FinishRunInput::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(skipped.skip_reason.as_deref(), Some("Nothing due."));
    assert_eq!(skipped.error, None);

    let failed = store.start_run(&created.id, None).unwrap();
    let failed = store
        .finish_run(
            &failed.id,
            &FinishRunInput {
                status: RunStatus::Error,
                error: Some("boom".to_owned()),
                ..FinishRunInput::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(failed.error.as_deref(), Some("boom"));
}

#[test]
fn finishing_a_run_that_is_not_there_answers_none() {
    let (store, _clock, _db) = store();
    assert!(
        store
            .finish_run("nope", &FinishRunInput::default())
            .unwrap()
            .is_none()
    );
    assert!(store.get_run("nope").unwrap().is_none());
}

#[test]
fn a_status_no_build_ever_wrote_reads_as_pending() {
    let (store, _clock, db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();
    let run = store.start_run(&created.id, None).unwrap();
    db.lock()
        .execute(
            "UPDATE automation_runs SET status = 'exploded' WHERE id = ?",
            [&run.id],
        )
        .unwrap();
    assert_eq!(
        store.get_run(&run.id).unwrap().unwrap().status,
        RunStatus::Pending
    );
}

#[test]
fn unreadable_warnings_read_as_none_rather_than_failing_the_row() {
    let (store, _clock, db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();
    let run = store.start_run(&created.id, None).unwrap();
    db.lock()
        .execute(
            "UPDATE automation_runs SET warnings_json = '{\"not\":\"an array\"}' WHERE id = ?",
            [&run.id],
        )
        .unwrap();
    assert!(store.get_run(&run.id).unwrap().unwrap().warnings.is_empty());
}

#[test]
fn a_run_page_is_newest_first_and_resumes_from_a_cursor() {
    let (store, clock, _db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();
    let mut runs = Vec::new();
    for _ in 0..5 {
        runs.push(store.start_run(&created.id, None).unwrap());
        clock.advance(Duration::from_millis(1));
    }

    let first = store
        .list_runs(
            &created.id,
            &ListRuns {
                limit: Some(2),
                ..ListRuns::default()
            },
        )
        .unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].id, runs[4].id, "newest first");

    let last = first.last().unwrap();
    let second = store
        .list_runs(
            &created.id,
            &ListRuns {
                limit: Some(2),
                after: Some(RunAfter {
                    started_at_ms: i64::try_from(last.started_at_ms).unwrap(),
                    id: last.id.clone(),
                }),
                ..ListRuns::default()
            },
        )
        .unwrap();
    assert_eq!(second.len(), 2);
    assert_eq!(second[0].id, runs[2].id);

    let offset = store
        .list_runs(
            &created.id,
            &ListRuns {
                limit: Some(2),
                offset: Some(2),
                ..ListRuns::default()
            },
        )
        .unwrap();
    assert_eq!(offset[0].id, runs[2].id);
}

#[test]
fn a_run_listing_sees_only_its_own_job() {
    let (store, _clock, _db) = store();
    let a = store.create_job(&job("a", every(1000), NOW)).unwrap();
    let b = store.create_job(&job("b", every(1000), NOW)).unwrap();
    store.start_run(&a.id, None).unwrap();
    store.start_run(&b.id, None).unwrap();
    store.start_run(&b.id, None).unwrap();

    assert_eq!(store.count_runs(&a.id).unwrap(), 1);
    assert_eq!(store.count_runs(&b.id).unwrap(), 2);
    assert_eq!(
        store.list_runs(&a.id, &ListRuns::default()).unwrap().len(),
        1
    );
}

#[test]
fn trimming_holds_each_job_to_its_own_ceiling() {
    // A five-minute job's afternoon must not evict a nightly job's year.
    let (store, clock, _db) = store();
    let busy = store.create_job(&job("busy", every(1000), NOW)).unwrap();
    let sparse = store
        .create_job(&job("sparse", every(86_400_000), NOW))
        .unwrap();

    let mut busy_runs = Vec::new();
    for index in 0..5 {
        busy_runs.push(
            store
                .start_run(&busy.id, Some(format!("session-{index}")))
                .unwrap(),
        );
        clock.advance(Duration::from_millis(1));
    }
    store.start_run(&sparse.id, None).unwrap();

    let trimmed = store.trim_runs(&busy.id, 2).unwrap();
    assert_eq!(trimmed.len(), 3);
    // The oldest three went, and each named the session it was the only
    // reference to.
    let gone: Vec<&str> = trimmed.iter().map(|run| run.id.as_str()).collect();
    assert!(gone.contains(&busy_runs[0].id.as_str()));
    assert!(gone.contains(&busy_runs[2].id.as_str()));
    assert!(!gone.contains(&busy_runs[4].id.as_str()));
    // Each names the session it was the only reference to. The selection has
    // no ORDER BY — it is a set, and the caller deletes every one of them.
    let mut sessions: Vec<&str> = trimmed
        .iter()
        .filter_map(|run| run.session_key.as_deref())
        .collect();
    sessions.sort_unstable();
    assert_eq!(sessions, ["session-0", "session-1", "session-2"]);

    assert_eq!(store.count_runs(&busy.id).unwrap(), 2);
    assert_eq!(store.count_runs(&sparse.id).unwrap(), 1, "untouched");

    // Nothing to do the second time.
    assert!(store.trim_runs(&busy.id, 2).unwrap().is_empty());
}

#[test]
fn boot_closes_out_runs_a_dead_process_left_pending() {
    let (store, clock, _db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();
    let abandoned = store.start_run(&created.id, None).unwrap();
    let settled = store.start_run(&created.id, None).unwrap();
    store
        .finish_run(
            &settled.id,
            &FinishRunInput {
                status: RunStatus::Ok,
                ..FinishRunInput::default()
            },
        )
        .unwrap();

    clock.advance(Duration::from_millis(100));
    assert_eq!(store.reconcile_pending("Interrupted.").unwrap(), 1);

    let closed = store.get_run(&abandoned.id).unwrap().unwrap();
    assert_eq!(closed.status, RunStatus::Error);
    assert_eq!(closed.error.as_deref(), Some("Interrupted."));
    assert_eq!(
        closed.finished_at_ms,
        Some(u64::try_from(NOW + 100).unwrap())
    );
    // The one that had already finished is untouched.
    assert_eq!(
        store.get_run(&settled.id).unwrap().unwrap().status,
        RunStatus::Ok
    );
}

// Opening an older database

#[test]
fn a_legacy_per_job_timezone_is_stripped_on_open() {
    // A cron schedule refuses unknown keys, so a row still carrying `tz` does
    // not parse — and every job in the listing would be dropped because one of
    // them is old.
    let (store, clock, db) = store();
    let created = store
        .create_job(&job("old", cron("0 9 * * *"), NOW))
        .unwrap();
    db.lock()
        .execute(
            r#"UPDATE automation_jobs SET schedule_json = '{"kind":"cron","expr":"0 9 * * *","tz":"Europe/Kyiv"}' WHERE id = ?"#,
            [&created.id],
        )
        .unwrap();
    // Before the sweep the row is unreadable.
    assert!(store.get_job(&created.id).unwrap().is_none());

    let reopened = AutomationStore::new(db, clock, ids("k")).unwrap();
    let job = reopened.get_job(&created.id).unwrap().unwrap();
    assert_eq!(job.schedule, cron("0 9 * * *"));
}

#[test]
fn a_row_whose_schedule_is_not_json_is_left_for_the_schema_to_report() {
    let (store, clock, db) = store();
    let created = store.create_job(&job("x", every(1000), NOW)).unwrap();
    db.lock()
        .execute(
            r#"UPDATE automation_jobs SET schedule_json = 'this is not json but mentions "tz"' WHERE id = ?"#,
            [&created.id],
        )
        .unwrap();

    // The sweep leaves it alone rather than inventing a rewrite.
    let reopened = AutomationStore::new(db.clone(), clock, ids("k")).unwrap();
    assert!(reopened.get_job(&created.id).unwrap().is_none());
    let stored: String = db
        .lock()
        .query_row(
            "SELECT schedule_json FROM automation_jobs WHERE id = ?",
            [&created.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored.starts_with("this is not json"));
}
