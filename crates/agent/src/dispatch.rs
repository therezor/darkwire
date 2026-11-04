//! The tool half of a turn: authorise, run, answer.
//!
//! `agent_loop` owns the turn: the prompt, the provider, the stream, the caps.
//! It hands each assistant message that asked for tools to
//! [`AgentLoop::dispatch`], which returns the messages the turn should append;
//! the events the operator sees go out through the sink as they happen.
//!
//! Three invariants live here, because this is the only place that can hold
//! them:
//!
//!  - **Every tool call gets a `tool` message.** Providers reject an
//!    `assistant` turn whose tool calls were never answered, so a call that was
//!    cancelled, denied, or arrived on an agent with tools switched off still
//!    produces a result. That is also why `dispatch` returns the whole batch:
//!    one append upstairs, never a partial write.
//!  - **Permission is checked between the `tool.call` event and execution**,
//!    the one place every transport passes through.
//!  - **Adjacent read-only calls run together; everything else runs in order.**
//!    `read, read, write, read` becomes `[read‖read]`, `write`, `read`, so a
//!    write is never reordered past a read. A delegation and a call that would
//!    prompt are both excluded.
//!
//! **This is the one place a result is truncated and fenced.** Every result, a
//! real execution, a denial, a cancellation or a delegation, takes the same two
//! steps in the same order, so the truncate-then-wrap order cannot differ
//! between paths.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::history::truncate_head_tail;
use darkwire_core::messages::{ToolOptions, tool_message};
use darkwire_core::{Clock, ErrorKind, Result};
use darkwire_protocol::{
    AgentEnvironment, AgentSettings, ChatMessage, Notice, NoticeKind, ToolApprovalRequest,
    ToolCall, ToolCallStarted, ToolPermission, ToolPermissions, ToolResult, ToolRisk,
};
use darkwire_providers::{BoxFuture, ChatResult};
use darkwire_security::{WrapToolOutputOptions, describe_injection_findings, wrap_tool_output};
use darkwire_tools::{ToolContext, ToolExecution, ToolInvocation, permission_for};
use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use indexmap::IndexMap;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::AgentLoop;
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
/// this exists for, a build under `exec` or a slow MCP server, produce no
/// output until they finish, so the loop is the only thing that can say.
pub const TOOL_HEARTBEAT_MS: u64 = 15_000;

/// How many read-only calls may be in flight at once.
///
/// A bound rather than a tuning knob: the calls are read-only, so there is no
/// contention to tune against. What it stops is a model asking for two hundred
/// files at once and opening two hundred file handles. Eight, because the
/// batches models emit are three to six.
pub const MAX_PARALLEL_TOOL_CALLS: usize = 8;

/// What a cancelled call records, so the `assistant` turn stays answered.
pub const CANCELLED_TOOL_RESULT: &str =
    "Cancelled: the turn was stopped before this tool finished.";

/// The model's arguments, as the UI should see them.
///
/// Best-effort on purpose: malformed JSON from a model is common enough that it
/// must not break the event stream, and the registry is what turns it into a
/// typed tool error the model can recover from. Here it is only displayed.
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
/// `config` rather than `permission_denied`: nothing was denied, the model was
/// never sent a tool list. The wording says the tool did not run, because the
/// other reading, that it ran and the output was lost, makes a model retry.
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
    /// A remembered answer is one a person gave, so a refusal is `Declined`.
    fn of(decision: &ApprovalDecision) -> ApprovalOutcome {
        if decision.approved {
            ApprovalOutcome::Approved
        } else {
            ApprovalOutcome::Denied(DenialReason::Declined)
        }
    }
}

/// What an agent permits now, tool by tool and delegation by delegation.
#[derive(Debug, Clone, Default)]
pub struct LivePermissions {
    /// Each tool's permission.
    pub tools: ToolPermissions,
    /// Each delegation's, keyed by the tool name the model calls it by.
    pub subagents: IndexMap<String, ToolPermission>,
}

/// The agent as its settings say now.
#[derive(Debug, Clone)]
pub struct LiveAgent {
    /// What a tool is handed as `ctx.config`.
    pub settings: AgentSettings,
    /// What it permits.
    pub permissions: LivePermissions,
}

/// Reads the agent before each tool call. `None` when it was deleted, switched
/// off or no longer builds, which stops the turn.
pub type LiveAgentSource = Arc<dyn Fn() -> Option<LiveAgent> + Send + Sync>;

/// What the tool half of a turn needs from the half above it.
///
/// `dispatch` and the loop's delegation must agree about it, and a subagent
/// needs the workspace and the chain that authorisation does not.
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
    /// Not derived from `tool_context.placement`: a delegation that inherits
    /// hands this down whole, and "nobody decided" and "the host" must stay two
    /// values.
    pub environment: AgentEnvironment,
    /// The conversation a person is watching. See [`ApprovalRequest`].
    pub root_session_key: String,
    /// Permissions as of the last refresh. `None` is the loop's own.
    pub permissions: Option<Arc<LivePermissions>>,
}

/// What one assistant turn's tool calls produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallOutcome {
    /// Whether the turn was stopped while the batch ran.
    pub cancelled: bool,
    /// Whether the agent stopped resolving while the batch ran.
    pub agent_gone: bool,
    /// The assistant message and one `tool` message per call, in the order the
    /// model asked. The caller appends them in one transaction, because a
    /// partial write is an orphaned tool result on every later request.
    pub pending: Vec<ChatMessage>,
}

/// A delegation's permission now. One removed since the turn began is refused.
fn delegation_permission(binding: &SubagentBinding, turn: &TurnScope) -> ToolPermission {
    match &turn.permissions {
        Some(live) if binding.permission != ToolPermission::Deny => live
            .subagents
            .get(&binding.tool_name)
            .copied()
            .unwrap_or(ToolPermission::Deny),
        _ => binding.permission,
    }
}

impl AgentLoop {
    /// A rule or permission saved while the turn runs decides the next call,
    /// not the next turn. `false` when the agent no longer resolves.
    fn refresh(&self, turn: &mut TurnScope) -> bool {
        let Some(read) = self.inner.live.as_ref() else {
            return true;
        };
        let Some(current) = read() else {
            return false;
        };
        turn.tool_context.config = Arc::new(current.settings);
        turn.permissions = Some(Arc::new(current.permissions));
        true
    }

    /// The permission `name` has now. A tool the turn was not offered stays
    /// off, because the model was never sent it.
    fn permission_of(&self, name: &str, turn: &TurnScope) -> ToolPermission {
        let offered = self.inner.tools.permission_for(name);
        match &turn.permissions {
            Some(live) if offered != ToolPermission::Deny => {
                permission_for(Some(&live.tools), name)
            }
            _ => offered,
        }
    }

    /// The binding for a call, or `None` when a registered tool wins.
    ///
    /// The same precedence the advertised definitions apply, so a shadowed
    /// subagent is neither advertised nor reachable by a lucky guess.
    fn subagent_for(&self, name: &str) -> Option<&SubagentBinding> {
        let binding = self.inner.subagents.get(name)?;
        if self.inner.tools.get(name).is_some() {
            None
        } else {
            Some(binding)
        }
    }

    fn risk_of(&self, name: &str) -> ToolRisk {
        self.inner
            .tools
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
    /// Returns the messages to append rather than appending them, so "the
    /// assistant message and all of its results land in one transaction" is a
    /// property of the shape. Once cancelled, or once the agent is gone, the
    /// remaining calls are not executed, but each still gets a result.
    pub(crate) async fn dispatch(
        &self,
        result: &ChatResult,
        turn: &mut TurnScope,
        sink: &EventSink,
    ) -> Result<ToolCallOutcome> {
        let mut pending: Vec<ChatMessage> = vec![ChatMessage::Assistant(result.message.clone())];
        let mut cancelled = turn.token.is_cancelled();
        let mut agent_gone = false;

        let calls = &result.message.tool_calls;
        let mut next = 0;
        while next < calls.len() {
            if !self.refresh(turn) {
                agent_gone = true;
                self.refuse_rest(&calls[next..], turn, sink, &mut pending)
                    .await;
                break;
            }
            // Grouped after the refresh, so a call that stopped being `allow`
            // leaves the group and is asked about on its own.
            let group = self.next_group(&calls[next..], turn);
            next += group.len();
            let turn = &*turn;
            // Stop reaches a delegation as a successful "stopped early", so
            // the token is the only signal for the calls after it.
            cancelled = cancelled || turn.token.is_cancelled();

            if let [solo] = group.as_slice() {
                sink.emit(self.call_event(solo, &turn.turn_id)).await;
                let execution = if cancelled {
                    cancelled_execution(&solo.name)
                } else {
                    match self
                        .authorize(solo, self.risk_of(&solo.name), turn, sink)
                        .await
                    {
                        Some(refusal) => refusal,
                        None => match self.subagent_for(&solo.name) {
                            Some(binding) => self.run_subagent(solo, binding, turn, sink).await?,
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

            if cancelled {
                for call in &group {
                    sink.emit(self.call_event(call, &turn.turn_id)).await;
                    let execution = cancelled_execution(&call.name);
                    pending.push(self.finish(call, execution, turn, sink).await);
                }
                continue;
            }

            let (group_cancelled, messages) = self.run_parallel(&group, turn, sink).await;
            cancelled = cancelled || group_cancelled;
            pending.extend(messages);
        }

        Ok(ToolCallOutcome {
            cancelled,
            agent_gone,
            pending,
        })
    }

    /// Answers every call left in the batch once the agent is gone, and says
    /// why once.
    async fn refuse_rest(
        &self,
        calls: &[ToolCall],
        turn: &TurnScope,
        sink: &EventSink,
        pending: &mut Vec<ChatMessage>,
    ) {
        tracing::warn!(
            session_key = %turn.session_key,
            turn_id = %turn.turn_id,
            agent_id = %self.inner.agent_id,
            "agent no longer resolves, stopping the turn"
        );
        sink.emit(Notice {
            tag: darkwire_protocol::NoticeTag,
            kind: NoticeKind::AgentRemoved,
            message: format!(
                "Stopped: the agent \"{}\" was deleted, switched off or no longer builds, so \
                 nothing more ran.",
                self.inner.agent_id
            ),
            turn_id: Some(turn.turn_id.clone()),
            call_id: None,
        })
        .await;
        for call in calls {
            sink.emit(self.call_event(call, &turn.turn_id)).await;
            let execution = denied_execution(&call.name, DenialReason::AgentRemoved);
            pending.push(self.finish(call, execution, turn, sink).await);
        }
    }

    /// Turns one finished execution into its `tool` message and the events that
    /// report it. Truncate first, wrap second: the other order cuts the closing
    /// delimiter off the envelope.
    async fn finish(
        &self,
        call: &ToolCall,
        execution: ToolExecution,
        turn: &TurnScope,
        sink: &EventSink,
    ) -> ChatMessage {
        let budget =
            usize::try_from(turn.tool_context.config.max_output_chars).unwrap_or(usize::MAX);
        let truncation = truncate_head_tail(&execution.content, budget);
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
            // A nonce this turn cannot fence with is a defect upstream. The
            // call still answers, with the content rather than with nothing.
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
                // The figure the `tool.result` event carried, so a reloaded
                // card says what the live one said.
                duration_ms: Some(execution.duration_ms),
            },
        ))
    }

    /// Whether this call may run beside its neighbours.
    ///
    /// `risk == Safe` already means "read-only, as declared by the tool or its
    /// server", so it is reused rather than adding a second field. The lookup
    /// is the scope's `get`, not `risk_of`, which answers `Safe` for a name it
    /// cannot resolve. `Allow` is required, which makes an approval prompt
    /// impossible inside a group.
    fn is_parallel_eligible(&self, call: &ToolCall, turn: &TurnScope) -> bool {
        if !self.inner.config.tools_enabled {
            return false;
        }
        // A delegation is a whole turn on another loop, not a tool call.
        if self.subagent_for(&call.name).is_some() {
            return false;
        }
        if self.inner.tools.get(&call.name).map(|tool| tool.risk()) != Some(ToolRisk::Safe) {
            return false;
        }
        self.permission_of(&call.name, turn) == ToolPermission::Allow
    }

    /// The run at the front of `calls` that may execute together, or the
    /// first call alone. Adjacent runs only, so a write is never reordered past
    /// a read.
    fn next_group(&self, calls: &[ToolCall], turn: &TurnScope) -> Vec<ToolCall> {
        let run: Vec<ToolCall> = calls
            .iter()
            .take(MAX_PARALLEL_TOOL_CALLS)
            .take_while(|call| self.is_parallel_eligible(call, turn))
            .cloned()
            .collect();
        if run.is_empty() {
            calls.iter().take(1).cloned().collect()
        } else {
            run
        }
    }

    /// One group, in flight together.
    ///
    /// One heartbeat for the group. Results are reported as they land, so a
    /// fast read does not spin until the slowest finishes, and the messages
    /// come back in the order the model asked, because a `tool` message that
    /// does not follow its call is a provider 400.
    async fn run_parallel(
        &self,
        group: &[ToolCall],
        turn: &TurnScope,
        sink: &EventSink,
    ) -> (bool, Vec<ChatMessage>) {
        for call in group {
            sink.emit(self.call_event(call, &turn.turn_id)).await;
        }

        let heartbeat_ms = self.inner.tool_heartbeat_ms;
        let clock = &*self.inner.clock;
        let started = clock.monotonic();
        let mut running: FuturesUnordered<BoxFuture<'_, (usize, ToolExecution)>> = group
            .iter()
            .enumerate()
            .map(|(index, call)| {
                let scope = Arc::clone(&self.inner.tools);
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
            let settled = if heartbeat_ms == 0 {
                running.next().await
            } else {
                let beat = tokio::time::sleep(Duration::from_millis(heartbeat_ms));
                tokio::select! {
                    settled = running.next() => settled,
                    () = beat => {
                        let elapsed_ms = elapsed_ms(clock, started);
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

    /// Whether this call may run, and if not, the result that says so.
    ///
    /// `None` means proceed. An abort during an approval is a cancellation,
    /// not a denial: a denial lets the model respond, a cancellation stops the
    /// turn.
    async fn authorize(
        &self,
        call: &ToolCall,
        risk: ToolRisk,
        turn: &TurnScope,
        sink: &EventSink,
    ) -> Option<ToolExecution> {
        // Not expressed as a permission: the agent's map still says `allow`,
        // and the request carried no tools, so this is a name the model made up.
        if !self.inner.config.tools_enabled {
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

        let binding = self.subagent_for(&call.name);
        let fallback = binding.map_or_else(
            || self.permission_of(&call.name, turn),
            |b| delegation_permission(b, turn),
        );
        // A denied tool is not in the definitions the model was sent, so this
        // means something advertised a tool this scope does not permit.
        if fallback == ToolPermission::Deny {
            return self
                .refuse(call, risk, fallback, DenialReason::Policy, turn, sink)
                .await;
        }

        // A tool whose permission depends on its arguments answers here. Asked
        // only once the tool is enabled: a rule can narrow or widen `ask`, never
        // switch a tool on.
        let args = parse_tool_args(&call.arguments_json);
        let policy = if binding.is_some() {
            None
        } else {
            self.inner
                .tools
                .get(&call.name)
                .and_then(|tool| tool.policy(&args, &turn.tool_context, fallback))
        };
        let permission = policy.as_ref().map_or(fallback, |policy| policy.permission);
        if permission == ToolPermission::Allow {
            return None;
        }
        if permission == ToolPermission::Deny {
            return self
                .refuse(call, risk, permission, DenialReason::Rule, turn, sink)
                .await;
        }

        let denial = {
            // No gate is the process saying `ask` runs unasked (`chat --yes`).
            let gate = self.inner.approvals.as_ref()?;

            let timeout_ms = turn.tool_context.config.approval_timeout_ms;
            let expires_at_ms = u64::try_from(self.inner.clock.now_ms()).unwrap_or(0) + timeout_ms;
            let (memory_key, command) = match policy {
                Some(policy) => (policy.memory_key, policy.command),
                None => (call.name.clone(), None),
            };
            let request = ApprovalRequest {
                session_key: turn.session_key.clone(),
                root_session_key: turn.root_session_key.clone(),
                agent_id: self.inner.agent_id.clone(),
                turn_id: turn.turn_id.clone(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                args: args.clone(),
                risk,
                memory_key,
                command: command.clone(),
                expires_at_ms,
                token: turn.token.clone(),
            };

            // Asked before the event goes out: a remembered answer must not
            // flash a prompt on every client first.
            let outcome = if gate.cannot_ask(&request) {
                ApprovalOutcome::Denied(DenialReason::NoPrompt)
            } else if let Some(decision) = gate.remembered(&request) {
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
                    command,
                })
                .await;
                decide(gate.as_ref(), &request, timeout_ms).await
            };

            match outcome {
                ApprovalOutcome::Approved => return None,
                ApprovalOutcome::Aborted => return Some(cancelled_execution(&call.name)),
                ApprovalOutcome::Denied(reason) => reason,
            }
        };
        self.refuse(call, risk, permission, denial, turn, sink)
            .await
    }

    /// Records a refusal and answers the call with it.
    async fn refuse(
        &self,
        call: &ToolCall,
        risk: ToolRisk,
        permission: ToolPermission,
        denial: DenialReason,
        turn: &TurnScope,
        sink: &EventSink,
    ) -> Option<ToolExecution> {
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

    /// One tool call, with a liveness event on a fixed cadence while it runs.
    ///
    /// The heartbeat races the call on tokio's timer, so a test advances a
    /// paused clock. The per-call timeout is the registry's alone.
    async fn execute_with_heartbeat(
        &self,
        call: &ToolCall,
        turn: &TurnScope,
        sink: &EventSink,
    ) -> ToolExecution {
        let clock = &*self.inner.clock;
        let started = clock.monotonic();
        let invocation = invocation_of(call);
        let mut running = Box::pin(self.inner.tools.execute(&invocation, &turn.tool_context));

        let heartbeat_ms = self.inner.tool_heartbeat_ms;
        if heartbeat_ms == 0 {
            return running.await;
        }

        loop {
            let beat = tokio::time::sleep(Duration::from_millis(heartbeat_ms));
            tokio::select! {
                execution = &mut running => return execution,
                () = beat => {
                    let elapsed_ms = elapsed_ms(clock, started);
                    sink.emit(progress(&turn.turn_id, call, elapsed_ms)).await;
                }
            }
        }
    }
}

/// Waits for a decision, a deadline, or the turn ending, whichever is first.
///
/// The deadline is enforced here because the case it exists for is a gate
/// that never answers: a tab closed on an open prompt. A gate that fails
/// denies; no failure of an approval mechanism safely reads as "go ahead".
async fn decide(
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
            // The gate's own deadline, reached first, is still nobody answering.
            Err(error) if error.kind == ErrorKind::Timeout => {
                ApprovalOutcome::Denied(DenialReason::Timeout)
            }
            Err(error) => {
                tracing::error!(tool = %request.name, err = %error.message, "approval gate failed");
                ApprovalOutcome::Denied(DenialReason::Declined)
            }
        },
        () = deadline => ApprovalOutcome::Denied(DenialReason::Timeout),
        () = request.token.cancelled() => ApprovalOutcome::Aborted,
    }
}

fn invocation_of(call: &ToolCall) -> ToolInvocation {
    ToolInvocation {
        name: call.name.clone(),
        arguments_json: Some(call.arguments_json.clone()),
    }
}

/// Whole milliseconds, because the protocol says an integer and a client that
/// validates its frames would drop a fractional one.
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
