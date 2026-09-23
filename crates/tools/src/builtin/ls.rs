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
//!
//! Every directory is opened through the workspace root without following its
//! last component, so one swapped for a symlink mid-walk is not descended.

use std::cmp::Ordering;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::DirExt as _;
use cap_std::fs::Dir;
use darkwire_core::Result;
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use darkwire_security::{JailCheck, WorkspaceJail};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::builtin::built;
use crate::builtin::shared::{clamp_note, format_bytes, in_root, root_failure};
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

/// One directory entry, as the walk sorts it.
struct Item {
    name: OsString,
    is_dir: bool,
    is_symlink: bool,
    /// Bytes, or `None` for a symlink, which is measured by following it.
    size: Option<u64>,
}

/// Directories first, then name, within each directory: the order someone
/// reading a listing expects, and stable across filesystems, which directory
/// order is not. A recursive listing is in tree order.
fn listing_order(left: &Item, right: &Item) -> Ordering {
    right.is_dir.cmp(&left.is_dir).then_with(|| {
        left.name
            .to_string_lossy()
            .encode_utf16()
            .cmp(right.name.to_string_lossy().encode_utf16())
    })
}

/// A directory's entries, sorted, with the unreadable ones counted.
fn entries(dir: &Dir, unreadable: &mut usize) -> std::io::Result<Vec<Item>> {
    let mut items = Vec::new();
    for entry in dir.entries()? {
        let Ok(entry) = entry else {
            *unreadable = unreadable.saturating_add(1);
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            *unreadable = unreadable.saturating_add(1);
            continue;
        };
        let size = if kind.is_symlink() || kind.is_dir() {
            None
        } else {
            entry.metadata().ok().map(|stats| stats.len())
        };
        items.push(Item {
            name: entry.file_name(),
            is_dir: kind.is_dir(),
            is_symlink: kind.is_symlink(),
            size,
        });
    }
    items.sort_by(listing_order);
    Ok(items)
}

/// Where one walk starts and what it may keep.
struct Walk<'a> {
    jail: &'a WorkspaceJail,
    root: &'a Dir,
    /// The start, relative to the workspace root.
    start: &'a Path,
    recursive: bool,
    cap: usize,
}

/// The entries under `start`, keeping at most `cap` of them.
///
/// Blocking, so it runs off the async runtime. An unreadable entry below the
/// start is counted and skipped; one at the start itself is the whole answer.
fn walk(walk: &Walk<'_>, token: &CancellationToken) -> std::io::Result<Listing> {
    let mut listing = Listing::default();
    if !walk.root.symlink_metadata(walk.start)?.is_dir() {
        return Ok(listing);
    }
    let first = entries(
        &walk.root.open_dir_nofollow(walk.start)?,
        &mut listing.unreadable,
    )?;
    // Depth first, in order: each frame is the entries still to visit in one
    // directory, its path under the start, and the same path for display. A
    // frame holds no handle, so a deep tree cannot run the process out of
    // descriptors.
    let mut stack = vec![(first.into_iter(), PathBuf::new(), String::new())];
    let mut visited = 0usize;
    while let Some((items, below_start, prefix)) = stack.last_mut() {
        if token.is_cancelled() {
            break;
        }
        let Some(item) = items.next() else {
            stack.pop();
            continue;
        };
        let name = format!("{prefix}{}", item.name.to_string_lossy());
        let below = if walk.recursive && item.is_dir {
            let path = below_start.join(&item.name);
            let opened = walk
                .root
                .open_dir_nofollow(walk.start.join(&path))
                .and_then(|child| {
                    let children = entries(&child, &mut listing.unreadable)?;
                    Ok((children.into_iter(), path, format!("{name}/")))
                });
            if opened.is_err() {
                listing.unreadable = listing.unreadable.saturating_add(1);
            }
            opened.ok()
        } else {
            None
        };
        visited = visited.saturating_add(1);
        if listing.lines.len() >= walk.cap {
            listing.omitted = listing.omitted.saturating_add(1);
            if visited >= COUNT_SCAN_LIMIT {
                listing.counted_partially = true;
                break;
            }
        } else {
            listing.lines.push(line(walk, &name, &item));
        }
        if let Some(frame) = below {
            stack.push(frame);
        }
    }
    Ok(listing)
}

/// One entry as the model reads it.
fn line(walk: &Walk<'_>, name: &str, item: &Item) -> String {
    if item.is_dir {
        return format!("{name}/");
    }
    let size = if item.is_symlink {
        followed_size(walk, name)
    } else {
        item.size
    };
    let size = size.map_or_else(|| "unreadable".to_owned(), format_bytes);
    format!("{name} ({size})")
}

/// The size of what a symlink points at, when that is inside the workspace.
///
/// Sizes are what make a listing useful for deciding whether to read
/// something, and the cap already bounds how many of these run. The jail
/// resolves the link, so one pointing out of the workspace reads as
/// unreadable rather than reporting a file outside it.
fn followed_size(walk: &Walk<'_>, name: &str) -> Option<u64> {
    let start = walk.start.to_string_lossy();
    let path = if start == "." {
        name.to_owned()
    } else {
        format!("{start}/{name}")
    };
    let JailCheck::Accept(accepted) = walk.jail.check(&path) else {
        return None;
    };
    walk.root
        .metadata(walk.jail.beneath(&accepted))
        .ok()
        .map(|stats| stats.len())
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

            let jail = Arc::clone(&ctx.jail);
            let recursive = args.recursive;
            let cap = usize::try_from(args.max_entries).unwrap_or(usize::MAX);
            let token = ctx.token.clone();
            let listing = in_root(&ctx.jail, &accepted, "listing", move |root, start| {
                let request = Walk {
                    jail: &jail,
                    root,
                    start,
                    recursive,
                    cap,
                };
                walk(&request, &token)
            })
            .await?
            .map_err(|error| root_failure(&error, &args.path, where_, &note))?;
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
