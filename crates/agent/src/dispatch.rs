//! The tool half of a turn: authorise, run, answer.
//!
//! `agent_loop` owns the turn — the prompt, the provider, the stream, the caps
//! — and hands each assistant message that asked for tools to
//! [`ToolDispatcher::dispatch`]. What comes back is the messages the turn
//! should append; the events the operator sees go out through the sink as they
//! happen. The store is not this file's to write.
//!
//! Three invariants live here rather than upstairs, because this is the only
//! place that can hold them:
//!
//!  - **Every tool call gets a `tool` message.** Providers reject an
//!    `assistant` turn whose tool calls were never answered, so a call that was
//!    cancelled, denied, or arrived on an agent with tools switched off still
//!    produces a result — no execution, still an answer. Stopping mid-tool
//!    without writing one would make the *next* turn fail on history the user
//!    cannot see, long after the stop that caused it. That is also why
//!    `dispatch` returns the whole batch: one append upstairs, never a partial
//!    write.
//!  - **Permission is checked between the `tool.call` event and execution**,
//!    which is the only place it can be checked once for every transport. A
//!    transport that gated it for itself would be one `if` away from an ungated
//!    one. The answer comes from the scope, because the scope is what knows
//!    whether a name resolved to a built-in or to a program in this agent's
//!    container. What this file decides is whether to ask; what the answer is,
//!    and how long it holds, belong to the gate.
//!  - **Adjacent read-only calls run together; everything else runs in order.**
//!    A model asks for six files in one message; fetching them one after
//!    another is the shape of the loop, not a requirement. Grouping is
//!    *adjacent* runs only, which is the safety property rather than a
//!    simplification: `read, read, write, read` becomes `[read‖read]`, `write`,
//!    `read`, so a write is never reordered past a read. A delegation and a
//!    call that would prompt are both excluded.
//!
//! A subagent call is authorised here too, on the same path, and then handed
//! back to the loop through [`SubagentDelegate`] — delegation needs the loop
//! resolver, the store and the lineage, none of which belong to a dispatcher.
//!
//! **This file is the one place a result is truncated and fenced**, and the
//! tool layer is deliberately left to do neither: the turn's context carries no
//! nonce, so the registry returns plain content and every result — a real
//! execution, a denial, a cancellation, a refused delegation — passes through
//! the same two steps in the same order here. Splitting them would mean the
//! synthesised results took a second path, and getting the truncate-then-wrap
//! order wrong on that path fails silently.
//!
//! Nothing here falls back to a default. Every collaborator is resolved by the
//! loop and passed in, so there is no branch in this file that a turn does not
//! take.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::history::truncate_head_tail;
use darkwire_core::messages::{ToolOptions, tool_message};
use darkwire_core::{Clock, ErrorKind, Result};
use darkwire_protocol::{
    AgentEnvironment, ChatMessage, Notice, NoticeKind, ToolApprovalRequest, ToolCall,
    ToolCallStarted, ToolPermission, ToolResult, ToolRisk,
};
use darkwire_providers::{BoxFuture, ChatResult};
use darkwire_security::{WrapToolOutputOptions, describe_injection_findings, wrap_tool_output};
use darkwire_tools::{ToolContext, ToolExecution, ToolInvocation, ToolScope};
use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use indexmap::IndexMap;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::approval::{
    ApprovalDecision, ApprovalGate, ApprovalRequest, DenialReason, denied_notice,
    denied_tool_result,
};
use crate::events::{AgentEvent, EventSink};
use crate::subagent::SubagentBinding;

/// How often a running tool reports that it is still running.
///
/// Long enough that a normal tool call never emits one, short enough that a UI
/// showing a spinner is never left guessing whether the process died. The tools
/// this exists for — a build under `exec`, a slow MCP server — produce no
/// output at all until they finish, so the loop is the only thing that can say.
pub const TOOL_HEARTBEAT_MS: u64 = 15_000;

/// How many read-only calls may be in flight at once.
///
/// A bound rather than a tuning knob, which is why it is here and not in
/// `ToolsConfig`: the calls it governs are read-only by definition, so there is
/// no contention story an operator would need to tune against — no GPU, no
/// shared container, no lock. What it stops is a model asking for two hundred
/// files at once and opening two hundred file handles to answer.
///
/// Eight, because the batches models actually emit are three to six.
pub const MAX_PARALLEL_TOOL_CALLS: usize = 8;

/// What a cancelled call records, so the `assistant` turn stays answered.
pub const CANCELLED_TOOL_RESULT: &str =
    "Cancelled: the turn was stopped before this tool finished.";

/// The model's arguments, as the UI should see them.
///
/// Parsing is best-effort on purpose: malformed JSON from a model is common
/// enough that it must not break the event stream, and the registry is the
/// thing that turns it into a typed tool error the model can recover from. Here
/// it is only being displayed.
pub fn parse_tool_args(arguments_json: &str) -> Value {
    if arguments_json.trim().is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    serde_json::from_str(arguments_json)
        .unwrap_or_else(|_| Value::String(arguments_json.to_owned()))
}

fn named(name: &str, mut execution: ToolExecution) -> ToolExecution {
    name.clone_into(&mut execution.name);
    execution
}

fn cancelled_execution(name: &str) -> ToolExecution {
    named(
        name,
        ToolExecution::error(ErrorKind::Aborted, CANCELLED_TOOL_RESULT),
    )
}

/// A call that arrived on an agent whose tools are switched off.
///
/// `config` rather than `permission_denied`: nothing was denied. The agent's
/// permission map is untouched and still says `allow`; this model was simply
/// never sent a tool list, so the call is one it invented. The wording says the
/// tool did not run, because the alternative reading — that it ran and the
/// output was lost — is the one that has a model retry the same command.
fn tools_disabled_execution(name: &str) -> ToolExecution {
    named(
        name,
        ToolExecution::error(
            ErrorKind::Config,
            format!(
                "Refused: tool calling is switched off for this model, so \"{name}\" did not run \
                 and nothing happened. No tools are available on this turn. Answer from the \
                 conversation, or tell the user what you would need to do and why you cannot."
            ),
        ),
    )
}

/// A call that was refused. `duration_ms` stays zero because nothing ran.
fn denied_execution(name: &str, reason: DenialReason) -> ToolExecution {
    named(
        name,
        ToolExecution::error(
            ErrorKind::PermissionDenied,
            denied_tool_result(name, reason),
        ),
    )
}

/// How an awaited approval ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalOutcome {
    Approved,
    Aborted,
    Denied(DenialReason),
}

impl ApprovalOutcome {
    /// What a decision the gate handed over without being awaited means.
    ///
    /// A refusal is `Declined` rather than `Policy`: a remembered answer is one
    /// a person gave, and the model is told the difference.
    fn of(decision: &ApprovalDecision) -> ApprovalOutcome {
        if decision.approved {
            ApprovalOutcome::Approved
        } else {
            ApprovalOutcome::Denied(DenialReason::Declined)
        }
    }
}

/// What the tool half of a turn needs from the half above it.
///
/// Named rather than inlined because it grew past the point where one inline
/// shape in two signatures is one shape: `dispatch` and the loop's delegation
/// must agree about it, and a subagent needs the workspace and the chain that
/// authorisation does not.
#[derive(Debug, Clone)]
pub struct TurnScope {
    /// The session the turn runs in.
    pub session_key: String,
    /// The turn.
    pub turn_id: String,
    /// This turn's tool-output nonce, computed once.
    pub nonce: String,
    /// The turn's cancellation, already a child of the caller's.
    pub token: CancellationToken,
    /// What a tool is handed when it runs.
    pub tool_context: ToolContext,
    /// The session's, so a subagent works in the folder its caller does.
    pub workspace_id: String,
    /// Ancestor agent ids, oldest first. See `refuse_delegation`.
    pub chain: Vec<String>,
    /// The environment and egress this turn settled on.
    ///
    /// Here rather than derived from `tool_context.placement` because a
    /// delegation that inherits hands this down whole, and a caller on the host
    /// must hand down the host. Reading it back out of an `Option` would make
    /// "nobody decided" and "the host" the same value again, which is the
    /// overload this field exists to end.
    pub environment: AgentEnvironment,
    /// The conversation a person is watching. See [`ApprovalRequest`].
    pub root_session_key: String,
}

/// One delegated task, run by the loop.
///
/// Delegation stays in the loop because a subagent's turn is a real turn on a
/// real loop — it needs the loop resolver, the store and the lineage. The
/// dispatcher only needs to know that a call may be answered by one.
///
/// Passed to [`ToolDispatcher::dispatch`] rather than held on the dispatcher,
/// because the loop owns the dispatcher and a delegate stored here would be a
/// reference cycle back into it.
pub trait SubagentDelegate: Send + Sync {
    /// Runs one delegation to completion, emitting the child's events on the
    /// way past.
    fn delegate<'a>(
        &'a self,
        call: &'a ToolCall,
        binding: &'a SubagentBinding,
        turn: &'a TurnScope,
        sink: &'a EventSink,
    ) -> BoxFuture<'a, Result<ToolExecution>>;
}

/// A delegate for a loop that has no subagents.
///
/// Reachable only if a binding exists without a resolver, which the loop's own
/// construction rules out; it exists so `dispatch` can be called without one.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDelegation;

impl SubagentDelegate for NoDelegation {
    fn delegate<'a>(
        &'a self,
        call: &'a ToolCall,
        binding: &'a SubagentBinding,
        _turn: &'a TurnScope,
        _sink: &'a EventSink,
    ) -> BoxFuture<'a, Result<ToolExecution>> {
        Box::pin(async move {
            Ok(crate::subagent::refused_execution(
                crate::subagent::DelegationRefusal::Unconfigured,
                binding,
                &[],
            ))
            .map(|execution| named(&call.name, execution))
        })
    }
}

/// What one assistant turn's tool calls produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallOutcome {
    /// Whether the turn was stopped while the batch ran.
    pub cancelled: bool,
    /// The assistant message and one `tool` message per call, in the order the
    /// model asked. The caller appends them — in one transaction, because a
    /// partial write is exactly the orphaned tool result the history walker
    /// then has to repair on every later request.
    pub pending: Vec<ChatMessage>,
}

/// Everything a dispatcher is built from. The loop resolves every default.
pub struct ToolDispatcherOptions {
    /// What this loop may call.
    pub tools: Arc<dyn ToolScope>,
    /// The agents this one may delegate to, keyed by tool name.
    pub subagents: IndexMap<String, SubagentBinding>,
    /// Who to ask before an `ask` tool runs. `None` means nobody is there.
    pub approvals: Option<Arc<dyn ApprovalGate>>,
    /// The tool layer's configuration; the approval deadline is read from it.
    /// How long an `ask` waits for an answer before counting as denied.
    pub approval_timeout_ms: u64,
    /// Whether the request advertises any tools at all.
    pub tools_enabled: bool,
    /// Head+tail budget for one tool result before it enters history.
    pub max_tool_result_chars: usize,
    /// `0` disables the heartbeat.
    pub heartbeat_ms: u64,
    /// Which agent is asking, for the approval request.
    pub agent_id: String,
    /// Wall-clock and monotonic time.
    pub clock: Arc<dyn Clock>,
}

/// Runs the tools one assistant turn asked for.
pub struct ToolDispatcher {
    tools: Arc<dyn ToolScope>,
    subagents: IndexMap<String, SubagentBinding>,
    approvals: Option<Arc<dyn ApprovalGate>>,
    approval_timeout_ms: u64,
    tools_enabled: bool,
    max_tool_result_chars: usize,
    heartbeat_ms: u64,
    agent_id: String,
    clock: Arc<dyn Clock>,
}

impl ToolDispatcher {
    /// A dispatcher over already-resolved collaborators.
    pub fn new(options: ToolDispatcherOptions) -> ToolDispatcher {
        ToolDispatcher {
            tools: options.tools,
            subagents: options.subagents,
            approvals: options.approvals,
            approval_timeout_ms: options.approval_timeout_ms,
            tools_enabled: options.tools_enabled,
            max_tool_result_chars: options.max_tool_result_chars,
            heartbeat_ms: options.heartbeat_ms,
            agent_id: options.agent_id,
            clock: options.clock,
        }
    }

    /// The binding for a call, or `None` when a registered tool wins.
    fn subagent_for(&self, name: &str) -> Option<&SubagentBinding> {
        let binding = self.subagents.get(name)?;
        // The same precedence the loop's advertised definitions apply, asked
        // the same way — so a shadowed subagent is not advertised *and* is not
        // reachable, rather than being invisible to the model and callable by a
        // lucky guess.
        if self.tools.get(name).is_some() {
            None
        } else {
            Some(binding)
        }
    }

    fn risk_of(&self, name: &str) -> ToolRisk {
        self.tools
            .get(name)
            .map_or(ToolRisk::Safe, |tool| tool.risk())
    }

    /// The `tool.call` event a call announces itself with.
    fn call_event(&self, call: &ToolCall, turn_id: &str) -> AgentEvent {
        ToolCallStarted {
            tag: darkwire_protocol::ToolCallTag,
            turn_id: turn_id.to_owned(),
            call_id: call.id.clone(),
            name: call.name.clone(),
            args: parse_tool_args(&call.arguments_json),
            risk: self.risk_of(&call.name),
        }
        .into()
    }

    /// Runs every tool the model asked for.
    ///
    /// Returns the messages to append rather than appending them: the store is
    /// the turn's to write, and handing back one list is what keeps "the
    /// assistant message and all of its results land in one transaction" a
    /// property of the shape rather than a rule to remember. Once cancelled,
    /// the remaining calls are not executed — but each still gets a result, so
    /// the `assistant` turn is never left with an unanswered tool call.
    pub async fn dispatch(
        &self,
        result: &ChatResult,
        turn: &TurnScope,
        sink: &EventSink,
        delegate: &dyn SubagentDelegate,
    ) -> Result<ToolCallOutcome> {
        let mut pending: Vec<ChatMessage> = vec![ChatMessage::Assistant(result.message.clone())];
        let mut cancelled = turn.token.is_cancelled();

        for group in self.group_calls(&result.message.tool_calls) {
            // A group of one is the whole of the path that existed before
            // parallelism, and it is the common case: no queue, nothing
            // allocated that a single call did not allocate before.
            if let [solo] = group.as_slice() {
                sink.emit(self.call_event(solo, &turn.turn_id)).await;

                let execution = if cancelled {
                    cancelled_execution(&solo.name)
                } else {
                    // Between the event and the execution, and nowhere else: a
                    // transport that gated it for itself would be one `if` away
                    // from an ungated one. A subagent is authorised here too —
                    // its binding carries a permission exactly as the scope
                    // carries a tool's, so `ask` gets the same prompt.
                    match self
                        .authorize(solo, self.risk_of(&solo.name), turn, sink)
                        .await
                    {
                        Some(refusal) => refusal,
                        None => match self.subagent_for(&solo.name) {
                            Some(binding) => delegate.delegate(solo, binding, turn, sink).await?,
                            None => self.execute_with_heartbeat(solo, turn, sink).await,
                        },
                    }
                };

                if execution.kind == Some(ErrorKind::Aborted) {
                    cancelled = true;
                }
                pending.push(self.finish(solo, execution, turn, sink).await);
                continue;
            }

            // Cancelled before the group started. Nothing runs, and every
            // member still gets its `tool` message — the same rule the
            // sequential path follows, applied to the whole run at once.
            if cancelled {
                for call in &group {
                    sink.emit(self.call_event(call, &turn.turn_id)).await;
                    let execution = cancelled_execution(&call.name);
                    pending.push(self.finish(call, execution, turn, sink).await);
                }
                continue;
            }

            let (group_cancelled, messages) = self.run_parallel(&group, turn, sink).await;
            if group_cancelled {
                cancelled = true;
            }
            pending.extend(messages);
        }

        Ok(ToolCallOutcome { cancelled, pending })
    }

    /// Turns one finished execution into its `tool` message and the events that
    /// report it.
    ///
    /// Shared by both paths so there is one place that truncates, wraps and
    /// reports. The alternative is two copies of the truncate-then-wrap order,
    /// and getting that order wrong in one of them fails silently.
    async fn finish(
        &self,
        call: &ToolCall,
        execution: ToolExecution,
        turn: &TurnScope,
        sink: &EventSink,
    ) -> ChatMessage {
        // Truncate first, wrap second. The other order cuts the closing
        // delimiter off the envelope, and a tool result the model cannot see
        // the end of is a tool result it reads as continuing into the
        // conversation.
        let truncation = truncate_head_tail(&execution.content, self.max_tool_result_chars);
        let truncated = truncation.truncated || execution.truncated;

        sink.emit(ToolResult {
            tag: darkwire_protocol::ToolResultTag,
            turn_id: turn.turn_id.clone(),
            call_id: call.id.clone(),
            ok: !execution.is_error,
            content: truncation.text.clone(),
            truncated,
            duration_ms: execution.duration_ms,
        })
        .await;

        let wrapped = wrap_tool_output(
            &truncation.text,
            &WrapToolOutputOptions::new(&call.name, &turn.nonce),
        );
        let content = match wrapped {
            Ok(wrapped) => {
                if !wrapped.findings.is_empty() {
                    let signals: Vec<&str> = wrapped
                        .findings
                        .iter()
                        .map(|finding| finding.signal.as_str())
                        .collect();
                    tracing::warn!(
                        tool = %call.name,
                        ?signals,
                        "prompt injection signals in tool output"
                    );
                    sink.emit(Notice {
                        tag: darkwire_protocol::NoticeTag,
                        kind: NoticeKind::PromptInjection,
                        message: describe_injection_findings(&wrapped.findings),
                        turn_id: Some(turn.turn_id.clone()),
                        call_id: Some(call.id.clone()),
                    })
                    .await;
                }
                wrapped.text
            }
            // A nonce this turn cannot fence with is a defect upstream, not
            // something the model can act on. The call still answers — an
            // unanswered tool call is a provider 400 on the next request —
            // and it answers with the content rather than with nothing.
            Err(error) => {
                tracing::error!(tool = %call.name, err = %error.message, "could not fence tool output");
                truncation.text.clone()
            }
        };

        ChatMessage::Tool(tool_message(
            &call.id,
            &call.name,
            content,
            ToolOptions {
                is_error: execution.is_error,
                truncated,
                // The same figure the `tool.result` event above carries. A
                // reader coming back to this session wants the card to say
                // what it said while the call was running, and nothing can
                // work out after the fact how long something took.
                duration_ms: Some(execution.duration_ms),
            },
        ))
    }

    /// Whether this call may run beside its neighbours.
    ///
    /// `risk == Safe` is the whole predicate, and reusing it rather than adding
    /// a second field is deliberate: it already means "read-only, as declared
    /// by the tool or its server", and an MCP server's read-only hint already
    /// sets that tool's *approval* bar — a strictly higher-stakes use of the
    /// same claim than setting a scheduling one. A second vocabulary for one
    /// fact is two things to keep in step.
    ///
    /// The lookup is the scope's `get`, not `risk_of`, because `risk_of`
    /// answers `Safe` for a name it cannot resolve. An invented name has to
    /// stay sequential and take its `not_found` on the ordinary path.
    ///
    /// `Allow` is required, and that is what makes an approval prompt
    /// impossible inside a group — so there is no question of whose prompt
    /// appears first, and no ordering to get wrong. A safe tool an operator set
    /// to `ask` simply runs on its own, which is right: if every read is
    /// prompted, the prompts are the latency.
    fn is_parallel_eligible(&self, call: &ToolCall) -> bool {
        if !self.tools_enabled {
            return false;
        }
        // A delegation is a whole turn on another loop, not a tool call. Two of
        // them at once is background agents, which this deliberately is not.
        if self.subagent_for(&call.name).is_some() {
            return false;
        }
        if self.tools.get(&call.name).map(|tool| tool.risk()) != Some(ToolRisk::Safe) {
            return false;
        }
        self.tools.permission_for(&call.name) == ToolPermission::Allow
    }

    /// Splits the batch into runs that may execute together.
    ///
    /// **Adjacent runs only.** See the header: a write must never be reordered
    /// past a read, and gathering every eligible call regardless of position
    /// would do exactly that.
    fn group_calls(&self, calls: &[ToolCall]) -> Vec<Vec<ToolCall>> {
        let mut groups: Vec<Vec<ToolCall>> = Vec::new();
        let mut run: Vec<ToolCall> = Vec::new();

        for call in calls {
            if !self.is_parallel_eligible(call) {
                if !run.is_empty() {
                    groups.push(std::mem::take(&mut run));
                }
                groups.push(vec![call.clone()]);
                continue;
            }
            run.push(call.clone());
            if run.len() == MAX_PARALLEL_TOOL_CALLS {
                groups.push(std::mem::take(&mut run));
            }
        }
        if !run.is_empty() {
            groups.push(run);
        }
        groups
    }

    /// One group, in flight together.
    ///
    /// **One heartbeat for the group, not one per call.** Authorisation and
    /// delegation are both excluded by `is_parallel_eligible`, so a group
    /// produces no concurrent event streams to interleave, only a liveness
    /// tick.
    ///
    /// **Results are reported as they land, not gathered at the end.** A card
    /// resolving on its own is the behaviour every renderer already shows for a
    /// delegation; waiting for the whole group would leave a fast read spinning
    /// until the slowest member finished.
    ///
    /// **The messages come back in the order the model asked**, whatever order
    /// they finished in, because the batch is a single append upstairs and a
    /// `tool` message that does not follow its tool call is a provider 400.
    async fn run_parallel(
        &self,
        group: &[ToolCall],
        turn: &TurnScope,
        sink: &EventSink,
    ) -> (bool, Vec<ChatMessage>) {
        for call in group {
            sink.emit(self.call_event(call, &turn.turn_id)).await;
        }

        let started = self.clock.monotonic();
        let mut running: FuturesUnordered<BoxFuture<'_, (usize, ToolExecution)>> = group
            .iter()
            .enumerate()
            .map(|(index, call)| {
                let scope = Arc::clone(&self.tools);
                let invocation = invocation_of(call);
                let context = turn.tool_context.clone();
                let future: BoxFuture<'_, (usize, ToolExecution)> =
                    Box::pin(async move { (index, scope.execute(&invocation, &context).await) });
                future
            })
            .collect();

        let mut in_flight: HashMap<usize, &ToolCall> = group.iter().enumerate().collect();
        let mut finished: Vec<(usize, ChatMessage)> = Vec::with_capacity(group.len());
        let mut cancelled = false;

        while !in_flight.is_empty() {
            let settled = if self.heartbeat_ms == 0 {
                running.next().await
            } else {
                let beat = tokio::time::sleep(Duration::from_millis(self.heartbeat_ms));
                tokio::select! {
                    settled = running.next() => settled,
                    () = beat => {
                        let elapsed_ms = elapsed_ms(&*self.clock, started);
                        // Every call still running, on one shared cadence.
                        let mut waiting: Vec<(usize, &ToolCall)> =
                            in_flight.iter().map(|(index, call)| (*index, *call)).collect();
                        waiting.sort_by_key(|(index, _)| *index);
                        for (_, call) in waiting {
                            sink.emit(progress(&turn.turn_id, call, elapsed_ms)).await;
                        }
                        continue;
                    }
                }
            };

            let Some((index, execution)) = settled else {
                break;
            };
            let Some(call) = in_flight.remove(&index) else {
                continue;
            };
            if execution.kind == Some(ErrorKind::Aborted) {
                cancelled = true;
            }
            finished.push((index, self.finish(call, execution, turn, sink).await));
        }

        finished.sort_by_key(|(index, _)| *index);
        (
            cancelled,
            finished.into_iter().map(|(_, message)| message).collect(),
        )
    }

    /// Whether this call may run — and, if not, the result that says so.
    ///
    /// `None` means proceed. Anything else is a [`ToolExecution`] that never
    /// executed, which is what keeps the "every tool call gets a `tool`
    /// message" rule true for a call the user refused: a denial the model
    /// cannot see is an unanswered tool call, and that is a provider 400 on the
    /// next turn rather than a refusal it can work around.
    ///
    /// An abort during an approval is a cancellation, not a denial. The
    /// difference matters to the caller: a denial lets the turn continue so the
    /// model can respond to it, while a cancellation stops the turn and the
    /// remaining calls.
    async fn authorize(
        &self,
        call: &ToolCall,
        risk: ToolRisk,
        turn: &TurnScope,
        sink: &EventSink,
    ) -> Option<ToolExecution> {
        // Before the permission lookup, and deliberately not expressed as one:
        // the agent's map is untouched and still says `allow`, so asking it
        // would run the call. The request carried no tools at all, which makes
        // anything arriving here a name the model invented — and this is the
        // one enforcement point every call passes through, including a
        // subagent's, so gating it here is what makes "nothing executes" true
        // rather than mostly true.
        //
        // A refusal rather than a silent drop, because every tool call must be
        // answered by a `tool` message: an unanswered one is a dangling call
        // the model waits on and a provider 400 on the next request.
        if !self.tools_enabled {
            tracing::warn!(
                session_key = %turn.session_key,
                turn_id = %turn.turn_id,
                tool = %call.name,
                risk = ?risk,
                "tool call refused: tools are switched off for this model"
            );
            sink.emit(Notice {
                tag: darkwire_protocol::NoticeTag,
                kind: NoticeKind::ToolsDisabled,
                message: format!(
                    "Refused \"{}\": tool calling is off for this model, so nothing ran.",
                    call.name
                ),
                turn_id: Some(turn.turn_id.clone()),
                call_id: Some(call.id.clone()),
            })
            .await;
            return Some(tools_disabled_execution(&call.name));
        }

        // The binding first, and only when the registry has no such name — the
        // same precedence `subagent_for` applies, asked once here so a shadowed
        // subagent is gated as the registered tool it actually is.
        let permission = self
            .subagent_for(&call.name)
            .map_or_else(|| self.tools.permission_for(&call.name), |b| b.permission);
        if permission == ToolPermission::Allow {
            return None;
        }

        let denial = if permission == ToolPermission::Deny {
            // Belt and braces. A denied tool is not in the definitions the
            // model was sent and `execute` would report it as `not_found`, so
            // reaching here means something advertised a tool this scope does
            // not permit — which is exactly the case an enforcement point
            // exists to catch.
            DenialReason::Policy
        } else {
            // `ask` with nobody to ask. Denying here would make the default
            // config refuse every `exec` in a terminal session, where the
            // operator asking for the command *is* the approval.
            let gate = self.approvals.as_ref()?;

            let timeout_ms = self.approval_timeout_ms;
            let expires_at_ms = u64::try_from(self.clock.now_ms()).unwrap_or(0) + timeout_ms;
            let args = parse_tool_args(&call.arguments_json);
            let request = ApprovalRequest {
                session_key: turn.session_key.clone(),
                root_session_key: turn.root_session_key.clone(),
                agent_id: self.agent_id.clone(),
                turn_id: turn.turn_id.clone(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                args: args.clone(),
                risk,
                expires_at_ms,
                token: turn.token.clone(),
            };

            // Asked before the event goes out, and the order is the whole
            // point. "This session" means the question stops being asked; a
            // prompt announced first would appear on every client and be
            // replaced in the same breath by the answer the gate already held.
            let outcome = if let Some(decision) = gate.remembered(&request) {
                ApprovalOutcome::of(&decision)
            } else {
                sink.emit(ToolApprovalRequest {
                    tag: darkwire_protocol::ToolApprovalRequestTag,
                    turn_id: turn.turn_id.clone(),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    args,
                    risk,
                    expires_at_ms,
                })
                .await;
                self.decide(gate.as_ref(), &request, timeout_ms).await
            };

            match outcome {
                ApprovalOutcome::Approved => return None,
                ApprovalOutcome::Aborted => return Some(cancelled_execution(&call.name)),
                ApprovalOutcome::Denied(reason) => reason,
            }
        };

        tracing::warn!(
            session_key = %turn.session_key,
            turn_id = %turn.turn_id,
            tool = %call.name,
            risk = ?risk,
            permission = ?permission,
            denial = ?denial,
            "tool call denied"
        );
        sink.emit(Notice {
            tag: darkwire_protocol::NoticeTag,
            kind: NoticeKind::ApprovalDenied,
            message: denied_notice(&call.name, denial),
            turn_id: Some(turn.turn_id.clone()),
            call_id: Some(call.id.clone()),
        })
        .await;
        Some(denied_execution(&call.name, denial))
    }

    /// Waits for a decision, a deadline, or the turn ending — whichever is
    /// first.
    ///
    /// The deadline is enforced here rather than left to the gate because the
    /// case it exists for is a gate that never answers: a browser tab closed on
    /// an open prompt, or a channel that has no way to render one. The timer is
    /// tokio's, so a test advances time instead of waiting five minutes.
    ///
    /// A gate that fails denies. There is no failure mode of an approval
    /// mechanism where the safe reading is "go ahead".
    async fn decide(
        &self,
        gate: &dyn ApprovalGate,
        request: &ApprovalRequest,
        timeout_ms: u64,
    ) -> ApprovalOutcome {
        let deadline = async {
            if timeout_ms == 0 {
                std::future::pending::<()>().await;
            } else {
                tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
            }
        };

        tokio::select! {
            answer = gate.ask(request) => match answer {
                Ok(decision) => {
                    tracing::info!(
                        tool = %request.name,
                        approved = decision.approved,
                        scope = ?decision.scope,
                        "approval decision"
                    );
                    if decision.approved {
                        ApprovalOutcome::Approved
                    } else {
                        ApprovalOutcome::Denied(DenialReason::Declined)
                    }
                }
                Err(error) if error.is_aborted() => ApprovalOutcome::Aborted,
                Err(error) => {
                    tracing::error!(tool = %request.name, err = %error.message, "approval gate failed");
                    ApprovalOutcome::Denied(DenialReason::Declined)
                }
            },
            () = deadline => ApprovalOutcome::Denied(DenialReason::Timeout),
            () = request.token.cancelled() => ApprovalOutcome::Aborted,
        }
    }

    /// One tool call, with a liveness event on a fixed cadence while it runs.
    ///
    /// The heartbeat is raced against the call on tokio's timer, so a test
    /// advances a paused clock instead of waiting 15 real seconds. The per-call
    /// timeout is not enforced here — the registry owns it, and owning it in
    /// two places is how a call ends up with two different deadlines.
    async fn execute_with_heartbeat(
        &self,
        call: &ToolCall,
        turn: &TurnScope,
        sink: &EventSink,
    ) -> ToolExecution {
        let started = self.clock.monotonic();
        let invocation = invocation_of(call);
        let mut running = Box::pin(self.tools.execute(&invocation, &turn.tool_context));

        if self.heartbeat_ms == 0 {
            return running.await;
        }

        loop {
            let beat = tokio::time::sleep(Duration::from_millis(self.heartbeat_ms));
            tokio::select! {
                execution = &mut running => return execution,
                () = beat => {
                    let elapsed_ms = elapsed_ms(&*self.clock, started);
                    sink.emit(progress(&turn.turn_id, call, elapsed_ms)).await;
                }
            }
        }
    }
}

fn invocation_of(call: &ToolCall) -> ToolInvocation {
    ToolInvocation {
        name: call.name.clone(),
        arguments_json: Some(call.arguments_json.clone()),
    }
}

/// Whole milliseconds, because a `tool.progress` frame *is* a `ServerMessage`
/// and the protocol says an integer. A client that validates its frames drops
/// the one that says the call finished, leaving a tool card spinning forever
/// over a tool that returned in a millisecond.
fn elapsed_ms(clock: &dyn Clock, started: Duration) -> u64 {
    u64::try_from(clock.monotonic().saturating_sub(started).as_millis()).unwrap_or(u64::MAX)
}

fn progress(turn_id: &str, call: &ToolCall, elapsed_ms: u64) -> AgentEvent {
    darkwire_protocol::ToolProgress {
        tag: darkwire_protocol::ToolProgressTag,
        turn_id: turn_id.to_owned(),
        call_id: call.id.clone(),
        elapsed_ms,
        message: Some(format!("{} is still running", call.name)),
    }
    .into()
}
