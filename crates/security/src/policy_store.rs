//! Installed toolboxes and container definitions.
//!
//! A definition is a file on disk and that file is the policy. Writing it is
//! the decision, the same way writing `config.yaml` is: there is no second
//! artefact recording that somebody consented to these bytes. What keeps that
//! honest is where the directory sits rather than what is in it — the policy
//! root is **beside** the workspace, never inside it. The jail root *is* the
//! workspace, so a definition kept in there would be writable by `write_file`,
//! and prompt injection would become a way to rewrite the policy the agent runs
//! under.
//!
//! **Every definition still carries a digest, and it is not consent.** It is
//! identity: two container definitions that differ never share a warm instance,
//! a definition edited while a command is running cancels that command, and an
//! idle container whose definition changed is swept. Those are properties of
//! *which bytes these are*, so the hash outlives the approval that used to be
//! recorded against it.
//!
//! ```text
//! <policy root>/
//! ├── toolboxes/<name>.yaml                 ghostai.toolbox/1
//! ├── tool-definitions/<name>.yaml          ghostai.tool/1
//! └── containers/<name>.yaml                ghostai.container/1
//! ```
//!
//! A tool definition has no digest of its own. It is covered by the digest of
//! every toolbox that names it, which is stricter than hashing it once: a
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

/// A toolbox that parsed and resolved every operation it grants.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledToolbox {
    /// The manifest and every operation it grants.
    pub resolved: ResolvedToolbox,
    /// Host path of the manifest.
    pub path: PathBuf,
}

impl InstalledToolbox {
    /// The digest over the manifest and every definition it names.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.resolved.digest
    }
}

/// A container definition that parsed and passed install policy.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledContainer {
    /// The validated definition.
    pub definition: ContainerDefinition,
    /// SHA-256 of the exact bytes on disk. Identity, not consent.
    pub digest: String,
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
    /// Why it cannot be used, or `None` when it can.
    pub problem: Option<String>,
}

/// One installed toolbox, with the operations it resolved to.
pub type ToolboxListing = Listing<ResolvedToolbox>;
/// One installed container definition.
pub type ContainerListing = Listing<ContainerDefinition>;

/// The definitions in one operator-controlled policy directory.
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
            .join(format!("{name}.yaml")))
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

    fn missing(kind: Kind, name: &str) -> GhostError {
        let noun = kind.noun();
        let hint = match kind {
            Kind::Toolbox => {
                "\n  Install one with `ghostai preset install`, or clear the agent's toolbox."
            }
            Kind::Container => {
                "\n  Create one in Settings, install one with `ghostai preset install`, or clear the agent's container."
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

    /// The toolbox an agent named, or a refusal saying what is wrong with it.
    pub fn require_toolbox(&self, name: &str) -> Result<InstalledToolbox> {
        let Some(bytes) = self.read(Kind::Toolbox, name)? else {
            return Err(Self::missing(Kind::Toolbox, name));
        };
        let resolved = self.resolve_toolbox(name, &bytes)?;
        Ok(InstalledToolbox {
            resolved,
            path: self.toolbox_path(name)?,
        })
    }

    /// The container an agent named, or a refusal saying what is wrong with it.
    pub fn require_container(&self, name: &str) -> Result<InstalledContainer> {
        let Some(bytes) = self.read(Kind::Container, name)? else {
            return Err(Self::missing(Kind::Container, name));
        };
        let (definition, digest) = Self::resolve_container(name, &bytes)?;
        Ok(InstalledContainer { definition, digest })
    }

    /// Every installed toolbox, usable or not.
    ///
    /// A broken manifest is reported rather than skipped: one that vanishes
    /// from the list because it fails to parse looks like one that was never
    /// installed, and the operator goes looking in the wrong place.
    pub fn list_toolboxes(&self) -> Vec<ToolboxListing> {
        self.list(Kind::Toolbox, |store, name, bytes| {
            store.resolve_toolbox(name, bytes)
        })
    }

    /// Every installed container definition, usable or not.
    pub fn list_containers(&self) -> Vec<ContainerListing> {
        self.list(Kind::Container, |_, name, bytes| {
            Self::resolve_container(name, bytes).map(|(definition, _)| definition)
        })
    }

    fn list<T>(
        &self,
        kind: Kind,
        resolve: impl Fn(&Self, &str, &[u8]) -> Result<T>,
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
                    Ok(value) => Listing {
                        name,
                        path,
                        value: Some(value),
                        problem: None,
                    },
                    Err(error) => Listing {
                        name,
                        path,
                        value: None,
                        problem: Some(error.message),
                    },
                }
            })
            .collect()
    }
}

/// The `<name>.yaml` entries under `dir`, sorted by UTF-16 code unit. Empty
/// when the directory cannot be read, which is the state of an install with no
/// policy at all.
pub(crate) fn definition_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|v| v.to_str()) == Some("yaml"))
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .collect();
    names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    names
}
