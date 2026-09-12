//! Which installed extensions an operator has approved, and what is on disk now.
//!
//! The same two halves that deliberately do not trust each other as
//! [`crate::ToolboxStore`]: the install directory is a set of files, editable by
//! anything with write access, and the approval is a row recording the digest of
//! the exact bytes that were reviewed. Neither is authority alone. Resolution
//! asks whether *these* bytes are approved, so editing an installed extension
//! silently revokes its approval and the next reconcile refuses with a sentence
//! naming the drift. Nobody has to remember to re-approve, because they cannot
//! avoid it.
//!
//! Unlike [`crate::ToolboxStore::require`], nothing here errors to describe an
//! extension that is not loadable. The host reconciles a whole directory at boot
//! and after every settings save, and one unapproved extension must not take the
//! other four down with it — so a refusal is a state on a row, and the sentence
//! explaining it is a field beside it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ghostai_core::{Clock, Database, ErrorKind, GhostError, Result, RowReader};
use ghostai_protocol::{ExtensionManifest, is_extension_id};
use rusqlite::params;

use crate::extension::{assert_extension_policy, extension_digest, read_extension_manifest};
use crate::toolbox_store::directory_names;

/// No comment may appear inside the column list: SQLite stores the text
/// verbatim and rewrites it by byte offset on `DROP COLUMN`.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS extension_approvals (
  id             TEXT    PRIMARY KEY,
  digest         TEXT    NOT NULL,
  approved_at_ms INTEGER NOT NULL
) STRICT;
";

const ROWS: RowReader = RowReader::new("extension_approvals");

/// Where an installed extension stands. Never "ready": loading decides that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtensionResolutionState {
    /// Installed, never approved.
    Unapproved,
    /// Approved once; the bytes on disk have changed since.
    Drifted,
    /// The manifest did not parse, the policy refused it, or the digest could
    /// not be computed.
    Failed,
    /// The bytes on disk are the approved ones.
    Approved,
}

/// One installed extension, resolved against its approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionResolution {
    /// The extension id, or the path when no manifest named one.
    pub id: String,
    /// The install directory.
    pub dir: PathBuf,
    /// Where it stands.
    pub state: ExtensionResolutionState,
    /// Absent when the manifest did not parse or the policy refused it.
    pub manifest: Option<ExtensionManifest>,
    /// The digest of what is on disk now. Empty when it could not be computed.
    pub digest: String,
    /// When the recorded approval was made.
    pub approved_at_ms: Option<i64>,
    /// Why it is not approved, phrased for the operator.
    pub problem: Option<String>,
}

fn failed(
    id: &str,
    dir: &Path,
    manifest: Option<ExtensionManifest>,
    error: &GhostError,
) -> ExtensionResolution {
    ExtensionResolution {
        id: id.to_owned(),
        dir: dir.to_path_buf(),
        state: ExtensionResolutionState::Failed,
        manifest,
        digest: String::new(),
        approved_at_ms: None,
        problem: Some(error.message.clone()),
    }
}

/// The approval ledger over `<root>/extensions`.
#[derive(Clone)]
pub struct ExtensionStore {
    db: Database,
    dir: PathBuf,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ExtensionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionStore")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl ExtensionStore {
    /// Opens the ledger, creating its table.
    pub fn new(
        db: Database,
        dir: impl Into<PathBuf>,
        clock: Arc<dyn Clock>,
    ) -> Result<ExtensionStore> {
        db.execute_batch(SCHEMA)?;
        Ok(ExtensionStore {
            db,
            dir: dir.into(),
            clock,
        })
    }

    /// The install directory for `id`, once the id is known to be a slug.
    pub fn dir_for(&self, id: &str) -> Result<PathBuf> {
        if !is_extension_id(id) {
            return Err(GhostError::new(
                ErrorKind::InvalidInput,
                format!("Not an extension id: {id}"),
            )
            .with_detail("id", id));
        }
        Ok(self.dir.join(id))
    }

    fn approval_for(&self, id: &str) -> Result<Option<(String, i64)>> {
        let conn = self.db.lock();
        let mut statement =
            conn.prepare("SELECT digest, approved_at_ms FROM extension_approvals WHERE id = ?")?;
        let mut rows = statement.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some((
                ROWS.string(row, "digest")?,
                ROWS.int(row, "approved_at_ms")?,
            ))),
            None => Ok(None),
        }
    }

    /// What `id`'s install directory holds, and whether it may run.
    ///
    /// Answers rather than errors about the extension, and the three refusals
    /// are three states because each has a different fix: approve it, re-approve
    /// it, or repair it. A single "not usable" would put the operator back to
    /// reading logs. Only a malformed id or a database failure is an error.
    pub fn resolve(&self, id: &str) -> Result<ExtensionResolution> {
        let dir = self.dir_for(id)?;
        self.resolve_in(id, &dir)
    }

    /// [`resolve`](Self::resolve) against an explicit directory.
    pub fn resolve_in(&self, id: &str, dir: &Path) -> Result<ExtensionResolution> {
        let manifest = match read_extension_manifest(dir)
            .and_then(|manifest| assert_extension_policy(&manifest, dir).map(|()| manifest))
        {
            Ok(manifest) => manifest,
            Err(error) => return Ok(failed(id, dir, None, &error)),
        };
        let digest = match extension_digest(dir) {
            Ok(digest) => digest,
            Err(error) => return Ok(failed(id, dir, Some(manifest), &error)),
        };

        let Some((approved_digest, approved_at_ms)) = self.approval_for(id)? else {
            return Ok(ExtensionResolution {
                id: id.to_owned(),
                dir: dir.to_path_buf(),
                state: ExtensionResolutionState::Unapproved,
                manifest: Some(manifest),
                digest,
                approved_at_ms: None,
                problem: Some(format!(
                    "Extension \"{id}\" is installed but has never been approved.\n  Review what it contributes with `ghostai extension list`, then\n  `ghostai extension approve {id}`."
                )),
            });
        };
        if approved_digest != digest {
            return Ok(ExtensionResolution {
                id: id.to_owned(),
                dir: dir.to_path_buf(),
                state: ExtensionResolutionState::Drifted,
                manifest: Some(manifest),
                digest,
                approved_at_ms: Some(approved_at_ms),
                problem: Some(format!(
                    "Extension \"{id}\" has changed since it was approved.\n  The files on disk no longer match the ones that were reviewed, so it\n  will not be loaded. Review the change, then `ghostai extension approve {id}`."
                )),
            });
        }
        Ok(ExtensionResolution {
            id: id.to_owned(),
            dir: dir.to_path_buf(),
            state: ExtensionResolutionState::Approved,
            manifest: Some(manifest),
            digest,
            approved_at_ms: Some(approved_at_ms),
            problem: None,
        })
    }

    /// Records the digest of what is on disk now. This *is* the approval.
    ///
    /// Errors where [`resolve`](Self::resolve) answers, and the asymmetry is
    /// deliberate: approving is an operator pressing a button about one
    /// extension, so a refusal is the answer to their request. Resolving is a
    /// sweep over a directory, where one bad row must not end the sweep.
    pub fn approve(&self, id: &str) -> Result<ExtensionResolution> {
        let dir = self.dir_for(id)?;
        let manifest = read_extension_manifest(&dir)?;
        assert_extension_policy(&manifest, &dir)?;
        let digest = extension_digest(&dir)?;
        let approved_at_ms = self.clock.now_ms();

        self.db.lock().execute(
            "INSERT INTO extension_approvals (id, digest, approved_at_ms)
             VALUES (?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               digest = excluded.digest,
               approved_at_ms = excluded.approved_at_ms",
            params![id, digest, approved_at_ms],
        )?;

        Ok(ExtensionResolution {
            id: id.to_owned(),
            dir,
            state: ExtensionResolutionState::Approved,
            manifest: Some(manifest),
            digest,
            approved_at_ms: Some(approved_at_ms),
            problem: None,
        })
    }

    /// Forgets an approval. The files stay on disk; they simply stop loading.
    pub fn revoke(&self, id: &str) -> Result<()> {
        self.db
            .lock()
            .execute("DELETE FROM extension_approvals WHERE id = ?", params![id])?;
        Ok(())
    }

    /// Every directory under the extensions root that looks like an install.
    ///
    /// A directory whose name is not a usable id is skipped silently rather than
    /// reported: `.DS_Store`, a dependency tree someone unpacked, an editor's
    /// backup folder. Reporting those as broken extensions would fill the panel
    /// with rows nobody can act on. A directory that *is* named like an extension
    /// and does not parse is a different thing entirely, and
    /// [`resolve`](Self::resolve) reports it.
    pub fn installed_ids(&self) -> Vec<String> {
        directory_names(&self.dir)
            .into_iter()
            .filter(|name| is_extension_id(name))
            .collect()
    }

    /// Resolves an extension from a path in `extensions.load` rather than the
    /// install root.
    ///
    /// The id comes from the manifest, and [`assert_extension_policy`] still
    /// requires the directory to be named after it — so an operator cannot point
    /// `load` at a checkout and get an extension registering under a name the
    /// directory does not say. `None` when the path is not a directory.
    pub fn resolve_path(&self, dir: &Path) -> Result<Option<ExtensionResolution>> {
        if !std::fs::metadata(dir).is_ok_and(|meta| meta.is_dir()) {
            return Ok(None);
        }
        let id = match read_extension_manifest(dir) {
            Ok(manifest) => manifest.id,
            Err(error) => {
                return Ok(Some(failed(&dir.to_string_lossy(), dir, None, &error)));
            }
        };
        self.resolve_in(&id, dir).map(Some)
    }
}
