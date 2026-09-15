//! Config in, one agent's effective settings out.
//!
//! An agent *is* its `agents.list` entry — there is nothing above it. The schema
//! fills what the entry does not name, so this module resolves rather than
//! merges, and "what does this agent run on" is answerable from one place.
//!
//! Three decisions worth stating, because each is the kind that looks arbitrary
//! later:
//!
//!  - **`default` is an agent, not the absence of one.** The schema prefaults an
//!    entry for it, so an install that has defined none still resolves to a
//!    complete [`EffectiveAgent`] under that id and nothing downstream needs an
//!    "or the global settings" branch. It is also the only agent whose `enabled`
//!    flag is ignored: switching it off would leave an install with no agent at
//!    all, which is not a state anything above here can do anything useful with.
//!  - **An unconfigured agent is a state, not an error.** An entry with no model
//!    parses, lists and edits; only a *turn* on it is refused. That is what lets
//!    an operator fix one from the screen that shows it rather than from a
//!    server that will not boot.
//!  - **Resolution is where an unbuildable agent is refused.** A toolbox setting
//!    that cannot be honoured fails here, during a reconfigure, which is
//!    all-or-nothing — so a settings save naming an unapproved toolbox is a
//!    refusal that changes nothing, rather than a turn that dies minutes later.
//!    Only the half decidable from config is checked here.

use ghostai_agent::SubagentBinding;
use ghostai_core::{ErrorKind, GhostError, Result};
use ghostai_protocol::rest::ConfigWarning;
use ghostai_protocol::{
    AgentContainer, AgentEntry, AgentSettings, AgentToolbox, Config, DEFAULT_AGENT_ID,
    DEFAULT_LIVE_STATE_TEMPLATE, NetworkMode, PromptMode, RESERVED_AGENT_IDS, ToolPermission,
    ToolPermissions, ToolPromptOverrides, ToolsConfig, default_agent_tools, is_agent_id,
    names_delimiter, subagent_tool_name,
};
use ghostai_security::assert_container_network;
use indexmap::IndexMap;

/// One agent, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveAgent {
    /// The `agents.list` key.
    pub id: String,
    /// Never empty: falls back to the id, so a UI never has to.
    pub label: String,
    /// The whole static system prompt, as a template.
    pub system_prompt: String,
    /// The per-iteration half's live-state section. Empty means the built-in.
    ///
    /// Resolved beside `system_prompt` rather than read from the config
    /// downstream, so every consumer sees one already-inherited answer.
    pub live_prompt: String,
    /// What is appended in the last few iterations of a turn.
    pub wrap_up_prompt: String,
    /// The `## Running commands` section.
    pub platform_prompt: String,
    /// The `## Toolbox: <name>` advertisement.
    pub toolbox_prompt: String,
    /// The `## Tool output policy` section.
    pub tool_policy_prompt: String,
    /// The `## Memory` section. Empty means the built-in; a space removes it.
    pub memory_prompt: String,
    /// The `## Skills` section, on the same contract.
    pub skills_prompt: String,
    /// Whether the static prompt is composed or taken whole.
    pub prompt_mode: PromptMode,
    /// This agent's replacements for what its tools say about themselves.
    pub tool_prompts: ToolPromptOverrides,
    /// Model, provider, temperature, effort, caps — this agent's own.
    pub settings: AgentSettings,
    /// Which tools this agent may call, and what happens when it does.
    pub tools: ToolPermissions,
    /// `config.tools` with this agent's exec overrides applied.
    pub tools_config: ToolsConfig,
    /// Where this agent's commands run.
    pub toolbox: AgentToolbox,
    /// Where command operations run.
    pub container: AgentContainer,
    /// The agents this one may delegate to, in the operator's order.
    ///
    /// Resolved to the shape the loop is constructed with rather than left as
    /// the stored refs, so the tool name is derived once — here — instead of in
    /// the loop, the editor and whatever asks next.
    pub subagents: Vec<SubagentBinding>,
}

/// Why a warning was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentWarningCode {
    /// A delegation to an id `agents.list` does not hold.
    MissingSubagent,
    /// A delegation to an agent that exists and is switched off.
    DisabledSubagent,
    /// An entry stored under a key that cannot name an agent.
    IllegalAgentId,
    /// Neither the tool-output policy nor the live-state section names the
    /// turn's delimiter.
    ToolPolicyMissingNonce,
    /// A tool prompt override naming a tool this agent will not advertise.
    UnknownToolPrompt,
    /// An agent that states no model, so a turn on it is refused.
    NoModel,
}

impl AgentWarningCode {
    /// The `snake_case` spelling, which is what a wire frame carries.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentWarningCode::MissingSubagent => "missing_subagent",
            AgentWarningCode::DisabledSubagent => "disabled_subagent",
            AgentWarningCode::IllegalAgentId => "illegal_agent_id",
            AgentWarningCode::ToolPolicyMissingNonce => "tool_policy_missing_nonce",
            AgentWarningCode::UnknownToolPrompt => "unknown_tool_prompt",
            AgentWarningCode::NoModel => "no_model",
        }
    }
}

/// Something the settings asked for that had to be ignored to keep going.
///
/// The counterpart to the errors below, and the distinction is *whose fault it
/// is and when*. A malformed entry is the operator's, and it is refused where
/// they wrote it. A reference to an agent that has since been deleted is
/// nobody's — the id it names was legal when it was written — so refusing it
/// would let one delete stop an install that was working a moment ago.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfigWarning {
    /// Whose settings raised it.
    pub agent_id: String,
    /// Which rule.
    pub code: AgentWarningCode,
    /// What to tell the operator.
    pub message: String,
    /// The other id involved, when there is one.
    pub subject: Option<String>,
}

impl AgentConfigWarning {
    /// The wire shape, for a transport that reports these to an operator.
    ///
    /// The subject is dropped rather than carried: the DTO names the agent whose
    /// settings raised the warning, and the other id involved is already in the
    /// sentence a person reads.
    pub fn to_dto(&self) -> ConfigWarning {
        ConfigWarning {
            code: self.code.as_str().to_owned(),
            message: self.message.clone(),
            agent_id: Some(self.agent_id.clone()),
        }
    }
}

/// Why an id did not name an agent that could run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMissReason {
    /// No entry, or one stored under a key that cannot name an agent.
    Unknown,
    /// An entry that exists and is switched off.
    Disabled,
}

/// What an id asked for and what would actually run.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentResolution {
    /// What was asked for, verbatim — including an id that names nothing.
    pub requested_id: String,
    /// What would actually run. Never absent: the default when the request
    /// missed.
    pub agent: EffectiveAgent,
    /// `None` when `requested_id` resolved to itself.
    pub miss: Option<AgentMissReason>,
}

/// `config.tools`, narrowed by whatever this agent overrode.
fn merge_tools_config(tools: &ToolsConfig, entry: Option<&AgentEntry>) -> ToolsConfig {
    let mut merged = tools.clone();
    let Some(patch) = entry.and_then(|entry| entry.exec.as_ref()) else {
        return merged;
    };
    let exec = &mut merged.exec;
    if let Some(enable) = patch.enable {
        exec.enable = enable;
    }
    if let Some(timeout_ms) = patch.timeout_ms {
        exec.timeout_ms = timeout_ms;
    }
    if let Some(path_append) = patch.path_append.clone() {
        exec.path_append = path_append;
    }
    if let Some(allowed) = patch.allowed_binaries.clone() {
        exec.allowed_binaries = allowed;
    }
    if let Some(denied) = patch.denied_binaries.clone() {
        exec.denied_binaries = denied;
    }
    if let Some(env) = patch.env_allowlist.clone() {
        exec.env_allowlist = env;
    }
    if let Some(max) = patch.max_output_bytes {
        exec.max_output_bytes = max;
    }
    merged
}

/// The label an agent id resolves to, without building the whole agent.
///
/// A subagent's label is read off the *target's* entry, not written on the
/// reference: one place to rename an agent, and a reference that keeps up.
fn label_of(config: &Config, id: &str) -> String {
    let label = config
        .agents
        .list
        .get(id)
        .map_or("", |entry| entry.label.as_str());
    if label.is_empty() {
        id.to_owned()
    } else {
        label.to_owned()
    }
}

/// The stored refs as the loop's bindings, dropping what cannot work.
///
/// Two kinds of bad ref, and they are bad at *different moments*, which is why
/// they get different answers:
///
///  - **Malformed** — a ref to itself, or the same target twice. Decidable from
///    this entry alone, and no edit to any *other* agent can cause it. Refused,
///    so a settings save reports it as the bad request it is and changes
///    nothing.
///  - **Dangling** — the target has been deleted or switched off. Caused by an
///    edit somewhere else entirely, possibly months ago, possibly by hand while
///    the server was down. Dropped with a warning.
///
/// The second is dropped rather than refused, and the distinction is about
/// *when*. A patch introducing a ref to nothing is still refused, by
/// [`prune_dangling_subagents`], which strips it before it can be written —
/// because a subagent that looks configured in the editor and reports "no such
/// agent" the first time the model reaches for it is a bug report rather than a
/// validation message. But a ref *already on disk* must not take the install
/// down with it: this runs inside the runtime's build, so failing here would let
/// one hand-edited line stop the server from starting at all.
///
/// What is *not* checked here: whether the target agent can actually resolve a
/// provider. That depends on credentials and on a provider being reachable, so
/// it is a runtime state rather than a config error — the delegation tool
/// handles an absent loop by telling the model, which is the right altitude.
fn resolve_subagents(
    config: &Config,
    id: &str,
    entry: Option<&AgentEntry>,
    warnings: &mut Vec<AgentConfigWarning>,
) -> Result<Vec<SubagentBinding>> {
    let refs = entry.map_or(&[][..], |entry| entry.subagents.as_slice());
    let mut bindings: Vec<SubagentBinding> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();

    for reference in refs {
        if reference.id == id {
            return Err(GhostError::new(
                ErrorKind::InvalidInput,
                format!("Agent \"{id}\" lists itself as a subagent."),
            )
            .with_detail("agentId", id));
        }
        if seen.contains(&reference.id.as_str()) {
            return Err(GhostError::new(
                ErrorKind::InvalidInput,
                format!(
                    "Agent \"{id}\" lists \"{}\" as a subagent twice.",
                    reference.id
                ),
            )
            .with_detail("agentId", id)
            .with_detail("subagentId", reference.id.clone()));
        }
        // `has_agent` rather than a lookup, so `default` — which usually has no
        // entry at all — is delegable like any other agent.
        if !has_agent(config, &reference.id) {
            let missing = !config.agents.list.contains_key(&reference.id);
            let known: Vec<&str> = config.agents.list.keys().map(String::as_str).collect();
            warnings.push(AgentConfigWarning {
                agent_id: id.to_owned(),
                code: if missing {
                    AgentWarningCode::MissingSubagent
                } else {
                    AgentWarningCode::DisabledSubagent
                },
                message: if missing {
                    format!(
                        "Agent \"{id}\" delegates to \"{}\", which does not exist. Known agents: {}",
                        reference.id,
                        known.join(", ")
                    )
                } else {
                    format!(
                        "Agent \"{id}\" delegates to \"{}\", which is switched off.",
                        reference.id
                    )
                },
                subject: Some(reference.id.clone()),
            });
            // Dropped rather than bound: a binding whose target cannot run would
            // put a tool in front of the model that fails every time it is
            // called, which reads to the model as its own mistake rather than as
            // a missing agent.
            continue;
        }

        seen.push(reference.id.as_str());
        bindings.push(SubagentBinding {
            tool_name: subagent_tool_name(&reference.id),
            agent_id: reference.id.clone(),
            label: label_of(config, &reference.id),
            prompt: reference.prompt.clone(),
            permission: reference.permission,
        });
    }

    Ok(bindings)
}

/// The live-state template this agent actually renders.
///
/// Empty inherits the built-in, which names the delimiter; a single space
/// deletes the section, which does not. The same rule the prompt builder
/// applies, asked here so the warning is about the prompt an agent will carry.
fn live_template(agent: &EffectiveAgent) -> &str {
    if agent.live_prompt.is_empty() {
        DEFAULT_LIVE_STATE_TEMPLATE
    } else {
        &agent.live_prompt
    }
}

/// The wire spelling of a network mode, for the message and the detail.
fn mode_name(mode: NetworkMode) -> &'static str {
    match mode {
        NetworkMode::None => "none",
        NetworkMode::Allowlist => "allowlist",
        NetworkMode::Open => "open",
    }
}

/// What can be decided from the config alone.
///
/// Whether the named toolbox and container *exist and are approved* is not here,
/// deliberately: that needs the policy store, which is disk, and this is the
/// pure inheritance rule. It is checked in the runtime's build, which is equally
/// all-or-nothing, so a settings save naming an unapproved toolbox is still a
/// refusal that changes nothing rather than a turn that dies later.
fn assert_buildable(agent: &EffectiveAgent, warnings: &mut Vec<AgentConfigWarning>) -> Result<()> {
    // A warning rather than a refusal, and the distinction is the whole design
    // of this feature: the envelopes around tool results are emitted whatever
    // this text says, so a policy naming neither hole is an agent that is *told*
    // less, not one that is *guarded* less. Refusing the save would make this
    // the one template an operator does not own after all. The delimiter has to
    // be named *somewhere*, not specifically here: the built-in policy
    // deliberately names none — it is prose that never changes, so it lives in
    // the prompt's cached half and the live-state section supplies the turn's
    // tag. What leaves the model unable to identify a fence is neither template
    // naming it, which takes two edits to reach.
    let policy = agent.tool_policy_prompt.trim();
    if !policy.is_empty() && !names_delimiter(policy) && !names_delimiter(live_template(agent)) {
        warnings.push(AgentConfigWarning {
            agent_id: agent.id.clone(),
            code: AgentWarningCode::ToolPolicyMissingNonce,
            message: format!(
                "Agent \"{}\" names {{{{tag}}}} in neither its tool-output policy nor its \
                 live-state section.\n  Tool results are still wrapped in the turn's delimiter; \
                 the model is just not told which one.",
                agent.id
            ),
            subject: None,
        });
    }

    if agent.settings.model.is_empty() {
        warnings.push(AgentConfigWarning {
            agent_id: agent.id.clone(),
            code: AgentWarningCode::NoModel,
            message: format!(
                "Agent \"{}\" states no model, so a turn on it is refused.\n  Choose one in \
                 Settings → Agents, or set its `model` in the config file.",
                agent.id
            ),
            subject: None,
        });
    }

    let network = &agent.container.network;
    if !agent.container.name.is_empty() && agent.toolbox.name.is_empty() {
        return Err(GhostError::new(
            ErrorKind::Config,
            format!(
                "Agent \"{}\" selects container \"{}\" but no toolbox.\n  A container only \
                 hosts a toolbox's approved operations, so one on its own would run\n  nothing. \
                 Select a toolbox, or clear the container.",
                agent.id, agent.container.name
            ),
        )
        .with_detail("agentId", agent.id.clone())
        .with_detail("container", agent.container.name.clone()));
    }
    if agent.container.name.is_empty() && network.mode != NetworkMode::None {
        return Err(GhostError::new(
            ErrorKind::Config,
            format!(
                "Agent \"{}\" asks for network \"{}\" but names no container.\n  Egress scoping \
                 is enforced by the container's gateway, so it means nothing\n  on the host.",
                agent.id,
                mode_name(network.mode)
            ),
        )
        .with_detail("agentId", agent.id.clone())
        .with_detail("mode", mode_name(network.mode)));
    }
    assert_container_network(network, &agent.id)?;
    Ok(())
}

/// One entry as a complete agent.
fn build(
    config: &Config,
    id: &str,
    entry: Option<&AgentEntry>,
    warnings: &mut Vec<AgentConfigWarning>,
) -> Result<EffectiveAgent> {
    let defaults = AgentEntry::default();
    let source = entry.unwrap_or(&defaults);
    let agent = EffectiveAgent {
        id: id.to_owned(),
        label: if source.label.is_empty() {
            id.to_owned()
        } else {
            source.label.clone()
        },
        system_prompt: source.system_prompt.clone(),
        live_prompt: source.live_prompt.clone(),
        wrap_up_prompt: source.wrap_up_prompt.clone(),
        platform_prompt: source.platform_prompt.clone(),
        toolbox_prompt: source.toolbox_prompt.clone(),
        tool_policy_prompt: source.tool_policy_prompt.clone(),
        memory_prompt: source.memory_prompt.clone(),
        skills_prompt: source.skills_prompt.clone(),
        prompt_mode: source.prompt_mode,
        tool_prompts: source.tool_prompts.clone(),
        settings: source.settings.clone(),
        // The `default` agent usually has no `agents.list` entry at all, and an
        // agent with no tools cannot do anything — so the seed is the fallback
        // here as well as the schema's, not only the schema's.
        tools: if entry.is_none() {
            default_agent_tools()
        } else {
            source.tools.clone()
        },
        tools_config: merge_tools_config(&config.tools, entry),
        toolbox: source.toolbox.clone(),
        container: source.container.clone(),
        subagents: resolve_subagents(config, id, entry, warnings)?,
    };
    assert_buildable(&agent, warnings)?;
    Ok(agent)
}

/// Tool prompt overrides naming a tool this agent will not advertise.
///
/// Separate from the buildability check because it needs something the pure
/// inheritance rule does not have: the toolbox's own programs, which are merged
/// over the agent's map when the loop is built and are not decidable from
/// `agents.list` alone. Checking without them would warn about every override on
/// a toolboxed agent, which is worse than not checking.
///
/// Field names are not checked here for the same reason one step further on —
/// they need the tool's JSON Schema, which lives in the registry. The editor
/// validates those against the live definitions as they are typed.
pub fn tool_prompt_warnings(
    agent: &EffectiveAgent,
    advertised: &[String],
) -> Vec<AgentConfigWarning> {
    agent
        .tool_prompts
        .keys()
        .filter(|name| !advertised.iter().any(|known| known == *name))
        .map(|name| AgentConfigWarning {
            agent_id: agent.id.clone(),
            code: AgentWarningCode::UnknownToolPrompt,
            message: format!(
                "Agent \"{}\" rewrites the description of \"{name}\", which it does not have.",
                agent.id
            ),
            subject: Some(name.clone()),
        })
        .collect()
}

/// Whether an id names an agent this config can run.
///
/// `default` always does. Anything else has to be in `agents.list` *and*
/// enabled — a disabled agent is invisible to everything except the settings
/// tree that is about to re-enable it.
pub fn has_agent(config: &Config, id: &str) -> bool {
    if id == DEFAULT_AGENT_ID {
        return true;
    }
    // The pattern check as well as the lookup, so an entry stored under a key
    // that is not a legal id is invisible everywhere rather than only to
    // `resolve_agents` — otherwise it could be delegated to and bound to, and
    // then fail later at the one place that turns an id into a path.
    if !is_agent_id(id) {
        return false;
    }
    config
        .agents
        .list
        .get(id)
        .is_some_and(|entry| entry.enabled)
}

/// The id an empty or absent request means.
fn requested(id: Option<&str>) -> &str {
    match id {
        None | Some("") => DEFAULT_AGENT_ID,
        Some(id) => id,
    }
}

/// One agent's effective settings.
///
/// `None` means the default agent, which is what a turn from a session nobody
/// has bound carries. Fails for an id that names nothing runnable — callers that
/// expect to be handed an arbitrary string from the wire should ask
/// [`has_agent`] first and report the miss in their own vocabulary.
pub fn resolve_agent(config: &Config, id: Option<&str>) -> Result<EffectiveAgent> {
    let agent_id = requested(id);
    let entry = config.agents.list.get(agent_id);

    if !has_agent(config, agent_id) {
        let known = list_agents(config)?
            .into_iter()
            .map(|agent| agent.id)
            .collect::<Vec<_>>()
            .join(", ");
        let message = if entry.is_none() || !is_agent_id(agent_id) {
            format!("No agent named \"{agent_id}\". Known agents: {known}")
        } else {
            format!("Agent \"{agent_id}\" is disabled.")
        };
        return Err(GhostError::new(ErrorKind::NotFound, message).with_detail("agentId", agent_id));
    }

    // Nothing is listening for warnings here; a caller resolving one agent has
    // nowhere to put a diagnostic. `resolve_agents` is the surface that collects
    // them.
    let mut discarded = Vec::new();
    build(config, agent_id, entry, &mut discarded)
}

/// The same answer as [`resolve_agent`], degrading to `default` instead of
/// failing.
///
/// The door for anything holding an id it did not choose — a session row, a
/// websocket frame, a browser's remembered preference. All three can name an
/// agent an operator deleted between when it was written and now, and none of
/// them is a place where "this install is broken" is a true thing to say.
///
/// It degrades on **absence**, never on **fault**: an agent that exists but
/// cannot be built — an egress rule that is not a CIDR, a toolbox network with
/// no toolbox — still fails. Those are settings that were never going to work,
/// and silently substituting a different agent for them would hide the one thing
/// the operator needs to see.
///
/// The caller decides what to do about the miss. Nothing here reports it,
/// because the vocabulary differs by surface: the hub raises a notice on the
/// turn, the context panel labels the figures it is showing, and the picker
/// marks the binding.
pub fn resolve_agent_or_default(config: &Config, id: Option<&str>) -> Result<AgentResolution> {
    let requested_id = requested(id).to_owned();
    let entry = config.agents.list.get(&requested_id);
    let mut discarded = Vec::new();

    if has_agent(config, &requested_id) {
        let agent = build(config, &requested_id, entry, &mut discarded)?;
        return Ok(AgentResolution {
            requested_id,
            agent,
            miss: None,
        });
    }

    // An entry under an unusable key reads as `Unknown` rather than `Disabled`:
    // it is switched on, it just cannot be reached by that name, and telling the
    // operator it is disabled would send them to a toggle that is already in the
    // position they want.
    let miss = if entry.is_none() || !is_agent_id(&requested_id) {
        AgentMissReason::Unknown
    } else {
        AgentMissReason::Disabled
    };
    let agent = build(
        config,
        DEFAULT_AGENT_ID,
        config.agents.list.get(DEFAULT_AGENT_ID),
        &mut discarded,
    )?;
    Ok(AgentResolution {
        requested_id,
        agent,
        miss: Some(miss),
    })
}

/// Every agent that can run a turn, plus what had to be ignored to build them.
///
/// The default one first, then insertion order — the order the operator wrote
/// them in `config.json` and the order the picker shows.
///
/// Warnings come out beside the agents rather than hanging off each one, because
/// an [`EffectiveAgent`] is held by the loop and the picker and half the settings
/// tree, and none of those wants to carry a diagnostics list it will never read.
pub fn resolve_agents(config: &Config) -> Result<(Vec<EffectiveAgent>, Vec<AgentConfigWarning>)> {
    let mut warnings: Vec<AgentConfigWarning> = Vec::new();
    let mut agents = vec![build(
        config,
        DEFAULT_AGENT_ID,
        config.agents.list.get(DEFAULT_AGENT_ID),
        &mut warnings,
    )?];

    for (id, entry) in &config.agents.list {
        if id == DEFAULT_AGENT_ID || !entry.enabled {
            continue;
        }
        // A key that is not a legal id got in by hand or through an older build:
        // this record's key is a plain string, deliberately, so that a file
        // carrying one still parses and can still be edited back out. It is
        // excluded rather than refused, because an id that cannot name a
        // directory cannot run a turn either — every id that reaches a path is
        // re-validated on the way.
        if !is_agent_id(id) {
            warnings.push(AgentConfigWarning {
                agent_id: id.clone(),
                code: AgentWarningCode::IllegalAgentId,
                message: format!(
                    "\"{id}\" is not a usable agent id, so that agent is being ignored.\n  Ids \
                     are lower-case letters, digits and hyphens, up to 40 characters."
                ),
                subject: None,
            });
            continue;
        }
        agents.push(build(config, id, Some(entry), &mut warnings)?);
    }
    Ok((agents, warnings))
}

/// Every agent that can run a turn, the default one first.
///
/// The warning-free half of [`resolve_agents`], kept because most callers are
/// answering "which agents are there" and have nowhere to put a diagnostic.
pub fn list_agents(config: &Config) -> Result<Vec<EffectiveAgent>> {
    Ok(resolve_agents(config)?.0)
}

/// One delegation this config no longer supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedSubagent {
    /// The agent that held the delegation.
    pub agent_id: String,
    /// The target that is gone.
    pub subagent_id: String,
}

/// The config with delegations to agents that no longer exist removed.
///
/// Owned by the reconfigure rather than by the merge, because "deleting
/// `agents.list.x` also edits `agents.list.y.subagents`" is knowledge about
/// agents and [`crate::merge_config_patch`] is a generic tree merge — putting it
/// there would make a *preview* of a patch change more than the patch said.
/// Owning it at the route was the other option and is worse: there is more than
/// one way into a write, and a second one would silently skip the healing.
///
/// It is what makes deleting a delegated-to agent work at all. The delete used
/// to leave a ref pointing at nothing, resolution failed on the rebuild, and
/// because the settings route rebuilds *before* it writes, the operator got a
/// server error and a file that had not changed — a delete that reported as a
/// fault and then did nothing.
///
/// Absent and disabled are treated differently on purpose:
///
///  - **Absent → pruned.** The ref can never work again. Only re-creating an
///    agent under the same id would revive it, and that is a new agent.
///  - **Disabled → kept.** Switching an agent off is documented as the
///    reversible half of deleting it, so a delegation has to survive it.
///    Resolution drops the *binding* and warns; the *ref* stays in the file, and
///    switching the agent back on restores the delegation.
pub fn prune_dangling_subagents(config: &Config) -> (Config, Vec<PrunedSubagent>) {
    let mut removed: Vec<PrunedSubagent> = Vec::new();
    let mut list: IndexMap<String, AgentEntry> = IndexMap::new();

    for (id, entry) in &config.agents.list {
        let mut kept = Vec::with_capacity(entry.subagents.len());
        for reference in &entry.subagents {
            // Present-but-disabled survives, so the test is the entry's
            // existence rather than `has_agent`, which also answers false for a
            // disabled agent.
            let exists =
                reference.id == DEFAULT_AGENT_ID || config.agents.list.contains_key(&reference.id);
            if exists {
                kept.push(reference.clone());
            } else {
                removed.push(PrunedSubagent {
                    agent_id: id.clone(),
                    subagent_id: reference.id.clone(),
                });
            }
        }
        let mut next = entry.clone();
        if kept.len() != entry.subagents.len() {
            next.subagents = kept;
        }
        list.insert(id.clone(), next);
    }

    // The same config back when nothing changed, so a healthy config is not
    // rewritten into an equal-but-different one on every single save.
    if removed.is_empty() {
        return (config.clone(), removed);
    }
    let mut healed = config.clone();
    healed.agents.list = list;
    (healed, removed)
}

/// Refuses a write that introduces an agent id nothing downstream can use.
///
/// The record's key is a plain string and stays that way: tightening the
/// *schema* would stop an install whose file already holds an odd key from
/// booting at all, which is the exact failure this whole area exists to remove.
/// So the rule lives on the write instead — a file already on disk keeps
/// loading, and nothing new gets in.
///
/// Before/after rather than a flat check for the same reason. A key that is
/// already stored has to stay *deletable*: an id that cannot be written is
/// otherwise an id that can never be removed, and the operator is stuck with an
/// agent they cannot get rid of through the only interface that edits agents.
///
/// `invalid_input` rather than `config`, because this is a request body being
/// refused rather than a fault in the operator's own file.
pub fn assert_writable_agent_ids(before: &Config, after: &Config) -> Result<()> {
    for id in after.agents.list.keys() {
        if before.agents.list.contains_key(id) || id == DEFAULT_AGENT_ID {
            continue;
        }
        if is_agent_id(id) && !RESERVED_AGENT_IDS.contains(&id.as_str()) {
            continue;
        }
        return Err(GhostError::new(
            ErrorKind::InvalidInput,
            format!(
                "\"{id}\" cannot be used as an agent id.\n  Ids are lower-case letters, digits \
                 and hyphens, up to 40 characters,\n  and cannot be a reserved device name."
            ),
        )
        .with_detail("agentId", id.clone()));
    }
    Ok(())
}

/// Whether an agent may call a tool at all.
///
/// **Absent counts as denied**, which is the rule the tools documentation
/// already states — "a tool absent from the map is not enabled" — and it is the
/// whole migration story for `memory` and `skill`: the seed is what a *new*
/// agent gets, so an install that predates them has neither until an operator
/// grants it.
///
/// `ask` counts as granted. The operator answers per call; the capability is
/// still one this agent has, so its prompt section belongs there.
pub fn granted(tools: &ToolPermissions, name: &str) -> bool {
    tools
        .get(name)
        .is_some_and(|permission| *permission != ToolPermission::Deny)
}
