//! Skills, as a section of the prompt.
//!
//! A skill reaches the model as one index line — name, description, path. The
//! model opens the file itself with `read` when the description tells it
//! the skill applies. This is what "the rest loaded when relevant" means, and
//! it costs about twenty tokens per skill instead of the whole sheet.
//!
//! That is why there is no skill budget: inlining skill bodies into the cached
//! half would be a decision made once, by an operator, about every turn, and
//! paid for on every turn.
//!
//! ## Which half it lands in
//!
//! The catalogue is a property of the workspace, so it goes in the static
//! section — the provider's cached prefix, read once per turn. There is no
//! runtime half: putting a per-turn value in the static one would end the
//! session's cached prefix on every turn, which is the cost the two-half split
//! exists to avoid, and nothing here varies per turn anyway.
//!
//! ## Nothing is cached on the instance
//!
//! One `AgentLoop` serves every session on an agent, and those sessions can be
//! bound to different workspaces. A contributor that remembered the catalogue
//! it read last turn would hand one workspace's skills to a concurrent turn in
//! another. So the static section re-reads — which is one directory listing and
//! a handful of small files, once per turn.

use std::path::Path;

use darkwire_protocol::json::js_trim;
use darkwire_protocol::{DEFAULT_AGENT_ID, DEFAULT_SKILLS_TEMPLATE, render_prompt_template};
use darkwire_providers::BoxFuture;
use indexmap::IndexMap;

use crate::prompt::{ContextContributor, StaticPromptContext, template_or};
use crate::skills::{SKILLS_DIRNAME, Skill, read_skills, skills_for_agent};

/// The section text for a catalogue.
///
/// Pure, and separate from the contributor for that reason: the ordering, the
/// empty cases and the template contract are the parts worth testing, and none
/// of them needs a filesystem to test.
///
/// An empty catalogue renders as the empty string, never as a bare heading —
/// `contributor_sections` drops a section that trims to nothing, so this is how
/// "no skills" becomes "no section" rather than a `## Skills` with nothing
/// under it.
///
/// The heading and the prose come from the operator's template, on the same
/// contract the other section templates keep: empty inherits
/// `DEFAULT_SKILLS_TEMPLATE`, a single space deletes the section. What stays in
/// code is the *shape* of the index line, because that is what `read` and
/// the catalogue agree on, not prose.
pub fn render_skills(skills: &[Skill], template: Option<&str>) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let resolved = template_or(template, DEFAULT_SKILLS_TEMPLATE);
    if js_trim(resolved).is_empty() {
        return String::new();
    }

    let index_lines = skills.iter().map(index_line).collect::<Vec<_>>().join("\n");

    // `{{index}}` carries its own leading blank line, so a template that places
    // it straight after its prose leaves no gap when the catalogue is empty.
    let mut values = IndexMap::new();
    values.insert("path".to_owned(), SKILLS_DIRNAME.to_owned());
    values.insert(
        "index".to_owned(),
        if index_lines.is_empty() {
            String::new()
        } else {
            format!("\n\n{index_lines}")
        },
    );
    values.insert("indexLines".to_owned(), index_lines);
    values.insert("count".to_owned(), skills.len().to_string());

    js_trim(&render_prompt_template(resolved, &values)).to_owned()
}

fn index_line(skill: &Skill) -> String {
    format!(
        "- `{}`: **{}**. {}",
        skill.path, skill.name, skill.description
    )
}

/// Reads the workspace's skills and places the catalogue in the prompt.
#[derive(Debug, Clone, Default)]
pub struct SkillsContributor {
    template: String,
    agent_id: String,
}

impl SkillsContributor {
    /// A contributor for `agent_id`, on the built-in wording.
    ///
    /// Whose catalogue this is — the agent whose loop the contributor hangs
    /// off, which the composition root already knows when it builds one.
    ///
    /// Deliberately not read from [`StaticPromptContext::agent_id`], which is
    /// the wrong answer twice over: it is absent when a session has no binding,
    /// so it is `None` in a prompt preview exactly where the answer is knowable
    /// and is `default`; and on a turn it is the *session's* agent, which can
    /// differ from the loop's own. Where they differ, this agent's template is
    /// the one being rendered, so this agent's scope is the one that applies.
    pub fn new(agent_id: impl Into<String>) -> SkillsContributor {
        SkillsContributor {
            template: String::new(),
            agent_id: agent_id.into(),
        }
    }

    /// The same, for the unnamed default agent.
    ///
    /// `default` rather than "advertise everything": there is one production
    /// call site and it always names an agent, so the fallback costs nothing —
    /// what it buys is that a bare contributor added later cannot quietly hand
    /// one agent's sheets to another.
    pub fn for_default_agent() -> SkillsContributor {
        SkillsContributor::new(DEFAULT_AGENT_ID)
    }

    /// The operator's wording for the section. Empty means the built-in; a
    /// single space renders nothing, which is how they delete it.
    #[must_use]
    pub fn with_template(mut self, template: impl Into<String>) -> SkillsContributor {
        self.template = template.into();
        self
    }
}

impl ContextContributor for SkillsContributor {
    fn name(&self) -> &'static str {
        "skills"
    }

    fn static_section<'a>(
        &'a self,
        context: &'a StaticPromptContext,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let skills = skills_for_agent(
                &read_skills(Path::new(&context.workspace_root)),
                &self.agent_id,
            );
            // After the filter, not before: an agent every sheet is scoped away
            // from has an empty catalogue, and an empty catalogue is no section
            // rather than a heading with nothing under it.
            if skills.is_empty() {
                return None;
            }

            let section = render_skills(&skills, Some(&self.template));
            // A template of a single space renders nothing, and `None` here is
            // what stops `contributor_sections` placing an empty section.
            if section.is_empty() {
                None
            } else {
                Some(section)
            }
        })
    }
}
