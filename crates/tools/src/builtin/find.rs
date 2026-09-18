//! `find` — locate files by name.
//!
//! The same library-not-a-subprocess argument as `grep`, and one decision of
//! its own: results come back newest first, by modification time, rather than
//! in path order. A model asking where the tests live wants any answer; a model
//! asking which files exist in a directory it has been editing wants the ones
//! it just touched, and those are the same query. Path order buries them under
//! whatever sorts first.
//!
//! A pattern with no slash matches a file name at any depth, so `*.rs` finds
//! every Rust file rather than only the ones in the root. That is gitignore's
//! rule and ripgrep's, and it is what a model means: a pattern anchored to a
//! directory it has not looked in yet would answer nothing, and a tool that
//! answers nothing to a reasonable question gets replaced by an `exec` call.

use std::path::PathBuf;
use std::time::SystemTime;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use globset::{GlobBuilder, GlobMatcher};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::builtin::built;
use crate::builtin::shared::clamp_note;
use crate::builtin::walk::files;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

/// The name, once.
const NAME: &str = "find";

/// Paths returned before the tool stops and says it stopped.
const DEFAULT_LIMIT: u64 = 1000;

fn dot() -> String {
    ".".to_owned()
}

fn default_limit() -> u64 {
    DEFAULT_LIMIT
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FindArgs {
    #[schemars(
        length(min = 1),
        description = "Glob for the file path, such as \"**/*.rs\" or \"src/**/test_*.py\". A glob with no slash matches a file name at any depth, so \"*.rs\" finds every Rust file."
    )]
    pattern: String,
    #[serde(default = "dot")]
    #[schemars(
        description = "Directory to search. Rooted at the workspace. Defaults to the whole workspace."
    )]
    path: String,
    #[serde(default = "default_limit")]
    #[schemars(
        range(min = 1, max = 10000),
        description = "Stop after this many files."
    )]
    limit: u64,
}

/// One search, resolved.
#[derive(Debug, Clone)]
pub struct FindRequest {
    /// The canonical path the jail accepted.
    pub root: PathBuf,
    /// The glob, as written.
    pub pattern: String,
    /// Path ceiling.
    pub limit: u64,
}

/// What one search found.
#[derive(Debug, Default, Clone)]
pub struct FindReport {
    /// Workspace-relative paths, newest first.
    pub paths: Vec<String>,
    /// Files that matched, including the ones past the limit.
    pub total: usize,
    /// Entries the walk could not read.
    pub unreadable: usize,
}

/// A glob that matches a bare name at any depth.
///
/// `**/` is prepended when the pattern names no directory, which is what makes
/// `*.rs` recursive. `literal_separator` keeps `*` from crossing a `/` so
/// `src/*.rs` still means one level.
fn compile(pattern: &str) -> Result<GlobMatcher> {
    let anchored = if pattern.contains('/') {
        pattern.to_owned()
    } else {
        format!("**/{pattern}")
    };
    build(&anchored).or_else(|error| {
        // A pattern that is only meaningful unanchored, such as one starting
        // with a brace alternation, is worth one retry as written before the
        // refusal, so the error names the model's own text.
        if anchored == pattern {
            return Err(error);
        }
        build(pattern).map_err(|_| error)
    })
}

/// One `globset` compile, with the refusal a model can act on.
fn build(pattern: &str) -> Result<GlobMatcher> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|error| {
            WireError::new(
                ErrorKind::InvalidInput,
                format!("pattern is not a valid glob: {error}"),
            )
            .with_detail("pattern", pattern)
        })
}

/// The search itself, synchronous, so a test can drive it without a runtime.
pub fn find_blocking(request: &FindRequest, token: &CancellationToken) -> Result<FindReport> {
    let matcher = compile(&request.pattern)?;
    let (candidates, tally) = files(&request.root, token, NAME)?;

    let mut hits: Vec<(Option<SystemTime>, String)> = Vec::new();
    for file in candidates {
        if token.is_cancelled() {
            return Err(WireError::aborted(NAME));
        }
        let relative = file.strip_prefix(&request.root).unwrap_or(&file);
        if !matcher.is_match(relative) {
            continue;
        }
        let modified = std::fs::metadata(&file)
            .ok()
            .and_then(|stats| stats.modified().ok());
        hits.push((modified, relative.to_string_lossy().into_owned()));
    }

    // Newest first, a file with no readable timestamp last, ties by path so two
    // runs of the same search agree.
    hits.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.encode_utf16().cmp(right.1.encode_utf16()))
    });

    let total = hits.len();
    let shown = usize::try_from(request.limit).unwrap_or(usize::MAX);
    Ok(FindReport {
        paths: hits.into_iter().take(shown).map(|(_, path)| path).collect(),
        total,
        unreadable: tally.unreadable,
    })
}

struct Find;

impl ToolHandler for Find {
    type Args = FindArgs;

    fn execute<'a>(
        &'a self,
        args: FindArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, NAME)?;
            let accepted = ctx.jail.accept(&args.path)?;
            // "the workspace" rather than "." so the sentence naming it reads as
            // a sentence: "in ." followed by a full stop is two dots and a
            // model reading it back copies the pair as a path.
            let where_ = if accepted.relative.is_empty() {
                "the workspace".to_owned()
            } else {
                accepted.relative.clone()
            };
            let note = clamp_note(&args.path, &accepted);

            let request = FindRequest {
                root: accepted.path.clone(),
                pattern: args.pattern.clone(),
                limit: args.limit,
            };
            let token = ctx.token.clone();
            let report = tokio::task::spawn_blocking(move || find_blocking(&request, &token))
                .await
                .map_err(|error| {
                    WireError::new(ErrorKind::Internal, format!("search failed: {error}"))
                })??;

            if report.paths.is_empty() {
                let pattern = &args.pattern;
                return Ok(ToolOutput::text(format!(
                    "No files match \"{pattern}\" under {where_}.{note}"
                ))
                .with_detail("total", 0));
            }

            let mut lines = report.paths.clone();
            let omitted = report.total.saturating_sub(report.paths.len());
            if omitted > 0 {
                let limit = args.limit;
                lines.push(format!(
                    "[find: {omitted} more files not shown (limit={limit}); narrow the pattern or raise limit.]"
                ));
            }
            if report.unreadable > 0 {
                lines.push(format!(
                    "[find: skipped {} unreadable entries.]",
                    report.unreadable
                ));
            }
            if !note.is_empty() {
                lines.push(format!("[find:{note}]"));
            }
            Ok(ToolOutput::text(lines.join("\n"))
                .with_detail("total", report.total)
                .with_detail("shown", report.paths.len()))
        })
    }
}

/// The `find` tool.
pub fn find_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            NAME,
            "Find files in the workspace by glob. The workspace is the root: \"/x\" and \"../x\" both resolve inside it, never outside. Skips what .gitignore skips. Results come back most recently modified first, so the files being worked on are at the top.",
        )
        .risk(ToolRisk::Safe)
        .annotations(ToolAnnotations {
            title: Some("Find files".to_owned()),
            read_only_hint: Some(true),
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        Find,
    ))
}
