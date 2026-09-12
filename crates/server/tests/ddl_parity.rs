//! The schema this crate's stores create is byte-for-byte the schema the
//! TypeScript stores created.
//!
//! `fixtures/sqlite` is written by the TypeScript suite: `sqlite_master.json`
//! is what SQLite stored after every store initialised a fresh file, and
//! `ddl.json` is every statement the constructors executed. An install that
//! upgrades in place opens the same file, so a difference of one byte here is
//! a table SQLite would refuse to recognise as the one it already has.
//!
//! This file covers the scheduler's three tables. The auth tables are checked
//! beside their own store.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ghostai_core::Database;
use ghostai_core::session_store::IdSource;
use ghostai_core::testkit::ManualClock;
use ghostai_server::automation_store::{self, AutomationStore};
use ghostai_server::notifications::{self, NotificationStore};
use serde_json::{Value, json};

const NOW: i64 = 1_700_000_000_000;

const TABLES: [&str; 3] = ["automation_jobs", "automation_runs", "notifications"];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sqlite")
}

fn fixture_json(name: &str) -> Value {
    let raw = std::fs::read_to_string(fixtures().join(name)).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn ids(prefix: &'static str) -> IdSource {
    use std::sync::atomic::{AtomicU64, Ordering};
    let n = AtomicU64::new(0);
    Box::new(move || format!("{prefix}{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

/// Both stores over `db`, the way the composition root builds them.
fn construct_stores(db: &Database) -> (AutomationStore, NotificationStore) {
    let clock = Arc::new(ManualClock::at(NOW));
    let jobs = AutomationStore::new(db.clone(), Arc::clone(&clock) as Arc<_>, ids("j")).unwrap();
    let notes = NotificationStore::new(db.clone(), clock, ids("n")).unwrap();
    (jobs, notes)
}

/// `sqlite_master` rows for the three tables and their indexes, as the fixture
/// records them.
fn schema_rows(db: &Database) -> Vec<Value> {
    let guard = db.lock();
    let mut statement = guard
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_master
              WHERE sql IS NOT NULL ORDER BY name",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok(json!({
                "type": row.get::<_, String>("type")?,
                "name": row.get::<_, String>("name")?,
                "tbl_name": row.get::<_, String>("tbl_name")?,
                "sql": row.get::<_, String>("sql")?,
            }))
        })
        .unwrap()
        .map(Result::unwrap)
        .filter(|row| TABLES.contains(&row["tbl_name"].as_str().unwrap()))
        .collect()
}

#[test]
fn a_fresh_database_stores_the_same_schema_text_the_typescript_stores_did() {
    let db = Database::in_memory().unwrap();
    let stores = construct_stores(&db);

    let expected: Vec<Value> = fixture_json("sqlite_master.json")["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| TABLES.contains(&row["tbl_name"].as_str().unwrap()))
        .cloned()
        .collect();
    let actual = schema_rows(&db);

    // Three tables and four indexes; the `TEXT PRIMARY KEY` auto-indexes carry
    // no SQL and are not in the fixture.
    assert_eq!(actual.len(), 7, "{actual:#?}");
    assert_eq!(actual, expected);
    drop(stores);
}

#[test]
fn constructing_the_stores_twice_changes_nothing() {
    let db = Database::in_memory().unwrap();
    drop(construct_stores(&db));
    let before = schema_rows(&db);
    drop(construct_stores(&db));
    assert_eq!(schema_rows(&db), before);
}

/// The `CREATE` statements in one fixture block, comments between statements
/// stripped. A comment *inside* a column list is part of the statement and
/// stays.
fn statements_in(block: &str) -> Vec<String> {
    block
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(|statement| {
            statement
                .lines()
                .skip_while(|line| line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|statement| !statement.is_empty())
        .collect()
}

fn normalised(ddl: &str) -> String {
    ddl.trim().trim_end_matches(';').to_owned()
}

#[test]
fn every_statement_the_typescript_constructors_ran_is_one_this_code_runs() {
    let ddl = fixture_json("ddl.json");

    for (store, ours) in [
        ("AutomationStore", automation_store::SCHEMA),
        ("NotificationStore", notifications::SCHEMA),
    ] {
        let mut expected = Vec::new();
        for entry in ddl["statements"].as_array().unwrap() {
            if entry["store"] != store {
                continue;
            }
            let sql = entry["sql"].as_str().unwrap();
            if sql.starts_with("PRAGMA") {
                // The pragma belongs to the shared connection, which every
                // store sets on open; the automation store repeats it because
                // its cascade is the only thing standing between deleting a job
                // and orphaning its run history.
                assert_eq!(sql, "PRAGMA foreign_keys = ON");
                continue;
            }
            expected.extend(statements_in(sql));
        }
        let actual: Vec<String> = ours.iter().map(|ddl| normalised(ddl)).collect();
        assert_eq!(actual, expected, "{store}");
    }
}

#[test]
fn the_created_by_ledger_names_exactly_the_two_columns_an_older_build_lacked() {
    let ddl = fixture_json("ddl.json");
    let ledger: Vec<&str> = ddl["ledger"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["store"] == "AutomationStore")
        .map(|entry| entry["sql"].as_str().unwrap())
        .collect();
    let ours: Vec<&str> = automation_store::CREATED_BY_LEDGER
        .iter()
        .map(|(_, ddl)| *ddl)
        .collect();
    assert_eq!(ours, ledger);

    let columns: Vec<&str> = automation_store::CREATED_BY_LEDGER
        .iter()
        .map(|(column, _)| *column)
        .collect();
    assert_eq!(columns, ["created_by_agent", "created_by_session"]);
}

#[test]
fn the_partial_index_is_the_one_the_timer_reads() {
    // The predicate is what makes the index small on a mature table: every
    // fired one-shot and every disabled job sits at `next_run_at_ms = 0`.
    assert!(
        automation_store::AUTOMATION_JOBS_DUE_INDEX
            .contains("WHERE enabled = 1 AND next_run_at_ms > 0")
    );
    assert!(
        notifications::NOTIFICATIONS_UNREAD_INDEX.contains("WHERE read_at_ms IS NULL"),
        "the bell's count reads the unread rows alone"
    );
}

#[test]
fn the_column_list_comment_is_the_one_sqlite_already_stored() {
    // The repository's rule is that no comment appears inside a column list,
    // because `ALTER TABLE DROP COLUMN` rewrites the stored text by byte
    // offset. This table is the exception on purpose: the TypeScript build
    // wrote the comment, SQLite kept it, and byte-identity with an existing
    // install outranks a rule about an operation nothing here performs.
    assert!(automation_store::AUTOMATION_JOBS_TABLE.contains("-- Who asked for this"));
    for ddl in [
        automation_store::AUTOMATION_RUNS_TABLE,
        notifications::NOTIFICATIONS_TABLE,
    ] {
        assert!(!ddl.contains("--"), "comment in DDL: {ddl}");
    }
}

#[test]
fn an_older_database_without_the_created_by_columns_gains_them() {
    let db = Database::in_memory().unwrap();
    // The table as a build that predates attribution created it.
    db.execute_batch(
        "CREATE TABLE automation_jobs (
           id               TEXT    PRIMARY KEY,
           name             TEXT    NOT NULL,
           schedule_json    TEXT    NOT NULL,
           payload_json     TEXT    NOT NULL,
           enabled          INTEGER NOT NULL DEFAULT 1,
           delete_after_run INTEGER NOT NULL DEFAULT 0,
           next_run_at_ms   INTEGER NOT NULL DEFAULT 0,
           last_run_at_ms   INTEGER NOT NULL DEFAULT 0,
           last_status      TEXT    NOT NULL DEFAULT 'pending',
           last_error       TEXT    NOT NULL DEFAULT '',
           run_count        INTEGER NOT NULL DEFAULT 0,
           created_at_ms    INTEGER NOT NULL,
           updated_at_ms    INTEGER NOT NULL
         ) STRICT;",
    )
    .unwrap();

    let clock = Arc::new(ManualClock::at(NOW));
    let store = AutomationStore::new(db.clone(), clock, ids("j")).unwrap();
    let columns = db.column_names("automation_jobs").unwrap();
    assert!(columns.iter().any(|name| name == "created_by_agent"));
    assert!(columns.iter().any(|name| name == "created_by_session"));
    // And the store reads through them rather than failing.
    assert_eq!(store.list_jobs().unwrap().len(), 0);
}
