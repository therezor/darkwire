//! `edit_file` — replace an exact string in a workspace file.
//!
//! String replacement rather than a diff or a line range, for one reason: a
//! unique literal is the only edit address a model can produce that stays
//! correct between the read and the write. Line numbers go stale the moment
//! anything else touches the file, and a unified diff asks the model to get
//! hunk arithmetic right — which it does not, reliably, and a wrong hunk
//! applies cleanly to the wrong place.
//!
//! The uniqueness rule is what makes that safe. `oldText` occurring twice is
//! ambiguous, and picking the first is a coin flip that silently edits the
//! wrong call site; the tool refuses and tells the model to include more
//! surrounding context. `replaceAll` is available when the model means every
//! occurrence, and saying so is a different claim from not having noticed.

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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditFileArgs {
    #[schemars(
        length(min = 1),
        description = "File to edit. Rooted at the workspace."
    )]
    path: String,
    #[schemars(
        length(min = 1),
        description = "Exact text to replace, including whitespace. Must occur exactly once unless replaceAll is true."
    )]
    old_text: String,
    #[schemars(description = "Replacement text. Use an empty string to delete.")]
    new_text: String,
    /// A real boolean only. The one coercion available reads the string
    /// `"false"` as `true`, and on a flag that decides between one replacement
    /// and every replacement, inverting the model's stated intent is far worse
    /// than rejecting a string it should not have sent.
    #[serde(default)]
    #[schemars(description = "Replace every occurrence instead of requiring exactly one.")]
    replace_all: bool,
}

struct EditFile;

fn utf16_len(text: &str) -> i64 {
    i64::try_from(text.encode_utf16().count()).unwrap_or(i64::MAX)
}

impl ToolHandler for EditFile {
    type Args = EditFileArgs;

    fn execute<'a>(
        &'a self,
        args: EditFileArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "edit_file")?;
            if args.old_text == args.new_text {
                return Err(WireError::new(
                    ErrorKind::InvalidInput,
                    "oldText and newText are identical; nothing to do.",
                )
                .with_detail("path", args.path.as_str()));
            }
            let accepted = ctx.jail.accept(&args.path)?;
            let where_ = accepted.relative.as_str();
            let note = clamp_note(&args.path, &accepted);

            let original = tokio::fs::read_to_string(&accepted.path)
                .await
                .map_err(|error| fs_failure(&error, where_, &note))?;

            let occurrences = original.matches(args.old_text.as_str()).count();
            if occurrences == 0 {
                return Err(WireError::new(
                    ErrorKind::NotFound,
                    format!(
                        "oldText was not found in {where_}. Read the file and copy the text exactly, including indentation."
                    ),
                )
                .with_detail("path", where_));
            }
            if occurrences > 1 && !args.replace_all {
                return Err(WireError::new(
                    ErrorKind::Conflict,
                    format!(
                        "oldText occurs {occurrences} times in {where_}. Include more surrounding context to make it unique, or set replaceAll."
                    ),
                )
                .with_detail("path", where_)
                .with_detail("occurrences", occurrences));
            }

            // Literal replacement: nothing in `newText` is a pattern, so a shell
            // variable, a regex or a price in dollars lands exactly as written.
            let updated = if args.replace_all {
                original.replace(args.old_text.as_str(), &args.new_text)
            } else {
                original.replacen(args.old_text.as_str(), &args.new_text, 1)
            };

            assert_not_aborted(&ctx.token, "edit_file")?;
            tokio::fs::write(&accepted.path, updated.as_bytes())
                .await
                .map_err(|error| fs_failure(&error, where_, &note))?;

            let delta = utf16_len(&updated) - utf16_len(&original);
            let plural = if occurrences == 1 { "" } else { "s" };
            let sign = if delta >= 0 { "+" } else { "" };
            Ok(ToolOutput::text(format!(
                "Replaced {occurrences} occurrence{plural} in {where_} ({sign}{delta} characters).{note}"
            ))
            .with_detail("path", where_)
            .with_detail("occurrences", occurrences)
            .with_detail("delta", delta))
        })
    }
}

/// The `edit_file` tool.
pub fn edit_file_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "edit_file",
            "Replace an exact string in an existing workspace file. The workspace is the root: \"/x\" and \"../x\" both resolve inside it, never outside. oldText must appear exactly once unless replaceAll is set, so read the file first and include enough surrounding context to be unambiguous.",
        )
        .risk(ToolRisk::Write)
        .annotations(ToolAnnotations {
            title: Some("Edit file".to_owned()),
            read_only_hint: Some(false),
            idempotent_hint: Some(false),
            ..ToolAnnotations::default()
        }),
        EditFile,
    ))
}
