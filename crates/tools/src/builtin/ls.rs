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

use std::cmp::Ordering;
use std::path::Path;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use walkdir::{DirEntry, WalkDir};

use crate::builtin::built;
use crate::builtin::shared::{clamp_note, format_bytes, fs_failure};
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

const DEFAULT_MAX_ENTRIES: u64 = 500;

/// How many entries a walk visits in all before it stops counting.
///
/// Past the cap nothing is kept, only counted, and counting a tree of millions
/// costs the whole walk's time for one number in a notice.
const COUNT_SCAN_LIMIT: usize = 100_000;

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

/// What one walk produced.
#[derive(Debug, Default)]
struct Listing {
    /// The shown entries, already formatted.
    lines: Vec<String>,
    /// Entries past the cap.
    omitted: usize,
    /// The count stopped at [`COUNT_SCAN_LIMIT`], so `omitted` is a floor.
    counted_partially: bool,
    /// Entries that could not be read, skipped rather than raised.
    unreadable: usize,
}

/// Directories first, then name, within each directory: the order someone
/// reading a listing expects, and stable across filesystems, which directory
/// order is not. A recursive listing is in tree order.
fn listing_order(left: &DirEntry, right: &DirEntry) -> Ordering {
    right
        .file_type()
        .is_dir()
        .cmp(&left.file_type().is_dir())
        .then_with(|| {
            left.file_name()
                .to_string_lossy()
                .encode_utf16()
                .cmp(right.file_name().to_string_lossy().encode_utf16())
        })
}

/// The entries under `root`, keeping at most `cap` of them.
///
/// Blocking, so it runs off the async runtime. An unreadable entry below the
/// root is counted and skipped; one at the root itself is the whole answer.
fn walk(
    root: &Path,
    recursive: bool,
    cap: usize,
    token: &CancellationToken,
) -> std::result::Result<Listing, std::io::Error> {
    let mut walker = WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .sort_by(listing_order);
    if !recursive {
        walker = walker.max_depth(1);
    }
    let mut listing = Listing::default();
    let mut visited = 0usize;
    for item in walker {
        if token.is_cancelled() {
            break;
        }
        let entry = match item {
            Ok(entry) => entry,
            Err(error) if error.depth() == 0 => {
                return Err(error
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other("directory walk failed")));
            }
            Err(_) => {
                listing.unreadable = listing.unreadable.saturating_add(1);
                continue;
            }
        };
        visited = visited.saturating_add(1);
        if listing.lines.len() >= cap {
            listing.omitted = listing.omitted.saturating_add(1);
            if visited >= COUNT_SCAN_LIMIT {
                listing.counted_partially = true;
                break;
            }
            continue;
        }
        listing.lines.push(line(root, &entry));
    }
    Ok(listing)
}

/// One entry as the model reads it.
fn line(root: &Path, entry: &DirEntry) -> String {
    let name = entry
        .path()
        .strip_prefix(root)
        .unwrap_or(entry.path())
        .to_string_lossy();
    if entry.file_type().is_dir() {
        return format!("{name}/");
    }
    // A `stat` per file, following a link: sizes are what make a listing
    // useful for deciding whether to read something, and the cap already
    // bounds how many of these run.
    let size = match std::fs::metadata(entry.path()) {
        Ok(stats) => format_bytes(stats.len()),
        Err(_) => "unreadable".to_owned(),
    };
    format!("{name} ({size})")
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

            let root = accepted.path.clone();
            let recursive = args.recursive;
            let cap = usize::try_from(args.max_entries).unwrap_or(usize::MAX);
            let token = ctx.token.clone();
            let listing = tokio::task::spawn_blocking(move || walk(&root, recursive, cap, &token))
                .await
                .map_err(|error| {
                    WireError::new(ErrorKind::Internal, format!("listing failed: {error}"))
                })?
                .map_err(|error| fs_failure(&error, where_, &note))?;
            assert_not_aborted(&ctx.token, "ls")?;

            let mut lines = listing.lines;
            if lines.is_empty() && listing.unreadable == 0 {
                return Ok(ToolOutput::text(format!("{where_} is empty.{note}")));
            }
            if listing.omitted > 0 {
                let more = if listing.counted_partially {
                    format!("more than {}", listing.omitted)
                } else {
                    listing.omitted.to_string()
                };
                lines.push(format!(
                    "… {more} more entries not shown (maxEntries={}).",
                    args.max_entries
                ));
            }
            if listing.unreadable > 0 {
                lines.push(format!(
                    "[ls: skipped {} unreadable entries.]",
                    listing.unreadable
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
