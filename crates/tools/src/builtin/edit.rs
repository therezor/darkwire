//! `edit` — replace exact strings in a workspace file.
//!
//! String replacement rather than a diff or a line range, for one reason: a
//! unique literal is the only edit address a model can produce that stays
//! correct between the read and the write. Line numbers go stale the moment
//! anything else touches the file, and a unified diff asks the model to get
//! hunk arithmetic right, which it does not reliably, and a wrong hunk applies
//! cleanly to the wrong place.
//!
//! The uniqueness rule is what makes that safe. `oldText` occurring twice is
//! ambiguous, and picking the first is a coin flip that silently edits the
//! wrong call site; the tool refuses and tells the model to include more
//! surrounding context. `replaceAll` is available when the model means every
//! occurrence, and saying so is a different claim from not having noticed.
//!
//! `edits` is the same rule several times over, in one call and one write. It
//! exists because a refactor is rarely one hunk, and four separate calls to
//! change four call sites can leave the file in a state that compiles in none
//! of the intermediate steps. Every `oldText` is matched against the file as it
//! was before any of them applied, so the model does not have to predict how
//! its own earlier edit moved the text, and either all of them land or none
//! does.

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::builtin::built;
use crate::builtin::shared::{clamp_note, fs_failure};
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

/// Lines of diff shown before the rest is summarised.
const MAX_DIFF_LINES: usize = 40;

/// Lines of context either side of a changed region in the diff.
const DIFF_CONTEXT: usize = 2;

/// How much of an `oldText` a refusal quotes back.
const QUOTE_CHARS: usize = 40;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditBlock {
    #[schemars(
        length(min = 1),
        description = "Exact text to replace, including whitespace. Must occur exactly once in the file as it was before any of these edits."
    )]
    old_text: String,
    #[schemars(description = "Replacement text. Use an empty string to delete.")]
    new_text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditArgs {
    #[schemars(
        length(min = 1),
        description = "File to edit. Rooted at the workspace."
    )]
    path: String,
    #[schemars(
        length(min = 1),
        description = "Exact text to replace, including whitespace. Must occur exactly once unless replaceAll is true. Use edits instead to make several replacements at once."
    )]
    old_text: Option<String>,
    #[schemars(description = "Replacement text. Use an empty string to delete.")]
    new_text: Option<String>,
    /// A real boolean only. The one coercion available reads the string
    /// `"false"` as `true`, and on a flag that decides between one replacement
    /// and every replacement, inverting the model's stated intent is far worse
    /// than rejecting a string it should not have sent.
    #[serde(default)]
    #[schemars(description = "Replace every occurrence instead of requiring exactly one.")]
    replace_all: bool,
    #[schemars(
        length(min = 1),
        description = "Several replacements applied together in one write. Each oldText is matched against the file as it was before any of them, must occur exactly once, and must not overlap another. All of them apply or none does."
    )]
    edits: Option<Vec<EditBlock>>,
}

/// One resolved replacement: where it lands and what it puts there.
struct Span {
    start: usize,
    end: usize,
    /// Which block asked for it, for the refusal that names an index.
    block: usize,
    new_text: String,
}

fn utf16_len(text: &str) -> i64 {
    i64::try_from(text.encode_utf16().count()).unwrap_or(i64::MAX)
}

/// `text` shortened for a refusal, so the model can see which block it was.
fn quote(text: &str) -> String {
    let flat = text.replace('\n', "\\n");
    if flat.chars().count() <= QUOTE_CHARS {
        return flat;
    }
    let kept: String = flat.chars().take(QUOTE_CHARS).collect();
    format!("{kept}…")
}

/// Every block's one match in the original, or the refusal saying why not.
///
/// Matching happens against the original for all of them at once, which is
/// what makes the call atomic and what makes a model's second block safe to
/// write without predicting the first block's effect.
fn resolve(original: &str, blocks: &[EditBlock], where_: &str) -> Result<Vec<Span>> {
    let mut spans: Vec<Span> = Vec::with_capacity(blocks.len());
    for (index, block) in blocks.iter().enumerate() {
        let number = index + 1;
        if block.old_text == block.new_text {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                format!("edit {number} has identical oldText and newText; nothing to do."),
            )
            .with_detail("path", where_)
            .with_detail("edit", number));
        }
        let found: Vec<usize> = original
            .match_indices(&block.old_text)
            .map(|(at, _)| at)
            .collect();
        match found.len() {
            0 => {
                return Err(WireError::new(
                    ErrorKind::NotFound,
                    format!(
                        "edit {number}: oldText \"{}\" was not found in {where_}. Read the file and copy the text exactly, including indentation.",
                        quote(&block.old_text)
                    ),
                )
                .with_detail("path", where_)
                .with_detail("edit", number));
            }
            1 => spans.push(Span {
                start: found[0],
                end: found[0] + block.old_text.len(),
                block: number,
                new_text: block.new_text.clone(),
            }),
            count => {
                return Err(WireError::new(
                    ErrorKind::Conflict,
                    format!(
                        "edit {number}: oldText \"{}\" occurs {count} times in {where_}. Include more surrounding context to make it unique.",
                        quote(&block.old_text)
                    ),
                )
                .with_detail("path", where_)
                .with_detail("edit", number)
                .with_detail("occurrences", count));
            }
        }
    }

    spans.sort_by_key(|span| span.start);
    for pair in spans.windows(2) {
        let (left, right) = (&pair[0], &pair[1]);
        if right.start < left.end {
            return Err(WireError::new(
                ErrorKind::Conflict,
                format!(
                    "edits {} and {} overlap in {where_}. Split them so each one names a separate region.",
                    left.block, right.block
                ),
            )
            .with_detail("path", where_));
        }
    }
    Ok(spans)
}

/// The original with every span replaced, in one pass.
fn apply(original: &str, spans: &[Span]) -> String {
    let mut updated = String::with_capacity(original.len());
    let mut cursor = 0usize;
    for span in spans {
        updated.push_str(&original[cursor..span.start]);
        updated.push_str(&span.new_text);
        cursor = span.end;
    }
    updated.push_str(&original[cursor..]);
    updated
}

/// A diff of what changed, built from the spans rather than by comparing files.
///
/// The spans already say exactly where the edit landed, so there is no
/// alignment to guess at. Each one becomes a hunk with a couple of lines either
/// side, which is what a reader needs to confirm the edit hit the right place.
fn diff(original: &str, spans: &[Span]) -> String {
    let lines: Vec<&str> = original.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut truncated = 0usize;

    for span in spans {
        let first = line_of(original, span.start);
        let last = line_of(original, span.end);
        let from = first.saturating_sub(DIFF_CONTEXT);
        let to = (last + DIFF_CONTEXT).min(lines.len().saturating_sub(1));
        let new_lines: Vec<&str> = span.new_text.split('\n').collect();

        let mut hunk: Vec<String> = Vec::new();
        hunk.push(format!(
            "@@ -{},{} +{},{} @@",
            first + 1,
            last - first + 1,
            first + 1,
            new_lines.len()
        ));
        for line in lines.iter().take(first).skip(from) {
            hunk.push(format!(" {line}"));
        }
        for line in lines.iter().take(last + 1).skip(first) {
            hunk.push(format!("-{line}"));
        }
        for line in &new_lines {
            hunk.push(format!("+{line}"));
        }
        for line in lines.iter().take(to + 1).skip(last + 1) {
            hunk.push(format!(" {line}"));
        }

        if out.len() + hunk.len() > MAX_DIFF_LINES {
            truncated = truncated.saturating_add(1);
            continue;
        }
        out.extend(hunk);
    }

    if truncated > 0 {
        out.push(format!("({truncated} more changed regions not shown)"));
    }
    out.join("\n")
}

/// The 0-based line number a byte offset falls on.
fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count()
}

struct Edit;

impl ToolHandler for Edit {
    type Args = EditArgs;

    fn execute<'a>(
        &'a self,
        args: EditArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "edit")?;
            let blocks = blocks_of(&args)?;
            let accepted = ctx.jail.accept(&args.path)?;
            let where_ = accepted.relative.as_str();
            let note = clamp_note(&args.path, &accepted);

            let raw = tokio::fs::read_to_string(&accepted.path)
                .await
                .map_err(|error| fs_failure(&error, where_, &note))?;
            // A byte-order mark is invisible, so the model never includes one in
            // `oldText`. Matching without it and writing it back is the only
            // reading that both finds the text and leaves the file as it was.
            let (mark, original) = match raw.strip_prefix('\u{feff}') {
                Some(rest) => ("\u{feff}", rest.to_owned()),
                None => ("", raw),
            };

            let (updated, occurrences) = if args.replace_all {
                // `replaceAll` is one block by definition, checked in `blocks_of`.
                let block = &blocks[0];
                let count = original.matches(block.old_text.as_str()).count();
                if count == 0 {
                    return Err(WireError::new(
                        ErrorKind::NotFound,
                        format!(
                            "oldText was not found in {where_}. Read the file and copy the text exactly, including indentation."
                        ),
                    )
                    .with_detail("path", where_));
                }
                (
                    original.replace(block.old_text.as_str(), &block.new_text),
                    count,
                )
            } else {
                let spans = resolve(&original, &blocks, where_)?;
                let text = apply(&original, &spans);
                let shown = diff(&original, &spans);
                assert_not_aborted(&ctx.token, "edit")?;
                return write_back(ctx, &accepted.path, mark, &text, where_, &note)
                    .await
                    .map(|()| {
                        report(
                            &original,
                            &text,
                            blocks.len(),
                            blocks.len(),
                            where_,
                            &note,
                            &shown,
                        )
                    });
            };

            assert_not_aborted(&ctx.token, "edit")?;
            write_back(ctx, &accepted.path, mark, &updated, where_, &note).await?;
            Ok(report(
                &original,
                &updated,
                1,
                occurrences,
                where_,
                &note,
                "",
            ))
        })
    }
}

/// The blocks one call asks for, whichever form it used.
fn blocks_of(args: &EditArgs) -> Result<Vec<EditBlock>> {
    match (&args.old_text, &args.new_text, &args.edits) {
        (Some(_), _, Some(_)) | (_, Some(_), Some(_)) => Err(WireError::new(
            ErrorKind::InvalidInput,
            "Use either oldText and newText or edits, not both.",
        )),
        (_, _, Some(edits)) if args.replace_all => {
            let _ = edits;
            Err(WireError::new(
                ErrorKind::InvalidInput,
                "replaceAll applies to a single oldText; it cannot be combined with edits.",
            ))
        }
        (_, _, Some(edits)) => Ok(edits
            .iter()
            .map(|block| EditBlock {
                old_text: block.old_text.clone(),
                new_text: block.new_text.clone(),
            })
            .collect()),
        (Some(old_text), Some(new_text), None) => Ok(vec![EditBlock {
            old_text: old_text.clone(),
            new_text: new_text.clone(),
        }]),
        (None, None, None) => Err(WireError::new(
            ErrorKind::InvalidInput,
            "Give oldText and newText, or give edits.",
        )),
        (Some(_), None, None) => Err(WireError::new(
            ErrorKind::InvalidInput,
            "oldText was given without newText.",
        )),
        (None, Some(_), None) => Err(WireError::new(
            ErrorKind::InvalidInput,
            "newText was given without oldText.",
        )),
    }
}

/// Writes the file back, byte-order mark restored.
async fn write_back(
    _ctx: &ToolContext,
    path: &std::path::Path,
    mark: &str,
    text: &str,
    where_: &str,
    note: &str,
) -> Result<()> {
    let mut bytes = Vec::with_capacity(mark.len() + text.len());
    bytes.extend_from_slice(mark.as_bytes());
    bytes.extend_from_slice(text.as_bytes());
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| fs_failure(error_ref(&error), where_, note))
}

/// Borrow helper: `fs_failure` takes a reference and the error is owned here.
fn error_ref(error: &std::io::Error) -> &std::io::Error {
    error
}

/// What the model reads after a successful edit.
fn report(
    original: &str,
    updated: &str,
    blocks: usize,
    occurrences: usize,
    where_: &str,
    note: &str,
    shown: &str,
) -> ToolOutput {
    let delta = utf16_len(updated) - utf16_len(original);
    let sign = if delta >= 0 { "+" } else { "" };
    let plural = if occurrences == 1 { "" } else { "s" };
    let head = if blocks > 1 {
        format!("Replaced {blocks} blocks in {where_} ({sign}{delta} characters).{note}")
    } else {
        format!(
            "Replaced {occurrences} occurrence{plural} in {where_} ({sign}{delta} characters).{note}"
        )
    };
    let body = if shown.is_empty() {
        head
    } else {
        format!("{head}\n\n{shown}")
    };
    ToolOutput::text(body)
        .with_detail("path", where_)
        .with_detail("blocks", blocks)
        .with_detail("occurrences", occurrences)
        .with_detail("delta", delta)
}

/// The `edit` tool.
pub fn edit_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "edit",
            "Replace exact strings in an existing workspace file. The workspace is the root: \"/x\" and \"../x\" both resolve inside it, never outside. oldText must appear exactly once unless replaceAll is set, so read the file first and include enough surrounding context to be unambiguous. Pass edits to make several replacements in one atomic write.",
        )
        .risk(ToolRisk::Write)
        .annotations(ToolAnnotations {
            title: Some("Edit file".to_owned()),
            read_only_hint: Some(false),
            idempotent_hint: Some(false),
            ..ToolAnnotations::default()
        }),
        Edit,
    ))
}
