//! `read_file` — the workspace read.
//!
//! Two constraints shape it, and both are about what happens on the *large*
//! file rather than the ordinary one:
//!
//!  - **The read is bounded before it happens.** Reading a 2 GB log whole
//!    allocates 2 GB and then the registry truncates it to 8 000 characters.
//!    The handler opens the file and reads at most a UTF-8 worst case of the
//!    output budget, so the memory cost is bounded by the budget rather than by
//!    whatever the model happened to point at.
//!
//!  - **Binary content is refused, not dumped.** A model given a few thousand
//!    bytes of a `.png` learns nothing and pays for the tokens twice — once in
//!    the tool result and again in every subsequent turn of history. The
//!    NUL-byte test is the cheap, reliable half of binary detection and is
//!    exactly the case that matters here.
//!
//! `offset`/`limit` are lines rather than bytes because that is the unit the
//! model reasons in, and because a byte range can split a codepoint.

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::AsyncReadExt as _;

use crate::builtin::built;
use crate::builtin::shared::{clamp_note, fs_failure};
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

/// UTF-8's worst case, so the byte budget can never cut short the char budget.
const BYTES_PER_CHAR: u64 = 4;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    #[schemars(
        length(min = 1),
        description = "File to read. Rooted at the workspace."
    )]
    path: String,
    #[schemars(
        range(min = 1),
        description = "1-based line number to start from. Omit to start at the beginning."
    )]
    offset: Option<u64>,
    #[schemars(
        range(min = 1),
        description = "Maximum number of lines to return. Omit to read to the end."
    )]
    limit: Option<u64>,
}

struct ReadFile;

/// The lines `offset`/`limit` select, or the sentence for an offset past the
/// end.
fn window(text: &str, offset: Option<u64>, limit: Option<u64>, where_: &str) -> Result<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let from = usize::try_from(offset.unwrap_or(1).saturating_sub(1)).unwrap_or(usize::MAX);
    if from >= lines.len() {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            format!(
                "{where_} has {} lines; offset {} is past the end.",
                lines.len(),
                offset.unwrap_or(1)
            ),
        ));
    }
    let to = match limit {
        None => lines.len(),
        Some(limit) => from
            .saturating_add(usize::try_from(limit).unwrap_or(usize::MAX))
            .min(lines.len()),
    };
    Ok(lines[from..to].join("\n"))
}

impl ToolHandler for ReadFile {
    type Args = ReadFileArgs;

    fn execute<'a>(
        &'a self,
        args: ReadFileArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "read_file")?;
            let accepted = ctx.jail.accept(&args.path)?;
            // Report where the read actually happened, not what was asked for:
            // they differ whenever the path was clamped, and naming the request
            // would teach the model that `/etc/hosts` is a path this workspace
            // has.
            let where_ = accepted.relative.as_str();
            let note = clamp_note(&args.path, &accepted);
            let budget = ctx.config.max_output_chars.saturating_mul(BYTES_PER_CHAR);

            let file = tokio::fs::File::open(&accepted.path)
                .await
                .map_err(|error| fs_failure(&error, where_, &note))?;
            let stats = file
                .metadata()
                .await
                .map_err(|error| fs_failure(&error, where_, &note))?;
            if stats.is_dir() {
                return Err(WireError::new(
                    ErrorKind::InvalidInput,
                    format!("{where_} is a directory. Use list_dir instead."),
                )
                .with_detail("path", where_));
            }
            if stats.len() == 0 {
                return Ok(ToolOutput::text(format!("{where_} is empty.")));
            }

            // One byte past the budget is enough to know the file was longer
            // without reading any more of it than the answer needs.
            let wanted = stats.len().min(budget.saturating_add(1));
            let mut bytes = Vec::with_capacity(usize::try_from(wanted).unwrap_or(0));
            file.take(wanted)
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| fs_failure(&error, where_, &note))?;

            if bytes.contains(&0) {
                return Err(WireError::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "{where_} looks like a binary file ({} bytes) and was not read.",
                        stats.len()
                    ),
                )
                .with_detail("path", where_)
                .with_detail("size", stats.len()));
            }

            let clipped = u64::try_from(bytes.len()).unwrap_or(u64::MAX) > budget;
            let kept = if clipped {
                &bytes[..usize::try_from(budget).unwrap_or(bytes.len())]
            } else {
                &bytes[..]
            };
            let mut text = String::from_utf8_lossy(kept).into_owned();

            if args.offset.is_some() || args.limit.is_some() {
                match window(&text, args.offset, args.limit, where_) {
                    Ok(selected) => text = selected,
                    // Past the end is an answer, not a failure: the model asked
                    // a question about a file that exists and gets told.
                    Err(error) => return Ok(ToolOutput::text(error.message)),
                }
            }

            assert_not_aborted(&ctx.token, "read_file")?;
            let clip = if clipped {
                format!(
                    "\n\n[read_file: showing the first {budget} of {} bytes. Use offset/limit for the rest.]",
                    stats.len()
                )
            } else {
                String::new()
            };
            // A successful read of a clamped path needs the note as much as a
            // failed one: content came back, and without this the model
            // believes it is holding the host's file.
            let noted = if note.is_empty() {
                String::new()
            } else {
                format!("\n\n[read_file:{note}]")
            };
            Ok(ToolOutput::text(format!("{text}{clip}{noted}")))
        })
    }
}

/// The `read_file` tool.
pub fn read_file_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "read_file",
            "Read a UTF-8 text file from the workspace. The workspace is the root: \"/x\" and \"../x\" both resolve inside it, never outside. Use offset/limit to page through a large file.",
        )
        .risk(ToolRisk::Safe)
        .annotations(ToolAnnotations {
            title: Some("Read file".to_owned()),
            read_only_hint: Some(true),
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        ReadFile,
    ))
}
