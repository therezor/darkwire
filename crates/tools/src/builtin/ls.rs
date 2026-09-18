//! `ls` — what is in a workspace directory.
//!
//! No filtering. The obvious temptation is to hide `node_modules`, `.git` and
//! dotfiles, and it is the wrong call for an agent tool: the model asked what
//! is there, and a listing that quietly omits things teaches it that the
//! directory is empty when it is not. The entry cap is the mechanism instead —
//! it bounds the output without lying about the contents, and it says how many
//! it dropped.
//!
//! Recursion never follows symlinks and so cannot loop. That matters more than
//! it sounds: a workspace containing a self-referential link is an ordinary
//! mistake, and a walk that followed it would run until it exhausted the path
//! length limit.

use std::path::{Path, PathBuf};

use darkwire_core::Result;
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;
use walkdir::WalkDir;

use crate::builtin::built;
use crate::builtin::shared::{clamp_note, format_bytes, fs_failure};
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

const DEFAULT_MAX_ENTRIES: u64 = 500;

fn dot() -> String {
    ".".to_owned()
}

fn default_max_entries() -> u64 {
    DEFAULT_MAX_ENTRIES
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListDirArgs {
    #[serde(default = "dot")]
    #[schemars(
        description = "Directory to list. Rooted at the workspace. Defaults to the root itself."
    )]
    path: String,
    #[serde(default)]
    #[schemars(description = "Walk subdirectories as well.")]
    recursive: bool,
    #[serde(default = "default_max_entries")]
    #[schemars(range(min = 1), description = "Stop after this many entries.")]
    max_entries: u64,
}

struct Entry {
    /// Relative to the directory that was asked for, not to the root the walk
    /// happens to have started from.
    name: String,
    is_directory: bool,
    absolute: PathBuf,
}

/// Every entry under `root`, directories first, then name — the order someone
/// reading a listing expects, and stable across filesystems, which directory
/// order is not.
fn walk(root: &Path, recursive: bool) -> std::result::Result<Vec<Entry>, std::io::Error> {
    let mut walker = WalkDir::new(root).follow_links(false).min_depth(1);
    if !recursive {
        walker = walker.max_depth(1);
    }
    let mut entries = Vec::new();
    for item in walker {
        let entry = item.map_err(|error| {
            error
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("directory walk failed"))
        })?;
        let name = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();
        entries.push(Entry {
            name,
            is_directory: entry.file_type().is_dir(),
            absolute: entry.into_path(),
        });
    }
    entries.sort_by(|left, right| {
        right
            .is_directory
            .cmp(&left.is_directory)
            .then_with(|| left.name.encode_utf16().cmp(right.name.encode_utf16()))
    });
    Ok(entries)
}

struct ListDir;

impl ToolHandler for ListDir {
    type Args = ListDirArgs;

    fn execute<'a>(
        &'a self,
        args: ListDirArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "ls")?;
            let accepted = ctx.jail.accept(&args.path)?;
            let where_ = if accepted.relative.is_empty() {
                "."
            } else {
                accepted.relative.as_str()
            };
            let note = clamp_note(&args.path, &accepted);

            let sorted = walk(&accepted.path, args.recursive)
                .map_err(|error| fs_failure(&error, where_, &note))?;
            let shown = usize::try_from(args.max_entries).unwrap_or(usize::MAX);
            let mut lines: Vec<String> = Vec::new();
            for entry in sorted.iter().take(shown) {
                assert_not_aborted(&ctx.token, "ls")?;
                if entry.is_directory {
                    lines.push(format!("{}/", entry.name));
                    continue;
                }
                // A `stat` per file: sizes are what make a listing useful for
                // deciding whether to read something, and the cap already
                // bounds how many of these run.
                let size = match tokio::fs::metadata(&entry.absolute).await {
                    Ok(stats) => format_bytes(stats.len()),
                    Err(_) => "unreadable".to_owned(),
                };
                lines.push(format!("{} ({size})", entry.name));
            }

            if lines.is_empty() {
                return Ok(ToolOutput::text(format!("{where_} is empty.{note}")));
            }
            let omitted = sorted.len().saturating_sub(shown);
            if omitted > 0 {
                lines.push(format!(
                    "… {omitted} more entries not shown (maxEntries={}).",
                    args.max_entries
                ));
            }
            if !note.is_empty() {
                lines.push(format!("[ls:{note}]"));
            }
            Ok(ToolOutput::text(lines.join("\n")))
        })
    }
}

/// The `ls` tool.
pub fn ls_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "ls",
            "List the contents of a workspace directory. The workspace is the root: \"/x\" and \"../x\" both resolve inside it, never outside. Directories are marked with a trailing slash and files show their size. Nothing is hidden; use maxEntries to bound a large tree.",
        )
        .risk(ToolRisk::Safe)
        .annotations(ToolAnnotations {
            title: Some("List directory".to_owned()),
            read_only_hint: Some(true),
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        ListDir,
    ))
}
