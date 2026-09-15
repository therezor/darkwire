//! Installed toolboxes and containers, and which of them an operator approved.
//!
//! Two halves that deliberately do not trust each other. A definition is a file
//! on disk, editable by anything with write access. The approval is a second
//! file recording the sha256 of the exact bytes that were reviewed. Neither is
//! authority on its own: resolution asks whether *these* bytes are approved, so
//! editing an installed definition silently revokes its approval and the next
//! turn refuses with a sentence naming the drift. Nobody has to remember to
//! re-approve, because they cannot avoid it.
//!
//! **The approval is a file rather than a database row**, and that is what lets
//! the sandbox service enforce the same answer. The service owns the container
//! engine and the app does not; both read this directory, neither writes the
//! other's state, and the approval they check is one artefact rather than two
//! that could disagree. A row in the app's database would have to be told to
//! the service over the socket, which would make the app the authority on what
//! the service is allowed to run.
//!
//! The policy directory sits **beside** the workspace, never inside it — the
//! same placement, and the same reason, as the shared directory: the jail root
//! *is* the workspace, so a definition kept in there would be writable by
//! `write_file`, and prompt injection would become a way to rewrite the policy
//! the agent runs under.
//!
//! ```text
//! <policy root>/
//! ├── toolboxes/<name>.json                 ghostai.toolbox/1
//! ├── toolboxes/<name>.approval.sha256      over the toolbox and its definitions
//! ├── tool-definitions/<name>.json          ghostai.tool/1
//! ├── containers/<name>.json                ghostai.container/1
//! └── containers/<name>.approval.sha256     over the definition alone
//! ```
//!
//! A tool definition has no approval of its own. It is covered by the hash of
//! every toolbox that names it, which is stricter than approving it once: a
//! definition shared by three toolboxes cannot be edited without all three
//! noticing.

use std::path::{Path, PathBuf};

use ghostai_core::{ErrorKind, GhostError, Result};
use ghostai_protocol::toolbox::ContainerDefinition;

use crate::container::{assert_container_policy, manifest_hash, parse_container};
use crate::toolbox::{ResolvedToolbox, assert_slug, invalid, resolve_bundle};

/// What kind of definition a name refers to. Each has its own directory, so a
/// toolbox and a container may share a name without either reaching the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Toolbox,
    Container,
}

impl Kind {
    fn directory(self) -> &'static str {
        match self {
            Kind::Toolbox => "toolboxes",
            Kind::Container => "containers",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Kind::Toolbox => "Toolbox",
            Kind::Container => "Container",
        }
    }

    fn command(self) -> &'static str {
        match self {
            Kind::Toolbox => "toolbox",
            Kind::Container => "container",
        }
    }
}

/// A toolbox that parsed, resolved its operations, and matches its approval.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovedToolbox {
    /// The manifest and every operation it grants.
    pub resolved: ResolvedToolbox,
    /// Host path of the manifest.
    pub path: PathBuf,
}

impl ApprovedToolbox {
    /// The hash the approval was recorded against.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.resolved.sha256
    }
}

/// A container definition that matches its independently recorded approval.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovedContainer {
    /// The validated definition.
    pub definition: ContainerDefinition,
    /// SHA-256 recorded by the approval.
    pub sha256: String,
}

/// One installed definition, usable or not.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing<T> {
    /// The name, which is its filename without the extension.
    pub name: String,
    /// Where it is installed.
    pub path: PathBuf,
    /// The parsed value, when it could be read and parsed.
    pub value: Option<T>,
    /// Whether the bytes on disk are the approved ones.
    pub approved: bool,
    /// Why it cannot be used, or `None` when it can.
    pub problem: Option<String>,
}

/// One installed toolbox, with the operations it resolved to.
pub type ToolboxListing = Listing<ResolvedToolbox>;
/// One installed container definition.
pub type ContainerListing = Listing<ContainerDefinition>;

/// The approval ledger over one operator-controlled policy directory.
#[derive(Clone)]
pub struct PolicyStore {
    root: PathBuf,
}

impl std::fmt::Debug for PolicyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyStore")
            .field("root", &self.root)
            .finish()
    }
}

impl PolicyStore {
    /// Opens the store over `<root>`, which holds `toolboxes/`, `containers/`
    /// and `tool-definitions/`.
    pub fn new(root: impl Into<PathBuf>) -> PolicyStore {
        PolicyStore { root: root.into() }
    }

    /// The directory the definitions are read from.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, kind: Kind, name: &str) -> Result<PathBuf> {
        assert_slug(name).map_err(|_| {
            GhostError::new(
                ErrorKind::InvalidInput,
                format!("Not a {} name: {name}", kind.command()),
            )
            .with_detail("name", name)
        })?;
        Ok(self
            .root
            .join(kind.directory())
            .join(format!("{name}.json")))
    }

    fn approval_path(&self, kind: Kind, name: &str) -> Result<PathBuf> {
        Ok(self.path_for(kind, name)?.with_extension("approval.sha256"))
    }

    /// Where a toolbox's manifest lives, once the name is known to be a slug.
    pub fn toolbox_path(&self, name: &str) -> Result<PathBuf> {
        self.path_for(Kind::Toolbox, name)
    }

    /// Where a container's definition lives, once the name is known to be a
    /// slug.
    pub fn container_path(&self, name: &str) -> Result<PathBuf> {
        self.path_for(Kind::Container, name)
    }

    fn read(&self, kind: Kind, name: &str) -> Result<Option<Vec<u8>>> {
        // A name that is not a slug is a caller error with its own message, and
        // letting the mapping below rewrap it as "could not be read" would
        // report a filesystem problem for what is really a rejected input.
        let path = self.path_for(kind, name)?;
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
                format!("{} \"{name}\" could not be read", kind.noun()),
            )
            .with_detail("name", name)
            .with_source(error)),
        }
    }

    fn approved_hash(&self, kind: Kind, name: &str) -> Result<Option<String>> {
        match std::fs::read_to_string(self.approval_path(kind, name)?) {
            Ok(hash) => Ok(Some(hash.trim().to_owned())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(invalid(error.to_string())),
        }
    }

    /// Writes through a temporary file so a crash cannot leave a half-written
    /// hash that matches nothing and refuses everything.
    fn write_approval(&self, kind: Kind, name: &str, hash: &str) -> Result<()> {
        let path = self.approval_path(kind, name)?;
        let temporary = path.with_extension(format!(
            "tmp-{}",
            crate::random::hex_lower(&rand::random::<[u8; 16]>())
        ));
        std::fs::write(&temporary, hash).map_err(|e| invalid(e.to_string()))?;
        std::fs::rename(&temporary, &path).map_err(|e| invalid(e.to_string()))
    }

    fn clear_approval(&self, kind: Kind, name: &str) -> Result<()> {
        match std::fs::remove_file(self.approval_path(kind, name)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(invalid(error.to_string())),
        }
    }

    /// Every failure mode gets its own sentence. "Not installed" and "installed
    /// but not approved" and "edited since approval" are three different things
    /// for an operator to do next, and collapsing them into one message turns a
    /// two-second fix into a hunt.
    fn check_approval(&self, kind: Kind, name: &str, hash: &str) -> Result<()> {
        let noun = kind.noun();
        let command = kind.command();
        let Some(approved) = self.approved_hash(kind, name)? else {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "{noun} \"{name}\" is installed but has never been approved.\n  Review what it asks for with `ghostai {command} list`, then `ghostai {command} approve {name}`."
                ),
            )
            .with_detail("name", name));
        };
        if approved != hash {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "{noun} \"{name}\" has changed since it was approved.\n  What is on disk no longer matches what was reviewed, so it will not be used.\n  Review the change with `ghostai {command} list`, then `ghostai {command} approve {name}`."
                ),
            )
            .with_detail("name", name)
            .with_detail("approved", approved)
            .with_detail("actual", hash.to_owned()));
        }
        Ok(())
    }

    fn missing(kind: Kind, name: &str) -> GhostError {
        let noun = kind.noun();
        let hint = match kind {
            Kind::Toolbox => {
                "\n  Install one with `ghostai preset install`, or clear the agent's toolbox."
            }
            Kind::Container => {
                "\n  Install one with `ghostai preset install`, or clear the agent's container."
            }
        };
        GhostError::new(
            ErrorKind::Config,
            format!(
                "No {} is installed under \"{name}\".{hint}",
                noun.to_lowercase()
            ),
        )
        .with_detail("name", name)
    }

    fn resolve_toolbox(&self, name: &str, bytes: &[u8]) -> Result<ResolvedToolbox> {
        let resolved = resolve_bundle(&self.root, bytes)?;
        if resolved.toolbox.name != name {
            return Err(invalid(format!(
                "Toolbox \"{name}\" names itself \"{}\"; a manifest's name is its filename.",
                resolved.toolbox.name
            )));
        }
        Ok(resolved)
    }

    fn resolve_container(name: &str, bytes: &[u8]) -> Result<(ContainerDefinition, String)> {
        let definition = parse_container(bytes)?;
        if definition.name != name {
            return Err(invalid(format!(
                "Container \"{name}\" names itself \"{}\"; a definition's name is its filename.",
                definition.name
            )));
        }
        assert_container_policy(&definition)?;
        Ok((definition, manifest_hash(bytes)))
    }

    /// The toolbox an agent named, or a refusal explaining which half is
    /// missing.
    pub fn require_toolbox(&self, name: &str) -> Result<ApprovedToolbox> {
        let Some(bytes) = self.read(Kind::Toolbox, name)? else {
            return Err(Self::missing(Kind::Toolbox, name));
        };
        let resolved = self.resolve_toolbox(name, &bytes)?;
        self.check_approval(Kind::Toolbox, name, &resolved.sha256)?;
        Ok(ApprovedToolbox {
            resolved,
            path: self.toolbox_path(name)?,
        })
    }

    /// The container an agent named, or a refusal explaining which half is
    /// missing.
    pub fn require_container(&self, name: &str) -> Result<ApprovedContainer> {
        let Some(bytes) = self.read(Kind::Container, name)? else {
            return Err(Self::missing(Kind::Container, name));
        };
        let (definition, sha256) = Self::resolve_container(name, &bytes)?;
        self.check_approval(Kind::Container, name, &sha256)?;
        Ok(ApprovedContainer { definition, sha256 })
    }

    /// Records the hash of what is on disk now. This *is* the approval.
    pub fn approve_toolbox(&self, name: &str) -> Result<ApprovedToolbox> {
        let Some(bytes) = self.read(Kind::Toolbox, name)? else {
            return Err(Self::missing(Kind::Toolbox, name));
        };
        let resolved = self.resolve_toolbox(name, &bytes)?;
        self.write_approval(Kind::Toolbox, name, &resolved.sha256)?;
        Ok(ApprovedToolbox {
            resolved,
            path: self.toolbox_path(name)?,
        })
    }

    /// Records the hash of the definition on disk now.
    pub fn approve_container(&self, name: &str) -> Result<ApprovedContainer> {
        let Some(bytes) = self.read(Kind::Container, name)? else {
            return Err(Self::missing(Kind::Container, name));
        };
        let (definition, sha256) = Self::resolve_container(name, &bytes)?;
        self.write_approval(Kind::Container, name, &sha256)?;
        Ok(ApprovedContainer { definition, sha256 })
    }

    /// Forgets an approval. The manifest stays on disk; it stops resolving.
    pub fn revoke_toolbox(&self, name: &str) -> Result<()> {
        self.clear_approval(Kind::Toolbox, name)
    }

    /// Forgets a container's approval without removing its definition.
    pub fn revoke_container(&self, name: &str) -> Result<()> {
        self.clear_approval(Kind::Container, name)
    }

    /// Every installed toolbox, usable or not.
    ///
    /// A broken manifest is reported rather than skipped: one that vanishes
    /// from the list because it fails to parse looks like one that was never
    /// installed, and the operator goes looking in the wrong place.
    pub fn list_toolboxes(&self) -> Vec<ToolboxListing> {
        self.list(Kind::Toolbox, |store, name, bytes| {
            let resolved = store.resolve_toolbox(name, bytes)?;
            let hash = resolved.sha256.clone();
            Ok((resolved, hash))
        })
    }

    /// Every installed container definition, usable or not.
    pub fn list_containers(&self) -> Vec<ContainerListing> {
        self.list(Kind::Container, |_, name, bytes| {
            Self::resolve_container(name, bytes)
        })
    }

    fn list<T>(
        &self,
        kind: Kind,
        resolve: impl Fn(&Self, &str, &[u8]) -> Result<(T, String)>,
    ) -> Vec<Listing<T>> {
        definition_names(&self.root.join(kind.directory()))
            .into_iter()
            .map(|name| {
                let path = self
                    .path_for(kind, &name)
                    .unwrap_or_else(|_| self.root.join(&name));
                let read = self
                    .read(kind, &name)
                    .and_then(|bytes| bytes.ok_or_else(|| Self::missing(kind, &name)))
                    .and_then(|bytes| resolve(self, &name, &bytes));
                match read {
                    Ok((value, hash)) => {
                        let approved = self.approved_hash(kind, &name).ok().flatten() == Some(hash);
                        Listing {
                            name,
                            path,
                            value: Some(value),
                            approved,
                            problem: (!approved)
                                .then(|| "not approved, or changed since approval".to_owned()),
                        }
                    }
                    Err(error) => Listing {
                        name,
                        path,
                        value: None,
                        approved: false,
                        problem: Some(error.message),
                    },
                }
            })
            .collect()
    }
}

/// The `<name>.json` entries under `dir`, sorted the way the approvals were
/// always listed — by UTF-16 code unit. Empty when the directory cannot be
/// read, which is the state of an install with no policy at all.
pub(crate) fn definition_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|v| v.to_str()) == Some("json"))
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .collect();
    names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    names
}
