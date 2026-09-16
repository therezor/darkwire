//! Delegation: one agent handing a task to another and waiting for the answer.
//!
//! This file is the pure half — what a binding is, what the model is told
//! about it, when a delegation is refused, and how a finished run becomes a
//! tool result. The impure half is `AgentLoop::run_subagent`, which is where
//! the child session is created and the child's events are forwarded.
//!
//! **Why the loop and not a tool.** Every other capability a model has is a
//! tool in `ghostai-tools`, and this deliberately is not, for two reasons that
//! are both structural rather than stylistic:
//!
//!  - `ghostai-tools` sits *below* this crate in the layer graph, so a tool
//!    that started a turn would invert the dependency Cargo enforces. A
//!    registry that could reach an `AgentLoop` is a registry that has the whole
//!    agent behind it.
//!  - A tool returns a string when it is done, which is exactly the wrong shape
//!    for something that runs for a minute and whose whole value to a watching
//!    operator is *what it did on the way*. The loop already streams events;
//!    delegation is forwarding them.
//!
//! What follows from that is the nice part: a subagent's tool calls stream,
//! approve and abort through the machinery that already exists, because they
//! are a real turn on a real loop and not a special case of one.

use std::sync::LazyLock;

use ghostai_core::{ErrorKind, GhostError, Result};
use ghostai_protocol::json::Object;
use ghostai_protocol::{
    StopReason, ToolDefinition, ToolPermission, ToolRisk, ToolSource, default_subagent_prompt,
};
use ghostai_tools::ToolExecution;
use indexmap::IndexMap;
use serde_json::{Value, json};

/// How deep delegation may go.
///
/// Three, because two is the shape people actually configure — an agent with a
/// researcher, and a researcher with a summariser — and the next number after
/// "enough" is where a cap belongs. It is a backstop rather than a budget: the
/// thing that actually stops runaway delegation is that every level costs a
/// whole turn, and the operator sees each one.
pub const MAX_SUBAGENT_DEPTH: usize = 3;

/// One agent this loop may delegate to, resolved.
///
/// Built by the composition root from an agent's configured subagents and
/// handed to the loop as a map keyed by `tool_name`. It is deliberately the
/// *only* map: the definitions the model is shown and the permission the gate
/// reads both come from here, so a subagent cannot be advertised and then
/// refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentBinding {
    /// The tool name the model calls it by. See `subagent_tool_name`.
    pub tool_name: String,
    /// The agent it resolves to.
    pub agent_id: String,
    /// Never empty — falls back to the id, as a resolved agent's label does.
    pub label: String,
    /// The operator's guidance. Empty means one is written for them.
    pub prompt: String,
    /// What the gate reads before the delegation runs.
    pub permission: ToolPermission,
    /// Whether the delegated turn runs where this one does.
    ///
    /// Carried on the binding rather than read off the target's entry because
    /// it is the *caller* that decides: see `SubagentRef::inherit_environment`.
    pub inherit_environment: bool,
}

/// The one argument a delegation takes, as JSON Schema. Built once.
static TASK_PARAMETERS: LazyLock<Object> = LazyLock::new(|| {
    let mut schema = Object::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert(
        "properties".to_owned(),
        json!({
            "task": {
                "type": "string",
                "description": "What the subagent should do, written as if to a colleague who \
                                cannot see this conversation. Include everything it needs; it \
                                does not share your history.",
            },
        }),
    );
    schema.insert("required".to_owned(), json!(["task"]));
    schema.insert("additionalProperties".to_owned(), json!(false));
    schema
});

/// The wire spelling of a stop reason, for the sentence a cut-short delegation
/// hands back to the model.
///
/// Restated rather than serialised through serde because it is prose the model
/// reads, and `tests/subagent.rs` holds it to the serialised form so the two
/// cannot come apart.
fn stop_reason_word(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Complete => "complete",
        StopReason::Aborted => "aborted",
        StopReason::MaxIterations => "max_iterations",
        StopReason::WallTimeout => "wall_timeout",
        StopReason::Error => "error",
    }
}

/// What the model reads when deciding whether to delegate.
///
/// The operator's sentence, if they wrote one, is used *as* the description
/// rather than appended to a generated one. A tool description is the entire
/// basis on which a model chooses to call something, so an operator who writes
/// "use this when you need facts you do not have; ask for a summary, not raw
/// sources" is writing the part of this feature that decides when it fires —
/// and a preamble in front of it would only dilute that.
///
/// The fallback names the agent and says the one thing a model cannot infer:
/// that the subagent starts from nothing and answers in prose. It lives in
/// `ghostai-protocol` because the settings UI shows it as the field's
/// placeholder, and a second copy here would be a promise about what the model
/// reads that could quietly stop being true.
fn describe_subagent(binding: &SubagentBinding) -> String {
    let own = binding.prompt.trim();
    if own.is_empty() {
        default_subagent_prompt(&binding.label)
    } else {
        own.to_owned()
    }
}

/// The tool definition a subagent is advertised as.
pub fn subagent_definition(binding: &SubagentBinding) -> ToolDefinition {
    ToolDefinition {
        name: binding.tool_name.clone(),
        description: describe_subagent(binding),
        parameters: TASK_PARAMETERS.clone(),
        // Not `exec`, and not a new band. `ToolRisk` describes what a call does
        // to the machine, and delegating does nothing to it — the subagent's
        // own calls carry their own bands, and those are the ones an operator
        // is asked about.
        risk: ToolRisk::Safe,
        source: ToolSource::Builtin,
        annotations: None,
    }
}

/// Why a delegation could not run.
///
/// All three are answered with a tool *result* rather than an error, for the
/// reason `denied_tool_result` exists: a model that is told its delegation was
/// refused can answer without it, and a turn that dies instead leaves the
/// operator with an error where an answer was possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DelegationRefusal {
    /// The agent has no provider or model, so no loop could be built for it.
    Unconfigured,
    /// The agent is already running above this call.
    Cycle,
    /// Delegation is already at [`MAX_SUBAGENT_DEPTH`].
    TooDeep,
}

/// The task string, as the model supplied it — or nothing usable.
pub fn parse_task(args: &Value) -> Option<String> {
    let task = args.get("task")?.as_str()?;
    if task.trim().is_empty() {
        None
    } else {
        Some(task.to_owned())
    }
}

/// Whether this delegation may proceed, given who is already above it.
///
/// `chain` is the ancestor agent ids, oldest first, and it is carried on the
/// turn rather than held on the loop for a reason worth stating: loops are one
/// per agent and shared through a cache, so anything depth-shaped stored on the
/// object would be wrong the moment the same agent appeared at two depths.
///
/// Checking membership catches indirect cycles — A delegates to B, B is
/// configured to delegate to A — which a config-time self-reference check
/// cannot see, because neither entry is wrong on its own.
pub fn refuse_delegation(chain: &[String], agent_id: &str) -> Option<DelegationRefusal> {
    if chain.iter().any(|id| id == agent_id) {
        return Some(DelegationRefusal::Cycle);
    }
    if chain.len() >= MAX_SUBAGENT_DEPTH {
        return Some(DelegationRefusal::TooDeep);
    }
    None
}

/// What the model reads. Phrased to stop a retry, as `denied_tool_result` is.
fn refusal_text(refusal: DelegationRefusal, binding: &SubagentBinding, chain: &[String]) -> String {
    let label = &binding.label;
    match refusal {
        DelegationRefusal::Unconfigured => format!(
            "Cannot delegate to \"{label}\": that agent has no provider or model configured, so \
             it cannot run. Do not call it again — do the work yourself, or tell the user their \
             \"{}\" agent needs setting up.",
            binding.agent_id
        ),
        DelegationRefusal::Cycle => {
            let mut path: Vec<&str> = chain.iter().map(String::as_str).collect();
            path.push(&binding.agent_id);
            format!(
                "Cannot delegate to \"{label}\": it is already running above this call ({}). Do \
                 not call it again — finish the work here.",
                path.join(" → ")
            )
        }
        DelegationRefusal::TooDeep => format!(
            "Cannot delegate to \"{label}\": delegation is {MAX_SUBAGENT_DEPTH} levels deep \
             already ({}). Do not call it again — do the work yourself, or return what you have.",
            chain.join(" → ")
        ),
    }
}

/// A delegation that never started, as the one `tool` message it still owes.
pub fn refused_execution(
    refusal: DelegationRefusal,
    binding: &SubagentBinding,
    chain: &[String],
) -> ToolExecution {
    let kind = if refusal == DelegationRefusal::Unconfigured {
        ErrorKind::Config
    } else {
        ErrorKind::PermissionDenied
    };
    let mut execution = ToolExecution::error(kind, refusal_text(refusal, binding, chain));
    execution.name.clone_from(&binding.tool_name);
    execution
}

/// What a delegation returns when the subagent ran to the end and said nothing.
const EMPTY_SUBAGENT_RESULT: &str =
    "The subagent finished without writing an answer. Treat it as having found nothing.";

/// The subagent's turn, as the delegating model's tool result.
///
/// The child's final text and nothing else. Not its tool calls, not its
/// reasoning, not a transcript — the entire point of delegating is that the
/// detour does not land in the caller's context window, and a result that
/// summarised the run would put a smaller version of it there anyway. What the
/// subagent did is on screen, and in its own session; what the model gets is
/// the answer.
///
/// A stop reason other than `complete` is still an answer, and is reported as
/// one with a line saying it was cut short — a subagent that hit its iteration
/// cap has usually found most of what was asked, and throwing that away to
/// report a failure serves nobody. Only `error` is a failed call.
///
/// **"Nothing" and "cut off with nothing" are different results**, and the
/// distinction is the whole reason the empty case is spelled out separately:
/// "it finished and found nothing" tells a model to stop looking, and reporting
/// that for a delegation the timeout killed is a lie it acts on.
pub fn subagent_result(
    binding: &SubagentBinding,
    text: &str,
    stop_reason: StopReason,
    duration_ms: u64,
) -> ToolExecution {
    let text = text.trim();
    let failed = stop_reason == StopReason::Error;
    let cut = !failed && stop_reason != StopReason::Complete;
    let label = &binding.label;
    let reason = stop_reason_word(stop_reason);

    let content = if cut {
        if text.is_empty() {
            format!(
                "The {label} agent stopped early ({reason}) without writing an answer. It did not \
                 finish — this is not a finding."
            )
        } else {
            format!("{text}\n\n(The {label} agent stopped early: {reason}.)")
        }
    } else if text.is_empty() {
        EMPTY_SUBAGENT_RESULT.to_owned()
    } else {
        text.to_owned()
    };

    let mut execution = if failed {
        ToolExecution::error(ErrorKind::Tool, content)
    } else {
        ToolExecution::ok(content)
    };
    execution.name.clone_from(&binding.tool_name);
    execution.duration_ms = duration_ms;
    execution
}

/// Builds the binding map a loop is constructed with.
///
/// Insertion-ordered, because the order the operator configured the subagents
/// in is the order the model is shown them in.
///
/// Refuses a duplicate tool name rather than letting the later entry win: two
/// subagents resolving to one name is an operator mistake that would otherwise
/// present as one of them silently never being callable.
pub fn subagent_map(
    bindings: impl IntoIterator<Item = SubagentBinding>,
) -> Result<IndexMap<String, SubagentBinding>> {
    let mut map: IndexMap<String, SubagentBinding> = IndexMap::new();
    for binding in bindings {
        if map.contains_key(&binding.tool_name) {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Two subagents resolve to the tool \"{}\"",
                    binding.tool_name
                ),
            )
            .with_detail("toolName", binding.tool_name)
            .with_detail("agentId", binding.agent_id));
        }
        map.insert(binding.tool_name.clone(), binding);
    }
    Ok(map)
}
