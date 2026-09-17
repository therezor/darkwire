//! The schema the Rust stores create is byte-for-byte the schema the
//! TypeScript stores created, and a database the TypeScript stores wrote opens
//! and reads back whole.
//!
//! `fixtures/sqlite` is written by the TypeScript suite: `sqlite_master.json`
//! is what SQLite stored after every store initialised a fresh file, `ddl.json`
//! is every statement the constructors executed, and `seed.db` is a populated
//! database with `seed.json` describing every row in it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{NOW, make_store_on};
use darkwire_core::Database;
use darkwire_core::ids::DEFAULT_WORKSPACE_ID;
use darkwire_core::paths::{ResolveWirePaths, WirePaths};
use darkwire_core::session_store::{self, ReadMessages, SessionStore};
use darkwire_core::testkit::ManualClock;
use darkwire_core::workspace_store::{self, WorkspaceStore};
use darkwire_protocol::messages::{ChatMessage, StopReason};
use rusqlite::types::ValueRef;
use serde_json::{Map, Value, json};

const TABLES: [&str; 4] = ["sessions", "messages", "turn_stats", "workspaces"];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sqlite")
}

fn fixture_json(name: &str) -> Value {
    let raw = std::fs::read_to_string(fixtures().join(name)).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn paths_in(root: &Path) -> WirePaths {
    let paths = WirePaths::resolve(ResolveWirePaths {
        root: Some(root.to_string_lossy().into_owned()),
        home: Some(root.to_path_buf()),
        ..ResolveWirePaths::default()
    })
    .unwrap();
    std::fs::create_dir_all(&paths.workspaces_dir).unwrap();
    paths
}

/// Both stores over `db`, the way the composition root builds them.
fn construct_stores(db: &Database, root: &Path) -> (SessionStore, WorkspaceStore) {
    let clock = Arc::new(ManualClock::at(NOW));
    let sessions = make_store_on(db.clone(), Arc::clone(&clock)).unwrap();
    let workspaces = WorkspaceStore::new(db.clone(), paths_in(root), clock).unwrap();
    (sessions, workspaces)
}

/// `sqlite_master` rows for the four tables and their indexes, as the fixture
/// records them. Auto-indexes carry no SQL and are not in the fixture.
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

fn all_ddl() -> Vec<&'static str> {
    session_store::SCHEMA
        .iter()
        .chain(workspace_store::SCHEMA)
        .copied()
        .chain(session_store::TURN_STATS_LEDGER.iter().map(|(_, ddl)| *ddl))
        .collect()
}

// the rules the consts must obey

#[test]
fn no_ddl_carries_a_comment() {
    // SQLite stores the text verbatim and `DROP COLUMN` rewrites it by byte
    // offset; a comment inside a column list can leave the schema unreadable.
    for ddl in all_ddl() {
        assert!(!ddl.contains("--"), "comment in DDL: {ddl}");
    }
}

#[test]
fn the_default_workspace_literal_is_the_default_workspace_id() {
    let literal = format!("DEFAULT '{DEFAULT_WORKSPACE_ID}'");
    assert!(session_store::SESSIONS_TABLE.contains(&literal));
    assert!(session_store::TURN_STATS_TABLE.contains(&literal));
    assert!(session_store::TURN_STATS_LEDGER[0].1.contains(&literal));
}

#[test]
fn the_ledger_names_exactly_the_five_columns_an_older_build_lacked() {
    let columns: Vec<&str> = session_store::TURN_STATS_LEDGER
        .iter()
        .map(|(column, _)| *column)
        .collect();
    assert_eq!(
        columns,
        [
            "workspace_id",
            "error",
            "generation_ms",
            "generation_tokens",
            "first_token_ms"
        ]
    );
    for (column, ddl) in session_store::TURN_STATS_LEDGER {
        assert!(ddl.starts_with(&format!("ALTER TABLE turn_stats ADD COLUMN {column} ")));
    }
}

// (a) a fresh database matches sqlite_master.json

#[test]
fn a_fresh_database_stores_the_same_schema_text_the_typescript_stores_did() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::in_memory().unwrap();
    let stores = construct_stores(&db, root.path());

    let expected: Vec<Value> = fixture_json("sqlite_master.json")["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| TABLES.contains(&row["tbl_name"].as_str().unwrap()))
        .cloned()
        .collect();
    let actual = schema_rows(&db);

    // Four tables and six indexes; the two `TEXT PRIMARY KEY` auto-indexes
    // carry no SQL.
    assert_eq!(actual.len(), 10, "{actual:#?}");
    assert_eq!(actual, expected);
    drop(stores);
}

#[test]
fn constructing_the_stores_twice_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::in_memory().unwrap();
    drop(construct_stores(&db, root.path()));
    let before = schema_rows(&db);
    drop(construct_stores(&db, root.path()));
    assert_eq!(schema_rows(&db), before);
}

// (b) seed.db, written by TypeScript, reads back whole

fn seed_copy(dir: &Path) -> PathBuf {
    let copy = dir.join("seed.db");
    std::fs::copy(fixtures().join("seed.db"), &copy).unwrap();
    copy
}

/// `SELECT *` as the fixture spells rows: integers as numbers, text as
/// strings, NULL as null.
fn table_rows(db: &Database, table: &str) -> Vec<Value> {
    let guard = db.lock();
    let mut statement = guard
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    statement
        .query_map([], |row| {
            let mut object = Map::new();
            for (index, name) in names.iter().enumerate() {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(f) => json!(f),
                    ValueRef::Text(bytes) => json!(String::from_utf8_lossy(bytes)),
                    ValueRef::Blob(bytes) => json!({ "hex": hex(bytes) }),
                };
                object.insert(name.clone(), value);
            }
            Ok(Value::Object(object))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[test]
fn the_seed_database_opens_and_every_row_of_the_four_tables_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&seed_copy(dir.path())).unwrap();
    // Running the constructors against an already-populated file is the
    // upgrade path: every CREATE is a no-op, the ledger finds every column
    // present, and the default workspace insert is ignored.
    let (sessions, workspaces) = construct_stores(&db, dir.path());

    let seed = fixture_json("seed.json");
    for table in TABLES {
        assert_eq!(
            table_rows(&db, table),
            seed["tables"][table].as_array().unwrap().clone(),
            "{table}"
        );
    }

    // And through the stores, not only the raw rows.
    let session = sessions.get_session("session-1").unwrap().unwrap();
    assert_eq!(session.title, "Fixture conversation");
    assert_eq!(session.workspace_id, "research");
    assert_eq!(session.agent_id.as_deref(), Some("default"));
    assert_eq!(session.metadata["source"], "fixture");
    assert_eq!(session.created_at_ms, 1_700_000_002_000);

    let messages = sessions
        .messages("session-1", &ReadMessages::default())
        .unwrap();
    assert_eq!(messages.len(), 4);
    let seqs: Vec<i64> = messages.iter().map(|m| m.seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4]);
    assert!(
        messages
            .iter()
            .all(|m| m.turn_id.as_deref() == Some("turn-1"))
    );
    let ChatMessage::Assistant(assistant) = &messages[1].message else {
        panic!("seq 2 should be the assistant's tool call");
    };
    assert_eq!(assistant.tool_calls[0].id, "call_1");
    assert_eq!(
        assistant.reasoning.as_deref(),
        Some("I should read the file.")
    );
    let ChatMessage::Tool(tool) = &messages[2].message else {
        panic!("seq 3 should be the tool result");
    };
    assert_eq!(tool.content, "hello");

    let stats = sessions.turn_stats("session-1", None).unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].provider, "ollama");
    assert_eq!(stats[0].stop_reason, StopReason::Complete);
    assert_eq!(stats[0].usage.cached_tokens, Some(100));
    assert_eq!(stats[0].generation_ms, Some(800));
    assert_eq!(stats[0].error, None);
    let usage = sessions.session_usage(&["session-1"]).unwrap();
    assert_eq!(usage["session-1"].total_tokens, 160);

    let listed = workspaces.list().unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, "default");
    assert_eq!(listed[1].id, "research");
    assert_eq!(listed[1].metadata["colour"], "teal");
    assert_eq!(listed[1].name, "Research");

    // The seed message count is what the listing reports.
    let summaries = sessions
        .list_sessions(&darkwire_core::session_store::ListSessions::default())
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].message_count, 4);
}

#[test]
fn the_seed_database_can_be_written_to_by_the_rust_stores() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&seed_copy(dir.path())).unwrap();
    let (sessions, _) = construct_stores(&db, dir.path());

    let record = sessions
        .append(
            "session-1",
            common::user_message("continued in Rust"),
            &darkwire_core::session_store::AppendOptions::default(),
        )
        .unwrap();
    // `next_seq` in the seed is 5.
    assert_eq!(record.seq, 5);
}

// (c) every statement ddl.json records is one this code executes

/// The `CREATE` statements in one fixture block, comments stripped.
fn statements_in(block: &str) -> Vec<String> {
    let without_comments: String = block
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    without_comments
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(str::to_owned)
        .collect()
}

fn normalised(ddl: &str) -> String {
    ddl.trim().trim_end_matches(';').to_owned()
}

#[test]
fn every_statement_the_typescript_constructors_ran_is_one_this_code_runs() {
    let ddl = fixture_json("ddl.json");

    let mut expected = Vec::new();
    for entry in ddl["statements"].as_array().unwrap() {
        let store = entry["store"].as_str().unwrap();
        let sql = entry["sql"].as_str().unwrap();
        match store {
            "SessionStore" | "WorkspaceStore" if sql.starts_with("PRAGMA") => {
                // The pragma belongs to `Database`, which every store shares.
                assert_eq!(sql, "PRAGMA foreign_keys = ON");
            }
            "SessionStore" | "WorkspaceStore" => expected.extend(statements_in(sql)),
            _ => {}
        }
    }
    let actual: Vec<String> = session_store::SCHEMA
        .iter()
        .chain(workspace_store::SCHEMA)
        .map(|ddl| normalised(ddl))
        .collect();
    assert_eq!(actual, expected);

    let ledger: Vec<&str> = ddl["ledger"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["store"] == "SessionStore")
        .map(|entry| entry["sql"].as_str().unwrap())
        .collect();
    let ours: Vec<&str> = session_store::TURN_STATS_LEDGER
        .iter()
        .map(|(_, ddl)| *ddl)
        .collect();
    assert_eq!(ours, ledger);
}

#[test]
fn the_foreign_keys_pragma_the_typescript_store_set_is_on_for_the_shared_connection() {
    let db = Database::in_memory().unwrap();
    let on: i64 = db
        .lock()
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(on, 1);
}
