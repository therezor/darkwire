//! The notification table: what it stores, what the bell counts, and how a
//! page is addressed.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ghostai_core::Database;
use ghostai_core::session_store::IdSource;
use ghostai_core::testkit::ManualClock;
use ghostai_protocol::ws::NotificationLevel;
use ghostai_server::notifications::{
    CreateNotificationInput, ListNotifications, NotificationAfter, NotificationStore,
};

const NOW: i64 = 1_700_000_000_000;

fn ids(prefix: &'static str) -> IdSource {
    let n = AtomicU64::new(0);
    Box::new(move || format!("{prefix}{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

fn store() -> (NotificationStore, Arc<ManualClock>, Database) {
    let clock = Arc::new(ManualClock::at(NOW));
    let db = Database::in_memory().unwrap();
    let store = NotificationStore::new(db.clone(), Arc::clone(&clock) as Arc<_>, ids("n")).unwrap();
    (store, clock, db)
}

fn input(title: &str) -> CreateNotificationInput {
    CreateNotificationInput {
        title: title.to_owned(),
        ..CreateNotificationInput::default()
    }
}

#[test]
fn a_new_notification_is_unread_and_carries_the_clocks_instant() {
    let (store, _clock, _db) = store();
    let raised = store.create(input("Run finished")).unwrap();

    assert_eq!(raised.id, "n1");
    assert_eq!(raised.title, "Run finished");
    assert_eq!(raised.body, "");
    assert_eq!(raised.level, NotificationLevel::Info);
    assert_eq!(raised.created_at_ms, u64::try_from(NOW).unwrap());
    assert_eq!(raised.read_at_ms, None);
    assert_eq!(store.get("n1").unwrap().unwrap(), raised);
}

#[test]
fn the_optional_columns_round_trip() {
    let (store, _clock, _db) = store();
    let raised = store
        .create(CreateNotificationInput {
            title: "Nightly digest".to_owned(),
            body: "Three things happened.".to_owned(),
            level: NotificationLevel::Warning,
            session_key: Some("session-1".to_owned()),
            job_id: Some("job-1".to_owned()),
        })
        .unwrap();

    let stored = store.get(&raised.id).unwrap().unwrap();
    assert_eq!(stored.body, "Three things happened.");
    assert_eq!(stored.level, NotificationLevel::Warning);
    assert_eq!(stored.session_key.as_deref(), Some("session-1"));
    assert_eq!(stored.job_id.as_deref(), Some("job-1"));
}

#[test]
fn a_missing_row_is_none_rather_than_an_error() {
    let (store, _clock, _db) = store();
    assert!(store.get("nope").unwrap().is_none());
}

#[test]
fn every_level_round_trips() {
    let (store, _clock, _db) = store();
    for level in [
        NotificationLevel::Info,
        NotificationLevel::Success,
        NotificationLevel::Warning,
        NotificationLevel::Error,
    ] {
        let raised = store
            .create(CreateNotificationInput {
                title: "x".to_owned(),
                level,
                ..CreateNotificationInput::default()
            })
            .unwrap();
        assert_eq!(store.get(&raised.id).unwrap().unwrap().level, level);
    }
}

#[test]
fn a_level_no_build_ever_wrote_reads_as_info_rather_than_failing() {
    // One bad row must not make the whole list unreadable.
    let (store, _clock, db) = store();
    db.lock()
        .execute(
            "INSERT INTO notifications (id, title, body, level, created_at_ms)
             VALUES ('odd', 'From the future', '', 'catastrophe', 1)",
            [],
        )
        .unwrap();
    assert_eq!(
        store.get("odd").unwrap().unwrap().level,
        NotificationLevel::Info
    );
}

#[test]
fn the_listing_is_newest_first_and_breaks_a_tie_on_the_id() {
    let (store, clock, _db) = store();
    store.create(input("first")).unwrap();
    // Same millisecond: the id decides, ascending.
    store.create(input("second")).unwrap();
    clock.advance(std::time::Duration::from_millis(10));
    store.create(input("third")).unwrap();

    let listed = store.list(&ListNotifications::default()).unwrap();
    let titles: Vec<&str> = listed.iter().map(|n| n.title.as_str()).collect();
    assert_eq!(titles, ["third", "first", "second"]);
}

#[test]
fn a_cursor_resumes_where_the_previous_page_ended() {
    let (store, clock, _db) = store();
    for index in 0..5 {
        store.create(input(&format!("n{index}"))).unwrap();
        clock.advance(std::time::Duration::from_millis(1));
    }

    let first = store
        .list(&ListNotifications {
            limit: Some(2),
            ..ListNotifications::default()
        })
        .unwrap();
    assert_eq!(first.len(), 2);

    let last = first.last().unwrap();
    let second = store
        .list(&ListNotifications {
            limit: Some(2),
            after: Some(NotificationAfter {
                created_at_ms: i64::try_from(last.created_at_ms).unwrap(),
                id: last.id.clone(),
            }),
            ..ListNotifications::default()
        })
        .unwrap();

    assert_eq!(second.len(), 2);
    // No overlap: the cursor is a position, not an offset.
    assert!(second.iter().all(|row| row.id != last.id));
    assert_eq!(second[0].title, "n2");
}

#[test]
fn an_offset_pages_for_a_numbered_pager() {
    let (store, clock, _db) = store();
    for index in 0..4 {
        store.create(input(&format!("n{index}"))).unwrap();
        clock.advance(std::time::Duration::from_millis(1));
    }

    let page = store
        .list(&ListNotifications {
            limit: Some(2),
            offset: Some(2),
            ..ListNotifications::default()
        })
        .unwrap();
    let titles: Vec<&str> = page.iter().map(|n| n.title.as_str()).collect();
    assert_eq!(titles, ["n1", "n0"]);
}

#[test]
fn unread_only_filters_the_page_and_the_count_together() {
    let (store, _clock, _db) = store();
    let read = store.create(input("seen")).unwrap();
    store.create(input("new")).unwrap();
    store.mark_read(&read.id).unwrap();

    let listed = store
        .list(&ListNotifications {
            unread_only: true,
            ..ListNotifications::default()
        })
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "new");

    assert_eq!(store.count(false).unwrap(), 2);
    assert_eq!(store.count(true).unwrap(), 1);
    assert_eq!(store.unread_count().unwrap(), 1);
}

#[test]
fn marking_read_twice_keeps_the_first_time_it_was_seen() {
    let (store, clock, _db) = store();
    let raised = store.create(input("once")).unwrap();

    clock.advance(std::time::Duration::from_millis(50));
    let first = store.mark_read(&raised.id).unwrap().unwrap();
    assert_eq!(first.read_at_ms, Some(u64::try_from(NOW + 50).unwrap()));

    clock.advance(std::time::Duration::from_millis(500));
    let again = store.mark_read(&raised.id).unwrap().unwrap();
    assert_eq!(again.read_at_ms, first.read_at_ms);
}

#[test]
fn marking_one_read_that_does_not_exist_answers_none() {
    let (store, _clock, _db) = store();
    assert!(store.mark_read("nope").unwrap().is_none());
}

#[test]
fn marking_all_read_reports_how_many_moved_and_is_idempotent() {
    let (store, _clock, _db) = store();
    store.create(input("a")).unwrap();
    store.create(input("b")).unwrap();

    assert_eq!(store.mark_all_read().unwrap(), 2);
    assert_eq!(store.unread_count().unwrap(), 0);
    assert_eq!(store.mark_all_read().unwrap(), 0);
}

#[test]
fn delete_reports_whether_there_was_anything_to_delete() {
    let (store, _clock, _db) = store();
    let raised = store.create(input("gone")).unwrap();
    assert!(store.delete(&raised.id).unwrap());
    assert!(!store.delete(&raised.id).unwrap());
    assert!(store.get(&raised.id).unwrap().is_none());
}

#[test]
fn delete_all_takes_the_unread_ones_too() {
    // A "delete all" that quietly kept the unread ones would leave the bell
    // still counting after the list looked empty.
    let (store, _clock, _db) = store();
    let read = store.create(input("seen")).unwrap();
    store.create(input("new")).unwrap();
    store.mark_read(&read.id).unwrap();

    assert_eq!(store.delete_all().unwrap(), 2);
    assert_eq!(store.count(false).unwrap(), 0);
    assert_eq!(store.unread_count().unwrap(), 0);
}

#[test]
fn a_second_store_over_the_same_connection_sees_the_same_rows() {
    let (store, clock, db) = store();
    store.create(input("shared")).unwrap();

    let other = NotificationStore::new(db, clock, ids("m")).unwrap();
    assert_eq!(other.count(false).unwrap(), 1);
}
