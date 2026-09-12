//! Memory, as a section of the prompt.
//!
//! **An index, not the contents.** One line per memory — the file to open, its
//! name, and what it is about — and the model opens the one it wants with
//! `read_file`. That is the same shape `skills_contributor` uses, and it is
//! what a *store* wants: one file per fact, growing for as long as the
//! workspace does. Inlining such a thing would re-send everything ever learned
//! on every request of every turn, and the only lever left would be a token cap
//! deciding what to forget by age rather than by relevance. An index costs a
//! line each and puts the choice where the question is.
//!
//! ## Which half it lands in
//!
//! The static section only. Memory is a property of the *workspace*, so it
//! belongs in the provider's cached prefix, read once per turn. Nothing about
//! it varies per message, so there is no runtime half.
//!
//! It is placed *after* skills in the contributor list, and that ordering is a
//! decision rather than an accident. Sections are appended in order so the
//! cached prefix grows at the end; memory is the section most likely to change
//! between turns, so it sits where a change invalidates the least.
//!
//! ## Nothing is cached on the instance
//!
//! One `AgentLoop` serves every session on an agent, and those sessions can be
//! bound to different workspaces. A contributor that remembered what it read
//! last turn would hand one workspace's memory to a concurrent turn in another.
//! So the static section re-reads — a directory of small files, once per turn.

use std::path::Path;

use ghostai_core::memory::{MEMORY_DIRNAME, Memory, read_memories};
use ghostai_protocol::json::js_trim;
use ghostai_protocol::{DEFAULT_MEMORY_TEMPLATE, render_prompt_template};
use ghostai_providers::BoxFuture;
use indexmap::IndexMap;

use crate::prompt::{ContextContributor, StaticPromptContext, template_or};

/// The section text for a set of memories.
///
/// Pure, and separate from the contributor for the reason `render_skills` is:
/// the ordering and what an empty folder renders as are the parts worth
/// testing, and neither needs a filesystem.
///
/// An empty folder renders as the empty string, never as a bare heading —
/// `contributor_sections` drops a section that trims to nothing, so this is how
/// "no memory" becomes "no section".
///
/// **There is no token budget, and the file-count cap is the only bound.** A
/// budget in tokens would never bind: an index line is roughly fifteen tokens,
/// so any plausible one affords more lines than the cap admits, and a number
/// that never decides anything reads as a lever and is not one.
///
/// Keeping memory on disk and out of the prompt is the other thing such a
/// budget gets asked to do, and it is a capability question rather than a size
/// one. It is answered by the `memory` tool's permission, which the composition
/// root gates this whole contributor on.
pub fn render_memory_section(memories: &[Memory], template: Option<&str>) -> String {
    // Before the template is resolved, not after: an empty folder rendered
    // through the built-in would place a paragraph explaining an index that is
    // not there.
    if memories.is_empty() {
        return String::new();
    }

    // The one statement of "empty inherits, whitespace deletes", shared with
    // the section templates rather than spelled again.
    let resolved = template_or(template, DEFAULT_MEMORY_TEMPLATE);
    if js_trim(resolved).is_empty() {
        return String::new();
    }

    let mut values = IndexMap::new();
    values.insert("path".to_owned(), MEMORY_DIRNAME.to_owned());
    values.insert(
        "index".to_owned(),
        memories
            .iter()
            .map(index_line)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    values.insert("count".to_owned(), memories.len().to_string());

    js_trim(&render_prompt_template(resolved, &values)).to_owned()
}

/// The path first, because that is the string handed back to `read_file`.
///
/// `render_index` in `ghostai-core` writes the same memories as relative
/// markdown links, and the two are deliberately different: `MEMORY.md` sits
/// inside `memory/` and is read by a person, while this is read by a model that
/// has to pass the path to a tool. A prefix it reconstructs is one it can
/// reconstruct wrongly.
///
/// The name is not repeated beside the path, because the path already ends in
/// it. The *kind* is, at two tokens a line, because a stated preference and a
/// pointer to a document are not the same claim and the description alone does
/// not always say which one this is.
fn index_line(memory: &Memory) -> String {
    format!(
        "- `{}` ({}) — {}",
        memory.path, memory.memory_type, memory.description
    )
}

/// Reads the workspace's memories and indexes them in the static prompt.
#[derive(Debug, Clone, Default)]
pub struct MemoryContributor {
    template: String,
}

impl MemoryContributor {
    /// A contributor on the built-in wording.
    pub fn new() -> MemoryContributor {
        MemoryContributor::default()
    }

    /// The operator's wording for the section. Empty means the built-in; a
    /// single space renders nothing, which is how they delete it.
    #[must_use]
    pub fn with_template(mut self, template: impl Into<String>) -> MemoryContributor {
        self.template = template.into();
        self
    }
}

impl ContextContributor for MemoryContributor {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn static_section<'a>(
        &'a self,
        context: &'a StaticPromptContext,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let memories = read_memories(Path::new(&context.workspace_root));
            if memories.is_empty() {
                return None;
            }

            let section = render_memory_section(&memories, Some(&self.template));
            if section.is_empty() {
                None
            } else {
                Some(section)
            }
        })
    }
}
