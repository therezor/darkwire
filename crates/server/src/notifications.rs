//! Notifications, on the connection everything else shares.
//!
//! They live in this crate rather than in `darkwire-core` for the same reason
//! the auth tables do: nothing below the transport raises one. A notification
//! is something a *user interface* shows — an automation run that finished
//! while the tab was closed, an approval that expired unanswered — and the
//! agent loop has no opinion about whether anyone is watching. What raises them
//! is the scheduler and the hub, both of which sit at this level.
//!
//! Two decisions worth stating:
//!
//!  - **Read is a timestamp, not a flag.** "When did this stop being new" is a
//!    question a UI asks — a badge that dims after a while, a digest of what
//!    arrived since a session started — and a boolean cannot answer it. The
//!    unread count is `read_at_ms IS NULL`, which the partial index serves
//!    directly.
//!
//!  - **The listing is keyset-paged over `(created_at_ms DESC, id ASC)`.** A
//!    notification arriving mid-scroll is exactly the case that makes an offset
//!    wrong, and it is also the case this table exists for.

use std::sync::Arc;

use darkwire_core::clock::Clock;
use darkwire_core::db::Database;
use darkwire_core::errors::Result;
use darkwire_core::session_store::IdSource;
use darkwire_core::sqlite_row::RowReader;
use darkwire_protocol::rest::Notification;
use darkwire_protocol::ws::NotificationLevel;
use rusqlite::{Row, params};

/// The `notifications` table.
///
/// `read_at_ms` is nullable because "unread" is the absence of a read time
/// rather than a flag beside one. No comment may appear inside the column list;
/// see [`darkwire_core::db`].
pub const NOTIFICATIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS notifications (
  id            TEXT    PRIMARY KEY,
  title         TEXT    NOT NULL,
  body          TEXT    NOT NULL DEFAULT '',
  level         TEXT    NOT NULL DEFAULT 'info',
  created_at_ms INTEGER NOT NULL,
  read_at_ms    INTEGER,
  session_key   TEXT,
  job_id        TEXT
) STRICT;";

/// The order the listing walks, and the tie-break the cursor addresses.
pub const NOTIFICATIONS_CREATED_INDEX: &str = "CREATE INDEX IF NOT EXISTS notifications_created ON notifications(created_at_ms DESC, id ASC);";

/// Partial on purpose: the bell's count is a scan of the unread rows alone,
/// which in a mature table is a small minority of them.
pub const NOTIFICATIONS_UNREAD_INDEX: &str = "CREATE INDEX IF NOT EXISTS notifications_unread ON notifications(created_at_ms DESC) WHERE read_at_ms IS NULL;";

/// Every DDL statement the store runs on construction, in order.
pub const SCHEMA: &[&str] = &[
    NOTIFICATIONS_TABLE,
    NOTIFICATIONS_CREATED_INDEX,
    NOTIFICATIONS_UNREAD_INDEX,
];

const READ: RowReader = RowReader::new("notifications");

/// What [`NotificationStore::create`] takes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateNotificationInput {
    /// The headline.
    pub title: String,
    /// The detail. Empty is normal.
    pub body: String,
    /// How loud.
    pub level: NotificationLevel,
    /// The conversation it is about, if any.
    pub session_key: Option<String>,
    /// The automation job that raised it, if any.
    pub job_id: Option<String>,
}

/// One position in the `(created_at_ms DESC, id ASC)` order.
///
/// The store takes the decoded position rather than an opaque cursor string:
/// encoding is a transport concern, and a store that had to decode one could
/// not be driven from a test without minting base64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationAfter {
    /// The `created_at_ms` of the last row of the previous page.
    pub created_at_ms: i64,
    /// Its id, which breaks a tie inside one millisecond.
    pub id: String,
}

/// How a caller asks for one page.
#[derive(Debug, Clone, Default)]
pub struct ListNotifications {
    /// Rows per page.
    pub limit: Option<i64>,
    /// For a numbered pager, where `after` is for a sequential reader.
    ///
    /// The two are alternatives and never combined — the route refuses the pair
    /// with a 400, because a page relative to a page is not a thing to ask for.
    pub offset: Option<i64>,
    /// Where the previous page ended.
    pub after: Option<NotificationAfter>,
    /// Only the rows with no read time.
    pub unread_only: bool,
}

/// The wire spelling of a level.
fn level_str(level: NotificationLevel) -> &'static str {
    match level {
        NotificationLevel::Info => "info",
        NotificationLevel::Success => "success",
        NotificationLevel::Warning => "warning",
        NotificationLevel::Error => "error",
    }
}

/// A level written by something that predates a level being added, or by an
/// extension, becomes `info` rather than failing the read. The alternative is
/// one bad row making the whole notification list unreadable.
fn read_level(row: &Row<'_>) -> Result<NotificationLevel> {
    Ok(match READ.string(row, "level")?.as_str() {
        "success" => NotificationLevel::Success,
        "warning" => NotificationLevel::Warning,
        "error" => NotificationLevel::Error,
        _ => NotificationLevel::Info,
    })
}

/// Milliseconds as the wire carries them. A negative stored value is a clock
/// that ran before the epoch, which the unsigned wire type cannot express.
fn as_wire_ms(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn row_to_notification(row: &Row<'_>) -> Result<Notification> {
    Ok(Notification {
        id: READ.string(row, "id")?,
        title: READ.string(row, "title")?,
        body: READ.string(row, "body")?,
        level: read_level(row)?,
        created_at_ms: as_wire_ms(READ.int(row, "created_at_ms")?),
        read_at_ms: READ.optional_int(row, "read_at_ms").map(as_wire_ms),
        session_key: READ.optional_string(row, "session_key"),
        job_id: READ.optional_string(row, "job_id"),
    })
}

/// The notification table, over the shared connection.
pub struct NotificationStore {
    db: Database,
    clock: Arc<dyn Clock>,
    new_id: IdSource,
}

impl std::fmt::Debug for NotificationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NotificationStore")
    }
}

impl NotificationStore {
    /// Creates the table if it is not there, and returns the store.
    pub fn new(db: Database, clock: Arc<dyn Clock>, new_id: IdSource) -> Result<NotificationStore> {
        for statement in SCHEMA {
            db.execute_batch(statement)?;
        }
        Ok(NotificationStore { db, clock, new_id })
    }

    /// Raises one, and returns it as it now stands.
    pub fn create(&self, input: CreateNotificationInput) -> Result<Notification> {
        let id = (self.new_id)();
        let created_at_ms = self.clock.now_ms();
        self.db.lock().execute(
            "INSERT INTO notifications (id, title, body, level, created_at_ms, read_at_ms, session_key, job_id)
             VALUES (?, ?, ?, ?, ?, NULL, ?, ?)",
            params![
                &id,
                &input.title,
                &input.body,
                level_str(input.level),
                created_at_ms,
                input.session_key.as_deref(),
                input.job_id.as_deref(),
            ],
        )?;

        Ok(Notification {
            id,
            title: input.title,
            body: input.body,
            level: input.level,
            created_at_ms: as_wire_ms(created_at_ms),
            read_at_ms: None,
            session_key: input.session_key,
            job_id: input.job_id,
        })
    }

    /// One notification by id.
    pub fn get(&self, id: &str) -> Result<Option<Notification>> {
        let guard = self.db.lock();
        let mut statement = guard.prepare("SELECT * FROM notifications WHERE id = ?")?;
        let mut rows = statement.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_notification(row)?)),
            None => Ok(None),
        }
    }

    /// A page, newest first.
    ///
    /// The predicate is the sort order written as a comparison, the same shape
    /// `SessionStore` listings use: strictly older, or the same millisecond and
    /// an id that sorts later. Two notifications raised in one millisecond are
    /// the normal case for an automation run that finishes several jobs.
    pub fn list(&self, options: &ListNotifications) -> Result<Vec<Notification>> {
        let limit = options.limit.unwrap_or(50);
        let offset = options.offset.unwrap_or(0);
        let after_at = options.after.as_ref().map(|after| after.created_at_ms);
        let after_id = options.after.as_ref().map(|after| after.id.as_str());

        let guard = self.db.lock();
        let mut statement = guard.prepare(
            "SELECT * FROM notifications
              WHERE (? = 0 OR read_at_ms IS NULL)
                AND (? IS NULL
                     OR created_at_ms < ?
                     OR (created_at_ms = ? AND id > ?))
              ORDER BY created_at_ms DESC, id ASC
              LIMIT ? OFFSET ?",
        )?;
        let mut rows = statement.query(params![
            i64::from(options.unread_only),
            after_at,
            after_at,
            after_at,
            after_id,
            limit,
            offset,
        ])?;

        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_notification(row)?);
        }
        Ok(out)
    }

    /// How many the same filter matches, ignoring the page.
    pub fn count(&self, unread_only: bool) -> Result<i64> {
        let guard = self.db.lock();
        let n = guard.query_row(
            "SELECT COUNT(*) AS n FROM notifications WHERE (? = 0 OR read_at_ms IS NULL)",
            params![i64::from(unread_only)],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(n)
    }

    /// How many have no read time. What the bell shows.
    pub fn unread_count(&self) -> Result<i64> {
        let guard = self.db.lock();
        let n = guard.query_row(
            "SELECT COUNT(*) AS n FROM notifications WHERE read_at_ms IS NULL",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(n)
    }

    /// Marks one read and returns it as it now stands.
    ///
    /// Idempotent in the way that matters: a second call does not move the
    /// timestamp, so "read at" keeps meaning the first time it was seen rather
    /// than the last time a tab was refreshed.
    pub fn mark_read(&self, id: &str) -> Result<Option<Notification>> {
        self.db.lock().execute(
            "UPDATE notifications SET read_at_ms = ? WHERE id = ? AND read_at_ms IS NULL",
            params![self.clock.now_ms(), id],
        )?;
        self.get(id)
    }

    /// Marks everything unread as read. Returns how many rows changed.
    pub fn mark_all_read(&self) -> Result<usize> {
        let changed = self.db.lock().execute(
            "UPDATE notifications SET read_at_ms = ? WHERE read_at_ms IS NULL",
            params![self.clock.now_ms()],
        )?;
        Ok(changed)
    }

    /// Removes one. `false` when there was nothing to remove.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let changed = self
            .db
            .lock()
            .execute("DELETE FROM notifications WHERE id = ?", params![id])?;
        Ok(changed > 0)
    }

    /// Empties the table, and reports how many went.
    ///
    /// Read *and* unread, which is the whole point of it: an operator clearing
    /// a backlog is clearing the backlog, and a "delete all" that quietly kept
    /// the unread ones would leave the bell still counting after the list
    /// looked empty. The route in front of it is what asks first.
    pub fn delete_all(&self) -> Result<usize> {
        let changed = self.db.lock().execute("DELETE FROM notifications", [])?;
        Ok(changed)
    }
}
