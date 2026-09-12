//! Where things live on disk.
//!
//! Config stores paths exactly as the operator typed them, `~` and all, because
//! expanding at parse time would write a machine's home directory back into the
//! file the moment anything saved it, which makes a config non-portable and a
//! container mount silently wrong. Expansion is therefore a load-time step, and
//! this is where it happens.
//!
//! These helpers know nothing about *safety*. Resolving a path here does not
//! make it legal for a tool to touch: that is the workspace jail in
//! `ghostai-security`, which verifies through `realpath` and is the only thing
//! that may decide an agent-supplied path is acceptable.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::errors::{ErrorKind, GhostError, Result};
use crate::ids::{DEFAULT_WORKSPACE_ID, is_extension_id, is_workspace_id};

/// Overrides the root for tests, CI, and multi-instance installs.
pub const HOME_ENV_VAR: &str = "GHOSTAI_HOME";

const DEFAULT_ROOT_DIRNAME: &str = ".ghostai";

/// Expands a leading `~` to `home`.
///
/// Only a bare `~` or a `~/`-prefixed path expands. `~user` is left untouched:
/// resolving another account's home needs a passwd lookup, and treating
/// `~alice` as a relative directory named `~alice` is the more predictable of
/// the two wrong answers.
pub fn expand_home(input: &str, home: &Path) -> PathBuf {
    if input == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = input
        .strip_prefix("~/")
        .or_else(|| input.strip_prefix("~\\"))
    {
        return home.join(rest);
    }
    PathBuf::from(input)
}

/// Expands `~` against `home` and resolves to an absolute, lexically normalised
/// path against `base`.
///
/// `base` is the directory a relative path is relative to. Anything originating
/// in config passes the config file's directory, so a relative `workspace`
/// means "beside the config" rather than "wherever the service was started".
pub fn resolve_path(input: &str, base: &Path, home: &Path) -> PathBuf {
    let expanded = expand_home(input, home);
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    normalise(&joined)
}

/// Lexical `.`/`..` normalisation with no filesystem access.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir) | None
                ) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Every directory and file GhostAI owns, resolved absolute.
///
/// Four of these are **outside the jail** on purpose. The jail root *is* the
/// workspace, so anything kept inside it is readable and writable by
/// `write_file`, which turns prompt injection into a way of rewriting what the
/// agent is told. What stays out here is what an agent must not be able to
/// author: the shared layer, container policy, sandbox transcripts and
/// installed extensions. Memory and skills stay *inside* the workspace, because
/// each is meant to be read, corrected and committed beside the project it
/// describes; the mitigation is `write_file: ask`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostPaths {
    /// `~/.ghostai` unless overridden. Everything below is derived from it.
    pub root: PathBuf,
    /// The default workspace, and the parent of every named one. A turn in
    /// `default` reaches every other workspace's files; named workspaces are
    /// isolated from each other.
    pub workspace: PathBuf,
    /// The layer agents working in one folder share, keyed by workspace.
    pub shared_dir: PathBuf,
    /// Installed toolboxes, one directory per toolbox holding `toolbox.json`.
    pub toolboxes_dir: PathBuf,
    /// Installed agent presets.
    pub presets_dir: PathBuf,
    /// The preset catalogue cache.
    pub catalogue_dir: PathBuf,
    /// Sandbox command transcripts. Moved out of the workspace after a
    /// symlink-planting host-file overwrite was demonstrated.
    pub runs_dir: PathBuf,
    /// The settings tree.
    pub config_file: PathBuf,
    /// The one SQLite file.
    pub db_file: PathBuf,
    /// Log output.
    pub logs_dir: PathBuf,
    /// Installed extensions, approved by a digest over every byte.
    pub extensions_dir: PathBuf,
    /// What an extension writes at runtime: a sibling of its install directory,
    /// never a child, or the first write would revoke its own approval.
    pub extension_data_dir: PathBuf,
    /// The encrypted credential vault.
    pub vault_file: PathBuf,
    /// The vault's key file, used only when no OS keychain is available.
    pub key_file: PathBuf,
}

/// Inputs to [`GhostPaths::resolve`].
#[derive(Debug, Clone, Default)]
pub struct ResolveGhostPaths {
    /// Wins over `GHOSTAI_HOME`, which wins over `~/.ghostai`.
    pub root: Option<String>,
    /// Defaults to `<root>/workspace`.
    pub workspace: Option<String>,
    /// The environment to consult; defaults to the process environment.
    pub env: Option<HashMap<String, String>>,
    /// The home directory; defaults to the process user's.
    pub home: Option<PathBuf>,
}

impl GhostPaths {
    /// Resolves the layout from flags, environment and home directory.
    pub fn resolve(options: ResolveGhostPaths) -> Result<GhostPaths> {
        let home = match options.home {
            Some(home) => home,
            None => std::env::home_dir().ok_or_else(|| {
                GhostError::new(ErrorKind::Config, "Cannot determine the home directory")
            })?,
        };
        let from_env = match &options.env {
            Some(env) => env.get(HOME_ENV_VAR).cloned(),
            None => std::env::var(HOME_ENV_VAR).ok(),
        };
        let root_input = options.root.or(from_env).unwrap_or_else(|| {
            home.join(DEFAULT_ROOT_DIRNAME)
                .to_string_lossy()
                .into_owned()
        });
        let root = {
            let expanded = expand_home(&root_input, &home);
            if expanded.is_absolute() {
                normalise(&expanded)
            } else {
                normalise(&std::env::current_dir()?.join(expanded))
            }
        };

        // Relative to the root, not the cwd: a workspace that moved because a
        // service was restarted from a different directory would orphan the
        // agent's memory files while leaving the database pointing at them.
        let workspace = match options.workspace {
            None => root.join("workspace"),
            Some(input) => resolve_path(&input, &root, &home),
        };

        Ok(GhostPaths {
            shared_dir: root.join("shared"),
            toolboxes_dir: root.join("toolboxes"),
            presets_dir: root.join("presets"),
            catalogue_dir: root.join("catalogue"),
            runs_dir: root.join("runs"),
            config_file: root.join("config.json"),
            db_file: root.join("ghost.db"),
            logs_dir: root.join("logs"),
            extensions_dir: root.join("extensions"),
            extension_data_dir: root.join("extension-data"),
            vault_file: root.join("vault.json"),
            key_file: root.join("vault.key"),
            workspace,
            root,
        })
    }
}

/// The directory one workspace owns.
///
/// The **only** place an id becomes a path, which is why it re-validates rather
/// than trusting its caller: ids reach this from a request body, a query string
/// and a `workspace_id` column an operator can edit by hand, and a single
/// unchecked call site is the whole containment argument gone. It does not
/// consult the registry: a detached workspace still has sessions, and they must
/// keep resolving to their own files rather than falling into someone else's.
///
/// `default` maps to the workspace root itself. That special case is the price
/// of "the default workspace is the folder that holds the others".
pub fn workspace_dir_for(paths: &GhostPaths, id: &str) -> Result<PathBuf> {
    if id == DEFAULT_WORKSPACE_ID {
        return Ok(paths.workspace.clone());
    }
    if !is_workspace_id(id) {
        return Err(not_an_id("a workspace", id));
    }
    Ok(paths.workspace.join(id))
}

/// The directory holding what every agent in one workspace may share.
///
/// Takes a *workspace* id and validates it as one: the sharing axis is the
/// working folder, so an agent id here would be a category error.
pub fn shared_dir_for(paths: &GhostPaths, workspace_id: &str) -> Result<PathBuf> {
    if !is_workspace_id(workspace_id) {
        return Err(not_an_id("a workspace", workspace_id));
    }
    Ok(paths.shared_dir.join(workspace_id))
}

/// Where one extension is installed. Re-validates for the reason
/// [`workspace_dir_for`] does.
pub fn extension_dir_for(paths: &GhostPaths, id: &str) -> Result<PathBuf> {
    Ok(paths.extensions_dir.join(assert_extension_id(id)?))
}

/// Where one extension may write: a sibling of the install directory, because
/// state written inside it would move the digest and revoke the approval.
pub fn extension_data_dir_for(paths: &GhostPaths, id: &str) -> Result<PathBuf> {
    Ok(paths.extension_data_dir.join(assert_extension_id(id)?))
}

fn assert_extension_id(id: &str) -> Result<&str> {
    if is_extension_id(id) {
        Ok(id)
    } else {
        Err(not_an_id("an extension", id))
    }
}

/// `what` carries its article ("a workspace", "an extension") so the message
/// reads as a sentence.
fn not_an_id(what: &str, id: &str) -> GhostError {
    GhostError::new(ErrorKind::InvalidInput, format!("Not {what} id: {id}")).with_detail("id", id)
}

/// Creates a directory (and its parents) and returns it, so it composes inside
/// an expression.
///
/// `0o700` because the root holds the credential vault's fallback key, session
/// transcripts, and whatever the agent has been told; the default mode leaves
/// all of that world-readable on a shared host.
pub fn ensure_dir(dir: &Path) -> Result<&Path> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    Ok(dir)
}
