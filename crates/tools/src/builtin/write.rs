//! `write` — create or replace a workspace file.
//!
//! Whole-file replacement, not append and not patch. `edit` is the tool
//! for a change to an existing file, and keeping the two separate is what
//! makes the approval prompt meaningful: "replace 4 kB of `src/index.ts`" and
//! "swap one string in `src/index.ts`" are different decisions, and a single
//! tool with a `mode` argument would present them as the same one.
//!
//! Parent directories are created, through the workspace root, so every
//! directory created is inside the workspace. Refusing to create them would
//! leave the model to call a directory tool it does not have.

use std::io::Write as _;

use darkwire_core::Result;
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::builtin::built;
use crate::builtin::shared::{
    clamp_note, format_bytes, in_root, is_irregular, not_regular, open_options, root_failure,
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

            let failed = |error: std::io::Error| root_failure(&error, &args.path, where_, &note);
            let irregular = in_root(&ctx.jail, &accepted, "write", |root, inside| {
                if let Some(parent) = inside
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                {
                    root.create_dir_all(parent)?;
                }
                Ok(is_irregular(root, inside))
            })
            .await?
            .map_err(failed)?;
            assert_not_aborted(&ctx.token, "write")?;
            // Checked before the open for a clear refusal, and again on the
            // open file, because a FIFO with a reader opens without error.
            if irregular {
                return Err(not_regular(where_, &note));
            }
            let content = args.content;
            let written = in_root(&ctx.jail, &accepted, "write", move |root, inside| {
                let mut options = open_options();
                options.write(true).create(true).truncate(false);
                let mut file = root.open_with(inside, &options)?.into_std();
                let stats = file.metadata()?;
                if !stats.is_dir() && !stats.is_file() {
                    return Ok(false);
                }
                file.set_len(0)?;
                file.write_all(content.as_bytes())?;
                file.flush()?;
                Ok(true)
            })
            .await?
            .map_err(failed)?;
            if !written {
                return Err(not_regular(where_, &note));
            }

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
