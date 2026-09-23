//! The one SQLite connection.
//!
//! Every store shares a single connection so all writes land in one WAL, and a
//! turn's assistant message plus every tool result commit together. The
//! connection sits behind a re-entrant lock: a store method that opens a
//! transaction and then calls another store's method on the same thread must
//! not deadlock on itself, while a second thread waits its turn. The lock is
//! never held across an `await`; async callers wrap a store call in
//! `spawn_blocking` when it may take more than a millisecond.
//!
//! There is no migration framework and that is a decision. Each store runs
//! `CREATE TABLE IF NOT EXISTS` on construction and, in exactly two places,
//! reads `PRAGMA table_info` to add a column an older build did not have. No
//! comment may appear inside a `CREATE TABLE` column list: SQLite stores the DDL
//! text verbatim and `ALTER TABLE DROP COLUMN` rewrites it by byte offset.

use std::cell::Cell;
use std::path::Path;
use std::sync::Arc;

use parking_lot::{ReentrantMutex, ReentrantMutexGuard};
use rusqlite::Connection;

use crate::errors::{ErrorKind, Result, WireError};
use crate::paths::ensure_dir;

struct Inner {
    conn: Connection,
    transaction_depth: Cell<u32>,
}

/// Ends the outermost transaction however `f` leaves it. Anything short of a
/// successful COMMIT, including a failed COMMIT or a panic, rolls back, so a
/// later write never joins a transaction that nobody will commit.
struct OpenTransaction<'a> {
    inner: &'a Inner,
    committed: bool,
}

impl<'a> OpenTransaction<'a> {
    fn begin(inner: &'a Inner) -> Self {
        inner.transaction_depth.set(1);
        OpenTransaction {
            inner,
            committed: false,
        }
    }
}

impl Drop for OpenTransaction<'_> {
    fn drop(&mut self) {
        self.inner.transaction_depth.set(0);
        if !self.committed {
            // The error that got us here is the one worth reporting; a
            // rollback failure on top of it is noise.
            let _ = self.inner.conn.execute_batch("ROLLBACK");
        }
    }
}

/// A shared handle to the process's one SQLite connection. Cheap to clone.
#[derive(Clone)]
pub struct Database {
    inner: Arc<ReentrantMutex<Inner>>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Database")
    }
}

/// The locked connection, for the duration of one store call.
pub struct Guard<'a>(ReentrantMutexGuard<'a, Inner>);

impl std::ops::Deref for Guard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.0.conn
    }
}

impl Database {
    /// Opens (or creates) the database file, creating its directory `0o700`.
    ///
    /// WAL lets the UI read a session while a turn is still writing to it.
    /// `synchronous = NORMAL` trades an fsync per commit for one per checkpoint;
    /// under WAL that risks only the last commits on power loss, never
    /// corruption, and the alternative is an fsync per streamed message.
    pub fn open(file: &Path) -> Result<Database> {
        if let Some(parent) = file.parent() {
            ensure_dir(parent)?;
        }
        let conn = Connection::open(file)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;",
        )?;
        Self::finish_open(conn)
    }

    /// An in-memory database for tests and the e2e harness.
    pub fn in_memory() -> Result<Database> {
        Self::finish_open(Connection::open_in_memory()?)
    }

    fn finish_open(conn: Connection) -> Result<Database> {
        // Without this, deleting a session silently orphans its messages;
        // SQLite defaults foreign keys off for backwards compatibility.
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Database {
            inner: Arc::new(ReentrantMutex::new(Inner {
                conn,
                transaction_depth: Cell::new(0),
            })),
        })
    }

    /// Locks the connection for one call. Re-entrant on the same thread.
    pub fn lock(&self) -> Guard<'_> {
        Guard(self.inner.lock())
    }

    /// Runs `f` inside a transaction, joining an outer one if present.
    ///
    /// Re-entrant by depth rather than by `SAVEPOINT`: the nesting here is one
    /// store calling another, and a partial rollback of the inner call is never
    /// what the outer one wants.
    pub fn transaction<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self.inner.lock();
        if guard.transaction_depth.get() > 0 {
            return f(&guard.conn);
        }
        guard.conn.execute_batch("BEGIN")?;
        let mut open = OpenTransaction::begin(&guard);
        let value = f(&guard.conn)?;
        guard.conn.execute_batch("COMMIT")?;
        open.committed = true;
        Ok(value)
    }

    /// Runs one or more statements with no result.
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        self.lock().execute_batch(sql)?;
        Ok(())
    }

    /// The names of `table`'s columns, for the add-missing-columns ledgers.
    pub fn column_names(&self, table: &str) -> Result<Vec<String>> {
        if !table
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(
                WireError::new(ErrorKind::Internal, "Not a table name").with_detail("table", table)
            );
        }
        let guard = self.lock();
        let mut statement = guard.prepare(&format!("PRAGMA table_info({table})"))?;
        let names = statement
            .query_map([], |row| row.get::<_, String>("name"))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(names)
    }
}
