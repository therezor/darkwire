//! Installed environment definitions.
//!
//! A definition is a file on disk and that file is the policy. Writing it is
//! the decision, the same way writing `config.yaml` is: there is no second
//! artefact recording that somebody consented to these bytes. What keeps that
//! honest is where the directory sits rather than what is in it — the policy
//! root is **beside** the workspace, never inside it. The jail root *is* the
//! workspace, so a definition kept in there would be writable by `write`,
//! and prompt injection would become a way to rewrite the policy the agent runs
//! under.
//!
//! **Every definition still carries a digest, and it is not consent.** It is
//! identity: two environment definitions that differ never share a warm instance,
//! a definition edited while a command is running cancels that command, and an
//! idle container whose definition changed is swept. Those are properties of
//! *which bytes these are*, so the hash outlives the approval that used to be
//! recorded against it.
//!
//! ```text
//! <policy root>/
//! └── environments/<name>.yaml              darkwire.environment/1
//! ```

use std::path::{Path, PathBuf};

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::environment::EnvironmentDefinition;

use crate::environment::{
    assert_environment_policy, assert_slug, invalid, manifest_hash, parse_environment,
};

/// An environment definition that parsed and passed install policy.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledEnvironment {
    /// The validated definition.
    pub definition: EnvironmentDefinition,
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

/// One installed environment definition.
pub type EnvironmentListing = Listing<EnvironmentDefinition>;

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
    /// Opens the store over `<root>`, which holds `environments/`.
    pub fn new(root: impl Into<PathBuf>) -> PolicyStore {
        PolicyStore { root: root.into() }
    }

    /// The directory the definitions are read from.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, name: &str) -> Result<PathBuf> {
        assert_slug(name).map_err(|_| {
            WireError::new(
                ErrorKind::InvalidInput,
                format!("Not an environment name: {name}"),
            )
            .with_detail("name", name)
        })?;
        Ok(self.root.join("environments").join(format!("{name}.yaml")))
    }

    /// Where an environment's definition lives, once the name is known to be a
    /// slug.
    pub fn environment_path(&self, name: &str) -> Result<PathBuf> {
        self.path_for(name)
    }

    fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        // A name that is not a slug is a caller error with its own message, and
        // letting the mapping below rewrap it as "could not be read" would
        // report a filesystem problem for what is really a rejected input.
        let path = self.path_for(name)?;
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
            Err(error) => Err(WireError::new(
                ErrorKind::Config,
                format!("Environment \"{name}\" could not be read"),
            )
            .with_detail("name", name)
            .with_source(error)),
        }
    }

    fn missing(name: &str) -> WireError {
        WireError::new(
            ErrorKind::Config,
            format!(
                "No environment is installed under \"{name}\".\n  Create one in Settings, or clear the agent's environment."
            ),
        )
        .with_detail("name", name)
    }

    /// Writes a definition, and reports the digest the bytes on disk now have.
    ///
    /// **Validated by reading back what it is about to write.** The bytes are
    /// serialised, then run through the same `resolve_environment` the load
    /// path uses, and only a definition that survives that is put on disk.
    /// Checking the in-memory struct instead would skip the schema validation
    /// `parse_environment` performs, which is how a file gets written that
    /// `list_environments` then reports as broken. Anything that saves here is
    /// something that loads.
    ///
    /// Written to a temporary file and renamed, so a reader never sees half a
    /// definition. A rename within one directory is atomic on every filesystem
    /// this runs on, and the pool reads these under no lock at all.
    ///
    /// The bytes are re-emitted from the parsed definition rather than patched
    /// in place, so comments and key order in a hand-written file are lost on
    /// the first save from here. That also moves the digest, which is correct:
    /// the digest is identity, and a definition an operator edited is a
    /// different definition.
    pub fn save_environment(&self, definition: &EnvironmentDefinition) -> Result<String> {
        let path = self.path_for(&definition.name)?;
        let bytes = serde_yaml_ng::to_string(definition)
            .map_err(|error| {
                WireError::new(
                    ErrorKind::Internal,
                    format!("Environment \"{}\" could not be written", definition.name),
                )
                .with_source(error)
            })?
            .into_bytes();
        let (_, digest) = Self::resolve_environment(&definition.name, &bytes)?;

        let dir = path.parent().unwrap_or(&path);
        std::fs::create_dir_all(dir).map_err(|error| Self::unwritable(&definition.name, error))?;
        // In the same directory as the target, because a rename across
        // filesystems is not one.
        let temporary = path.with_extension("yaml.tmp");
        std::fs::write(&temporary, &bytes)
            .and_then(|()| std::fs::rename(&temporary, &path))
            .map_err(|error| {
                let _ = std::fs::remove_file(&temporary);
                Self::unwritable(&definition.name, error)
            })?;
        Ok(digest)
    }

    /// Removes an installed definition. A name that is not installed is a
    /// refusal rather than a silent success: an operator deleting something
    /// that is already gone has usually named the wrong thing.
    pub fn remove_environment(&self, name: &str) -> Result<()> {
        let path = self.path_for(name)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(Self::missing(name)),
            Err(error) => Err(Self::unwritable(name, error)),
        }
    }

    fn unwritable(name: &str, error: std::io::Error) -> WireError {
        WireError::new(
            ErrorKind::Storage,
            format!("Environment \"{name}\" could not be written"),
        )
        .with_detail("name", name)
        .with_source(error)
    }

    fn resolve_environment(name: &str, bytes: &[u8]) -> Result<(EnvironmentDefinition, String)> {
        let definition = parse_environment(bytes)?;
        if definition.name != name {
            return Err(invalid(format!(
                "Environment \"{name}\" names itself \"{}\"; a definition's name is its filename.",
                definition.name
            )));
        }
        assert_environment_policy(&definition)?;
        Ok((definition, manifest_hash(bytes)))
    }

    /// The environment an agent named, or a refusal saying what is wrong with it.
    pub fn require_environment(&self, name: &str) -> Result<InstalledEnvironment> {
        let Some(bytes) = self.read(name)? else {
            return Err(Self::missing(name));
        };
        let (definition, digest) = Self::resolve_environment(name, &bytes)?;
        Ok(InstalledEnvironment { definition, digest })
    }

    /// Every installed environment definition, usable or not.
    pub fn list_environments(&self) -> Vec<EnvironmentListing> {
        self.list(|name, bytes| {
            Self::resolve_environment(name, bytes).map(|(definition, _)| definition)
        })
    }

    fn list<T>(&self, resolve: impl Fn(&str, &[u8]) -> Result<T>) -> Vec<Listing<T>> {
        definition_names(&self.root.join("environments"))
            .into_iter()
            .map(|name| {
                let path = self
                    .path_for(&name)
                    .unwrap_or_else(|_| self.root.join(&name));
                let read = self
                    .read(&name)
                    .and_then(|bytes| bytes.ok_or_else(|| Self::missing(&name)))
                    .and_then(|bytes| resolve(&name, &bytes));
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
