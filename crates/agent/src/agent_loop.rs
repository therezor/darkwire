//! The agent loop.
//!
//! One turn is: append what the user said, then repeat — assemble a request
//! from history, stream the model's answer, run whatever tools it asked for,
//! append the results — until the model answers without calling a tool, or a
//! cap stops it.
//!
//! [`AgentLoop::run`] spawns that work and hands back a [`Turn`]: a bounded
//! channel of events, a completion, and a guard. **Dropping the `Turn` cancels
//! the turn's token**, so abandoning the stream unwinds the turn through
//! exactly the path an explicit stop takes, and a consumer that stops reading
//! stops the turn rather than filling memory behind it.
//!
//! Everything here is either a cost decision or a correctness decision, and the
//! ones that look like details are the ones that matter:
//!
//!  - **The nonce and the tool registry entries are computed once per turn**, not
//!    per iteration. Both sit in the part of the prompt providers cache;
//!    regenerating them mid-turn would rewrite the prefix and throw the cache
//!    away five times over for no semantic change.
//!  - **The caps are checked at the top of the iteration.** Checking after the
//!    provider call lets a turn exceed its wall-clock cap by one full request
//!    plus its tool calls, which on a slow local model is minutes.
//!  - **Steering is drained before the caps are checked**, so a correction that
//!    arrives during the last legal iteration is still in history when that
//!    iteration builds its request.
//!  - **A steering message arriving while the model composes its final answer
//!    makes the loop carry on, not stop.** Ending the turn there discards the
//!    correction, and from the outside a discarded correction is
//!    indistinguishable from an ignored one.
//!  - **An error response is never appended to history.** A provider 400
//!    written into the transcript is replayed on every subsequent request in
//!    that session, so one malformed turn becomes a permanently poisoned
//!    session.
//!  - **The results of one assistant turn are appended in one transaction.** A
//!    partial write is an orphaned tool result, which the history walker then
//!    has to repair on every later request.
//!
//! Running the tools themselves lives in `dispatch` — authorisation, the
//! approval gate, the heartbeat, truncation and the envelope — along with the
//! two invariants that only that module can hold: every call gets an answer,
//! and permission is checked in exactly one place.
//!
//! The cancellation token threads from the caller through the provider request,
//! tool execution and any child process. There is one cancellation mechanism,
//! and this is it.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::{Stream, StreamExt as _};
use ghostai_core::history::HistoryOptions;
use ghostai_core::messages::Content;
use ghostai_core::messages::{AssistantOptions, assistant_message, system_message, user_message};
use ghostai_core::session_store::{AppendOptions, CreateSession, UpdateSession};
use ghostai_core::session_title::derive_session_title;
use ghostai_core::{
    Clock, ErrorKind, GhostError, Result, SessionRecord, SessionStore, SystemClock,
    TurnStatsRecord, text_of,
};
use ghostai_protocol::json::Object;
use ghostai_protocol::{
    AgentEnvironment, AgentSettings, AssistantDelta, ChatMessage, DEFAULT_AGENT_ID,
    DEFAULT_WORKSPACE_ID, ErrorCode, ErrorEvent, NoticeKind, ReasoningDelta, SUBAGENT_METADATA_KEY,
    SUBAGENT_ORIGIN, StopReason, SubagentLineage, SubagentRunRef, ToolDefinition,
    ToolPromptOverrides, ToolsConfig, Usage, apply_tool_prompts, with_subagent_run,
};
use ghostai_providers::{ChatProvider, ChatRequest, ChatResult, ChatStreamEvent, empty_usage};
use ghostai_security::{JailResolver, OsRandom, RandomSource, create_tool_output_nonce};
use ghostai_tools::{
    AutomationResolver, EnvironmentResolver, Placed, PlacementRequest, ToolContext, ToolScope,
};
use indexmap::IndexMap;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::approval::ApprovalGate;
use crate::attachments::{AttachmentCache, MaterialiseOptions, materialise_attachments};
use crate::context::{MeasureContext, measure_context};
use crate::dispatch::{
    SubagentDelegate, TOOL_HEARTBEAT_MS, ToolDispatcher, ToolDispatcherOptions, TurnScope,
    parse_tool_args,
};
use crate::events::{AgentEvent, EVENT_CHANNEL_CAPACITY, EventSink};
use crate::prompt::{
    BuildRawPrompt, BuildRuntimeBlock, BuildStaticPrompt, ContextContributor, Host, PromptAgent,
    PromptTools, RuntimePromptContext, StaticPromptContext, build_raw_prompt, build_runtime_block,
    build_static_prompt, contributor_sections, runtime_reminder,
};
use crate::steering::{SteeringQueue, steering_text};
use crate::subagent::{
    DelegationRefusal, SubagentBinding, parse_task, refuse_delegation, refused_execution,
    subagent_definition, subagent_result,
};
use crate::text_tool_call::{text_tool_call_correction, text_tool_call_name};

/// The head+tail budget for one tool result before it enters history.
const DEFAULT_MAX_TOOL_RESULT_CHARS: usize = ghostai_core::history::DEFAULT_MAX_TOOL_RESULT_CHARS;

fn max_iterations_text(max_iterations: u64) -> String {
    format!(
        "I stopped after {max_iterations} tool iterations without finishing. Tell me which part \
         to focus on, or break the task into smaller steps."
    )
}

/// Whole seconds, rounded to nearest — the figure a person reads, not a bound.
fn seconds(ms: u64) -> u64 {
    (ms + 500) / 1_000
}

fn wall_timeout_text(elapsed_ms: u64, cap_ms: u64) -> String {
    format!(
        "I ran out of time for this turn — {}s against a {}s cap. Ask me to continue, or narrow \
         the task.",
        seconds(elapsed_ms),
        seconds(cap_ms)
    )
}

/// Core error kinds to the wire's error codes.
///
/// A table rather than a chain of conditionals, so a kind added to the taxonomy
/// without a code here lands on `internal` rather than on whichever branch
/// happened to be last.
fn error_code_for(kind: ErrorKind) -> ErrorCode {
    match kind {
        ErrorKind::InvalidInput | ErrorKind::Conflict => ErrorCode::BadRequest,
        ErrorKind::NotFound => ErrorCode::NotFound,
        ErrorKind::PermissionDenied | ErrorKind::JailEscape => ErrorCode::Unauthorized,
        ErrorKind::Network | ErrorKind::Provider | ErrorKind::Timeout => ErrorCode::ProviderError,
        ErrorKind::Tool => ErrorCode::ToolError,
        ErrorKind::RateLimited => ErrorCode::RateLimited,
        ErrorKind::Config => ErrorCode::ConfigInvalid,
        ErrorKind::Aborted | ErrorKind::Storage | ErrorKind::Extension | ErrorKind::Internal => {
            ErrorCode::Internal
        }
    }
}

fn sum_optional(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, other) | (other, None) => other,
        (Some(a), Some(b)) => Some(a + b),
    }
}

/// Adds one request's usage to the turn's running total.
fn accumulate_usage(total: &Usage, next: &Usage) -> Usage {
    Usage {
        prompt_tokens: total.prompt_tokens + next.prompt_tokens,
        completion_tokens: total.completion_tokens + next.completion_tokens,
        total_tokens: total.total_tokens + next.total_tokens,
        cached_tokens: sum_optional(total.cached_tokens, next.cached_tokens),
        reasoning_tokens: sum_optional(total.reasoning_tokens, next.reasoning_tokens),
    }
}

/// The agent a loop belongs to, as the loop needs it.
///
/// [`PromptAgent`] plus the id, which is what `turn.start` reports and what the
/// turn's stats row records. Deliberately not the whole resolved agent: the
/// model, the tools and the approvals have already been turned into the
/// collaborators around this loop by the time it is constructed, and carrying
/// them again would invite something below here to read the copy instead.
#[derive(Debug, Clone, Default)]
pub struct LoopAgent {
    /// Who the turn is being run by.
    pub prompt: PromptAgent,
    /// The agent's id.
    pub id: String,
    /// This agent's replacements for what its tools say about themselves.
    ///
    /// Beside the prompt templates rather than passed alongside the registry,
    /// because it is the same kind of thing: text the operator owns, scoped to
    /// one agent. The registry's definitions are memoised across every agent in
    /// the process, so this cannot be applied there.
    pub tool_prompts: Option<ToolPromptOverrides>,
    /// The operator's wording for the section that says where commands land.
    ///
    /// Here rather than on [`PromptAgent`] because an agent always has an
    /// identity and does not always have tools: the prompt layer receives these
    /// as [`PromptTools`], which it is handed or is not.
    pub platform_prompt: Option<String>,
    /// The operator's wording for the tool-output policy.
    pub tool_policy_prompt: Option<String>,
}

/// Resolves the loop for a subagent.
///
/// A resolver rather than a map of loops, because loops are built lazily and
/// cached — handing one a set of them at construction would build every
/// subagent's provider whether or not it was ever used, and pin the ones that
/// were not.
///
/// `None` means that agent cannot run, which is a refusal the model is told
/// about rather than an error.
pub trait LoopResolver: Send + Sync {
    /// The loop for `agent_id`, or `None` when it cannot run.
    fn loop_for(&self, agent_id: &str) -> Option<AgentLoop>;
}

/// Everything a loop is built from.
pub struct AgentLoopOptions {
    /// Already wrapped in resilience by whatever built it.
    pub provider: Arc<dyn ChatProvider>,
    /// What this loop may call.
    ///
    /// A scope rather than the registry itself, so an agent restricted to a
    /// subset is not a special case anywhere below here.
    pub tools: Arc<dyn ToolScope>,
    /// Where the conversation lives.
    pub store: Arc<SessionStore>,
    /// Supplies the jail for a turn, keyed by its session's workspace.
    ///
    /// A resolver rather than one jail, because a session records which
    /// workspace it belongs to and two sessions in one process can be in
    /// different ones.
    pub jails: Arc<dyn JailResolver>,
    /// Supplies the scheduler a turn's automation tool writes through, keyed
    /// the same way and for the same reason: the port is scoped to the agent
    /// and the session, so a job records who asked for it and an agent cannot
    /// reach another's.
    pub automation: Option<Arc<dyn AutomationResolver>>,
    /// Supplies the place this agent's commands run, keyed the same way. A
    /// loop with none runs them on the host, which is what an install with no
    /// environment service configured does.
    pub environments: Option<Arc<dyn EnvironmentResolver>>,
    /// Where built-in command execution runs.
    pub environment: AgentEnvironment,
    /// Defaults to the schema's defaults, so a caller with no config file
    /// works.
    pub config: AgentSettings,
    /// The tool layer's configuration.
    pub tools_config: Arc<ToolsConfig>,
    /// Overrides `config.model`. One of the two must be non-empty.
    pub model: Option<String>,
    /// Which agent this loop is. Absent is the unnamed default.
    ///
    /// Constructor-bound, like the model and the tools, because it is the same
    /// kind of decision: a loop *is* one agent. The composition root builds one
    /// per agent rather than making every turn re-resolve who it belongs to,
    /// and a turn already running therefore keeps the agent it started under.
    pub agent: Option<LoopAgent>,
    /// The zone the prompt's clock is printed in — the install's setting.
    ///
    /// A closure rather than a value, because it is read once per turn and an
    /// operator who changes it in settings should not have to restart to be
    /// believed. Absent means the host zone.
    ///
    /// This is the same zone the automation tool's cron expressions are read
    /// in, and that is the whole point of threading it this far: the tool tells
    /// the model to write the hour it sees on the clock beside it, which is
    /// only true if the clock and the scheduler agree.
    pub time_zone: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    /// Sections the loop knows nothing about.
    pub contributors: Vec<Arc<dyn ContextContributor>>,
    /// Who to ask before a tool whose permission is `ask` runs.
    ///
    /// Absent means nobody is there to ask, and `ask` then runs the tool, which
    /// is what keeps a terminal session working. `deny` is enforced with or
    /// without a gate. Any transport that exposes this agent to something other
    /// than its operator's own keyboard should install one.
    pub approvals: Option<Arc<dyn ApprovalGate>>,
    /// The agents this one may delegate to, keyed by the tool name each is
    /// called by. Built with `subagent_map`.
    ///
    /// One map, not two: the definitions the model is shown and the permission
    /// the gate reads both come from here, so a subagent cannot be advertised
    /// and then refused.
    pub subagents: IndexMap<String, SubagentBinding>,
    /// Resolves the loop for a subagent.
    pub resolve_loop: Option<Arc<dyn LoopResolver>>,
    /// The queue this loop drains. Shared with whatever pushes into it.
    pub steering: Arc<SteeringQueue>,
    /// Wall-clock and monotonic time.
    pub clock: Arc<dyn Clock>,
    /// The nonce source. Never pin this outside a test.
    pub random: Arc<dyn RandomSource>,
    /// Turn and session ids. Injected so a test asserts on stable values.
    pub new_id: Arc<dyn Fn() -> String + Send + Sync>,
    /// Source for the exec env allow-list.
    pub env: Arc<HashMap<String, String>>,
    /// The host the prompt describes. Injected so a prompt is assertable.
    pub host: Host,
    /// `0` disables the heartbeat.
    pub tool_heartbeat_ms: u64,
    /// Head+tail budget for one tool result before it enters history.
    pub max_tool_result_chars: usize,
}

impl AgentLoopOptions {
    /// The mandatory collaborators, with every optional seam at its default.
    pub fn new(
        provider: Arc<dyn ChatProvider>,
        tools: Arc<dyn ToolScope>,
        store: Arc<SessionStore>,
        jails: Arc<dyn JailResolver>,
    ) -> AgentLoopOptions {
        AgentLoopOptions {
            provider,
            tools,
            store,
            jails,
            automation: None,
            environments: None,
            environment: AgentEnvironment::default(),
            config: AgentSettings::default(),
            tools_config: Arc::new(ToolsConfig::default()),
            model: None,
            agent: None,
            time_zone: None,
            contributors: Vec::new(),
            approvals: None,
            subagents: IndexMap::new(),
            resolve_loop: None,
            steering: Arc::new(SteeringQueue::new()),
            clock: Arc::new(SystemClock),
            random: Arc::new(OsRandom),
            new_id: Arc::new(uuid_like),
            env: Arc::new(HashMap::new()),
            host: Host::default(),
            tool_heartbeat_ms: TOOL_HEARTBEAT_MS,
            max_tool_result_chars: DEFAULT_MAX_TOOL_RESULT_CHARS,
        }
    }
}

/// A turn id for a caller that supplied no source.
///
/// Ids are injected everywhere that matters — the store mints message ids, the
/// transport mints turn ids it has already published — so this exists only so
/// `AgentLoopOptions::new` has something to default to.
fn uuid_like() -> String {
    let mut bytes = [0u8; 10];
    OsRandom.fill(&mut bytes);
    ghostai_protocol::new_uuid(u64::try_from(SystemClock.now_ms()).unwrap_or(0), &bytes)
}

/// What starts a turn.
pub struct TurnInput {
    /// The conversation.
    pub session_key: String,
    /// A string for the common case; parts for an image the user attached.
    pub content: Content,
    /// Where the message came from. Recorded as the session's origin.
    pub channel: Option<String>,
    /// Which agent the session should be created under.
    pub agent_id: Option<String>,
    /// The workspace to create the session in, if it does not exist yet.
    ///
    /// Ignored for a session that already exists: a transport that mints a
    /// session key passes what the user picked, and the loop never trusts it
    /// over the stored row.
    pub workspace_id: Option<String>,
    /// Supplied by the caller when it has already told a client the id.
    pub turn_id: Option<String>,
    /// The agents already running above this turn, oldest first.
    ///
    /// Empty for a turn a person started. Carried on the input rather than held
    /// on the loop because loops are one per agent and shared through a cache —
    /// anything depth-shaped stored on the object would be wrong the moment the
    /// same agent appeared at two depths.
    pub chain: Vec<String>,
    /// The session a person is looking at, when this turn is a subagent's.
    ///
    /// Absent means this turn *is* that session. It reaches the approval
    /// request, and nothing else reads it.
    pub root_session_key: Option<String>,
    /// Where the caller's commands run, offered to this turn.
    ///
    /// Carried on the input for the same reason `chain` is: loops are one per
    /// agent and shared through a cache, so anything that depends on *who
    /// called* would be wrong the moment the same agent appeared under two
    /// callers.
    ///
    /// Present on every delegated turn, host included. Absent means there is no
    /// caller, which is the top of a chain. What the turn does with it is the
    /// agent's own `alwaysUseOwn`.
    pub inherited_environment: Option<AgentEnvironment>,
}

impl TurnInput {
    /// A turn on `session_key` saying `content`, with every other seam unset.
    pub fn new(session_key: impl Into<String>, content: impl Into<Content>) -> TurnInput {
        TurnInput {
            session_key: session_key.into(),
            content: content.into(),
            channel: None,
            agent_id: None,
            workspace_id: None,
            turn_id: None,
            chain: Vec::new(),
            root_session_key: None,
            inherited_environment: None,
        }
    }
}

/// What the context inspector knows about the session it is inspecting.
#[derive(Debug, Clone, Default)]
pub struct PromptPreviewInput {
    /// The conversation.
    pub session_key: String,
    /// Defaults to `web`. A terminal's context command passes `cli`.
    pub channel: Option<String>,
    /// The agent the session is bound to.
    pub agent_id: Option<String>,
}

/// The prompt as two messages at two ends of the request, kept apart.
///
/// They are billed differently and that is the reason they are separate here:
/// the static half is the system message and the provider's cached prefix,
/// while the runtime block is the trailing turn re-read at full price on every
/// iteration. Joining them would hand the inspector a single figure whose whole
/// purpose is to be broken in two.
///
/// In raw mode `runtime_block` is empty — the operator's template is one blob
/// placed entirely in the system message, which is the cost that mode chooses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptPreview {
    /// The cached prefix.
    pub static_prompt: String,
    /// The trailing turn.
    pub runtime_block: String,
}

/// What a finished turn produced.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnResult {
    /// The turn.
    pub turn_id: String,
    /// Why it stopped.
    pub stop_reason: StopReason,
    /// Provider requests made. Never more than the configured cap.
    pub iterations: u64,
    /// What it cost.
    pub usage: Usage,
    /// The final answer, or the explanation of why there is none.
    pub text: String,
}

/// Cancels the turn when the consumer lets go of it.
///
/// This is the whole of "abandoning the stream unwinds the turn". A spawned
/// task has no hook that fires when its consumer walks away, so the guard
/// travels with the receiver instead and the token it holds is the turn's.
#[derive(Debug)]
pub struct TurnGuard {
    token: CancellationToken,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

/// One turn, running.
///
/// A bounded channel of events plus a completion. The bound is the backpressure
/// a `for await` gave for free; the guard is what makes dropping this unwind
/// the turn.
#[derive(Debug)]
pub struct Turn {
    events: mpsc::Receiver<AgentEvent>,
    result: oneshot::Receiver<Result<TurnResult>>,
    guard: TurnGuard,
}

impl Turn {
    /// The next event, or `None` once the turn has emitted its last.
    pub async fn next_event(&mut self) -> Option<AgentEvent> {
        self.events.recv().await
    }

    /// The turn's cancellation. Cancelling it stops the turn as a stop frame
    /// would, and the turn still reports its end.
    pub fn token(&self) -> &CancellationToken {
        &self.guard.token
    }

    /// Stops the turn. The events already emitted still arrive, and `turn.end`
    /// follows with `aborted`.
    pub fn stop(&self) {
        self.guard.token.cancel();
    }

    /// Drains whatever is left and returns the outcome.
    ///
    /// Draining is not a convenience: the channel is bounded, so a caller that
    /// awaited the result without reading the events would deadlock a turn that
    /// had more than the buffer's worth left to say.
    pub async fn finish(mut self) -> Result<TurnResult> {
        while self.events.recv().await.is_some() {}
        match self.result.try_recv() {
            Ok(result) => result,
            // The task ended without answering, which means it was cancelled
            // between the last event and the send. Nothing finished, so there
            // is nothing to report.
            Err(_) => Err(GhostError::aborted("Turn")),
        }
    }

    /// Every event the turn emitted, and its outcome.
    pub async fn collect(mut self) -> (Vec<AgentEvent>, Result<TurnResult>) {
        let mut events = Vec::new();
        while let Some(event) = self.events.recv().await {
            events.push(event);
        }
        let result = match self.result.try_recv() {
            Ok(result) => result,
            Err(_) => Err(GhostError::aborted("Turn")),
        };
        (events, result)
    }
}

impl Stream for Turn {
    type Item = AgentEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<AgentEvent>> {
        self.events.poll_recv(cx)
    }
}

/// Clears the session's steering queue however the turn ended.
///
/// A guard rather than a line at each exit, because "however it ended" includes
/// the early return a vanished consumer takes.
struct SteeringGuard {
    steering: Arc<SteeringQueue>,
    session_key: String,
}

impl Drop for SteeringGuard {
    fn drop(&mut self) {
        self.steering.clear(&self.session_key);
    }
}

/// What a turn works out once and reuses on every iteration.
///
/// Exactly one of the two is populated, decided by the agent's prompt mode. A
/// union would be tidier to read and worse to use: both fields are consumed by
/// one composition step that already branches on the mode, and a second
/// discriminant would have to be kept in step with it.
struct Preamble {
    /// Template mode: the cached static half, built once. Empty in raw mode.
    static_prompt: String,
    /// Raw mode: the contributor sections, which may have done I/O. Empty
    /// otherwise.
    static_sections: Vec<String>,
}

/// The running totals one turn accumulates.
///
/// Gathered into a struct rather than left as a dozen locals so the iteration
/// body can take them by reference and stay readable.
#[derive(Debug, Default)]
struct TurnState {
    iteration: u64,
    elapsed_ms: u64,
    stop_reason: Option<StopReason>,
    /// Why the turn failed, set alongside every error stop.
    ///
    /// Recorded on the stats row rather than appended to history: everything in
    /// the conversation is replayed into every later provider request, so an
    /// error written there would fail its way into the prompt forever. This is
    /// the only durable copy — the `error` event carrying the same words is
    /// unsequenced and gone the moment it is delivered.
    error_message: Option<String>,
    final_text: String,
    usage: Usage,
    /// Time the model actually spent emitting tokens, summed over the requests
    /// that reported it.
    ///
    /// Accumulated beside the usage and from the same result, which is what
    /// keeps the two halves of a rate matched: a request that aborted or failed
    /// reports neither, so it contributes no tokens *and* no time.
    generation_ms: f64,
    /// The completion tokens produced inside those windows, and only those.
    ///
    /// The other half of the pair, and not an optimisation — the turn's own
    /// completion count includes tokens this cannot time. Ollama emits a bare
    /// tool call as a *single* frame however long its arguments are, so such a
    /// request reports a window of zero while charging for every token in it.
    /// Divided by somebody else's window those tokens read as free.
    generation_tokens: u64,
    /// Turn start to the first token anyone saw, once per turn.
    ///
    /// Measured from the *turn* rather than from the request that produced the
    /// token, because it sits beside the elapsed time and has to be readable
    /// against it. Set once and never resummed: only the first request pays the
    /// weight load, so accumulating would report a five-step turn as five cold
    /// starts.
    first_token_ms: Option<f64>,
    /// Set for exactly one iteration, then cleared. See `text_tool_call`.
    correction: Option<String>,
    corrected_once: bool,
    first_seq: i64,
    last_seq: i64,
}

/// The turn's rounded timings, shared by the stats row and the end event.
///
/// Whole milliseconds because both shapes say an integer, and a client that
/// validates its frames would drop the one saying the turn ended. Built once
/// rather than twice so the stored row and the event cannot come to disagree
/// about the same turn.
///
/// Absent rather than zero when nothing was measured: absence is what separates
/// "not measured" from "measured as zero", and a rate needs that distinction to
/// know when to fall back.
#[derive(Debug, Clone, Copy, Default)]
struct Timings {
    generation_ms: Option<u64>,
    generation_tokens: Option<u64>,
    first_token_ms: Option<u64>,
}

impl TurnState {
    fn timings(&self) -> Timings {
        Timings {
            generation_ms: (self.generation_ms > 0.0).then(|| round_ms(self.generation_ms)),
            generation_tokens: (self.generation_ms > 0.0).then_some(self.generation_tokens),
            first_token_ms: self.first_token_ms.map(round_ms),
        }
    }
}

/// The session metadata bag, as the protocol's helpers take it.
///
/// The store keeps a `serde_json::Map` and the protocol an `IndexMap`; with
/// `preserve_order` the two hold the same order, and this is the conversion
/// between the two spellings of it.
fn to_object(map: &serde_json::Map<String, serde_json::Value>) -> Object {
    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// The other direction. See [`to_object`].
fn from_object(object: Object) -> serde_json::Map<String, serde_json::Value> {
    object.into_iter().collect()
}

/// A count as SQLite stores it. Every value here is a duration or a token
/// count, so the clamp is a type conversion rather than a policy.
fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a duration in milliseconds, clamped non-negative below"
)]
fn round_ms(value: f64) -> u64 {
    if value <= 0.0 {
        0
    } else {
        value.round() as u64
    }
}

/// Everything a turn closes over, behind one `Arc`.
///
/// [`AgentLoop`] is a handle over this rather than the state itself, so `run`
/// can take `&self` and still hand the work to a task that outlives the call —
/// and so a subagent resolver can answer with a loop by cloning a pointer
/// rather than rebuilding a provider.
struct LoopInner {
    provider: Arc<dyn ChatProvider>,
    tools: Arc<dyn ToolScope>,
    agent: Option<LoopAgent>,
    agent_id: String,
    store: Arc<SessionStore>,
    jails: Arc<dyn JailResolver>,
    automation: Option<Arc<dyn AutomationResolver>>,
    environments: Option<Arc<dyn EnvironmentResolver>>,
    environment: AgentEnvironment,
    config: AgentSettings,
    tools_config: Arc<ToolsConfig>,
    model_id: String,
    contributors: Vec<Arc<dyn ContextContributor>>,
    time_zone: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    subagents: IndexMap<String, SubagentBinding>,
    resolve_loop: Option<Arc<dyn LoopResolver>>,
    steering: Arc<SteeringQueue>,
    clock: Arc<dyn Clock>,
    random: Arc<dyn RandomSource>,
    new_id: Arc<dyn Fn() -> String + Send + Sync>,
    env: Arc<HashMap<String, String>>,
    host: Host,
    dispatcher: ToolDispatcher,
}

/// One agent's loop: a provider, a tool scope and a store, turned into turns.
#[derive(Clone)]
pub struct AgentLoop {
    inner: Arc<LoopInner>,
}

impl std::fmt::Debug for AgentLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoop")
            .field("agent_id", &self.inner.agent_id)
            .field("model", &self.inner.model_id)
            .field("provider", &self.inner.provider.id())
            .finish_non_exhaustive()
    }
}

impl AgentLoop {
    /// Builds a loop, or refuses when no model is configured.
    ///
    /// The dispatcher is built last, so a loop that refused to construct never
    /// has one. Every value it takes is already resolved here — the dispatcher
    /// defaults nothing itself, which is what keeps it free of branches no turn
    /// takes.
    pub fn new(options: AgentLoopOptions) -> Result<AgentLoop> {
        let model = options
            .model
            .clone()
            .unwrap_or_else(|| options.config.model.clone());
        if model.is_empty() {
            return Err(GhostError::new(
                ErrorKind::Config,
                "No model configured for the agent loop",
            )
            .with_detail("provider", options.provider.id()));
        }

        let agent_id = options
            .agent
            .as_ref()
            .map_or_else(|| DEFAULT_AGENT_ID.to_owned(), |agent| agent.id.clone());

        let dispatcher = ToolDispatcher::new(ToolDispatcherOptions {
            tools: Arc::clone(&options.tools),
            subagents: options.subagents.clone(),
            approvals: options.approvals.clone(),
            tools_config: Arc::clone(&options.tools_config),
            tools_enabled: options.config.tools_enabled,
            max_tool_result_chars: options.max_tool_result_chars,
            heartbeat_ms: options.tool_heartbeat_ms,
            agent_id: agent_id.clone(),
            clock: Arc::clone(&options.clock),
        });

        Ok(AgentLoop {
            inner: Arc::new(LoopInner {
                provider: options.provider,
                tools: options.tools,
                agent: options.agent,
                agent_id,
                store: options.store,
                jails: options.jails,
                automation: options.automation,
                environments: options.environments,
                environment: options.environment,
                config: options.config,
                tools_config: options.tools_config,
                model_id: model,
                contributors: options.contributors,
                time_zone: options.time_zone,
                subagents: options.subagents,
                resolve_loop: options.resolve_loop,
                steering: options.steering,
                clock: options.clock,
                random: options.random,
                new_id: options.new_id,
                env: options.env,
                host: options.host,
                dispatcher,
            }),
        })
    }

    /// The model a turn on this loop would use.
    pub fn model(&self) -> &str {
        &self.inner.model_id
    }

    /// The provider a turn on this loop would reach.
    pub fn provider(&self) -> &str {
        self.inner.provider.id()
    }

    /// Which agent this loop is.
    pub fn agent_id(&self) -> &str {
        &self.inner.agent_id
    }

    /// The queue this loop drains. Exposed so a transport can push into it.
    pub fn steering(&self) -> &Arc<SteeringQueue> {
        &self.inner.steering
    }

    /// Queues a correction for the turn currently running on `session_key`.
    pub fn steer(&self, session_key: &str, content: impl Into<String>) {
        self.inner
            .steering
            .push(session_key, content, self.inner.clock.now_ms());
    }

    /// The tool registry entries a turn on this loop would send.
    ///
    /// Exposed for the same reason the prompt preview is: describing what a
    /// turn would carry has to come from the object that carries it. Rebuilding
    /// the list from the registry instead gives the built-ins narrowed by the
    /// agent's allow-list and nothing else — the subagent delegation tools are
    /// composed on *top* of that scope, so they were missing from the inspector
    /// and, worse, missing from its token count.
    ///
    /// Tools switched off empties it here rather than at the request, and that
    /// placement is the point: every consumer — the request, the text-tool-call
    /// correction, the context inspector's count — then agrees on the same
    /// answer without any of them learning about the setting. A gate at the
    /// request would leave the panel listing tools the model was never offered.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let inner = &self.inner;
        if !inner.config.tools_enabled {
            return Vec::new();
        }

        let mut tools: Vec<ToolDefinition> = inner.tools.definitions().to_vec();

        // Appended rather than merged and re-sorted. The registry's list is
        // already sorted, and keeping the subagents in the operator's
        // configured order at the end is both the order they were written in
        // and a block a reader can see as one thing.
        let registered: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
        for binding in inner.subagents.values() {
            // The registry wins. A name can only collide with one an MCP server
            // or an extension registered — no built-in starts with the subagent
            // prefix — and silently shadowing it would take a tool away from
            // the model with nothing anywhere saying so.
            if registered.contains(&binding.tool_name) {
                tracing::warn!(
                    tool = %binding.tool_name,
                    agent_id = %binding.agent_id,
                    "subagent hidden by a registered tool of the same name"
                );
                continue;
            }
            tools.push(subagent_definition(binding));
        }

        self.with_tool_prompts(tools)
    }

    /// The operator's wording in place of the compiled one.
    ///
    /// Last, and after the subagents are appended, so one pass covers
    /// built-ins, MCP and extension tools and delegation
    /// alike — and so an override for a subagent tool beats the operator's
    /// subagent prompt, being the more specific of the two. Doing it in the
    /// registry instead would have to happen before the subagents exist, and
    /// would put per-agent text into a list that is memoised across every
    /// agent.
    ///
    /// A miss is logged rather than refused. The settings layer already warned
    /// the operator at save time; this is the backstop for a tool that left the
    /// list afterwards, because an MCP server went down or `exec` was
    /// switched off.
    fn with_tool_prompts(&self, definitions: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
        let Some(overrides) = self
            .inner
            .agent
            .as_ref()
            .and_then(|a| a.tool_prompts.as_ref())
        else {
            return definitions;
        };

        let applied = apply_tool_prompts(&definitions, overrides);
        if !applied.unknown_tools.is_empty() || !applied.unknown_fields.is_empty() {
            tracing::warn!(
                tools = ?applied.unknown_tools,
                fields = ?applied.unknown_fields,
                "tool prompt override names something this agent does not advertise"
            );
        }
        applied.definitions
    }

    /// The place a request resolves to. A loop with no resolver runs on the
    /// host, which is what an install with no environment service does.
    fn resolve_placement(&self, request: &PlacementRequest) -> Placed {
        self.inner
            .environments
            .as_ref()
            .map_or_else(Placed::host, |resolver| resolver.for_turn(request))
    }

    /// Where a turn on this loop would land, for a caller that is not running
    /// one. Used by the prompt preview, so what it shows is what a turn would
    /// carry rather than a second guess at it.
    fn place(
        &self,
        selection: &AgentEnvironment,
        agent_id: &str,
        workspace_id: &str,
        session_key: &str,
        workspace_root: String,
    ) -> Placed {
        self.resolve_placement(&PlacementRequest {
            agent_id: agent_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            session_key: session_key.to_owned(),
            environment: selection.name.clone(),
            network: selection.network.clone(),
            workspace_root,
        })
    }

    /// The tool-shaped prompt inputs for this turn, or nothing when there are
    /// none.
    ///
    /// Where the tools setting crosses into prompt assembly, and it crosses as
    /// presence rather than as a flag: off, the prompt layer is handed no
    /// [`PromptTools`] at all and therefore has no container, no policy wording
    /// and no command wording to render from. Nothing downstream is told why,
    /// and nothing downstream needs a branch to find out.
    fn prompt_tools(&self, placed: &Placed) -> Option<PromptTools> {
        let inner = &self.inner;
        if !inner.config.tools_enabled {
            return None;
        }
        let agent = inner.agent.as_ref();
        Some(PromptTools {
            policy_prompt: agent.and_then(|a| a.tool_policy_prompt.clone()),
            platform_prompt: agent.and_then(|a| a.platform_prompt.clone()),
            confined: placed.environment.confined(),
        })
    }

    fn contributor_refs(&self) -> Vec<&dyn ContextContributor> {
        self.inner.contributors.iter().map(AsRef::as_ref).collect()
    }

    fn is_raw(&self) -> bool {
        matches!(
            self.inner.agent.as_ref().and_then(|a| a.prompt.prompt_mode),
            Some(ghostai_protocol::PromptMode::Raw)
        )
    }

    /// The once-per-turn half, in whichever form this agent's mode needs it.
    ///
    /// Both modes have the same obligation and it is the reason this is
    /// separate from composition: a contributor's static section may do I/O, so
    /// it runs once per turn and never per iteration. Template mode wants the
    /// finished static prompt; raw mode wants the contributor sections on their
    /// own, because a raw template places them itself.
    async fn preamble(&self, context: &StaticPromptContext, placed: &Placed) -> Preamble {
        let contributors = self.contributor_refs();
        if self.is_raw() {
            return Preamble {
                static_prompt: String::new(),
                static_sections: contributor_sections(&contributors, context).await,
            };
        }

        let tools = self.prompt_tools(placed);
        Preamble {
            static_prompt: build_static_prompt(BuildStaticPrompt {
                context,
                agent: self.inner.agent.as_ref().map(|a| &a.prompt),
                contributors: &contributors,
                tools: tools.as_ref(),
                host: self.inner.host.clone(),
            })
            .await,
            static_sections: Vec::new(),
        }
    }

    /// The prompt for one iteration. One function, so the preview cannot drift
    /// from the turn.
    fn compose_prompt(
        &self,
        preamble: &Preamble,
        context: &RuntimePromptContext,
        nonce: &str,
        correction: Option<&str>,
        placed: &Placed,
    ) -> PromptPreview {
        let inner = &self.inner;
        let tools = self.prompt_tools(placed);
        let contributors = self.contributor_refs();
        let zone = inner.time_zone.as_ref().map(|read| read());

        if self.is_raw() {
            // One blob, and it stays in the system message. There is no cached
            // prefix to protect here — the operator's template places
            // everything itself, so splitting it across two messages would move
            // text they positioned.
            return PromptPreview {
                static_prompt: build_raw_prompt(&BuildRawPrompt {
                    context,
                    agent: inner.agent.as_ref().map(|a| &a.prompt),
                    tools: tools.as_ref(),
                    host: inner.host.clone(),
                    nonce,
                    static_sections: &preamble.static_sections,
                    contributors: &contributors,
                    time_zone: zone.as_deref(),
                    correction,
                }),
                runtime_block: String::new(),
            };
        }

        let agent = inner.agent.as_ref();
        PromptPreview {
            static_prompt: preamble.static_prompt.clone(),
            runtime_block: build_runtime_block(&BuildRuntimeBlock {
                context,
                live_prompt: agent.and_then(|a| a.prompt.live_prompt.as_deref()),
                wrap_up_prompt: agent.and_then(|a| a.prompt.wrap_up_prompt.as_deref()),
                tools: tools.as_ref(),
                nonce,
                contributors: &contributors,
                time_zone: zone.as_deref(),
                correction,
            }),
        }
    }

    /// The prompt a turn on `session_key` would be sent, without running one.
    ///
    /// This exists so the context inspector shows the prompt the agent actually
    /// uses rather than a second assembly of it. Composing the two halves
    /// outside the loop would work today and quietly lie later: memory and
    /// skills arrive as contributors attached to *this* object, and a
    /// reimplementation elsewhere cannot see them.
    ///
    /// **Both halves, separately.** They are two different messages at two ends
    /// of the request and they are billed differently. Returning one joined
    /// string would report a number the inspector exists to break apart.
    ///
    /// The runtime half is built at iteration 1 with a throwaway nonce. Both
    /// are per-turn values with no meaning outside a turn, and the alternative
    /// — reporting the nonce of some other turn — would be worse than reporting
    /// one that was never used.
    pub async fn preview_prompt(&self, input: &PromptPreviewInput) -> Result<PromptPreview> {
        let inner = &self.inner;
        // The stored session decides, exactly as it does in `run`. A preview
        // that reported the default workspace's root for a session bound to
        // another one would describe a prompt no turn on it will ever carry.
        let stored = inner.store.get_session(&input.session_key)?;
        let workspace_id = stored.as_ref().map_or_else(
            || DEFAULT_WORKSPACE_ID.to_owned(),
            |s| s.workspace_id.clone(),
        );
        let jail = match stored {
            None => inner.jails.default_jail(),
            Some(_) => inner.jails.for_workspace(&workspace_id),
        };

        let context = StaticPromptContext {
            workspace_root: jail.root().to_string_lossy().into_owned(),
            workspace_id,
            session_key: input.session_key.clone(),
            agent_id: input.agent_id.clone(),
            channel: input.channel.clone().unwrap_or_else(|| "web".to_owned()),
        };
        let runtime = RuntimePromptContext {
            static_context: context.clone(),
            iteration: 1,
            max_iterations: inner.config.max_tool_iterations,
            now_ms: inner.clock.now_ms(),
        };

        // Resolved the same way a turn resolves it, so the preview describes
        // the prompt a turn would actually carry. A preview has no caller, so
        // there is nothing to inherit: this agent's own environment or the host.
        let placed = self.place(
            &inner.environment,
            &context
                .agent_id
                .clone()
                .unwrap_or_else(|| DEFAULT_AGENT_ID.to_owned()),
            &context.workspace_id,
            &context.session_key,
            jail.root().to_string_lossy().into_owned(),
        );
        let preamble = self.preamble(&context, &placed).await;
        let nonce = create_tool_output_nonce(inner.random.as_ref());
        Ok(self.compose_prompt(&preamble, &runtime, &nonce, None, &placed))
    }
}

/// What one turn holds for its whole life: the things resolved before the first
/// iteration and never re-derived.
///
/// The jail, the sandbox, the runner and the automation port are all resolved
/// once and captured, so a workspace switch or a config change mid-turn cannot
/// move where this turn's tools act.
struct TurnContext {
    session: SessionRecord,
    prompt_context: StaticPromptContext,
    /// Where this turn's commands run and what that place says about itself,
    /// resolved once beside the jail. The prompt reads both.
    placed: Placed,
    scope: TurnScope,
    nonce: String,
    tool_definitions: Vec<ToolDefinition>,
    turn_id: String,
}

impl AgentLoop {
    /// Runs one turn, emitting events as they happen.
    ///
    /// The task is spawned with a child of `parent`, and the returned [`Turn`]
    /// carries a guard that cancels that child when it is dropped. That is what
    /// makes abandoning the stream unwind the turn: the provider request, the
    /// running tool and any child process are all watching the same token.
    pub fn run(&self, input: TurnInput, parent: &CancellationToken) -> Turn {
        let token = parent.child_token();
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (result_tx, result_rx) = oneshot::channel();

        let loop_handle = self.clone();
        let task_token = token.clone();
        tokio::spawn(async move {
            let sink = EventSink::new(events_tx);
            let outcome = loop_handle.run_turn(input, &task_token, &sink).await;
            let _ = result_tx.send(outcome);
        });

        Turn {
            events: events_rx,
            result: result_rx,
            guard: TurnGuard { token },
        }
    }

    /// Opens the turn: the session row, the jail, the sandbox and the first
    /// append.
    ///
    /// Every fallible step that must happen *after* `turn.start` is emitted
    /// lives in `run_turn`; this is what has to happen before there is a turn
    /// to report at all.
    fn open_turn(&self, input: &TurnInput, token: &CancellationToken) -> Result<TurnContext> {
        let inner = &self.inner;
        let turn_id = input.turn_id.clone().unwrap_or_else(|| (inner.new_id)());
        let channel = input.channel.clone().unwrap_or_else(|| "cli".to_owned());

        // Ensure first, then read what came back. A claimed workspace can only
        // ever *create* a session in one — the store ignores it for a row that
        // already exists — so the workspace a turn runs in is the one stored on
        // the session, never the one this request happens to claim. That is
        // what makes switching workspaces in the UI safe while a turn is
        // running, and what stops a crafted frame from pointing an existing
        // session's tools at another workspace's files.
        let session = inner.store.ensure_session(
            &input.session_key,
            CreateSession {
                origin: Some(channel.clone()),
                workspace_id: input.workspace_id.clone(),
                agent_id: input.agent_id.clone(),
                ..CreateSession::default()
            },
        )?;
        // Captured once, for the life of the turn: every tool call below closes
        // over this jail, so a workspace switch mid-turn cannot move it.
        let jail = inner.jails.for_workspace(&session.workspace_id);

        // This agent's own rule, applied to what its caller offered. A caller
        // always offers, so an agent that brings its own environment is the one
        // deciding not to take it, rather than every caller having to know.
        let selection = if inner.environment.always_use_own {
            inner.environment.clone()
        } else {
            input
                .inherited_environment
                .clone()
                .unwrap_or_else(|| inner.environment.clone())
        };

        // Resolved once per turn, beside the jail and for the same reason: a
        // placement is a property of (agent, workspace, session), and
        // re-deriving it per tool call would let a mid-turn config change move
        // it.
        let placement = PlacementRequest {
            agent_id: session
                .agent_id
                .clone()
                .unwrap_or_else(|| DEFAULT_AGENT_ID.to_owned()),
            workspace_id: session.workspace_id.clone(),
            session_key: input.session_key.clone(),
            environment: selection.name.clone(),
            network: selection.network.clone(),
            workspace_root: jail.root().to_string_lossy().into_owned(),
        };
        let automation = inner
            .automation
            .as_ref()
            .and_then(|a| a.for_turn(&placement));
        // The place, resolved from the same key. `sandboxed` is derived from it
        // rather than set beside it, so the guard's host-shaped refusals lift
        // exactly when the command stops starting on this machine.
        let placed = self.resolve_placement(&placement);
        let environment = Arc::clone(&placed.environment);

        let mut tool_context = ToolContext::new(jail.clone(), Arc::clone(&inner.tools_config));
        tool_context.placement = Some(placement.clone());
        tool_context.token = token.clone();
        tool_context.clock = Arc::clone(&inner.clock);
        tool_context.env = Arc::clone(&inner.env);
        tool_context.automation = automation;
        tool_context.sandboxed = environment.confined();
        tool_context.runner = environment;
        // Deliberately left unset: `dispatch` is the one place a result is
        // truncated and fenced, and a registry that fenced too would produce an
        // envelope inside an envelope. See the dispatch module header.
        tool_context.nonce = None;

        // The stored row wins, for the same reason the workspace does: a
        // history built under one agent's prompt and tools must not silently
        // continue under another's.
        let prompt_context = StaticPromptContext {
            workspace_root: jail.root().to_string_lossy().into_owned(),
            workspace_id: session.workspace_id.clone(),
            session_key: input.session_key.clone(),
            agent_id: session.agent_id.clone(),
            channel: channel.clone(),
        };

        // Once per turn, both of them: see the module header.
        let nonce = create_tool_output_nonce(inner.random.as_ref());
        let tool_definitions = self.tool_definitions();

        let scope = TurnScope {
            session_key: input.session_key.clone(),
            turn_id: turn_id.clone(),
            nonce: nonce.clone(),
            token: token.clone(),
            tool_context,
            workspace_id: session.workspace_id.clone(),
            chain: input.chain.clone(),
            environment: selection.clone(),
            root_session_key: input
                .root_session_key
                .clone()
                .unwrap_or_else(|| input.session_key.clone()),
        };

        Ok(TurnContext {
            session,
            prompt_context,
            placed,
            scope,
            nonce,
            tool_definitions,
            turn_id,
        })
    }

    /// A conversation nobody has named yet takes its name from the first thing
    /// said in it.
    ///
    /// Guarded on the *stored* title, so this can only ever fire once: a
    /// session that has one — derived here on an earlier turn, or typed by a
    /// user through the rename route — never re-enters the branch. That makes
    /// "a manual rename is never clobbered" a property of the code rather than
    /// a convention someone has to remember.
    ///
    /// Here rather than in a transport because the web is one door of several.
    /// The terminal and every channel run this same loop, and a title derived
    /// in one of them is a title all of them show.
    fn name_the_session(&self, session: &SessionRecord, opening: &ChatMessage) -> Result<()> {
        if !session.title.is_empty() {
            return Ok(());
        }
        let title = derive_session_title(&text_of(opening));
        if title.is_empty() {
            return Ok(());
        }
        self.inner.store.update_session(
            &session.key,
            UpdateSession {
                title: Some(title),
                ..UpdateSession::default()
            },
        )?;
        Ok(())
    }

    /// Records what the turn cost, and never lets that fail the turn.
    ///
    /// The only defensive write in this file, and the asymmetry is the point:
    /// an append is load-bearing — a missing one is a provider 400 on the next
    /// request — whereas a stats row is a number on an info popover. Failing
    /// here would take down a turn that has already completed and already been
    /// persisted, which is strictly worse than a conversation with one gap in
    /// its accounting.
    fn record_stats(&self, stats: &TurnStatsRecord) {
        if let Err(error) = self.inner.store.record_turn_stats(stats) {
            tracing::warn!(
                err = %error.message,
                session_key = %stats.session_key,
                turn_id = %stats.turn_id,
                "failed to record turn stats"
            );
        }
    }

    /// The whole turn, from the opening append to the closing event.
    async fn run_turn(
        &self,
        input: TurnInput,
        token: &CancellationToken,
        sink: &EventSink,
    ) -> Result<TurnResult> {
        let inner = &self.inner;
        // Clears whatever ended the turn — completion, a cap, an abandoned
        // consumer — so nothing queued for it leaks into the next one.
        let _steering = SteeringGuard {
            steering: Arc::clone(&inner.steering),
            session_key: input.session_key.clone(),
        };

        let turn = self.open_turn(&input, token)?;
        let opening = inner.store.append(
            &turn.scope.session_key,
            ChatMessage::User(user_message(input.content.clone())),
            &AppendOptions {
                turn_id: Some(turn.turn_id.clone()),
            },
        )?;
        self.name_the_session(&turn.session, &opening.message)?;

        let mut state = TurnState {
            first_seq: opening.seq,
            last_seq: opening.seq,
            usage: empty_usage(),
            ..TurnState::default()
        };

        let started_at = inner.clock.monotonic();
        // Both clocks, deliberately. The monotonic one caps the wall timeout
        // and must stay monotonic — an NTP step backwards through a wall-clock
        // cap would end a turn that had barely started. The other is what a
        // human reads, and is only ever subtracted from another reading of
        // itself.
        let started_at_ms = inner.clock.now_ms();

        // Emitted before every fallible step below. Building the preamble
        // awaits and resolving the sandbox reaches a container daemon that may
        // be down — either can fail. Opening the turn after them meant a
        // failure there unwound before the turn existed, so the error named a
        // turn no client had seen start, the transcript invented an orphan turn
        // with no first seq, and the one thing the reader wanted — a way to
        // re-run it — was the one thing there was no address for.
        sink.emit(ghostai_protocol::TurnStart {
            tag: ghostai_protocol::TurnStartTag,
            session_key: turn.scope.session_key.clone(),
            turn_id: turn.turn_id.clone(),
            first_seq: u64::try_from(state.first_seq).ok(),
            agent_id: inner.agent_id.clone(),
            model: inner.model_id.clone(),
            provider: inner.provider.id().to_owned(),
        })
        .await;

        let preamble = self.preamble(&turn.prompt_context, &turn.placed).await;
        // Attachments are read from disk on every iteration, because the
        // request is rebuilt on every iteration. Scoped to the turn and
        // discarded with it, so a six-tool turn reads one image once rather
        // than six times.
        let mut attachments: AttachmentCache = AttachmentCache::new();

        while state.iteration < inner.config.max_tool_iterations {
            self.drain_steering(&turn, &mut state)?;

            if token.is_cancelled() {
                state.stop_reason = Some(StopReason::Aborted);
                break;
            }

            state.elapsed_ms = u64::try_from(
                inner
                    .clock
                    .monotonic()
                    .saturating_sub(started_at)
                    .as_millis(),
            )
            .unwrap_or(u64::MAX);
            let cap = inner.config.loop_wall_timeout_ms;
            if cap > 0 && state.elapsed_ms >= cap {
                tracing::warn!(
                    session_key = %turn.scope.session_key,
                    turn_id = %turn.turn_id,
                    elapsed_ms = state.elapsed_ms,
                    wall_timeout_ms = cap,
                    "turn wall timeout"
                );
                state.stop_reason = Some(StopReason::WallTimeout);
                break;
            }

            state.iteration += 1;
            let flow = self
                .one_iteration(
                    &turn,
                    &preamble,
                    &mut state,
                    &mut attachments,
                    started_at,
                    sink,
                )
                .await?;
            if flow == Flow::Stop {
                break;
            }
        }

        self.close_turn(&turn, state, started_at_ms, sink).await
    }

    /// Appends whatever was steered before this iteration built its request.
    fn drain_steering(&self, turn: &TurnContext, state: &mut TurnState) -> Result<()> {
        for message in self.inner.steering.drain(&turn.scope.session_key) {
            let written = self.inner.store.append(
                &turn.scope.session_key,
                ChatMessage::User(user_message(steering_text(&message))),
                &AppendOptions {
                    turn_id: Some(turn.turn_id.clone()),
                },
            )?;
            state.last_seq = written.seq;
        }
        Ok(())
    }
}

/// Whether the iteration wants another one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

impl AgentLoop {
    /// One request, and whatever the model asked for after it.
    async fn one_iteration(
        &self,
        turn: &TurnContext,
        preamble: &Preamble,
        state: &mut TurnState,
        attachments: &mut AttachmentCache,
        started_at: Duration,
        sink: &EventSink,
    ) -> Result<Flow> {
        let inner = &self.inner;
        let runtime = RuntimePromptContext {
            static_context: turn.prompt_context.clone(),
            iteration: state.iteration,
            max_iterations: inner.config.max_tool_iterations,
            now_ms: inner.clock.now_ms(),
        };
        // Consumed, not left standing: it describes what the *previous*
        // iteration did, and a correction that persisted would be scolding the
        // model for something it has already stopped doing.
        let correction = state.correction.take();
        let prompt = self.compose_prompt(
            preamble,
            &runtime,
            &turn.nonce,
            correction.as_deref(),
            &turn.placed,
        );
        let request = self.build_request(turn, &prompt, attachments)?;

        // What the adapter reports is a duration from *its* request; what a
        // reader wants is a duration from the turn. Captured here so the two
        // can be added, which is also what makes the figure survive an opening
        // request that measured nothing.
        let requested_at = inner.clock.monotonic();
        let result = match self.stream_once(request, turn, sink).await {
            Streamed::Result(result) => result,
            Streamed::Aborted => {
                state.stop_reason = Some(StopReason::Aborted);
                return Ok(Flow::Stop);
            }
            Streamed::Failed(message) => {
                state.stop_reason = Some(StopReason::Error);
                state.error_message = Some(message);
                return Ok(Flow::Stop);
            }
        };

        state.usage = accumulate_usage(&state.usage, &result.usage);
        // Beside the usage it was measured against, and only ever from a
        // result. Both sides together, and only when there is a window to speak
        // of: a request whose content arrived in one frame measured a real
        // zero, and its tokens have to sit out with it.
        if let Some(window) = result.generation_ms.filter(|ms| *ms > 0.0) {
            state.generation_ms += window;
            state.generation_tokens += result.usage.completion_tokens;
        }
        // Rebased onto the turn: everything before this request — the preamble,
        // any earlier request, any tool that ran — is time the reader spent
        // waiting for a first token. Adding a duration to an interval rather
        // than subtracting two instants, so the adapter's clock and this one
        // need not share an epoch.
        if state.first_token_ms.is_none()
            && let Some(first) = result.first_token_ms
        {
            let waited = requested_at.saturating_sub(started_at).as_secs_f64() * 1000.0;
            state.first_token_ms = Some(waited + first);
        }

        if result.message.tool_calls.is_empty() {
            return self.finish_answer(turn, &result, state, sink).await;
        }

        self.run_tools(turn, &result, &prompt, state, sink).await
    }

    /// The request one iteration sends.
    fn build_request(
        &self,
        turn: &TurnContext,
        prompt: &PromptPreview,
        attachments: &mut AttachmentCache,
    ) -> Result<ChatRequest> {
        let inner = &self.inner;
        // Re-read every iteration: the tool results this turn just wrote are
        // part of the next request, and reading them back from the store is
        // what keeps history and the request identical rather than merely
        // similar.
        let history = inner.store.history(
            &turn.scope.session_key,
            &HistoryOptions {
                max_tool_result_chars: 0,
                ..HistoryOptions::default()
            },
        )?;
        // Attachments become readable here and nowhere else. Storage holds a
        // path; a provider needs bytes or characters, and only this scope has
        // the jail that resolves one to the other. Doing it before the provider
        // — which is already wrapped in resilience — is also what keeps the
        // degradation ladder looking at real image parts rather than at
        // references it would strip without reading.
        let history = materialise_attachments(
            history,
            &turn.scope.tool_context.jail,
            MaterialiseOptions {
                // Constant for the life of the turn, which is what makes it
                // safe against the cache beside it: that key is path, size and
                // mtime, and does not know about this.
                images: inner.config.vision_enabled,
                ..MaterialiseOptions::default()
            },
            Some(attachments),
        );

        let mut messages = Vec::with_capacity(history.len() + 2);
        messages.push(ChatMessage::System(system_message(&prompt.static_prompt)));
        messages.extend(history);
        // The volatile half, after the history rather than before it. A
        // provider's cache ends at the first byte that differs from the last
        // request, so a clock or an iteration counter placed ahead of the
        // conversation re-prices the whole conversation on every iteration.
        // Sent, never stored: the store is the conversation, and this is
        // scaffolding for one request.
        if !prompt.runtime_block.is_empty() {
            messages.push(ChatMessage::User(user_message(runtime_reminder(
                &prompt.runtime_block,
            ))));
        }

        Ok(ChatRequest {
            model: inner.model_id.clone(),
            messages,
            tools: turn.tool_definitions.clone(),
            tool_choice: None,
            max_tokens: Some(inner.config.max_tokens),
            // Both left unset rather than sent as a null when unconfigured: a
            // null is not the same as saying nothing, and is rejected by the
            // providers that accept no temperature at all.
            temperature: inner.config.temperature,
            reasoning_effort: inner.config.reasoning_effort,
            // Keyed on the session so every request in a conversation lands on
            // the same cache shard. Providers that do not know the field ignore
            // it, and the one that rejects it is handled by the degradation
            // ladder.
            cache_key: Some(turn.scope.session_key.clone()),
        })
    }

    /// Streams one request, emitting deltas as they arrive.
    async fn stream_once(
        &self,
        request: ChatRequest,
        turn: &TurnContext,
        sink: &EventSink,
    ) -> Streamed {
        let token = turn.scope.token.clone();
        let mut stream = self.inner.provider.stream(request, token.clone());
        let mut result: Option<ChatResult> = None;

        loop {
            let next = tokio::select! {
                // The provider is select!ed against the token rather than
                // trusted to observe it, because an adapter that ignores
                // cancellation would otherwise hold the turn open past a stop.
                () = token.cancelled() => return Streamed::Aborted,
                next = stream.next() => next,
            };
            let Some(event) = next else { break };
            match event {
                Ok(ChatStreamEvent::Text(text)) => {
                    if !text.is_empty() {
                        sink.emit(AssistantDelta {
                            tag: ghostai_protocol::AssistantDeltaTag,
                            turn_id: turn.turn_id.clone(),
                            text,
                        })
                        .await;
                    }
                }
                Ok(ChatStreamEvent::Reasoning(text)) => {
                    if !text.is_empty() {
                        sink.emit(ReasoningDelta {
                            tag: ghostai_protocol::ReasoningDeltaTag,
                            turn_id: turn.turn_id.clone(),
                            text,
                        })
                        .await;
                    }
                }
                Ok(ChatStreamEvent::Done(done)) => result = Some(done),
                Err(error) => {
                    if error.is_aborted() {
                        return Streamed::Aborted;
                    }
                    tracing::error!(
                        session_key = %turn.scope.session_key,
                        turn_id = %turn.turn_id,
                        kind = %error.kind,
                        err = %error.message,
                        "provider request failed"
                    );
                    sink.emit(ErrorEvent {
                        tag: ghostai_protocol::ErrorTag,
                        code: error_code_for(error.kind),
                        message: error.message.clone(),
                        retryable: error.retryable,
                        turn_id: Some(turn.turn_id.clone()),
                    })
                    .await;
                    return Streamed::Failed(error.message);
                }
            }
        }

        let Some(result) = result else {
            {
                // A stream that ends without its completion has not reported
                // tool calls, usage or a finish reason. Treating that as an
                // empty answer would silently end the turn on a transport bug.
                let message = "The provider ended the stream without a result.".to_owned();
                sink.emit(ErrorEvent {
                    tag: ghostai_protocol::ErrorTag,
                    code: ErrorCode::ProviderError,
                    message: message.clone(),
                    retryable: true,
                    turn_id: Some(turn.turn_id.clone()),
                })
                .await;
                return Streamed::Failed(message);
            }
        };
        Streamed::Result(result)
    }

    /// The model answered without calling a tool.
    async fn finish_answer(
        &self,
        turn: &TurnContext,
        result: &ChatResult,
        state: &mut TurnState,
        sink: &EventSink,
    ) -> Result<Flow> {
        let inner = &self.inner;
        let written = inner.store.append(
            &turn.scope.session_key,
            ChatMessage::Assistant(result.message.clone()),
            &AppendOptions {
                turn_id: Some(turn.turn_id.clone()),
            },
        )?;
        state.last_seq = written.seq;
        state.final_text = text_of(&written.message);

        // A call the model wrote out instead of making. Left alone, this ends
        // the turn complete with a JSON blob as the answer and no sign anywhere
        // that the model tried to act. One correction per turn: a model that
        // gets it wrong twice is not going to be talked round, and a loop of
        // corrections would burn the iteration budget saying the same thing.
        let names: Vec<String> = turn
            .tool_definitions
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        let attempted = if state.corrected_once {
            None
        } else {
            text_tool_call_name(&state.final_text, &names)
        };
        if let Some(attempted) = attempted {
            state.corrected_once = true;
            state.correction = Some(text_tool_call_correction(&attempted));
            tracing::warn!(
                session_key = %turn.scope.session_key,
                turn_id = %turn.turn_id,
                iteration = state.iteration,
                tool = %attempted,
                "model wrote a tool call as text; correcting it"
            );
            sink.emit(ghostai_protocol::Notice {
                tag: ghostai_protocol::NoticeTag,
                kind: NoticeKind::Degraded,
                message: format!(
                    "The model wrote a call to `{attempted}` as text instead of calling it. \
                     Asking it again."
                ),
                turn_id: Some(turn.turn_id.clone()),
                call_id: None,
            })
            .await;
            return Ok(Flow::Continue);
        }

        if state.final_text.is_empty() {
            // Not an error, and deliberately not retried: the provider
            // answered, the model simply wrote nothing outside its reasoning
            // channel. Small local models do this, and a low token cap makes
            // any reasoning model do it. Logged because the turn is otherwise
            // indistinguishable from a successful one in every record it
            // leaves.
            tracing::warn!(
                session_key = %turn.scope.session_key,
                turn_id = %turn.turn_id,
                iteration = state.iteration,
                reasoning_chars = result.message.reasoning.as_ref().map_or(0, String::len),
                "model produced neither an answer nor a tool call"
            );
        }

        // The correction arrived while this answer was being composed. Keep
        // going so it is answered, rather than ending a turn the user has
        // already asked to change.
        if inner.steering.has_pending(&turn.scope.session_key) {
            return Ok(Flow::Continue);
        }
        state.stop_reason = Some(StopReason::Complete);
        Ok(Flow::Stop)
    }

    /// The model asked for tools. Run them, append, report.
    async fn run_tools(
        &self,
        turn: &TurnContext,
        result: &ChatResult,
        prompt: &PromptPreview,
        state: &mut TurnState,
        sink: &EventSink,
    ) -> Result<Flow> {
        let inner = &self.inner;
        let outcome = inner
            .dispatcher
            .dispatch(result, &turn.scope, sink, self)
            .await?;
        // One transaction, and the store stays the turn's to write. A partial
        // write is exactly the orphaned tool result the history walker then has
        // to repair on every later request.
        let written = inner.store.append_many(
            &turn.scope.session_key,
            outcome.pending,
            &AppendOptions {
                turn_id: Some(turn.turn_id.clone()),
            },
        )?;
        state.last_seq = written.last().map_or(0, |record| record.seq);

        // The history just grew, which is the only thing that moves the number.
        // Emitted here rather than at the end because a turn that calls twenty
        // tools appends most of a window before it ends, and "will this fit" is
        // asked while that is happening — a bar that moves once a turn is a bar
        // that answers after the question stopped mattering.
        //
        // Measured from the prompt this iteration already composed, so the only
        // new work is the pass over the messages.
        //
        // Root loop only. A subagent's context belongs to its own session, and
        // reporting it would move the operator's bar to a figure describing a
        // conversation they are not reading. `ContextUsage` is deliberately
        // outside `NestedAgentEvent`, so this is enforced by the compiler when
        // a subagent's events are wrapped rather than by this condition alone.
        if turn.scope.root_session_key == turn.scope.session_key {
            let report = measure_context(&MeasureContext {
                store: &inner.store,
                tools: &turn.tool_definitions,
                session_key: &turn.scope.session_key,
                prompt,
                context_window_tokens: inner.config.context_window_tokens,
            })?;
            sink.emit(ghostai_protocol::ContextUsage {
                tag: ghostai_protocol::ContextUsageTag,
                session_key: turn.scope.session_key.clone(),
                estimated_tokens: u64::try_from(report.estimated_tokens).unwrap_or(u64::MAX),
                context_window_tokens: report.context_window_tokens,
                breakdown: report.breakdown.to_map(),
            })
            .await;
        }

        if outcome.cancelled {
            state.stop_reason = Some(StopReason::Aborted);
            return Ok(Flow::Stop);
        }
        Ok(Flow::Continue)
    }

    /// Records the turn, reports its end, and answers the caller.
    ///
    /// A consumer that has gone gets neither: the original was a generator, and
    /// abandoning one ran its cleanup and nothing after it — no stats row and
    /// no closing event. The two agree, and neither is a turn that finished.
    async fn close_turn(
        &self,
        turn: &TurnContext,
        mut state: TurnState,
        started_at_ms: i64,
        sink: &EventSink,
    ) -> Result<TurnResult> {
        let inner = &self.inner;
        let stop_reason = state.stop_reason.unwrap_or(StopReason::MaxIterations);

        if sink.is_closed() {
            return Err(GhostError::aborted("Turn"));
        }

        if matches!(
            stop_reason,
            StopReason::MaxIterations | StopReason::WallTimeout
        ) {
            // Unlike an error, this is persisted: the next turn's history has
            // to explain why the task stopped half-done, or the model reads its
            // own truncated work as complete.
            state.final_text = if stop_reason == StopReason::MaxIterations {
                max_iterations_text(inner.config.max_tool_iterations)
            } else {
                wall_timeout_text(state.elapsed_ms, inner.config.loop_wall_timeout_ms)
            };
            let written = inner.store.append(
                &turn.scope.session_key,
                ChatMessage::Assistant(assistant_message(
                    state.final_text.as_str(),
                    AssistantOptions::default(),
                )),
                &AppendOptions {
                    turn_id: Some(turn.turn_id.clone()),
                },
            )?;
            state.last_seq = written.seq;
            sink.emit(AssistantDelta {
                tag: ghostai_protocol::AssistantDeltaTag,
                turn_id: turn.turn_id.clone(),
                text: state.final_text.clone(),
            })
            .await;
        }

        let ended_at_ms = inner.clock.now_ms();
        let timings = state.timings();
        self.record_stats(&TurnStatsRecord {
            turn_id: turn.turn_id.clone(),
            session_key: turn.scope.session_key.clone(),
            agent_id: turn.session.agent_id.clone().unwrap_or_default(),
            // The workspace this turn actually ran in, read off the row the
            // jail was resolved from at the top of the turn. Recorded rather
            // than derived later: the session can be moved afterwards, and then
            // nothing else could say which files this turn was able to reach.
            workspace_id: turn.session.workspace_id.clone(),
            provider: inner.provider.id().to_owned(),
            model: inner.model_id.clone(),
            started_at_ms,
            ended_at_ms,
            iterations: i64::try_from(state.iteration).unwrap_or(i64::MAX),
            stop_reason,
            usage: state.usage,
            generation_ms: timings.generation_ms.map(to_i64),
            generation_tokens: timings.generation_tokens.map(to_i64),
            first_token_ms: timings.first_token_ms.map(to_i64),
            error: state.error_message.clone(),
        });

        sink.emit(ghostai_protocol::TurnEnd {
            tag: ghostai_protocol::TurnEndTag,
            turn_id: turn.turn_id.clone(),
            stop_reason,
            usage: Some(state.usage),
            iterations: state.iteration,
            elapsed_ms: u64::try_from(ended_at_ms - started_at_ms).ok(),
            generation_ms: timings.generation_ms,
            generation_tokens: timings.generation_tokens,
            first_token_ms: timings.first_token_ms,
            first_seq: u64::try_from(state.first_seq).ok(),
            last_seq: u64::try_from(state.last_seq).ok(),
        })
        .await;

        Ok(TurnResult {
            turn_id: turn.turn_id.clone(),
            stop_reason,
            iterations: state.iteration,
            usage: state.usage,
            text: state.final_text,
        })
    }
}

/// What one streamed request produced.
enum Streamed {
    Result(ChatResult),
    /// The turn was stopped. Not an error, and never reported as one.
    Aborted,
    /// The provider failed. The message is what the stats row records.
    Failed(String),
}

impl SubagentDelegate for AgentLoop {
    fn delegate<'a>(
        &'a self,
        call: &'a ghostai_protocol::ToolCall,
        binding: &'a SubagentBinding,
        turn: &'a TurnScope,
        sink: &'a EventSink,
    ) -> ghostai_providers::BoxFuture<'a, Result<ghostai_tools::ToolExecution>> {
        Box::pin(self.run_subagent(call, binding, turn, sink))
    }
}

impl AgentLoop {
    /// One delegated task: a whole turn on another agent's loop, awaited.
    ///
    /// Every event the child produces reaches the operator as it happens rather
    /// than as a paragraph at the end. Everything the child does — its tool
    /// calls, its approvals, its abort — runs on the machinery that already
    /// exists, because it is a real turn and not a simulation of one.
    ///
    /// Four decisions carry the weight:
    ///
    ///  - **The child gets a session of its own, in the caller's workspace.**
    ///    Its own, because context isolation is the feature: a subagent that
    ///    inherited the conversation would put the detour back in the window
    ///    this exists to keep clear. The caller's *workspace*, because a
    ///    researcher that could not read the files being discussed would be
    ///    useless — the folder belongs to the session, and a delegation does
    ///    not leave it.
    ///  - **The pointer is written to the parent before the run, not after.** A
    ///    turn that is abandoned mid-delegation still leaves a child session,
    ///    and a child session nothing points at is one nothing can show or
    ///    delete.
    ///  - **The delegation cap is the *caller's*.** The delegator caps its
    ///    delegate; an agent cannot grant itself more time by being called.
    ///  - **A timeout cancels the child and not the turn.** The cap's token is
    ///    a child of the turn's, so the caller gets a tool result saying the
    ///    subagent was cut short and can carry on — which is what the cap is
    ///    for.
    async fn run_subagent(
        &self,
        call: &ghostai_protocol::ToolCall,
        binding: &SubagentBinding,
        turn: &TurnScope,
        sink: &EventSink,
    ) -> Result<ghostai_tools::ToolExecution> {
        let inner = &self.inner;

        if let Some(refusal) = refuse_delegation(&turn.chain, &binding.agent_id) {
            tracing::warn!(
                tool = %call.name,
                agent_id = %binding.agent_id,
                chain = ?turn.chain,
                refusal = ?refusal,
                "delegation refused"
            );
            return Ok(refused_execution(refusal, binding, &turn.chain));
        }

        let Some(child) = inner
            .resolve_loop
            .as_ref()
            .and_then(|resolve| resolve.loop_for(&binding.agent_id))
        else {
            tracing::warn!(
                tool = %call.name,
                agent_id = %binding.agent_id,
                "delegation refused: the subagent cannot run"
            );
            return Ok(refused_execution(
                DelegationRefusal::Unconfigured,
                binding,
                &turn.chain,
            ));
        };

        let args = parse_tool_args(&call.arguments_json);
        let Some(task) = parse_task(&args) else {
            let mut execution = ghostai_tools::ToolExecution::error(
                ErrorKind::InvalidInput,
                format!(
                    "Invalid arguments for {}: \"task\" must be a non-empty string describing \
                     what the subagent should do.",
                    call.name
                ),
            );
            execution.name.clone_from(&call.name);
            return Ok(execution);
        };

        let depth = turn.chain.len() + 1;
        let session_key = (inner.new_id)();
        self.open_subagent_session(&session_key, binding, turn, call, depth)?;

        let started = inner.clock.monotonic();
        // The cap's token is a child of the turn's, so cancelling it stops the
        // delegation and leaves the caller free to carry on.
        let cap = turn.token.child_token();
        let capped = inner.config.subagent_timeout_ms;
        let timer = (capped > 0).then(|| {
            let token = cap.clone();
            let agent_id = inner.agent_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(capped)).await;
                tracing::warn!(agent_id = %agent_id, "delegation cap reached");
                token.cancel();
            })
        });

        let mut run = child.run(
            TurnInput {
                channel: Some(SUBAGENT_ORIGIN.to_owned()),
                agent_id: Some(binding.agent_id.clone()),
                workspace_id: Some(turn.workspace_id.clone()),
                chain: {
                    let mut chain = turn.chain.clone();
                    chain.push(inner.agent_id.clone());
                    chain
                },
                root_session_key: Some(turn.root_session_key.clone()),
                // Always offered, and built from the place this turn resolved
                // so a caller on the host hands down the host rather than
                // nothing. Whether the child takes it is the child's own
                // `alwaysUseOwn`, read where its turn opens.
                inherited_environment: Some(turn.environment.clone()),
                ..TurnInput::new(session_key.clone(), task)
            },
            &cap,
        );

        while let Some(event) = run.next_event().await {
            if let Some(wrapped) =
                wrap_subagent_event(turn, &call.id, binding, &session_key, depth, event)
            {
                sink.emit(wrapped).await;
            }
        }
        let outcome = run.finish().await;
        if let Some(timer) = timer {
            timer.abort();
        }

        let duration_ms =
            u64::try_from(inner.clock.monotonic().saturating_sub(started).as_millis())
                .unwrap_or(u64::MAX);

        Ok(match outcome {
            Ok(result) => subagent_result(binding, &result.text, result.stop_reason, duration_ms),
            // A child that never reported is one the cap or the turn stopped.
            // Still an answer rather than a failed turn, and phrased as one
            // that was cut short rather than as one that found nothing.
            Err(_) => subagent_result(binding, "", StopReason::Aborted, duration_ms),
        })
    }

    /// Creates the child's session and points the parent at it.
    fn open_subagent_session(
        &self,
        session_key: &str,
        binding: &SubagentBinding,
        turn: &TurnScope,
        call: &ghostai_protocol::ToolCall,
        depth: usize,
    ) -> Result<()> {
        let inner = &self.inner;
        let lineage = SubagentLineage {
            parent_session_key: turn.session_key.clone(),
            parent_turn_id: turn.turn_id.clone(),
            parent_call_id: call.id.clone(),
            agent_id: binding.agent_id.clone(),
            depth: u64::try_from(depth).unwrap_or(u64::MAX),
        };
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            SUBAGENT_METADATA_KEY.to_owned(),
            serde_json::to_value(&lineage).unwrap_or(serde_json::Value::Null),
        );
        inner.store.ensure_session(
            session_key,
            CreateSession {
                origin: Some(SUBAGENT_ORIGIN.to_owned()),
                workspace_id: Some(turn.workspace_id.clone()),
                agent_id: Some(binding.agent_id.clone()),
                metadata: Some(metadata),
                ..CreateSession::default()
            },
        )?;

        // Defensive for the same reason the stats row is, and no more: this is
        // how a reloaded transcript finds the run again, which is worth a write
        // and is not worth failing a turn that has otherwise worked.
        let run = SubagentRunRef {
            session_key: session_key.to_owned(),
            agent_id: binding.agent_id.clone(),
            // The label too, so a reloaded transcript can name the card before
            // the fetch that fills it in resolves.
            label: binding.label.clone(),
        };
        match inner.store.get_session(&turn.session_key) {
            Ok(Some(parent)) => {
                if let Err(error) = inner.store.update_session(
                    &turn.session_key,
                    UpdateSession {
                        metadata: Some(from_object(with_subagent_run(
                            &to_object(&parent.metadata),
                            &call.id,
                            &run,
                        ))),
                        ..UpdateSession::default()
                    },
                ) {
                    tracing::warn!(
                        err = %error.message,
                        session_key = %turn.session_key,
                        call_id = %call.id,
                        "failed to record the subagent session pointer"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                err = %error.message,
                session_key = %turn.session_key,
                "failed to read the parent session"
            ),
        }
        Ok(())
    }
}

/// One event from a subagent, addressed to the card it belongs under.
///
/// An event from *the child's own* subagent is forwarded rather than wrapped
/// again: only the turn id is rewritten to this turn's, because the rest of its
/// address already names the grandchild's delegating call. That is what keeps
/// the payload non-recursive at any depth.
///
/// A context report is dropped, and `None` is the honest answer rather than a
/// wrapped one. A child measures *its own* session, and a delegation that
/// filled its window says nothing about the conversation on screen — so there
/// is no address to give the frame and nothing sensible to show. The loop
/// already declines to emit one below the root, so this branch should be
/// unreachable; it exists because `ContextUsage` sits outside
/// `NestedAgentEvent`, which is what turns "a subagent must not report context"
/// from a convention into something the compiler checks here.
fn wrap_subagent_event(
    turn: &TurnScope,
    call_id: &str,
    binding: &SubagentBinding,
    session_key: &str,
    depth: usize,
    event: AgentEvent,
) -> Option<AgentEvent> {
    match event {
        AgentEvent::Subagent(mut body) => {
            body.turn_id.clone_from(&turn.turn_id);
            Some(AgentEvent::Subagent(body))
        }
        AgentEvent::ContextUsage(_) => None,
        AgentEvent::Nested(inner) => {
            Some(AgentEvent::Subagent(ghostai_protocol::SubagentEventBody {
                tag: ghostai_protocol::SubagentEventTag,
                turn_id: turn.turn_id.clone(),
                parent_session_key: turn.session_key.clone(),
                parent_call_id: call_id.to_owned(),
                agent_id: binding.agent_id.clone(),
                label: binding.label.clone(),
                session_key: session_key.to_owned(),
                depth: u64::try_from(depth).unwrap_or(u64::MAX),
                event: inner,
            }))
        }
    }
}
