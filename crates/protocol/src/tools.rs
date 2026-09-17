//! Tool wire shapes and the permission model.
//!
//! **A permission is per tool, and it is the only thing that decides.** An
//! agent carries a `name -> allow | ask | deny` map, and a name the map does
//! not mention is not enabled at all — so "which tools does this agent have"
//! and "what happens when it calls one" are one question with one answer,
//! rather than two mechanisms that can disagree.
//!
//! `risk` survives as **metadata**. It is declared by the tool, it rides on
//! the `tool.call` and `tool.approvalRequest` events so a card can badge
//! itself, and it seeds the permission a newly created agent starts a tool at.
//! It decides nothing at call time.

use garde::Validate;
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json::Object;

/// What a tool can do, worst case.
///
/// Bands rather than a boolean because the useful default differs per band:
/// reads are fine unattended, writes are usually fine inside the workspace
/// jail, exec and network are the two an operator wants to see before they
/// happen. Advisory since permissions became per tool — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    /// Reads only.
    #[default]
    Safe,
    /// Writes inside the workspace.
    Write,
    /// Runs a process.
    Exec,
    /// Reaches the network.
    Network,
}

/// What an agent may do with one tool.
///
/// `deny` and *absent* mean the same thing to the runtime — the tool is not in
/// the definitions the model is sent. Both spellings exist because a UI needs
/// somewhere to put the off switch: an operator switching a tool off writes
/// `deny`, and that survives a round trip, where deleting the key would make
/// the row disappear from the editor entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermission {
    /// Runs unattended.
    Allow,
    /// Runs after an operator approves the call.
    Ask,
    /// Not offered to the model.
    Deny,
}

/// Tool name → permission.
///
/// A map rather than a list of pairs so a patch that mentions one tool is one
/// key, and so the whole map replaces cleanly — removing a tool has to be
/// expressible, and a deep merge of two lists cannot express it.
pub type ToolPermissions = IndexMap<String, ToolPermission>;

/// The tools that ship in the box.
///
/// Named here, below the crate that implements them, because the default agent
/// tool map needs the *names* without the implementations, to seed a new agent
/// with them.
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "list_dir",
    "exec",
    "automation",
    "memory",
    "skill",
    "tool_search",
];

/// Where a registered tool came from, so unregistering by source can be exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    /// Compiled in.
    #[default]
    Builtin,
    /// Proxied from an MCP server.
    Mcp,
    /// Registered by an extension.
    Extension,
}

/// Hints about a tool's effects, mirroring MCP's annotation vocabulary so
/// built-in tools and tools proxied from an MCP server describe themselves the
/// same way.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolAnnotations {
    /// A human-readable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The tool changes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    /// The tool may destroy data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// Calling it twice is the same as once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// The tool reaches beyond the environment it runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// A tool as advertised to a model or listed over REST.
///
/// `parameters` is an already-computed JSON Schema object, kept opaque:
/// validating a JSON Schema *document* would buy nothing, and the value is
/// produced by this repository rather than accepted from a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolDefinition {
    /// The name the model calls it by.
    #[garde(length(utf16, min = 1))]
    pub name: String,
    /// The sentence that decides whether the model reaches for it.
    pub description: String,
    /// The argument schema, as a JSON Schema object.
    pub parameters: Object,
    /// What it can do, worst case.
    #[serde(default)]
    pub risk: ToolRisk,
    /// Where it was registered from.
    #[serde(default)]
    pub source: ToolSource,
    /// MCP-style effect hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub annotations: Option<ToolAnnotations>,
}

/// How long an approval decision holds.
///
/// `session` is what makes the prompt tolerable in practice: approving `exec`
/// once per session rather than once per call. It is also the longest an answer
/// given in a prompt can hold. A standing permission is a configuration
/// decision, so it is made where it can be seen and revoked, by setting the
/// tool's permission to `allow` on the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    /// This call only.
    #[default]
    Once,
    /// Every call of this tool for the rest of the session.
    Session,
}

/// An operator's replacement for one tool's prose.
///
/// A tool's description is the sentence that decides whether the model reaches
/// for it. **Prose only, and that boundary is load-bearing.** `type`,
/// `required`, `enum` and the rest of the schema stay generated from the
/// tool's own argument type, which is also what validates a call. Letting an
/// operator supply a schema would let the advertised shape drift from the
/// accepted one, and the failure mode is a model dutifully passing a field
/// that then fails validation on every call. Unknown keys are refused rather
/// than dropped, so a misspelled `fields` is an error and not a silent no-op.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct ToolPromptOverride {
    /// Replaces the tool's description. Empty means the built-in.
    ///
    /// A single space advertises the tool with no description at all, the same
    /// "empty inherits, whitespace deletes" rule the prompt templates use.
    #[serde(default)]
    pub description: String,
    /// Top-level parameter name → its description.
    ///
    /// Top-level only: a path syntax reaching into nested schemas would be a
    /// second mini-language for the sake of a field whose parent description
    /// can say the same thing in a sentence. A name not in the schema is
    /// reported and dropped rather than added — inventing a property would
    /// advertise an argument the model then passes and the tool then rejects.
    #[serde(default)]
    pub fields: IndexMap<String, String>,
}

/// Tool name → its prose overrides. Keyed the way permissions are.
pub type ToolPromptOverrides = IndexMap<String, ToolPromptOverride>;

/// The definitions with an operator's wording applied, and what could not be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedToolPrompts {
    /// The definitions as the model will see them.
    pub definitions: Vec<ToolDefinition>,
    /// Override keys naming no advertised tool.
    pub unknown_tools: Vec<String>,
    /// `<tool>.<field>` pairs naming no property in that tool's schema.
    pub unknown_fields: Vec<String>,
}

/// The `properties` map of a JSON Schema object, when it has one.
fn properties_of(parameters: &Object) -> Option<&serde_json::Map<String, Value>> {
    parameters.get("properties").and_then(Value::as_object)
}

/// The definitions, with each operator's wording in place of the compiled one.
///
/// Pure, and the only place a definition's prose is rewritten — so "what does
/// the model actually see" has one answer and the context inspector shows it
/// without reassembling anything. Definitions are copied, never edited in
/// place: the compiled list is shared by every turn on every agent, and writing
/// one agent's wording into it would give that wording to all of them.
pub fn apply_tool_prompts(
    definitions: &[ToolDefinition],
    overrides: &ToolPromptOverrides,
) -> AppliedToolPrompts {
    if overrides.is_empty() {
        return AppliedToolPrompts {
            definitions: definitions.to_vec(),
            unknown_tools: Vec::new(),
            unknown_fields: Vec::new(),
        };
    }

    let mut seen = Vec::new();
    let mut unknown_fields = Vec::new();

    let applied = definitions
        .iter()
        .map(|definition| {
            let Some(rewrite) = overrides.get(&definition.name) else {
                return definition.clone();
            };
            seen.push(definition.name.as_str());

            let mut next = definition.clone();
            if !rewrite.fields.is_empty() {
                if let Some(properties) = properties_of(&definition.parameters) {
                    let mut properties = properties.clone();
                    let mut changed = false;
                    for (field, description) in &rewrite.fields {
                        match properties.get_mut(field).and_then(Value::as_object_mut) {
                            Some(property) => {
                                property.insert(
                                    "description".into(),
                                    Value::String(description.clone()),
                                );
                                changed = true;
                            }
                            None => unknown_fields.push(format!("{}.{field}", definition.name)),
                        }
                    }
                    if changed {
                        next.parameters
                            .insert("properties".into(), Value::Object(properties));
                    }
                } else {
                    // A tool whose schema has no `properties` at all. Every
                    // named field is unknown, and saying so once per field
                    // matches what the operator wrote.
                    for field in rewrite.fields.keys() {
                        unknown_fields.push(format!("{}.{field}", definition.name));
                    }
                }
            }
            if !rewrite.description.is_empty() {
                crate::json::js_trim(&rewrite.description).clone_into(&mut next.description);
            }
            next
        })
        .collect();

    AppliedToolPrompts {
        definitions: applied,
        unknown_tools: overrides
            .keys()
            .filter(|name| !seen.contains(&name.as_str()))
            .cloned()
            .collect(),
        unknown_fields,
    }
}
