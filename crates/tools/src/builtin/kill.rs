//! `kill`: stop a background job this session started.

use darkwire_core::Result;
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::builtin::built;
use crate::builtin::exec::background_refusal;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KillArgs {
    #[schemars(length(min = 1), description = "The job id exec returned.")]
    id: String,
}

struct Kill;

impl ToolHandler for Kill {
    type Args = KillArgs;

    fn execute<'a>(
        &'a self,
        args: KillArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "kill")?;
            let Some(port) = &ctx.background else {
                return Ok(ToolOutput::error(
                    "Background jobs are not available here, so there is nothing to kill.",
                ));
            };
            let id = args.id.trim();
            Ok(match port.kill(id) {
                Ok(()) => ToolOutput::text(format!(
                    "Stopped job {id}. Its result arrives as a message."
                )),
                Err(refusal) => ToolOutput::error(background_refusal(refusal)),
            })
        })
    }
}

/// The `kill` tool.
///
/// `Safe`: it reaches only jobs this session started, and stopping one is what
/// the person would do from the strip above the composer.
pub fn kill_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new("kill", "Stop a background job started with exec.")
            .risk(ToolRisk::Safe)
            .annotations(ToolAnnotations {
                title: Some("Stop job".to_owned()),
                ..ToolAnnotations::default()
            }),
        Kill,
    ))
}
