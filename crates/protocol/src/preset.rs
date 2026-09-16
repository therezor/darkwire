//! An agent preset: an installable agent definition.
//!
//! A preset is a JSON file, beside a catalogue's environment
//! definitions, or bundled with the CLI — that `ghostai agent install` turns
//! into an entry in `agents.list`.
//! After install it is ordinary agent config: the operator edits it in the UI,
//! and nothing remembers where it came from. A preset is a starting point, not
//! a subscription, which is why there is no version field to reconcile and no
//! update command to run.
//!
//! The shape is a strict subset of [`AgentEntry`], and what is *absent* is the
//! point: no model, provider, temperature or token caps, because those describe
//! an install and a preset describes a role; no `exec` patch and no `enabled`
//! flag, because installing a disabled agent is a contradiction; and the
//! environment references are the same names an agent carries, with
//! everything that could widen a boundary living in the approved environment
//! definition, so a preset can express nothing a settings save could not. `tools_enabled` is
//! the one settings knob a preset may set, because one preset exists to switch
//! it off. `skills` is an install instruction rather than agent config, so
//! [`preset_to_agent_entry`] drops it.

use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{AgentEntry, AgentEnvironment, PromptMode, SubagentRef, default_agent_tools};
use crate::ids::SLUG_ID_PATTERN;
use crate::json::{literal, prefault};
use crate::tools::ToolPermissions;

literal! {
    /// The preset format tag. Bumped only for a breaking change; refused when
    /// unrecognised.
    pub struct AgentPresetSchemaTag = "ghostai.agent-preset/1";
}

/// An installable agent definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AgentPreset {
    /// Always `ghostai.agent-preset/1`.
    pub schema: AgentPresetSchemaTag,
    /// Becomes the `agents.list` key. The install command holds it to the
    /// same rules the UI does, since a preset arrives from disk rather than
    /// from the form that enforces them.
    #[garde(length(utf16, min = 1, max = 40))]
    pub id: String,
    /// Shown in the UI. Empty falls back to the id.
    #[serde(default)]
    pub label: String,
    /// The whole static prompt.
    #[serde(default)]
    pub system_prompt: String,
    /// See [`AgentEntry::live_prompt`].
    #[serde(default)]
    pub live_prompt: String,
    /// See [`AgentEntry::wrap_up_prompt`].
    #[serde(default)]
    pub wrap_up_prompt: String,
    /// See [`AgentEntry::platform_prompt`].
    #[serde(default)]
    pub platform_prompt: String,
    /// See [`AgentEntry::tool_policy_prompt`].
    #[serde(default)]
    pub tool_policy_prompt: String,
    /// See [`AgentEntry::memory_prompt`].
    #[serde(default)]
    pub memory_prompt: String,
    /// See [`AgentEntry::skills_prompt`].
    #[serde(default)]
    pub skills_prompt: String,
    /// See [`AgentEntry::prompt_mode`].
    #[serde(default)]
    pub prompt_mode: PromptMode,
    /// The one settings knob a preset may set. Unset takes the entry's `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_enabled: Option<bool>,
    /// Replaces, never merges — the same rule as [`AgentEntry::tools`].
    #[serde(default = "default_agent_tools")]
    pub tools: ToolPermissions,
    /// Command placement for built-in `exec`.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub environment: AgentEnvironment,
    /// Agents this one may delegate to.
    #[serde(default)]
    #[garde(dive)]
    pub subagents: Vec<SubagentRef>,
    /// Skill directories to copy out of the catalogue's `skills/` and into the
    /// workspace's, named by directory.
    ///
    /// Slug-shaped because each name becomes a path segment and arrived over
    /// the network: `..`, `/` and a leading `~` are unrepresentable, so the
    /// copier never has to judge a traversal — a preset carrying one fails to
    /// parse. Deliberately stricter than what a *workspace* may hold, where a
    /// sheet directory is whatever a person named it.
    #[serde(default)]
    #[garde(length(max = 32), inner(pattern(SLUG_ID_PATTERN)))]
    pub skills: Vec<String>,
}

/// The `agents.list` entry a preset installs as.
///
/// Every field the preset does not carry takes the entry's own default, so a
/// field added to [`AgentEntry`] later gets its default here without this
/// function knowing it exists. `tools_enabled` is applied only when the preset
/// set it; `id`, `schema` and `skills` never reach the entry.
pub fn preset_to_agent_entry(preset: &AgentPreset) -> AgentEntry {
    let mut entry = AgentEntry {
        label: preset.label.clone(),
        system_prompt: preset.system_prompt.clone(),
        live_prompt: preset.live_prompt.clone(),
        wrap_up_prompt: preset.wrap_up_prompt.clone(),
        platform_prompt: preset.platform_prompt.clone(),
        tool_policy_prompt: preset.tool_policy_prompt.clone(),
        memory_prompt: preset.memory_prompt.clone(),
        skills_prompt: preset.skills_prompt.clone(),
        prompt_mode: preset.prompt_mode,
        enabled: true,
        tools: preset.tools.clone(),
        environment: preset.environment.clone(),
        subagents: preset.subagents.clone(),
        ..AgentEntry::default()
    };
    if let Some(tools_enabled) = preset.tools_enabled {
        entry.settings.tools_enabled = tools_enabled;
    }
    entry
}
