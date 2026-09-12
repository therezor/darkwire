//! `skill` — open one of the workspace's instruction sheets.
//!
//! "A tool whose entire job is to return the bytes of a workspace file is a
//! worse `read_file` — one more name in every agent's permission map, one more
//! schema in every request, to reach a file the agent could already open."
//! That argument is about *reading*, and about reading it is right.
//!
//! The reason it does not address is the permission. A tool carries `allow`,
//! `ask` or `deny` per agent, and that is exactly "this capability is on, off,
//! or gated" — already in the config, already in the settings UI, already
//! per-agent. Without a `skill` tool there is no way to turn skills off for
//! one agent short of adding a `skillsEnabled` boolean beside the permission
//! map, which is a second switch for one thing and the way the two end up
//! disagreeing.
//!
//! So the cost is real and is paid on purpose. What it buys is that denying
//! `skill` removes the catalogue from the prompt too.
//!
//! The name is the directory name, and the path is derived rather than taken:
//! a `name` that tries to climb out of `skills/` is refused by the jail, not by
//! a check here that could be forgotten.
//!
//! ## A sheet's `agents:` scope is not enforced here, on purpose
//!
//! A `SKILL.md` may carry an `agents:` line naming who it is for, and this tool
//! ignores it: it will open a sheet scoped to somebody else. Scope decides what
//! an agent's *catalogue* advertises — what costs tokens in a prompt — and not
//! what may be read. Checking it here would gate one door while `read_file`
//! walks past the other.

use ghostai_core::Result;
use ghostai_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::builtin::built;
use crate::builtin::shared::fs_failure;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

/// Restated rather than imported: they belong to the agent's skill loader,
/// which depends on this crate and so cannot be depended on from here. Two
/// short strings is the cheaper of the two costs.
const SKILLS_DIRNAME: &str = "skills";
const SKILL_FILENAME: &str = "SKILL.md";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SkillArgs {
    #[schemars(
        length(min = 1),
        description = "The skill’s directory name, exactly as the catalogue in your prompt lists it."
    )]
    name: String,
}

struct Skill;

impl ToolHandler for Skill {
    type Args = SkillArgs;

    fn execute<'a>(
        &'a self,
        args: SkillArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "skill")?;

            // Through the jail rather than joined here: `name` came from the
            // model, and the jail is the only thing permitted to judge an
            // agent-supplied path.
            let where_ = format!("{SKILLS_DIRNAME}/{}/{SKILL_FILENAME}", args.name);
            let accepted = ctx.jail.accept(&where_)?;

            let text = tokio::fs::read_to_string(&accepted.path)
                .await
                .map_err(|error| fs_failure(&error, &accepted.relative, ""))?;

            Ok(ToolOutput::text(text)
                .with_detail("path", accepted.relative.as_str())
                .with_detail("skill", args.name.as_str()))
        })
    }
}

/// The `skill` tool.
pub fn skill_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "skill",
            "Read one of this workspace’s skills — the full instruction sheet behind a line in the skills catalogue. Open it before acting on what it names.",
        )
        .risk(ToolRisk::Safe)
        .annotations(ToolAnnotations {
            title: Some("Open skill".to_owned()),
            read_only_hint: Some(true),
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        Skill,
    ))
}
