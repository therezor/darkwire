//! Copying skill sheets out of the catalogue and into a workspace.
//!
//! A preset's `skills` list names directories under `<catalogue>/skills/`; this
//! copies each one to `<workspace>/skills/<name>/`, byte for byte. Both
//! `ghostai preset install` and `ghostai agent install` need it, which is why
//! it is here rather than inside either.
//!
//! ## It is a copy, not an install
//!
//! Nothing is rewritten on the way in, and nothing records afterwards that a
//! sheet arrived with a preset. The sheet declares its own `agents:` line, so
//! what an operator reads in the catalogue is what lands in the workspace, and
//! editing it afterwards is editing their own file rather than diverging from
//! something. There is no approval gate and no hash: a sheet is prose, and the
//! preset's own `system_prompt` — unapproved prose from the same catalogue,
//! already — sets the bar. Running the command is the operator action.
//!
//! ## Nothing here refuses
//!
//! A missing sheet, a symlink, a sheet over the bounds: each costs that sheet
//! and a line in the report. This is deliberately unlike the toolbox half,
//! which refuses — and the asymmetry is the point. An agent whose toolbox is
//! missing cannot run at all, so accepting the entry would write a config the
//! server refuses to boot on. An agent missing a sheet runs, with one fewer
//! index line.
//!
//! ## The bounds are a floor under a bad publish
//!
//! Not a security boundary. The names have already been checked as slugs by the
//! time they reach here, so a traversal cannot be represented; what is left is a
//! catalogue that shipped something enormous by accident, and the answer to
//! that is a warning rather than a half-filled disk.
//!
//! The name check is repeated here anyway, in [`install_skills`], rather than
//! being left to the parser. It is the one rule in this module whose failure
//! would be a path outside the workspace rather than a missing index line, and
//! a rule like that should hold wherever the value is used rather than wherever
//! it was last looked at.

use std::path::{Path, PathBuf};

use ghostai_agent::{SKILL_FILENAME, SKILLS_DIRNAME, parse_skill_agents};
use ghostai_core::frontmatter::parse_frontmatter;
use ghostai_core::paths::workspace_dir_for;
use ghostai_core::{ErrorKind, GhostError, GhostPaths, Result};
use ghostai_protocol::{DEFAULT_WORKSPACE_ID, is_slug_id};

/// Files in one sheet directory. A sheet is a page and its attachments.
pub const MAX_SKILL_FILES: usize = 64;

/// One file in a sheet directory.
///
/// Deliberately not the prompt budget a sheet's *page* is held to. A sheet
/// directory legitimately holds a checklist, a template or a script, and none
/// of those reach the prompt unless the model opens them.
pub const MAX_SKILL_FILE_BYTES: u64 = 1024 * 1024;

/// Everything one install writes, across every sheet.
pub const MAX_SKILL_TOTAL_BYTES: u64 = 8 * 1024 * 1024;

/// How deep a sheet directory may nest.
pub const MAX_SKILL_DEPTH: usize = 4;

/// The mode every directory this writes is created with.
///
/// `0o700` for the reason the GhostAI root is: a workspace holds whatever the
/// agent has been told, and the default mode leaves it world-readable on a
/// shared host.
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// What one preset asks to be copied.
#[derive(Debug, Clone)]
pub struct SkillInstallRequest<'a> {
    /// The preset asking, used only for the scope cross-check.
    pub preset_id: &'a str,
    /// Sheet directory names, which should already be slugs.
    pub names: &'a [String],
    /// The catalogue's `skills/`. Absent means every name is missing.
    pub catalogue_skills_dir: Option<&'a Path>,
    /// `<workspace>/skills`. Absent skips the copy entirely.
    pub target_dir: Option<&'a Path>,
    /// Overwrite a sheet directory the workspace already has.
    pub force: bool,
}

/// One sheet that was written, and how many files it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenSheet {
    /// The sheet directory's name.
    pub name: String,
    /// Files copied.
    pub files: usize,
}

/// What one install did, phrased for the report above it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillInstallResult {
    /// Sheets written, with how many files each took.
    pub written: Vec<WrittenSheet>,
    /// Already in the workspace, and left alone.
    pub kept: Vec<String>,
    /// Named by a preset, not in this catalogue.
    pub missing: Vec<String>,
    /// One line each, already phrased for the report.
    pub warnings: Vec<String>,
}

/// Copies the sheets a preset names, and reports rather than fails.
pub fn install_skills(request: &SkillInstallRequest<'_>) -> SkillInstallResult {
    let mut result = SkillInstallResult::default();
    let (Some(target_dir), false) = (request.target_dir, request.names.is_empty()) else {
        return result;
    };

    let mut budget = Budget {
        remaining: i128::from(MAX_SKILL_TOTAL_BYTES),
    };

    for name in request.names {
        // Before the join, because the join is what a name that is not a slug
        // would escape through.
        if !is_slug_id(name) {
            result.warnings.push(format!(
                "skill \"{name}\" is not a usable name, so it was skipped"
            ));
            continue;
        }

        let Some(source) = sheet_dir(request.catalogue_skills_dir, name) else {
            result.missing.push(name.clone());
            continue;
        };

        let destination = target_dir.join(name);
        if destination.exists() && !request.force {
            result.kept.push(name.clone());
            continue;
        }

        let Some(files) = copy_tree(
            &source,
            &destination,
            &mut budget,
            &mut result.warnings,
            name,
        ) else {
            continue;
        };

        result.written.push(WrittenSheet {
            name: name.clone(),
            files,
        });
        if let Some(mismatch) = scope_mismatch(&source, name, request.preset_id) {
            result.warnings.push(mismatch);
        }
    }

    result
}

/// Where a workspace's sheets go.
///
/// **Resolution only — nothing is created here.** Every install path calls this
/// while working out its paths, long before it knows whether any preset names a
/// sheet, so a directory creation in here would make `<root>/workspace` on
/// every `ghostai agent install` and would turn an unwritable root into a
/// missing-file error thrown over whatever the real problem was. The
/// directories are made by the copy, which happens only when there is something
/// to write.
///
/// A named workspace must already exist, and that check *is* this function's
/// job: [`workspace_dir_for`] validates the *shape* of an id and joins, never
/// consulting the registry, which lives in SQLite. So `-W typo` would otherwise
/// make a tree no UI ever lists and nothing ever reads.
pub fn skills_target_dir(paths: &GhostPaths, workspace_id: &str) -> Result<PathBuf> {
    if workspace_id == DEFAULT_WORKSPACE_ID {
        return Ok(paths.workspace.join(SKILLS_DIRNAME));
    }

    let dir = workspace_dir_for(paths, workspace_id)?;
    if !dir.exists() {
        return Err(GhostError::new(
            ErrorKind::InvalidInput,
            format!(
                "There is no {workspace_id} workspace at {}.\n  Workspaces are created in the web \
                 UI, or by a session bound to one.\n  Leave -W off to install into the default \
                 workspace.",
                dir.display()
            ),
        )
        .with_detail("workspaceId", workspace_id)
        .with_detail("dir", dir.to_string_lossy()));
    }
    Ok(dir.join(SKILLS_DIRNAME))
}

/// The catalogue's copy of one sheet, if it has one with a `SKILL.md`.
fn sheet_dir(skills_dir: Option<&Path>, name: &str) -> Option<PathBuf> {
    let dir = skills_dir?.join(name);
    if !dir.join(SKILL_FILENAME).exists() {
        return None;
    }
    // The metadata of the link rather than of its target, so a symlinked sheet
    // directory is not followed out of the catalogue — the same property
    // reading a workspace's own sheets gets from its directory entries.
    std::fs::symlink_metadata(&dir)
        .ok()
        .filter(std::fs::Metadata::is_dir)
        .map(|_| dir)
}

/// What is left of the whole-install byte budget.
///
/// Signed and wider than the budget it holds, so the subtraction that trips it
/// can go negative without wrapping — which is what "the file that broke the
/// budget" has to do to be detected at all.
struct Budget {
    remaining: i128,
}

/// One directory waiting to be copied, and how deep it sits.
struct Level {
    from: PathBuf,
    to: PathBuf,
    depth: usize,
}

/// Copies one sheet, or reports why it stopped.
///
/// Answers with the file count, or `None` when a bound was hit — in which case
/// whatever was already written stays. Rolling back would mean deleting files
/// in a directory an operator may have put things in, which is a larger claim
/// than this has any business making.
fn copy_tree(
    source: &Path,
    destination: &Path,
    budget: &mut Budget,
    warnings: &mut Vec<String>,
    name: &str,
) -> Option<usize> {
    let mut files = 0usize;
    let mut stack = vec![Level {
        from: source.to_path_buf(),
        to: destination.to_path_buf(),
        depth: 0,
    }];

    while let Some(level) = stack.pop() {
        if level.depth > MAX_SKILL_DEPTH {
            warnings.push(format!(
                "skill \"{name}\" nests deeper than {MAX_SKILL_DEPTH} levels; the rest was not \
                 copied"
            ));
            return None;
        }

        if make_dir(&level.to).is_err() {
            warnings.push(format!(
                "skill \"{name}\" could not be written to {}; the rest was not copied",
                level.to.display()
            ));
            return None;
        }

        let Ok(entries) = std::fs::read_dir(&level.from) else {
            warnings.push(format!(
                "skill \"{name}\" could not be read from {}; the rest was not copied",
                level.from.display()
            ));
            return None;
        };

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let from = level.from.join(&file_name);
            let Ok(kind) = entry.file_type() else {
                continue;
            };

            // Neither followed nor copied as a link. A catalogue is a package
            // manager's output, so a link in one is a packaging accident rather
            // than an attack — but following it would copy from outside the
            // catalogue, and recreating it would put a dangling link in the
            // workspace.
            if kind.is_symlink() {
                warnings.push(format!(
                    "skill \"{name}\" contains a symlink ({}), which was skipped",
                    file_name.to_string_lossy()
                ));
                continue;
            }

            if kind.is_dir() {
                stack.push(Level {
                    from,
                    to: level.to.join(&file_name),
                    depth: level.depth + 1,
                });
                continue;
            }

            if !kind.is_file() {
                continue;
            }

            files += 1;
            if files > MAX_SKILL_FILES {
                warnings.push(format!(
                    "skill \"{name}\" holds more than {MAX_SKILL_FILES} files; the rest was not \
                     copied"
                ));
                return None;
            }

            let bytes = entry.metadata().map(|meta| meta.len()).unwrap_or_default();
            if bytes > MAX_SKILL_FILE_BYTES {
                warnings.push(format!(
                    "skill \"{name}\" has a file over {} KB ({}), which was skipped",
                    MAX_SKILL_FILE_BYTES / 1024,
                    file_name.to_string_lossy()
                ));
                files -= 1;
                continue;
            }

            budget.remaining -= i128::from(bytes);
            if budget.remaining < 0 {
                warnings.push(format!(
                    "the sheets came to more than {} MB; \"{name}\" was not finished",
                    MAX_SKILL_TOTAL_BYTES / 1024 / 1024
                ));
                return None;
            }

            if std::fs::copy(&from, level.to.join(&file_name)).is_err() {
                warnings.push(format!(
                    "skill \"{name}\" has a file that could not be copied ({}), which was skipped",
                    file_name.to_string_lossy()
                ));
                files -= 1;
            }
        }
    }

    Some(files)
}

#[cfg(unix)]
fn make_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(dir)
}

#[cfg(not(unix))]
fn make_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// The warning for a sheet scoped away from the agent that brought it.
///
/// Only when the sheet names agents and this preset is not among them. An
/// absent or empty `agents:` means every agent, which includes this one, so it
/// is not a mismatch — stated here because "it did not warn" should read as a
/// decision rather than as a case nobody thought about.
///
/// A malformed `agents:` line is the *workspace's* problem once the sheet is
/// copied, and it is reported on the turn that reads it, where the operator can
/// act on it. Warning here as well would report the same file twice for one
/// mistake, in a command whose output is about what it installed.
fn scope_mismatch(source: &Path, name: &str, preset_id: &str) -> Option<String> {
    // `sheet_dir` already proved the file is there, so a failure here is a race
    // or a permission problem — either way not worth a line about scope.
    let text = std::fs::read_to_string(source.join(SKILL_FILENAME)).ok()?;
    let front = parse_frontmatter(&text);
    let agents = parse_skill_agents(front.fields.get("agents").map(String::as_str), name);

    if agents.is_empty() || agents.contains(&preset_id.to_lowercase()) {
        return None;
    }
    Some(format!(
        "skill \"{name}\" is scoped to {}, so the {preset_id} agent will not see it",
        agents.join(", ")
    ))
}
