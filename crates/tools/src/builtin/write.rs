//! `write` — create or replace a workspace file.
//!
//! Whole-file replacement, not append and not patch. `edit` is the tool
//! for a change to an existing file, and keeping the two separate is what
//! makes the approval prompt meaningful: "replace 4 kB of `src/index.ts`" and
//! "swap one string in `src/index.ts`" are different decisions, and a single
//! tool with a `mode` argument would present them as the same one.
//!
//! Parent directories are created. The path has already been through the jail,
//! so every directory created is inside the workspace by construction — and
//! refusing to create them would leave the model to call a directory tool it
//! does not have.

use darkwire_core::Result;
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;

use crate::builtin::built;
use crate::builtin::shared::{
    assert_regular, assert_regular_or_missing, clamp_note, format_bytes, fs_failure, open_flags,
};
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteFileArgs {
    #[schemars(
        length(min = 1),
        description = "File to write. Rooted at the workspace. Parent directories are created."
    )]
    path: String,
    #[schemars(description = "Full new contents of the file. Existing contents are replaced.")]
    content: String,
}

struct WriteFile;

impl ToolHandler for WriteFile {
    type Args = WriteFileArgs;

    fn execute<'a>(
        &'a self,
        args: WriteFileArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "write")?;
            let accepted = ctx.jail.accept(&args.path)?;
            let where_ = accepted.relative.as_str();
            let note = clamp_note(&args.path, &accepted);
            let bytes = u64::try_from(args.content.len()).unwrap_or(u64::MAX);

            if let Some(parent) = accepted.path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| fs_failure(&error, where_, &note))?;
            }
            assert_not_aborted(&ctx.token, "write")?;
            // Checked before the open for a clear refusal, and again on the
            // open file, because a FIFO with a reader opens without error.
            assert_regular_or_missing(&accepted.path, where_, &note)?;
            let failed = |error: std::io::Error| fs_failure(&error, where_, &note);
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .custom_flags(open_flags())
                .open(&accepted.path)
                .await
                .map_err(failed)?;
            assert_regular(&file.metadata().await.map_err(failed)?, where_, &note)?;
            file.set_len(0).await.map_err(failed)?;
            file.write_all(args.content.as_bytes())
                .await
                .map_err(failed)?;
            file.flush().await.map_err(failed)?;

            Ok(
                ToolOutput::text(format!("Wrote {} to {where_}.{note}", format_bytes(bytes)))
                    .with_detail("path", where_)
                    .with_detail("bytes", bytes),
            )
        })
    }
}

/// The `write` tool.
pub fn write_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "write",
            "Write a UTF-8 text file in the workspace, replacing it if it exists. The workspace is the root: \"/x\" and \"../x\" both resolve inside it, never outside. Use edit to change part of an existing file.",
        )
        .risk(ToolRisk::Write)
        .annotations(ToolAnnotations {
            title: Some("Write file".to_owned()),
            read_only_hint: Some(false),
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        WriteFile,
    ))
}
