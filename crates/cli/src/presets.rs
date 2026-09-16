//! Where agent presets are found on disk.
//!
//! **One kind of thing, in one shape, in one place.** A preset is a YAML file
//! named `<id>.yaml` — the filename *is* the agent id — and that is true
//! whether it came from the catalogue or an operator wrote it this morning.
//! There is no second location and no second format: an agent that works in a
//! container says so in its own `container.name` and is otherwise an ordinary
//! preset, which is why one lives beside the container-less ones rather than
//! beside the manifest of the box it names.
//!
//! That is the whole of the resolution order, and it is two directories:
//!
//!  1. `<root>/presets/<id>.yaml` — an operator's own. Adding one is adding a
//!     file; there is no install step, because there is nothing to install.
//!  2. `agents/<id>.yaml` in the catalogue — the ones `ghostai preset install`
//!     offers. Where that directory *is* is [`crate::catalogue`]'s question,
//!     and it is passed in here rather than resolved, because the answer
//!     depends on the root and on `--from`.
//!
//! Operator first, so a local preset wins over the catalogue's of the same
//! name. What somebody put on this machine is more specific than what a package
//! guessed.

use std::path::{Path, PathBuf};

use garde::Validate as _;
use ghostai_core::{ErrorKind, GhostError, Result};
use ghostai_protocol::AgentPreset;

/// The extension every preset file carries. The stem is the agent id.
pub const PRESET_SUFFIX: &str = ".yaml";

/// Every directory a preset name is searched in, nearest first.
///
/// Both are passed in rather than resolved here, so a test can point them at a
/// temporary home without moving the real one.
pub fn preset_dirs(presets_dir: &Path, agents_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![presets_dir.to_path_buf()];
    dirs.extend(agents_dir.map(Path::to_path_buf));
    dirs
}

/// Parses one preset, naming the file in every refusal.
///
/// Two failures, kept apart because they send an operator to two different
/// places: text that is not YAML at all is a typo in the file, and YAML that is
/// not a preset is a file written against a different shape.
pub fn parse_preset(text: &str, source: &str) -> Result<AgentPreset> {
    let raw: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).map_err(|error| {
        GhostError::new(ErrorKind::Config, format!("{source} is not valid YAML"))
            .with_detail("source", source)
            .with_source(error)
    })?;

    let preset: AgentPreset = serde_yaml_ng::from_value(raw).map_err(|error| {
        let issues = vec![format!("  (root): {error}")];
        invalid_preset(source, &issues).with_source(error)
    })?;

    if let Err(report) = preset.validate() {
        let issues: Vec<String> = report
            .iter()
            .map(|(path, error)| issue_line(&path.to_string(), error))
            .collect();
        return Err(invalid_preset(source, &issues));
    }
    Ok(preset)
}

/// One constraint the parsed preset breaks, as `  path: message`.
///
/// The shape the settings loader writes its own issues in, because an operator
/// reading either is reading the same kind of list and the indent is what makes
/// it one under the sentence above it.
fn issue_line(path: &str, error: &garde::Error) -> String {
    let label = if path.is_empty() { "(root)" } else { path };
    format!("  {label}: {error}")
}

fn invalid_preset(source: &str, issues: &[String]) -> GhostError {
    GhostError::new(
        ErrorKind::Config,
        format!(
            "{source} is not a valid agent preset:\n{}",
            issues.join("\n")
        ),
    )
    .with_detail("source", source)
    .with_detail("issues", issues.to_vec())
}

/// Reads and parses one preset file.
pub fn read_preset(path: &Path) -> Result<AgentPreset> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        GhostError::new(
            ErrorKind::Config,
            format!("{} could not be read", path.display()),
        )
        .with_detail("path", path.to_string_lossy())
        .with_source(error)
    })?;
    parse_preset(&text, &path.to_string_lossy())
}

/// The ids in a directory, sorted.
///
/// A missing directory is empty rather than a failure: `<root>/presets` exists
/// only once somebody has put something in it.
///
/// Reads names, not contents. A test holds every shipped file's `id` equal to
/// its filename, which is what lets a listing stay a directory read.
pub fn list_presets(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().into_owned();
            Some(name.strip_suffix(PRESET_SUFFIX)?.to_owned())
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// The ids installable from any of `dirs`, deduplicated and sorted.
pub fn list_all_presets(dirs: &[PathBuf]) -> Vec<String> {
    let mut ids: Vec<String> = dirs.iter().flat_map(|dir| list_presets(dir)).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The first directory in `dirs` holding `<id>.yaml`, or `None`.
pub fn find_preset(dirs: &[PathBuf], id: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(format!("{id}{PRESET_SUFFIX}")))
        .find(|path| path.exists())
}
