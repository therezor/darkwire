//! `memory` — read, save and delete what the agent has learned here.
//!
//! Three actions, two data fields, and **no `path` argument**. That last part
//! is the whole reason this is not a worse `write`: the folder is derived from
//! the jail root, so there is no path for a model to get wrong and nothing for
//! the jail to adjudicate.
//!
//! A *key* is not a path, but it does put a model-chosen string into a
//! filename, so the guarantee the jail gives a path has to be re-established
//! here. It is, in `memory_slug` — the result is `[a-z0-9-]` and cannot contain
//! a separator or a `..`, so it cannot leave `memory/` by construction.
//!
//! ## Why a tool rather than `read` and `exec`
//!
//! Because a permission is per agent. `memory` carries one — `allow`, `ask`,
//! `deny` — and an agent that holds it may well not hold the filesystem tools:
//! a chat-only agent, or one behind lazy tool discovery. Leaving the reading to
//! `read` and the removing to `exec` gives that agent an index it cannot open
//! and a wrong memory it cannot remove. One permission covers the whole
//! feature, which is also why no `memoryEnabled` boolean exists beside it.
//!
//! ## Why saving a key twice replaces
//!
//! Because it is the only way a model can correct itself. A wrong memory is the
//! worst failure the feature has — it is in every future turn on that folder —
//! and without replacement a fact that has changed can only be recorded a
//! second time, leaving the index carrying two contradictory lines with nothing
//! to say which is current. That is also why both the tool description and the
//! prompt section say to read a key before replacing it: `save` takes the whole
//! content, so writing one without reading it first drops whatever else it
//! held.

use darkwire_core::Result;
use darkwire_core::memory::{
    MAX_MEMORY_NAME_CHARS, delete_memory, memory_slug, read_memory, save_memory,
};
use darkwire_protocol::json::js_trim;
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::builtin::built;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

/// Long enough for a page, short enough that one call cannot become the whole
/// store. The format says one topic per memory, and a two-thousand-character
/// note is already several.
const MAX_CONTENT_CHARS: usize = 2000;

/// The three actions, as the schema spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Action {
    Read,
    Save,
    Delete,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MemoryArgs {
    #[schemars(description = "read returns a memory, save writes one, delete removes one.")]
    action: Action,
    #[schemars(
        length(min = 1, max = MAX_MEMORY_NAME_CHARS),
        description = "Memory key, such as auth-sessions."
    )]
    key: String,
    #[schemars(description = "Memory content. Required only when saving.")]
    content: Option<String>,
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

            // Checked here as well as in the store so the model gets a sentence
            // it can act on rather than an error from two layers down.
            let Some(key) = memory_slug(&args.key) else {
                return Ok(ToolOutput::error(format!(
                    "No usable key in \"{}\". Keys are letters, digits and hyphens, such as `auth-sessions`.",
                    args.key
                )));
            };
            let root = ctx.jail.root();

            Ok(match args.action {
                Action::Read => match read_memory(root, &key) {
                    Some(memory) => ToolOutput::text(memory.content).with_detail("key", key),
                    None => ToolOutput::error(format!(
                        "No memory `{key}`. The keys are listed under Memory in your prompt."
                    )),
                },
                Action::Save => {
                    let content = args.content.as_deref().map_or("", js_trim);
                    if content.is_empty() {
                        return Ok(ToolOutput::error("Give content to save."));
                    }
                    if content.chars().count() > MAX_CONTENT_CHARS {
                        return Ok(ToolOutput::error(format!(
                            "Too long: {MAX_CONTENT_CHARS} characters at most. Split it across keys, one topic each."
                        )));
                    }

                    let saved = save_memory(root, &key, content)?;
                    let verb = if saved.replaced { "Replaced" } else { "Saved" };
                    ToolOutput::text(format!("{verb} {}", saved.key))
                        .with_detail("key", saved.key.as_str())
                        .with_detail("replaced", saved.replaced)
                        .with_detail("total", saved.total)
                }
                Action::Delete => {
                    let removed = delete_memory(root, &key)?;
                    let text = if removed.existed {
                        format!("Deleted {}", removed.key)
                    } else {
                        format!("No memory `{}`", removed.key)
                    };
                    ToolOutput::text(text)
                        .with_detail("key", removed.key.as_str())
                        .with_detail("existed", removed.existed)
                        .with_detail("total", removed.total)
                }
            })
        })
    }
}

/// The `memory` tool.
pub fn memory_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "memory",
            "Persistent memory. Every key is listed in the Memory section of your prompt.
read and delete take an exact listed key. save creates a key or replaces one.
Saving overwrites the whole memory, so read a key before you change it.
One topic per memory, a short kebab-case key, content starting with a # heading.
Do not use file tools to reach memory.",
        )
        .risk(ToolRisk::Write)
        .annotations(ToolAnnotations {
            title: Some("Memory".to_owned()),
            read_only_hint: Some(false),
            // A key is an address. Writing the same content to it twice is the
            // same state, and deleting it twice is the same state as well.
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        Memory,
    ))
}
