//! `Database`: opening, the pragmas, transactions and re-entrancy.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::thread;

use darkwire_core::{Database, ErrorKind, WireError};

fn pragma(db: &Database, name: &str) -> String {
    db.lock()
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, String>(0))
        .unwrap()
}

fn pragma_int(db: &Database, name: &str) -> i64 {
    db.lock()
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, i64>(0))
        .unwrap()
}

fn count(db: &Database) -> i64 {
    db.lock()
        .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn open_creates_the_directory_and_sets_the_pragmas() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("nested").join("deeper").join("darkwire.db");

    let db = Database::open(&file).unwrap();

    assert!(file.is_file());
    assert_eq!(pragma(&db, "journal_mode"), "wal");
    // NORMAL is 1.
    assert_eq!(pragma_int(&db, "synchronous"), 1);
    assert_eq!(pragma_int(&db, "busy_timeout"), 5000);
    assert_eq!(pragma_int(&db, "foreign_keys"), 1);
}

#[cfg(unix)]
#[test]
fn open_creates_the_directory_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("private").join("darkwire.db");
    Database::open(&file).unwrap();

    let mode = std::fs::metadata(dir.path().join("private"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn open_fails_where_the_directory_cannot_be_created() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, "not a directory").unwrap();

    let error = Database::open(&blocker.join("darkwire.db")).unwrap_err();
    assert!(matches!(
        error.kind,
        ErrorKind::Storage | ErrorKind::NotFound
    ));
}

#[test]
fn in_memory_turns_foreign_keys_on() {
    let db = Database::in_memory().unwrap();
    assert_eq!(pragma_int(&db, "foreign_keys"), 1);
    assert_eq!(pragma(&db, "journal_mode"), "memory");
}

#[test]
fn foreign_keys_cascade_because_the_pragma_is_on() {
    let db = Database::in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE parent (id INTEGER PRIMARY KEY);
         CREATE TABLE child (parent_id INTEGER REFERENCES parent(id) ON DELETE CASCADE);
         INSERT INTO parent VALUES (1);
         INSERT INTO child VALUES (1);
         DELETE FROM parent;",
    )
    .unwrap();
    let children: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM child", [], |row| row.get(0))
        .unwrap();
    assert_eq!(children, 0);
}

#[test]
fn clones_share_one_connection() {
    let db = Database::in_memory().unwrap();
    let other = db.clone();
    db.execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
        .unwrap();
    assert_eq!(count(&other), 1);
    assert_eq!(format!("{db:?}"), "Database");
}

#[test]
fn a_transaction_commits_when_the_closure_succeeds() {
    let db = Database::in_memory().unwrap();
    db.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();

    let inserted = db
        .transaction(|conn| {
            conn.execute("INSERT INTO t VALUES (1)", [])?;
            conn.execute("INSERT INTO t VALUES (2)", [])?;
            Ok(2)
        })
        .unwrap();

    assert_eq!(inserted, 2);
    assert_eq!(count(&db), 2);
    // Nothing is left open: a fresh BEGIN would fail inside a transaction.
    db.execute_batch("BEGIN; COMMIT;").unwrap();
}

#[test]
fn a_transaction_rolls_back_when_the_closure_fails() {
    let db = Database::in_memory().unwrap();
    db.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();

    let error = db
        .transaction(|conn| {
            conn.execute("INSERT INTO t VALUES (1)", [])?;
            Err::<(), _>(WireError::new(ErrorKind::Conflict, "changed my mind"))
        })
        .unwrap_err();

    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(count(&db), 0);
    // The rollback closed the transaction.
    db.execute_batch("BEGIN; COMMIT;").unwrap();
}

#[test]
fn a_transaction_rolls_back_when_a_statement_fails() {
    let db = Database::in_memory().unwrap();
    db.execute_batch("CREATE TABLE t (x INTEGER PRIMARY KEY)")
        .unwrap();

    let error = db
        .transaction(|conn| {
            conn.execute("INSERT INTO t VALUES (1)", [])?;
            conn.execute("INSERT INTO t VALUES (1)", [])?;
            Ok(())
        })
        .unwrap_err();

    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(count(&db), 0);
}

#[test]
fn a_nested_transaction_joins_the_outer_one() {
    let db = Database::in_memory().unwrap();
    db.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();

    let error = db
        .transaction(|conn| {
            conn.execute("INSERT INTO t VALUES (1)", [])?;
            // The inner call neither begins nor commits: its write belongs to
            // the outer transaction and is undone with it.
            db.transaction(|inner| {
                inner.execute("INSERT INTO t VALUES (2)", [])?;
                Ok(())
            })?;
            assert_eq!(count(&db), 2);
            Err::<(), _>(WireError::new(ErrorKind::Aborted, "outer gave up"))
        })
        .unwrap_err();

    assert_eq!(error.kind, ErrorKind::Aborted);
    assert_eq!(count(&db), 0);
}

#[test]
fn an_inner_failure_does_not_end_the_outer_transaction() {
    let db = Database::in_memory().unwrap();
    db.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();

    db.transaction(|conn| {
        conn.execute("INSERT INTO t VALUES (1)", [])?;
        let inner = db.transaction(|_| Err::<(), _>(WireError::new(ErrorKind::Tool, "inner")));
        assert_eq!(inner.unwrap_err().kind, ErrorKind::Tool);
        conn.execute("INSERT INTO t VALUES (2)", [])?;
        Ok(())
    })
    .unwrap();

    assert_eq!(count(&db), 2);
}

#[test]
fn the_lock_is_re_entrant_from_within_a_transaction() {
    let db = Database::in_memory().unwrap();
    db.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();

    db.transaction(|conn| {
        conn.execute("INSERT INTO t VALUES (1)", [])?;
        // A store method locking again on the same thread must not deadlock.
        let guard = db.lock();
        let seen: i64 = guard.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?;
        assert_eq!(seen, 1);
        db.execute_batch("INSERT INTO t VALUES (2)")?;
        Ok(())
    })
    .unwrap();

    assert_eq!(count(&db), 2);
}

#[test]
fn another_thread_waits_its_turn() {
    let db = Database::in_memory().unwrap();
    db.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();

    let (locked, saw_lock) = std::sync::mpsc::channel::<i64>();
    let outcome = db.transaction(|conn| {
        conn.execute("INSERT INTO t VALUES (1)", [])?;
        let other = {
            let db = db.clone();
            thread::spawn(move || {
                // Blocks until the transaction below commits and releases the
                // lock, so what it sees is the committed row.
                let seen = count(&db);
                locked.send(seen).unwrap();
            })
        };
        // While this thread holds the lock the other cannot have reported in,
        // however long it has been trying.
        thread::sleep(std::time::Duration::from_millis(20));
        assert!(saw_lock.try_recv().is_err());
        Ok(other)
    });

    let other = outcome.unwrap();
    other.join().unwrap();
    assert_eq!(saw_lock.recv().unwrap(), 1);
}

#[test]
fn column_names_lists_the_table_in_declaration_order() {
    let db = Database::in_memory().unwrap();
    db.execute_batch("CREATE TABLE t (b INTEGER, a TEXT, c BLOB)")
        .unwrap();
    assert_eq!(db.column_names("t").unwrap(), ["b", "a", "c"]);
    assert!(db.column_names("missing").unwrap().is_empty());
}

#[test]
fn column_names_refuses_anything_that_is_not_a_bare_table_name() {
    let db = Database::in_memory().unwrap();
    for bad in ["t); DROP TABLE t; --", "a b", "t.x"] {
        let error = db.column_names(bad).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal);
        assert_eq!(error.details["table"], bad);
    }
    // An empty name passes the character check and fails in SQLite instead.
    assert_eq!(db.column_names("").unwrap_err().kind, ErrorKind::Storage);
}

#[test]
fn execute_batch_reports_a_bad_statement_as_storage() {
    let db = Database::in_memory().unwrap();
    let error = db.execute_batch("THIS IS NOT SQL").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
}
