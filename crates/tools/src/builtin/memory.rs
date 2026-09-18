//! `memory` — record one durable fact about the workspace.
//!
//! One operation, four fields, and **no `path` argument**. That last part is
//! the whole reason this is not a worse `write_file`: the folder is derived
//! from the jail root, so there is no path for a model to get wrong and
//! nothing for the jail to adjudicate.
//!
//! A *name* is not a path, but it does put a model-chosen string into a
//! filename, so the guarantee the jail gives a path has to be re-established
//! here. It is, in `memory_slug` — the result is `[a-z0-9-]` and cannot contain a
//! separator or a `..`, so it cannot leave `memory/` by construction. This tool
//! calls it to report a clear failure; `save_memory` calls it again because it
//! is the function that turns a string into a path.
//!
//! ## Why a tool at all
//!
//! "A tool whose entire job is to return the bytes of a workspace file is a
//! worse `read_file`" is correct about *reading*. This is the case it does not
//! cover: a tool carries a permission — `allow`, `ask`, `deny` — and that
//! permission is already per-agent, already in the config and already in the
//! settings UI. It is therefore the switch for the whole feature, and inventing
//! a `memoryEnabled` boolean beside it would be a second way to say the same
//! thing.
//!
//! ## Why writing a name twice replaces
//!
//! Because it is the only way a model can correct itself. A wrong memory is
//! the worst failure the feature has — it is in every future turn on that
//! folder — and without replacement a fact that has changed can only be
//! recorded a second time, leaving the index carrying two contradictory lines
//! with nothing to say which is current. The blast radius of a rewrite is one
//! fact under a name the model chose deliberately; `Replaced` in the result
//! and a diff in git are what make it visible.
//!
//! ## What it does not do
//!
//! There is no read operation: the index is already in the prompt and
//! `read_file` opens what it names. There is no delete either — a superseded
//! memory is corrected by writing the same name, and one that should not exist
//! at all is a file that `exec` can remove. Because the prompt index is scanned
//! from the folder rather than read from `MEMORY.md`, removing a file by hand
//! takes effect on the very next turn.

use darkwire_core::Result;
use darkwire_core::memory::{
    MAX_MEMORY_DESCRIPTION_CHARS, MAX_MEMORY_NAME_CHARS, MemoryInput, MemoryType, memory_slug,
    save_memory,
};
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::builtin::built;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

/// Long enough for a paragraph, short enough that one call cannot become the
/// whole store. The format says one fact per file, and a two-thousand-character
/// note is already several.
const MAX_BODY_CHARS: u32 = 2000;

/// The four kinds, as the schema spells them.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum MemoryKind {
    User,
    Feedback,
    Project,
    Reference,
}

impl From<MemoryKind> for MemoryType {
    fn from(kind: MemoryKind) -> MemoryType {
        match kind {
            MemoryKind::User => MemoryType::User,
            MemoryKind::Feedback => MemoryType::Feedback,
            MemoryKind::Project => MemoryType::Project,
            MemoryKind::Reference => MemoryType::Reference,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MemoryArgs {
    #[schemars(
        length(min = 1, max = MAX_MEMORY_NAME_CHARS),
        description = "A short kebab-case name for this one fact, unique in this workspace, such as `ui-stack-preferences`. It becomes the filename and is what a [[link]] in another memory refers to. Calling this again with the same name replaces that memory, which is how you correct one."
    )]
    name: String,
    #[schemars(
        length(min = 1, max = MAX_MEMORY_DESCRIPTION_CHARS),
        description = "One line saying what this is about. It is the only part that reaches your prompt, and the whole basis on which you later decide the memory is worth opening."
    )]
    description: String,
    #[serde(rename = "type")]
    #[schemars(
        description = "What kind of fact this is. `user`: who the person is and what they prefer. `feedback`: how they have asked you to work, and why. `project`: an ongoing goal or constraint the code does not state. `reference`: a pointer to something outside this workspace."
    )]
    kind: MemoryKind,
    #[schemars(
        length(min = 1, max = MAX_BODY_CHARS),
        description = "The fact itself, in the form you want to read it back in. One fact per call. Refer to a related memory as [[its-name]]."
    )]
    body: String,
}

struct Memory;

impl ToolHandler for Memory {
    type Args = MemoryArgs;

    fn execute<'a>(
        &'a self,
        args: MemoryArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "memory")?;

            // Checked here as well so the model gets a sentence it can act on
            // rather than an error from two layers down.
            let Some(name) = memory_slug(&args.name) else {
                return Ok(ToolOutput::error(format!(
                    "Nothing usable as a filename in \"{}\". Names are letters, digits and hyphens. Try something like `build-conventions`.",
                    args.name
                )));
            };

            let saved = save_memory(
                ctx.jail.root(),
                &MemoryInput {
                    name,
                    description: args.description,
                    memory_type: args.kind.into(),
                    body: args.body,
                },
            )?;

            let verb = if saved.replaced {
                "Replaced"
            } else {
                "Recorded"
            };
            let renamed = if saved.name == args.name {
                String::new()
            } else {
                format!(" (named `{}`)", saved.name)
            };

            Ok(ToolOutput::text(format!(
                "{verb} {}{renamed}. Its line is in your prompt from the next turn; open the file when it looks relevant.",
                saved.path
            ))
            .with_detail("name", saved.name.as_str())
            .with_detail("path", saved.path.as_str())
            .with_detail("replaced", saved.replaced)
            .with_detail("total", saved.total))
        })
    }
}

/// The `memory` tool.
pub fn memory_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "memory",
            "Record one durable fact about this workspace for future sessions. Each call writes memory/<name>.md and puts a one-line summary of it in your prompt from now on; the fact itself stays on disk until you open it with read_file. Use it for what stays true (preferences, conventions, where things live), not for what only matters in this conversation. Calling it again with the same name replaces that memory.",
        )
        .risk(ToolRisk::Write)
        .annotations(ToolAnnotations {
            title: Some("Remember".to_owned()),
            read_only_hint: Some(false),
            // Two identical calls leave one file with those contents. A name
            // is an address, and writing the same thing to the same address
            // twice is the same state.
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        Memory,
    ))
}
