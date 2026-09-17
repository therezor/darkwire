//! The workspace registry.
//!
//! A workspace is a named folder the user owns: their files, their memories
//! and their skills. Each one is a directory under `workspaces_dir`, and
//! `default` is one of them rather than the tree the others sit in. They are
//! siblings and none can read another, because `../sibling` resolves outside
//! the asking workspace's own jail root.
//!
//! `default` is still special in one way only: every install has it, it cannot
//! be deleted, and it is what a session falls back to.
//!
//! **It lives in `darkwire-core`, not `darkwire-server`.** The runtime builds a
//! jail from an id and the agent binds a turn to one, and neither may depend
//! on the server.
//!
//! **It is a table, not a JSON file.** The registry has a cross-table
//! invariant with `sessions` — a workspace cannot be detached while sessions
//! still name it — and two stores holding that invariant across two storage
//! mechanisms is exactly the shape that produces orphans.
//!
//! **There is no `path` column.** Storing a directory would make "managed
//! directories only" a convention rather than a fact, and the first API that
//! accepted one would hand an authenticated caller the whole filesystem. The
//! directory is derived from the id by [`workspace_dir_for`], which also means
//! pointing `workspaces_dir` somewhere else moves every workspace at once.

use std::sync::Arc;

use rusqlite::{Row, params};
use serde_json::{Map, Value};

use crate::clock::Clock;
use crate::db::Database;
use crate::errors::{ErrorKind, Result, WireError};
use crate::ids::{
    DEFAULT_WORKSPACE_ID, MAX_SLUG_ID_LENGTH, RESERVED_WORKSPACE_IDS, derive_workspace_id,
    is_workspace_id,
};
use crate::paths::{WirePaths, ensure_dir, shared_dir_for, workspace_dir_for};
use crate::sqlite_row::{RowReader, parse_metadata};

/// The `workspaces` table. No comment may appear inside the column list; see
/// [`crate::db`].
pub const WORKSPACES_TABLE: &str = "CREATE TABLE IF NOT EXISTS workspaces (
  id            TEXT    PRIMARY KEY,
  name          TEXT    NOT NULL DEFAULT '',
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  is_default    INTEGER NOT NULL DEFAULT 0,
  metadata_json TEXT    NOT NULL DEFAULT '{}'
) STRICT;";

/// Partial and unique: exactly one row may be the default, and the index
/// covers only that row rather than every `0`.
pub const WORKSPACES_DEFAULT_INDEX: &str = "CREATE UNIQUE INDEX IF NOT EXISTS workspaces_default ON workspaces(is_default) WHERE is_default = 1;";

/// Every DDL statement the store runs on construction, in order.
pub const SCHEMA: &[&str] = &[WORKSPACES_TABLE, WORKSPACES_DEFAULT_INDEX];

/// One registered workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRecord {
    /// The slug, and the name of its folder under `workspaces_dir`.
    pub id: String,
    /// The display name.
    pub name: String,
    /// When it was registered.
    pub created_at_ms: i64,
    /// When it was last renamed or moved.
    pub updated_at_ms: i64,
    /// True for exactly one row, which cannot be deleted.
    pub is_default: bool,
    /// An untyped bag other things write into.
    pub metadata: Map<String, Value>,
}

/// What [`WorkspaceStore::create`] takes.
#[derive(Debug, Clone, Default)]
pub struct CreateWorkspace {
    /// The display name; trimmed, and must not be blank.
    pub name: String,
    /// Derived from the name when absent.
    pub id: Option<String>,
    /// Initial metadata.
    pub metadata: Option<Map<String, Value>>,
}

const READ: RowReader = RowReader::new("workspaces");

fn row_to_workspace(row: &Row<'_>) -> Result<WorkspaceRecord> {
    Ok(WorkspaceRecord {
        id: READ.string(row, "id")?,
        name: READ.string(row, "name")?,
        created_at_ms: READ.int(row, "created_at_ms")?,
        updated_at_ms: READ.int(row, "updated_at_ms")?,
        is_default: READ.int(row, "is_default")? == 1,
        metadata: parse_metadata(&READ.string(row, "metadata_json")?),
    })
}

fn is_reserved(id: &str) -> bool {
    RESERVED_WORKSPACE_IDS.contains(&id)
}

fn not_found(id: &str) -> WireError {
    WireError::new(ErrorKind::NotFound, format!("No workspace called \"{id}\""))
        .with_detail("id", id)
}

fn already_exists(id: &str) -> WireError {
    WireError::new(
        ErrorKind::Conflict,
        format!("A workspace called \"{id}\" already exists"),
    )
    .with_detail("id", id)
}

/// The registry of workspaces, over the shared connection.
pub struct WorkspaceStore {
    db: Database,
    paths: WirePaths,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for WorkspaceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceStore")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl WorkspaceStore {
    /// Creates the table if needed and registers the default workspace.
    ///
    /// The default is `INSERT OR IGNORE` for the same reason
    /// `SessionStore::ensure_session` is: two processes opening the same file
    /// both end up with one row rather than one of them failing on the primary
    /// key.
    ///
    /// Its folder is created here, because `default` is a folder like any
    /// other now and nothing else would make one. `create` does the same for
    /// the workspaces it registers.
    pub fn new(db: Database, paths: WirePaths, clock: Arc<dyn Clock>) -> Result<WorkspaceStore> {
        for statement in SCHEMA {
            db.execute_batch(statement)?;
        }
        ensure_dir(&workspace_dir_for(&paths, DEFAULT_WORKSPACE_ID)?)?;
        let now = clock.now_ms();
        db.lock().execute(
            "INSERT OR IGNORE INTO workspaces (id, name, created_at_ms, updated_at_ms, is_default)
       VALUES (?, ?, ?, ?, 1)",
            params![DEFAULT_WORKSPACE_ID, "Default", now, now],
        )?;
        Ok(WorkspaceStore { db, paths, clock })
    }

    /// The default first, then by name — the order the switcher renders.
    pub fn list(&self) -> Result<Vec<WorkspaceRecord>> {
        let guard = self.db.lock();
        let mut statement = guard.prepare_cached(
            "SELECT * FROM workspaces ORDER BY is_default DESC, name COLLATE NOCASE ASC, id ASC",
        )?;
        let rows = statement.query_and_then([], row_to_workspace)?;
        rows.collect()
    }

    /// One workspace by id, or `None`.
    pub fn get(&self, id: &str) -> Result<Option<WorkspaceRecord>> {
        let guard = self.db.lock();
        let mut statement = guard.prepare_cached("SELECT * FROM workspaces WHERE id = ?")?;
        let mut rows = statement.query_and_then([id], row_to_workspace)?;
        rows.next().transpose()
    }

    /// Registers a workspace and makes sure its directory exists.
    ///
    /// A slug can collide with something already sitting in the workspaces
    /// folder. An existing directory is adopted rather than refused, which is
    /// what makes "delete keeps the files, recreate with the same name"
    /// round-trip. An existing *file* is a refusal, because the alternative is
    /// a workspace whose every operation fails with `ENOTDIR`.
    pub fn create(&self, options: CreateWorkspace) -> Result<WorkspaceRecord> {
        let name = options.name.trim();
        if name.is_empty() {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                "A workspace needs a name",
            ));
        }

        let id = match options.id {
            Some(id) => id,
            None => self.unique_slug(&derive_workspace_id(name))?,
        };
        if !is_workspace_id(&id) {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                format!(
                    "Not a usable workspace id: {id}. Use 1-40 lowercase letters, digits and \
                     hyphens."
                ),
            )
            .with_detail("id", id));
        }
        if is_reserved(&id) {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                format!("\"{id}\" is reserved and cannot name a workspace"),
            )
            .with_detail("id", id));
        }
        if self.get(&id)?.is_some() {
            return Err(already_exists(&id));
        }

        let directory = workspace_dir_for(&self.paths, &id)?;
        if let Ok(existing) = std::fs::metadata(&directory)
            && !existing.is_dir()
        {
            return Err(WireError::new(
                ErrorKind::Conflict,
                format!("\"{id}\" already exists in the workspaces folder and is not a folder"),
            )
            .with_detail("id", id));
        }
        ensure_dir(&directory)?;

        let now = self.clock.now_ms();
        let metadata = Value::Object(options.metadata.unwrap_or_default()).to_string();
        self.db.lock().execute(
            "INSERT INTO workspaces (id, name, created_at_ms, updated_at_ms, is_default, metadata_json)
       VALUES (?, ?, ?, ?, 0, ?)",
            params![id, name, now, now, metadata],
        )?;

        self.get(&id)?.ok_or_else(|| {
            WireError::new(
                ErrorKind::Storage,
                format!("Workspace {id} vanished immediately after creation"),
            )
        })
    }

    /// Changes the display name; nothing on disk moves.
    pub fn rename(&self, id: &str, name: &str) -> Result<WorkspaceRecord> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                "A workspace needs a name",
            ));
        }
        let existing = self.get(id)?.ok_or_else(|| not_found(id))?;
        self.db.lock().execute(
            "UPDATE workspaces SET name = ?, updated_at_ms = ? WHERE id = ?",
            params![trimmed, self.clock.now_ms(), id],
        )?;
        Ok(WorkspaceRecord {
            name: trimmed.to_owned(),
            ..existing
        })
    }

    /// Moves a workspace to a different folder: the row's id, and the tree on
    /// disk.
    ///
    /// The id **is** the directory name, so this is a `rename(2)` and a
    /// primary-key update that have to agree. It is one operation rather than
    /// two because the two failure modes of doing it separately are both
    /// unrecoverable by hand: a row pointing at a folder that is not there, or
    /// a folder nothing in the registry can name.
    ///
    /// **Everything that resolves through the id is the caller's to repoint.**
    /// Sessions carry a `workspace_id` and `SessionStore::reassign_workspace`
    /// is how they follow; a cached jail keyed on the old id has to be evicted.
    /// Neither is done here, for the reason stated on [`Self::delete`]: a store
    /// reaching into another store's table is how their schemas start to drift.
    ///
    /// Three things it refuses, each because the alternative is worse than a
    /// refusal:
    ///
    ///  - **The default**, whose id is fixed. Every install has a `default`,
    ///    sessions fall back to it and the jail cache builds it by name, so an
    ///    id that could move is an id none of them can rely on. Moving the
    ///    whole tree is `DARKWIRE_WORKSPACES`'s job.
    ///  - **A folder something already occupies.** `rename(2)` onto an existing
    ///    empty directory succeeds on POSIX, which would silently swallow it.
    ///  - **A reserved or malformed id**, on the rules that guard every other
    ///    place an id becomes a path.
    ///
    /// What it does *not* protect is a signed URL already in flight: those
    /// carry the old folder and stop resolving. They are minted for seconds at
    /// a time and the alternative is a folder nobody can correct, so that is
    /// the trade.
    pub fn relocate(&self, id: &str, folder: &str) -> Result<WorkspaceRecord> {
        let existing = self.get(id)?.ok_or_else(|| not_found(id))?;
        if existing.is_default {
            return Err(WireError::new(
                ErrorKind::Conflict,
                "The default workspace's folder is named after its id, which is fixed",
            )
            .with_detail("id", id));
        }
        if folder == id {
            return Ok(existing);
        }

        if !is_workspace_id(folder) {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                format!(
                    "Not a usable workspace folder: {folder}. Use 1-40 lowercase letters, digits \
                     and hyphens."
                ),
            )
            .with_detail("id", folder));
        }
        if is_reserved(folder) {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                format!("\"{folder}\" is reserved and cannot name a folder"),
            )
            .with_detail("id", folder));
        }
        if self.get(folder)?.is_some() {
            return Err(already_exists(folder));
        }

        let from = workspace_dir_for(&self.paths, id)?;
        let to = workspace_dir_for(&self.paths, folder)?;
        // Checked rather than left to `rename(2)`, which happily replaces an
        // empty directory at the destination — and the thing it would replace
        // is a folder the user or the agent put there.
        if std::fs::symlink_metadata(&to).is_ok() {
            return Err(WireError::new(
                ErrorKind::Conflict,
                format!("\"{folder}\" already exists in the workspaces folder"),
            )
            .with_detail("id", folder));
        }

        // The directory first. A row updated before a `rename(2)` that then
        // fails — a permission error, a cross-device link — would leave the
        // registry naming a folder that does not exist, and every turn in that
        // workspace creating an empty one beside the real files.
        std::fs::rename(&from, &to).map_err(|error| {
            WireError::new(
                ErrorKind::Storage,
                format!("Could not move the workspace folder to \"{folder}\""),
            )
            .with_detail("id", folder)
            .with_source(error)
        })?;

        // The layer agents working in one folder share is keyed by workspace
        // id too, and lives outside the jail. Leaving it behind would be a
        // workspace that silently loses what it had pooled.
        let shared_from = shared_dir_for(&self.paths, id)?;
        if std::fs::symlink_metadata(&shared_from).is_ok() {
            std::fs::rename(&shared_from, shared_dir_for(&self.paths, folder)?)?;
        }

        self.db.lock().execute(
            "UPDATE workspaces SET id = ?, updated_at_ms = ? WHERE id = ?",
            params![folder, self.clock.now_ms(), id],
        )?;

        self.get(folder)?.ok_or_else(|| {
            WireError::new(
                ErrorKind::Storage,
                format!("Workspace {id} vanished while being moved to {folder}"),
            )
        })
    }

    /// Detaches a workspace: the row goes, the directory stays.
    ///
    /// Keeping the files is the point. A delete in a web UI is one click away
    /// from a misclick, and there is no undo for a recursive remove of a tree
    /// the user has been working in — whereas a detached directory can be
    /// re-adopted by creating a workspace with the same name.
    ///
    /// The caller is responsible for the sessions: `SessionStore::count_by_workspace`
    /// decides whether to refuse, and `reassign_workspace` is the way through.
    /// Not done here, because the two tables belong to two stores and a store
    /// reaching into another's is how their schemas start to drift.
    pub fn delete(&self, id: &str) -> Result<()> {
        let existing = self.get(id)?.ok_or_else(|| not_found(id))?;
        if existing.is_default {
            return Err(WireError::new(
                ErrorKind::Conflict,
                "The default workspace cannot be deleted",
            )
            .with_detail("id", id));
        }
        self.db
            .lock()
            .execute("DELETE FROM workspaces WHERE id = ?", [id])?;
        Ok(())
    }

    /// `base`, or `base-2`, `base-3`… — the first that is free.
    fn unique_slug(&self, base: &str) -> Result<String> {
        if self.get(base)?.is_none() && !is_reserved(base) {
            return Ok(base.to_owned());
        }
        for suffix in 2u64.. {
            // Truncate the stem rather than the suffix, so a long name cannot
            // produce an id that fails the length rule.
            let tail = format!("-{suffix}");
            let stem: String = base
                .chars()
                .take(MAX_SLUG_ID_LENGTH.saturating_sub(tail.len()))
                .collect();
            let candidate = format!("{stem}{tail}");
            if self.get(&candidate)?.is_none() {
                return Ok(candidate);
            }
        }
        Err(WireError::new(
            ErrorKind::Internal,
            "Ran out of workspace slugs",
        ))
    }
}
