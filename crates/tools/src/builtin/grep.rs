//! `grep` — find text in the workspace.
//!
//! Ripgrep as a library, not as a child process, and the three reasons are the
//! whole design. The search has to stay inside the workspace jail, which a
//! spawned binary given a model-written pattern cannot promise. It has to work
//! in a container image that ships no search binary, because the alternative is
//! a tool that exists on a laptop and not in production. And it has to bound
//! its own output, because the registry's cut keeps the head and the tail of a
//! result and silently loses the matches in between.
//!
//! The limit is a count of matches rather than a count of characters for the
//! same reason. Three hundred hits truncated to fit a character budget tell the
//! model nothing about how many it did not see; a hundred hits and a sentence
//! saying the limit was reached tell it exactly what to do next.
//!
//! Three result modes rather than one because the useful question changes.
//! `matches` answers "what does this code look like", `files` answers "where
//! should I look", and `count` answers "how much of this is there" — and the
//! last two cost a fraction of the first on a term that appears everywhere,
//! which is precisely when the first one is useless.

use std::path::{Path, PathBuf};

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::overrides::OverrideBuilder;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::builtin::built;
use crate::builtin::shared::clamp_note;
use crate::builtin::walk::{WalkTally, files};
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

/// The name, once, for the error paths that have to say it.
const NAME: &str = "grep";

/// Matches returned before the tool stops and says it stopped.
const DEFAULT_LIMIT: u64 = 100;

/// Where a matching line is cut.
///
/// A minified bundle is one line of two hundred thousand characters. The match
/// is real and the line is not worth reading, so the line is clipped and the
/// clip is named.
const MAX_LINE_CHARS: usize = 500;

/// Context lines allowed either side of a match.
const MAX_CONTEXT: u64 = 10;

fn dot() -> String {
    ".".to_owned()
}

fn default_limit() -> u64 {
    DEFAULT_LIMIT
}

/// What a result looks like.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GrepMode {
    /// Every matching line, with its path and line number.
    #[default]
    Matches,
    /// The path of each file that matches, once.
    Files,
    /// How many matches each file holds.
    Count,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GrepArgs {
    #[schemars(
        length(min = 1),
        description = "Regular expression in Rust regex syntax. No lookaround and no backreferences. Set literal to search for the text exactly as written instead."
    )]
    pattern: String,
    #[serde(default = "dot")]
    #[schemars(
        description = "File or directory to search. Rooted at the workspace. Defaults to the whole workspace."
    )]
    path: String,
    #[schemars(
        description = "Only search files whose path matches this glob, such as \"*.rs\" or \"src/**/*.ts\". A glob with no slash matches a file name at any depth."
    )]
    glob: Option<String>,
    #[serde(default)]
    #[schemars(description = "Match regardless of letter case.")]
    ignore_case: bool,
    #[serde(default)]
    #[schemars(description = "Search for the pattern as literal text rather than as a regex.")]
    literal: bool,
    #[serde(default)]
    #[schemars(
        range(max = 10),
        description = "Lines of surrounding context to show either side of each match."
    )]
    context: u64,
    #[serde(default)]
    #[schemars(
        description = "matches returns the matching lines, files returns only the paths that match, count returns the number of matches per file. Prefer files or count for a term that appears everywhere."
    )]
    mode: GrepMode,
    #[serde(default = "default_limit")]
    #[schemars(
        range(min = 1, max = 1000),
        description = "Stop after this many matches, or this many files in files and count mode."
    )]
    limit: u64,
}

/// One search, resolved: everything the blocking half needs and nothing a
/// model wrote.
#[derive(Debug, Clone)]
pub struct GrepRequest {
    /// The canonical path the jail accepted.
    pub root: PathBuf,
    /// The pattern, already known to compile.
    pub pattern: String,
    /// Path glob, or none.
    pub glob: Option<String>,
    /// Case-insensitive matching.
    pub ignore_case: bool,
    /// Literal rather than regex.
    pub literal: bool,
    /// Context lines either side.
    pub context: u64,
    /// What to return.
    pub mode: GrepMode,
    /// Match or file ceiling.
    pub limit: u64,
}

/// What one search found.
#[derive(Debug, Default, Clone)]
pub struct GrepReport {
    /// Rendered lines, in the shape `mode` asks for.
    pub lines: Vec<String>,
    /// Matches seen, up to the limit.
    pub matches: usize,
    /// Files that held at least one match.
    pub files: usize,
    /// Whether the limit stopped the search early.
    pub limited: bool,
    /// Files skipped because they are binary.
    pub binary: usize,
    /// Entries the walk could not read.
    pub unreadable: usize,
}

/// A matching line, or a context line, as the model reads it.
///
/// `path:12: text` for a match and `path-11- text` for context, which is
/// ripgrep's own distinction and the reason a model can tell one from the other
/// without being told.
fn render(path: &str, line: u64, text: &str, matched: bool) -> String {
    let body = clip(text.trim_end_matches(['\n', '\r']));
    if matched {
        format!("{path}:{line}: {body}")
    } else {
        format!("{path}-{line}- {body}")
    }
}

/// A line cut to [`MAX_LINE_CHARS`], on a character boundary, saying it was cut.
fn clip(text: &str) -> String {
    let count = text.chars().count();
    if count <= MAX_LINE_CHARS {
        return text.to_owned();
    }
    let kept: String = text.chars().take(MAX_LINE_CHARS).collect();
    let dropped = count - MAX_LINE_CHARS;
    format!("{kept} [+{dropped} chars]")
}

/// Collects one file's hits into the report.
struct Collector<'a> {
    path: &'a str,
    report: &'a mut GrepReport,
    mode: GrepMode,
    limit: usize,
    /// Matches in this file, for `count`.
    here: usize,
}

impl Collector<'_> {
    /// Whether the ceiling `mode` cares about has been reached.
    fn full(&self) -> bool {
        match self.mode {
            GrepMode::Matches => self.report.matches >= self.limit,
            GrepMode::Files | GrepMode::Count => self.report.files >= self.limit,
        }
    }
}

impl Sink for Collector<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, hit: &SinkMatch<'_>) -> std::io::Result<bool> {
        self.report.matches = self.report.matches.saturating_add(1);
        self.here = self.here.saturating_add(1);
        if self.here == 1 {
            self.report.files = self.report.files.saturating_add(1);
        }
        match self.mode {
            GrepMode::Matches => {
                let line = hit.line_number().unwrap_or_default();
                let text = String::from_utf8_lossy(hit.bytes());
                self.report.lines.push(render(self.path, line, &text, true));
            }
            // One hit is the whole answer for both, so stop reading this file.
            GrepMode::Files => {
                self.report.lines.push(self.path.to_owned());
                return Ok(false);
            }
            GrepMode::Count => {}
        }
        if self.full() {
            self.report.limited = true;
            return Ok(false);
        }
        Ok(true)
    }

    fn context(&mut self, _searcher: &Searcher, line: &SinkContext<'_>) -> std::io::Result<bool> {
        if self.mode != GrepMode::Matches {
            return Ok(true);
        }
        let number = line.line_number().unwrap_or_default();
        let text = String::from_utf8_lossy(line.bytes());
        self.report
            .lines
            .push(render(self.path, number, &text, false));
        Ok(true)
    }

    fn binary_data(&mut self, _searcher: &Searcher, _offset: u64) -> std::io::Result<bool> {
        self.report.binary = self.report.binary.saturating_add(1);
        Ok(false)
    }
}

/// The search itself, synchronous, so a test can drive it without a runtime.
///
/// Returns [`ErrorKind::Aborted`] the moment the token is cancelled, checked
/// once per file rather than once per walk: a tree of ten thousand files must
/// not hold a cancelled turn open while it finishes.
pub fn grep_blocking(request: &GrepRequest, token: &CancellationToken) -> Result<GrepReport> {
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(request.ignore_case)
        .fixed_strings(request.literal)
        .line_terminator(Some(b'\n'))
        .build(&request.pattern)
        .map_err(|error| {
            WireError::new(
                ErrorKind::InvalidInput,
                format!("pattern is not a valid regular expression: {error}"),
            )
            .with_detail("pattern", request.pattern.as_str())
        })?;

    let context = usize::try_from(request.context).unwrap_or(0);
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .before_context(context)
        .after_context(context)
        .build();

    let (candidates, tally) = search_set(request, token)?;
    // A search of a single file strips to nothing against itself, so the paths
    // are shown relative to its directory and the file keeps its name.
    let base = if request.root.is_file() {
        request.root.parent().unwrap_or(&request.root)
    } else {
        request.root.as_path()
    };
    let mut report = GrepReport {
        unreadable: tally.unreadable,
        ..GrepReport::default()
    };
    let limit = usize::try_from(request.limit).unwrap_or(usize::MAX);

    for file in candidates {
        if token.is_cancelled() {
            return Err(WireError::aborted(NAME));
        }
        let relative = display_path(base, &file);
        let mut collector = Collector {
            path: &relative,
            report: &mut report,
            mode: request.mode,
            limit,
            here: 0,
        };
        let outcome = searcher.search_path(&matcher, &file, &mut collector);
        let here = collector.here;
        if outcome.is_err() {
            report.unreadable = report.unreadable.saturating_add(1);
            continue;
        }
        if request.mode == GrepMode::Count && here > 0 {
            report.lines.push(format!("{relative}: {here}"));
        }
        if report.limited
            || (request.mode != GrepMode::Matches && report.files >= limit)
            || (request.mode == GrepMode::Matches && report.matches >= limit)
        {
            report.limited = true;
            break;
        }
    }
    Ok(report)
}

/// The files one request searches: the root itself, or the walk under it.
fn search_set(
    request: &GrepRequest,
    token: &CancellationToken,
) -> Result<(Vec<PathBuf>, WalkTally)> {
    if request.root.is_file() {
        return Ok((vec![request.root.clone()], WalkTally::default()));
    }
    let (mut found, tally) = files(&request.root, token, NAME)?;
    if let Some(glob) = request.glob.as_deref() {
        let mut overrides = OverrideBuilder::new(&request.root);
        overrides.add(glob).map_err(|error| {
            WireError::new(
                ErrorKind::InvalidInput,
                format!("glob is not a valid pattern: {error}"),
            )
            .with_detail("glob", glob)
        })?;
        let matcher = overrides.build().map_err(|error| {
            WireError::new(
                ErrorKind::InvalidInput,
                format!("glob is not a valid pattern: {error}"),
            )
            .with_detail("glob", glob)
        })?;
        found.retain(|path| matcher.matched(path, false).is_whitelist());
    }
    Ok((found, tally))
}

/// A found path as the model should see it: relative to what it asked about.
fn display_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .into_owned()
}

struct Grep;

impl ToolHandler for Grep {
    type Args = GrepArgs;

    fn execute<'a>(
        &'a self,
        args: GrepArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            // Before the spawn, so an already-cancelled turn costs no thread.
            assert_not_aborted(&ctx.token, NAME)?;
            if args.context > MAX_CONTEXT {
                return Err(WireError::new(
                    ErrorKind::InvalidInput,
                    format!("context must be {MAX_CONTEXT} lines or fewer."),
                ));
            }
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

            let request = GrepRequest {
                root: accepted.path.clone(),
                pattern: args.pattern.clone(),
                glob: args.glob.clone(),
                ignore_case: args.ignore_case,
                literal: args.literal,
                context: args.context,
                mode: args.mode,
                limit: args.limit,
            };
            let token = ctx.token.clone();
            let report = tokio::task::spawn_blocking(move || grep_blocking(&request, &token))
                .await
                .map_err(|error| {
                    WireError::new(ErrorKind::Internal, format!("search failed: {error}"))
                })??;

            Ok(render_report(&report, &args, &where_, &note))
        })
    }
}

/// The report as the model reads it, trailers last.
fn render_report(report: &GrepReport, args: &GrepArgs, where_: &str, note: &str) -> ToolOutput {
    let mut lines = report.lines.clone();
    if lines.is_empty() {
        let pattern = &args.pattern;
        return ToolOutput::text(format!("No matches for \"{pattern}\" in {where_}.{note}"))
            .with_detail("matches", 0)
            .with_detail("files", 0);
    }
    if report.limited {
        let limit = args.limit;
        let unit = if args.mode == GrepMode::Matches {
            "matches"
        } else {
            "files"
        };
        lines.push(format!(
            "[grep: limit of {limit} {unit} reached; refine the pattern, use mode=files, or raise limit.]"
        ));
    }
    if report.binary > 0 || report.unreadable > 0 {
        lines.push(format!(
            "[grep: skipped {} binary and {} unreadable files.]",
            report.binary, report.unreadable
        ));
    }
    if !note.is_empty() {
        lines.push(format!("[grep:{note}]"));
    }
    ToolOutput::text(lines.join("\n"))
        .with_detail("matches", report.matches)
        .with_detail("files", report.files)
        .with_detail("limited", report.limited)
        .with_detail("binarySkipped", report.binary)
}

/// The `grep` tool.
pub fn grep_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            NAME,
            "Search the workspace for text with a regular expression. The workspace is the root: \"/x\" and \"../x\" both resolve inside it, never outside. Skips what .gitignore skips and skips binary files. Use mode=files to find which files match and mode=count to find how many, which cost far less than the lines themselves on a common term.",
        )
        .risk(ToolRisk::Safe)
        .annotations(ToolAnnotations {
            title: Some("Search".to_owned()),
            read_only_hint: Some(true),
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        Grep,
    ))
}
