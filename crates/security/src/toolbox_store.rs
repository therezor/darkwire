//! Installed toolboxes, and which of them an operator has approved.
//!
//! Two halves that deliberately do not trust each other. The manifest is a file
//! on disk, editable by anything with write access. The approval is a row in the
//! database recording the sha256 of the exact bytes that were reviewed. Neither
//! is authority on its own: resolution asks whether *these* bytes are approved,
//! so editing an installed toolbox silently revokes its approval and the next
//! turn refuses with a sentence naming the drift. Nobody has to remember to
//! re-approve, because they cannot avoid it.
//!
//! It lives in this crate rather than in `ghostai-core` for a reason the layer
//! graph makes non-negotiable: core may not depend on security, and the approval
//! check needs [`parse_toolbox`] and [`assert_toolbox_policy`]. Putting the table
//! here also keeps the whole "may this agent run in this container" decision
//! inside the crate that is meant to hold every such decision.
//!
//! The toolboxes directory sits **beside** the workspace, never inside it — the
//! same placement, and the same reason, as the shared directory: the jail root
//! *is* the workspace, so a manifest kept in there would be writable by
//! `write_file`, and prompt injection would become a way to rewrite the policy
//! the agent runs under.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use ghostai_core::{Clock, Database, ErrorKind, GhostError, Result, RowReader};
use ghostai_protocol::Toolbox;
use regex::Regex;
use rusqlite::params;

use crate::toolbox::{assert_toolbox_policy, manifest_hash, parse_toolbox};

/// No comment may appear inside the column list: SQLite stores the text
/// verbatim and rewrites it by byte offset on `DROP COLUMN`.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS toolbox_approvals (
  name           TEXT    PRIMARY KEY,
  manifest_sha256 TEXT   NOT NULL,
  image          TEXT    NOT NULL,
  approved_at_ms INTEGER NOT NULL
) STRICT;
";

const ROWS: RowReader = RowReader::new("toolbox_approvals");

static TOOLBOX_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9][a-z0-9-]{0,63}$")
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// A toolbox that parsed, passed policy, and matches its recorded approval.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovedToolbox {
    /// The manifest.
    pub toolbox: Toolbox,
    /// Host path of the manifest, for the read-only mount into the container.
    ///
    /// The mount is the manifest's whole directory, so anything an install put
    /// beside `toolbox.json` rides along with it. Nothing here reads any of it —
    /// what the model is told about a toolbox comes from the manifest's own
    /// `tools` entries and from the agent's system prompt.
    pub manifest_path: PathBuf,
    /// The hash the approval was recorded against.
    pub manifest_sha256: String,
}

/// What [`ToolboxStore::list`] reports, including the toolboxes that are *not*
/// usable.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolboxListing {
    /// The directory name.
    pub name: String,
    /// Where the manifest is, or would be.
    pub manifest_path: PathBuf,
    /// The manifest, when it parsed and passed policy.
    pub toolbox: Option<Toolbox>,
    /// Whether the bytes on disk are the approved ones.
    pub approved: bool,
    /// Why it cannot be used, or `None` when it can.
    pub problem: Option<String>,
}

/// The approval ledger over `<root>/toolboxes`.
#[derive(Clone)]
pub struct ToolboxStore {
    db: Database,
    dir: PathBuf,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ToolboxStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolboxStore")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl ToolboxStore {
    /// Opens the ledger, creating its table.
    pub fn new(
        db: Database,
        dir: impl Into<PathBuf>,
        clock: Arc<dyn Clock>,
    ) -> Result<ToolboxStore> {
        db.execute_batch(SCHEMA)?;
        Ok(ToolboxStore {
            db,
            dir: dir.into(),
            clock,
        })
    }

    /// Where `name`'s manifest lives, once the name is known to be a slug.
    pub fn manifest_path_for(&self, name: &str) -> Result<PathBuf> {
        if !TOOLBOX_NAME.is_match(name) {
            return Err(GhostError::new(
                ErrorKind::InvalidInput,
                format!("Not a toolbox name: {name}"),
            )
            .with_detail("name", name));
        }
        Ok(self.dir.join(name).join("toolbox.json"))
    }

    /// Raw manifest bytes, or `None` when nothing is installed under that name.
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        // A name that is not a slug is a caller error with its own message, and
        // letting the mapping below rewrap it as "could not be read" would report
        // a filesystem problem for what is really a rejected input.
        let path = self.manifest_path_for(name)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(GhostError::new(
                ErrorKind::Config,
                format!("Toolbox \"{name}\" could not be read"),
            )
            .with_detail("name", name)
            .with_source(error)),
        }
    }

    fn approved_hash(&self, name: &str) -> Result<Option<String>> {
        let conn = self.db.lock();
        let mut statement =
            conn.prepare("SELECT manifest_sha256 FROM toolbox_approvals WHERE name = ?")?;
        let mut rows = statement.query(params![name])?;
        match rows.next()? {
            Some(row) => Ok(Some(ROWS.string(row, "manifest_sha256")?)),
            None => Ok(None),
        }
    }

    /// The toolbox an agent named, or a refusal explaining which half is missing.
    ///
    /// Every failure mode gets its own sentence. "Not installed" and "installed
    /// but not approved" and "edited since approval" are three different things
    /// for an operator to do next, and collapsing them into one message turns a
    /// two-second fix into a hunt.
    pub fn require(&self, name: &str) -> Result<ApprovedToolbox> {
        let Some(bytes) = self.read(name)? else {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "No toolbox is installed under \"{name}\".\n  Build and install one with `ghostai preset install`, or clear the agent's toolbox."
                ),
            )
            .with_detail("name", name));
        };

        let toolbox = parse_toolbox(&bytes)?;
        assert_toolbox_policy(&toolbox)?;

        let hash = manifest_hash(&bytes);
        let Some(approved) = self.approved_hash(name)? else {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Toolbox \"{name}\" is installed but has never been approved.\n  Review what it asks for with `ghostai toolbox list`, then `ghostai toolbox approve {name}`."
                ),
            )
            .with_detail("name", name));
        };
        if approved != hash {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Toolbox \"{name}\" has changed since it was approved.\n  The manifest on disk no longer matches the one that was reviewed, so it will\n  not be used. Review the change with `ghostai toolbox list`, then approve it with\n  `ghostai toolbox approve {name}`."
                ),
            )
            .with_detail("name", name)
            .with_detail("approved", approved)
            .with_detail("actual", hash));
        }

        Ok(ApprovedToolbox {
            toolbox,
            manifest_path: self.manifest_path_for(name)?,
            manifest_sha256: hash,
        })
    }

    /// Records the hash of what is on disk now. This *is* the approval.
    pub fn approve(&self, name: &str) -> Result<ApprovedToolbox> {
        let Some(bytes) = self.read(name)? else {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!("No toolbox is installed under \"{name}\""),
            )
            .with_detail("name", name));
        };
        let toolbox = parse_toolbox(&bytes)?;
        assert_toolbox_policy(&toolbox)?;
        let hash = manifest_hash(&bytes);

        self.db.lock().execute(
            "INSERT INTO toolbox_approvals (name, manifest_sha256, image, approved_at_ms)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(name) DO UPDATE SET
               manifest_sha256 = excluded.manifest_sha256,
               image = excluded.image,
               approved_at_ms = excluded.approved_at_ms",
            params![name, hash, toolbox.image, self.clock.now_ms()],
        )?;

        Ok(ApprovedToolbox {
            toolbox,
            manifest_path: self.manifest_path_for(name)?,
            manifest_sha256: hash,
        })
    }

    /// Forgets an approval. The manifest stays on disk; it simply stops resolving.
    pub fn revoke(&self, name: &str) -> Result<()> {
        self.db.lock().execute(
            "DELETE FROM toolbox_approvals WHERE name = ?",
            params![name],
        )?;
        Ok(())
    }

    /// Every installed toolbox, usable or not.
    ///
    /// A broken manifest is reported rather than skipped: a toolbox that vanishes
    /// from the list because it fails to parse looks like one that was never
    /// installed, and the operator goes looking in the wrong place.
    pub fn list(&self) -> Vec<ToolboxListing> {
        directory_names(&self.dir)
            .into_iter()
            .map(|name| {
                let manifest_path = self.dir.join(&name).join("toolbox.json");
                match self.describe(&name) {
                    Ok(Some((toolbox, approved))) => ToolboxListing {
                        name,
                        manifest_path,
                        toolbox: Some(toolbox),
                        approved,
                        problem: (!approved)
                            .then(|| "not approved, or changed since approval".to_owned()),
                    },
                    Ok(None) => ToolboxListing {
                        name,
                        manifest_path,
                        toolbox: None,
                        approved: false,
                        problem: Some("no manifest".to_owned()),
                    },
                    Err(error) => ToolboxListing {
                        name,
                        manifest_path,
                        toolbox: None,
                        approved: false,
                        problem: Some(error.message),
                    },
                }
            })
            .collect()
    }

    /// One listing's worth of work: the manifest and whether it is approved, or
    /// `None` when there is no manifest.
    fn describe(&self, name: &str) -> Result<Option<(Toolbox, bool)>> {
        let Some(bytes) = self.read(name)? else {
            return Ok(None);
        };
        let toolbox = parse_toolbox(&bytes)?;
        assert_toolbox_policy(&toolbox)?;
        let approved = self.approved_hash(name)? == Some(manifest_hash(&bytes));
        Ok(Some((toolbox, approved)))
    }
}

/// The directory entries under `dir` that are directories, sorted the way the
/// approvals were always listed — by UTF-16 code unit. Empty when the directory
/// cannot be read.
pub(crate) fn directory_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    names
}
