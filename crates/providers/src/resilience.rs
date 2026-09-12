//! One decorator for retry and degradation, over both streaming and not.
//!
//! One decorator rather than two near-identical routines, one per calling
//! style, which drift apart the moment either is fixed. `chat` and `stream`
//! share [`Recovery`]: the same ladder, the same backoff, the same abort
//! handling, differing only in how the underlying call is invoked.
//!
//! Two ideas carry the design.
//!
//! **Degradation is a declarative ladder.** A rejected request is not always a
//! dead request: the model may simply not accept a parameter this one carried.
//! Each [`DegradationStep`] states what it can repair and how, in the order
//! that costs the least: drop `prompt_cache_key`, then merge the trailing
//! turn, then `reasoning_effort`, then `tool_choice`, then images, then the
//! oldest turns. Every step is a pure function of the request, so each is
//! unit-testable on its own and the ladder is data rather than control flow.
//!
//! **A degradation is not a retry.** They have different budgets. Retries
//! exist for transient failures and are capped low because each one costs a
//! round trip against a provider that is already unhappy. Degradations are
//! bounded by the ladder itself (a step that has fired cannot fire again,
//! because the thing it removes is gone), so charging them to the retry budget
//! would leave a request unrepairable for want of an attempt it never needed.
//!
//! The honest limit: **a stream that has already emitted output is not
//! retried.** Restarting it would replay text the user has already seen, and
//! there is no way to un-send it. So recovery, including the non-streaming
//! fallback for a malformed event stream, applies only before the first delta.
//! After that the error propagates, which is the truthful outcome.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{BoxStream, StreamExt};
use ghostai_core::messages::{has_images, without_images};
use ghostai_core::{GhostError, Result, sleep};
use ghostai_protocol::{ChatMessage, ContentPart, ModelInfo, UserMessage};
use ghostai_security::{OsRandom, RandomSource};
use tokio_util::sync::CancellationToken;

use crate::errors::{ProviderError, ProviderErrorReason};
use crate::measure::estimate_message_tokens;
use crate::registry::ProviderSpec;
use crate::types::{BoxFuture, ChatProvider, ChatRequest, ChatResult, ChatStreamEvent};

/// A repair the ladder can apply to a rejected request.
///
/// `apply` returns `None` when there is nothing left to change, which is what
/// stops the ladder from looping: a step whose parameter is already absent
/// declines rather than producing an identical request.
#[derive(Debug, Clone, Copy)]
pub struct DegradationStep {
    /// Stable name, used to record that the step has fired.
    pub id: &'static str,
    /// What the user is told, once, when this step fires.
    pub description: &'static str,
    /// Whether this step could help with `error` against `request`.
    pub applies: fn(&ProviderError, &ChatRequest) -> bool,
    /// The repaired request, or `None` when there is nothing to repair.
    pub apply: fn(&ChatRequest) -> Option<ChatRequest>,
}

/// Reasons a *request-shaped* repair could help.
///
/// `UnsupportedParam` is the provider naming the field. `InvalidRequest` is
/// every local inference server, which returns a bare 400 with prose: no
/// code, no `param`. Including it is what makes the ladder work off-OpenAI,
/// and it is safe: each step only removes something the request actually
/// carried, so against a genuinely malformed request the ladder runs out and
/// the original error surfaces.
fn is_repairable(error: &ProviderError) -> bool {
    matches!(
        error.reason,
        ProviderErrorReason::UnsupportedParam | ProviderErrorReason::InvalidRequest
    )
}

/// Whether the provider blamed a specific parameter other than this step's.
///
/// Plural because one setting does not always reach the wire under one name:
/// `reasoning_effort` is sent as `reasoning_effort` almost everywhere and as
/// `reasoning` by OpenRouter, and a step that only knew the first name would
/// decline to fire on exactly the endpoint that named the second.
fn blames_other(error: &ProviderError, params: &[&str]) -> bool {
    error
        .param
        .as_deref()
        .is_some_and(|param| !param.is_empty() && !params.contains(&param))
}

fn drop_reasoning_effort_applies(error: &ProviderError, request: &ChatRequest) -> bool {
    is_repairable(error)
        && !blames_other(error, &["reasoning_effort", "reasoning"])
        && request.reasoning_effort.is_some()
}

fn drop_reasoning_effort_apply(request: &ChatRequest) -> Option<ChatRequest> {
    request.reasoning_effort?;
    Some(ChatRequest {
        reasoning_effort: None,
        ..request.clone()
    })
}

fn drop_tool_choice_applies(error: &ProviderError, request: &ChatRequest) -> bool {
    is_repairable(error) && !blames_other(error, &["tool_choice"]) && request.tool_choice.is_some()
}

/// Only `tool_choice` goes, never `tools`. Removing the tools would produce a
/// turn where the model cannot act and answers from memory instead: a wrong
/// answer rather than a failed request, which is worse.
fn drop_tool_choice_apply(request: &ChatRequest) -> Option<ChatRequest> {
    request.tool_choice?;
    Some(ChatRequest {
        tool_choice: None,
        ..request.clone()
    })
}

fn strip_images_applies(error: &ProviderError, request: &ChatRequest) -> bool {
    is_repairable(error) && request.messages.iter().any(has_images)
}

/// The question that came with the image is still worth asking; a text-only
/// answer beats losing the turn.
fn strip_images_apply(request: &ChatRequest) -> Option<ChatRequest> {
    if !request.messages.iter().any(has_images) {
        return None;
    }
    Some(ChatRequest {
        messages: request
            .messages
            .iter()
            .cloned()
            .map(without_images)
            .collect(),
        ..request.clone()
    })
}

/// How much of the history one `truncate_turns` step removes.
const TRUNCATION_FRACTION: f64 = 0.35;

/// Drops the oldest turns, keeping the request legal.
///
/// Measured in estimated tokens rather than message count, because message
/// counts say nothing: ten one-line exchanges and one pasted stack trace are
/// the same number and nowhere near the same request.
///
/// Measured on the *body*, through the same encoder the request is built
/// with. The distinction is not cosmetic here: a rejection for context length
/// is about what the provider received, and pricing the stored records
/// instead inflates the total by text that was never sent, the model's own
/// reasoning. `target` is a fraction of that total, so an inflated one is
/// reached while less real history has been cut, and the retry can fail on
/// length again and burn another rung of the ladder.
///
/// The system message is preserved wherever the cut lands (it is the agent's
/// instructions, not conversation) and the survivors are realigned so a `tool`
/// result never outlives the `assistant` message that requested it. Cutting
/// blindly by token budget is exactly how a context-length retry becomes a
/// provider 400 about an orphaned tool result.
pub fn truncate_oldest_turns(messages: &[ChatMessage]) -> Option<Vec<ChatMessage>> {
    let (system, body) = match messages.split_first() {
        Some((first @ ChatMessage::System(_), rest)) => (Some(first), rest),
        _ => (None, messages),
    };
    if body.len() <= 1 {
        return None;
    }

    let sizes: Vec<usize> = body.iter().map(estimate_message_tokens).collect();
    #[allow(
        clippy::cast_precision_loss,
        reason = "token estimates are far below 2^53"
    )]
    let target = sizes.iter().sum::<usize>() as f64 * TRUNCATION_FRACTION;

    // Never drop the turn this request is answering. That is the last message,
    // except when the loop has appended the prompt's runtime half as a
    // trailing user message, in which case the question is the one before it
    // and keeping only the last would leave a clock with nothing to answer.
    // One extra, not a whole run: two consecutive user turns is that shape,
    // and more than two is ordinary history nobody promised to keep.
    let floor = body.len() - if last_two_are_user(body) { 2 } else { 1 };
    let mut dropped = 0.0;
    let mut cut = 0;
    #[allow(
        clippy::cast_precision_loss,
        reason = "token estimates are far below 2^53"
    )]
    while cut < floor && dropped < target {
        dropped += sizes[cut] as f64;
        cut += 1;
    }
    if cut == 0 {
        return None;
    }

    let kept = align_to_legal_start(&body[cut..]);
    if kept.is_empty() {
        return None;
    }
    Some(system.into_iter().chain(kept).cloned().collect())
}

/// Drops leading `tool` messages whose `assistant` was cut away.
///
/// `find_legal_start` in `ghostai-core` answers the same question for stored
/// history; this is the same invariant applied to a request that a truncation
/// step just reshaped, and it deliberately does not import the history
/// windowing around it. That path owns the message window and tool-output
/// caps, neither of which applies to a request already on its way out.
fn align_to_legal_start(messages: &[ChatMessage]) -> &[ChatMessage] {
    let mut declared: HashSet<&str> = HashSet::new();
    let mut start = 0;
    for (index, message) in messages.iter().enumerate() {
        match message {
            ChatMessage::Assistant(assistant) => {
                declared.extend(assistant.tool_calls.iter().map(|call| call.id.as_str()));
            }
            ChatMessage::Tool(tool) if !declared.contains(tool.tool_call_id.as_str()) => {
                start = index + 1;
                declared.clear();
            }
            _ => {}
        }
    }
    &messages[start..]
}

fn drop_prompt_cache_key_applies(error: &ProviderError, request: &ChatRequest) -> bool {
    is_repairable(error)
        && !blames_other(error, &["prompt_cache_key"])
        && request.cache_key.is_some()
}

/// First on the ladder because it is the only repair that costs nothing. The
/// field is a routing hint for the provider's prompt cache; without it
/// requests still cache, they just may not land on the machine already
/// holding the prefix. Everything below this point costs the answer
/// something.
fn drop_prompt_cache_key_apply(request: &ChatRequest) -> Option<ChatRequest> {
    request.cache_key.as_ref()?;
    Some(ChatRequest {
        cache_key: None,
        ..request.clone()
    })
}

fn last_two_are_user(messages: &[ChatMessage]) -> bool {
    matches!(messages, [.., ChatMessage::User(_), ChatMessage::User(_)])
}

fn merge_trailing_user_applies(error: &ProviderError, request: &ChatRequest) -> bool {
    is_repairable(error) && last_two_are_user(&request.messages)
}

/// Folds a trailing user turn into the one before it.
///
/// The loop sends the prompt's volatile half as a trailing user message, so
/// the conversation stays inside the cached prefix. When the history already
/// ends with a user message that produces two in a row, which OpenAI and the
/// mainstream compatible endpoints accept, and strict-alternation shims
/// reject.
///
/// Merging is a repair and not the default shape because the default is the
/// one that caches: two messages keep the boundary between what the user said
/// and what the harness added, and a provider that accepts them needs no help.
fn merge_trailing_user_apply(request: &ChatRequest) -> Option<ChatRequest> {
    let [
        rest @ ..,
        ChatMessage::User(previous),
        ChatMessage::User(last),
    ] = request.messages.as_slice()
    else {
        return None;
    };
    let content: Vec<ContentPart> = previous
        .content
        .iter()
        .chain(last.content.iter())
        .cloned()
        .collect();
    let merged = ChatMessage::User(UserMessage {
        role: previous.role,
        content,
    });
    Some(ChatRequest {
        messages: rest
            .iter()
            .cloned()
            .chain(std::iter::once(merged))
            .collect(),
        ..request.clone()
    })
}

fn truncate_turns_applies(error: &ProviderError, _request: &ChatRequest) -> bool {
    error.reason == ProviderErrorReason::ContextLength
}

fn truncate_turns_apply(request: &ChatRequest) -> Option<ChatRequest> {
    truncate_oldest_turns(&request.messages).map(|messages| ChatRequest {
        messages,
        ..request.clone()
    })
}

/// The ladder, cheapest repair first.
///
/// Order is the policy: dropping a cache-routing hint costs nothing, merging
/// the trailing turn costs a message boundary, losing `reasoning_effort` costs
/// answer quality, losing images costs information the user supplied, and
/// losing turns costs the conversation's memory. Each step is tried only after
/// the one above it has failed or does not apply.
pub const DEFAULT_DEGRADATION_STEPS: [DegradationStep; 6] = [
    DegradationStep {
        id: "drop_prompt_cache_key",
        description: "retrying without prompt_cache_key",
        applies: drop_prompt_cache_key_applies,
        apply: drop_prompt_cache_key_apply,
    },
    DegradationStep {
        id: "merge_trailing_user",
        description: "retrying with the trailing turn merged",
        applies: merge_trailing_user_applies,
        apply: merge_trailing_user_apply,
    },
    DegradationStep {
        id: "drop_reasoning_effort",
        description: "retrying without reasoning_effort",
        applies: drop_reasoning_effort_applies,
        apply: drop_reasoning_effort_apply,
    },
    DegradationStep {
        id: "drop_tool_choice",
        description: "retrying without tool_choice",
        applies: drop_tool_choice_applies,
        apply: drop_tool_choice_apply,
    },
    DegradationStep {
        id: "strip_images",
        description: "retrying with images removed",
        applies: strip_images_applies,
        apply: strip_images_apply,
    },
    DegradationStep {
        id: "truncate_turns",
        description: "retrying with the oldest turns dropped",
        applies: truncate_turns_applies,
        apply: truncate_turns_apply,
    },
];

/// What kind of recovery a notice reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoticeKind {
    /// The request was rewritten by a ladder step.
    Degraded,
    /// The same request was repeated after a wait.
    Retry,
    /// An unreadable stream was answered in one piece instead.
    Fallback,
}

/// What the decorator did about a failure. Surfaced to the UI as a `notice`
/// event; never an error.
#[derive(Debug, Clone, PartialEq)]
pub struct ResilienceNotice {
    /// Degraded, retried or fell back.
    pub kind: NoticeKind,
    /// For a person.
    pub message: String,
    /// Which attempt at the same request this was.
    pub attempt: u32,
    /// The wait before a retry, so a test can assert the schedule as data.
    pub delay_ms: Option<u64>,
    /// The failure that prompted it.
    pub error: ProviderError,
}

/// A jitter factor in `[0, 1)`.
pub type JitterFn = Arc<dyn Fn() -> f64 + Send + Sync>;

/// A callback for notices.
pub type NoticeFn = Arc<dyn Fn(ResilienceNotice) + Send + Sync>;

/// How the decorator is configured. Every field defaults.
#[derive(Clone, Default)]
pub struct ResilienceOptions {
    /// Attempts at the *same* request. Degradations are budgeted separately.
    pub max_attempts: Option<u32>,
    /// The first backoff, before jitter.
    pub base_delay_ms: Option<u64>,
    /// The ceiling on any one wait, including one the provider asked for.
    pub max_delay_ms: Option<u64>,
    /// Replaces [`DEFAULT_DEGRADATION_STEPS`].
    pub steps: Option<Vec<DegradationStep>>,
    /// Jitter factor in `[0, 1)`. Injected rather than drawn from a global
    /// generator, so a test asserting a backoff schedule gets one.
    pub jitter: Option<JitterFn>,
    /// The randomness behind the default jitter.
    pub random: Option<Arc<dyn RandomSource>>,
    /// Told about every degradation, retry and fallback.
    pub on_notice: Option<NoticeFn>,
}

impl std::fmt::Debug for ResilienceOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResilienceOptions")
            .field("max_attempts", &self.max_attempts)
            .field("base_delay_ms", &self.base_delay_ms)
            .field("max_delay_ms", &self.max_delay_ms)
            .field("steps", &self.steps.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_BASE_DELAY_MS: u64 = 500;
const DEFAULT_MAX_DELAY_MS: u64 = 8_000;

/// The backoff parameters [`backoff_delay_ms`] reads.
#[derive(Clone)]
pub struct BackoffOptions {
    /// The first backoff, before jitter.
    pub base_delay_ms: u64,
    /// The ceiling on any one wait.
    pub max_delay_ms: u64,
    /// Jitter factor in `[0, 1)`.
    pub jitter: JitterFn,
}

/// Exponential backoff with full jitter, honouring `Retry-After` when given.
///
/// Full jitter rather than a fixed schedule because the failure that most
/// needs backing off, a shared rate limit, is the one where every client
/// retries at the same moment. A deterministic delay reconverges them into
/// the same spike.
pub fn backoff_delay_ms(attempt: u32, error: &ProviderError, options: &BackoffOptions) -> u64 {
    if let Some(retry_after) = error.retry_after_ms {
        return u64::try_from(retry_after.max(0))
            .unwrap_or(u64::MAX)
            .min(options.max_delay_ms);
    }
    let exponential = options
        .base_delay_ms
        .saturating_mul(1u64 << attempt.saturating_sub(1).min(62))
        .min(options.max_delay_ms);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "delays are small positive millisecond counts"
    )]
    let jittered = (exponential as f64 * (options.jitter)()).round() as u64;
    jittered
}

/// A uniform fraction in `[0.5, 1)` from `random`: full jitter, floored so a
/// retry never fires immediately.
fn jitter_from(random: Arc<dyn RandomSource>) -> JitterFn {
    Arc::new(move || {
        let mut bytes = [0u8; 4];
        random.fill(&mut bytes);
        0.5 + f64::from(u32::from_be_bytes(bytes)) / 2f64.powi(32) / 2.0
    })
}

struct Resolved {
    max_attempts: u32,
    backoff: BackoffOptions,
    steps: Vec<DegradationStep>,
    notify: Option<NoticeFn>,
}

impl Resolved {
    fn from(options: ResilienceOptions) -> Resolved {
        let jitter = options
            .jitter
            .unwrap_or_else(|| jitter_from(options.random.unwrap_or_else(|| Arc::new(OsRandom))));
        Resolved {
            max_attempts: options.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS),
            backoff: BackoffOptions {
                base_delay_ms: options.base_delay_ms.unwrap_or(DEFAULT_BASE_DELAY_MS),
                max_delay_ms: options.max_delay_ms.unwrap_or(DEFAULT_MAX_DELAY_MS),
                jitter,
            },
            steps: options
                .steps
                .unwrap_or_else(|| DEFAULT_DEGRADATION_STEPS.to_vec()),
            notify: options.on_notice,
        }
    }

    fn notify(&self, notice: ResilienceNotice) {
        if let Some(notify) = &self.notify {
            notify(notice);
        }
    }
}

/// The recovery state machine, shared by both call styles.
///
/// Extracted so `chat` and `stream` cannot disagree about what a 429 means.
/// It owns the mutable part (which steps have fired, which attempt this is,
/// and the request as it currently stands) and `recover` returns whether there
/// is anything left to try. The caller returns the error when it says no.
struct Recovery {
    request: ChatRequest,
    used: HashSet<&'static str>,
    attempt: u32,
    iteration: u32,
    /// Each step fires at most once, so the ladder is finite; the extra room
    /// is for the attempt that follows the last degradation.
    ceiling: u32,
}

impl Recovery {
    fn new(request: ChatRequest, config: &Resolved) -> Recovery {
        Recovery {
            request,
            used: HashSet::new(),
            attempt: 1,
            iteration: 0,
            ceiling: config.max_attempts
                + u32::try_from(config.steps.len()).unwrap_or(u32::MAX)
                + 1,
        }
    }

    async fn recover(
        &mut self,
        error: &ProviderError,
        config: &Resolved,
        token: &CancellationToken,
    ) -> Result<bool> {
        self.iteration += 1;
        // A cancelled turn is not a failure to recover from; retrying it would
        // ignore the one signal the user sent.
        if error.reason == ProviderErrorReason::Aborted || self.iteration >= self.ceiling {
            return Ok(false);
        }

        for step in &config.steps {
            if self.used.contains(step.id) || !(step.applies)(error, &self.request) {
                continue;
            }
            let Some(degraded) = (step.apply)(&self.request) else {
                continue;
            };
            self.used.insert(step.id);
            self.request = degraded;
            config.notify(ResilienceNotice {
                kind: NoticeKind::Degraded,
                message: step.description.to_owned(),
                attempt: self.attempt,
                delay_ms: None,
                error: error.clone(),
            });
            return Ok(true);
        }

        if !error.retryable || self.attempt >= config.max_attempts {
            return Ok(false);
        }

        let delay_ms = backoff_delay_ms(self.attempt, error, &config.backoff);
        config.notify(ResilienceNotice {
            kind: NoticeKind::Retry,
            message: format!("retrying in {delay_ms} ms after {}", error.reason),
            attempt: self.attempt,
            delay_ms: Some(delay_ms),
            error: error.clone(),
        });
        self.attempt += 1;
        // Aborting mid-backoff returns the `aborted` error here, which is the
        // intended exit: the token is the same one threaded into the request.
        sleep(Duration::from_millis(delay_ms), token).await?;
        Ok(true)
    }
}

/// Runs `run` against the request, recovering until the ladder and the retry
/// budget are both spent.
async fn attempt<T, F>(
    request: ChatRequest,
    config: &Resolved,
    token: &CancellationToken,
    run: impl Fn(ChatRequest) -> F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let mut recovery = Recovery::new(request, config);
    loop {
        match run(recovery.request.clone()).await {
            Ok(value) => return Ok(value),
            Err(raw) => {
                let error = ProviderError::of(&raw);
                if !recovery.recover(&error, config, token).await? {
                    return Err(raw);
                }
            }
        }
    }
}

/// A provider wrapped in retry, degradation and a streaming fallback.
///
/// It is a [`ChatProvider`], so it composes: the agent loop holds one trait
/// object whether or not anything is wrapped around it, and a test can drive
/// the bare adapter directly.
pub struct Resilient {
    inner: Arc<dyn ChatProvider>,
    config: Arc<Resolved>,
}

impl std::fmt::Debug for Resilient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resilient")
            .field("provider", &self.inner.id())
            .finish_non_exhaustive()
    }
}

/// Wraps `provider` with retry, degradation and a streaming fallback.
pub fn with_resilience(
    provider: Arc<dyn ChatProvider>,
    options: ResilienceOptions,
) -> Arc<dyn ChatProvider> {
    Arc::new(Resilient {
        inner: provider,
        config: Arc::new(Resolved::from(options)),
    })
}

impl ChatProvider for Resilient {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn spec(&self) -> &ProviderSpec {
        self.inner.spec()
    }

    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
        token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<ChatResult>> {
        Box::pin(async move {
            let inner = Arc::clone(&self.inner);
            attempt(request.clone(), &self.config, token, |req| {
                let inner = Arc::clone(&inner);
                async move { inner.chat(&req, token).await }
            })
            .await
        })
    }

    fn stream(
        &self,
        request: ChatRequest,
        token: CancellationToken,
    ) -> BoxStream<'static, Result<ChatStreamEvent>> {
        let state = StreamRecovery {
            inner: Arc::clone(&self.inner),
            config: Arc::clone(&self.config),
            token,
            recovery: Recovery::new(request, &self.config),
            current: None,
            emitted: false,
            pending: VecDeque::new(),
            finished: false,
        };
        futures::stream::unfold(state, |mut state| async move {
            state.next().await.map(|event| (event, state))
        })
        .boxed()
    }

    fn list_models<'a>(
        &'a self,
        token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<ModelInfo>>> {
        self.inner.list_models(token)
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        self.inner.close()
    }
}

/// The streaming path.
///
/// Events are forwarded as they arrive; buffering an attempt so it could be
/// replayed would remove the only reason to stream. `emitted` is the price of
/// that: once a delta has reached the consumer the turn is committed, and a
/// later failure can only be raised, never retried over the top of text the
/// user is already reading.
struct StreamRecovery {
    inner: Arc<dyn ChatProvider>,
    config: Arc<Resolved>,
    token: CancellationToken,
    recovery: Recovery,
    current: Option<BoxStream<'static, Result<ChatStreamEvent>>>,
    emitted: bool,
    pending: VecDeque<ChatStreamEvent>,
    finished: bool,
}

impl StreamRecovery {
    fn fail(&mut self, error: GhostError) -> Result<ChatStreamEvent> {
        self.finished = true;
        self.current = None;
        Err(error)
    }

    async fn next(&mut self) -> Option<Result<ChatStreamEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.finished {
                return None;
            }
            let current = match self.current.as_mut() {
                Some(current) => current,
                None => self.current.insert(
                    self.inner
                        .stream(self.recovery.request.clone(), self.token.child_token()),
                ),
            };
            match current.next().await {
                None => {
                    self.finished = true;
                    return None;
                }
                Some(Ok(event)) => {
                    self.emitted = true;
                    if matches!(event, ChatStreamEvent::Done(_)) {
                        self.finished = true;
                        self.current = None;
                    }
                    return Some(Ok(event));
                }
                Some(Err(raw)) => {
                    self.current = None;
                    if self.emitted {
                        return Some(self.fail(raw));
                    }
                    let error = ProviderError::of(&raw);
                    if error.reason == ProviderErrorReason::StreamParse {
                        return match self.fallback(&error).await {
                            Ok(()) => continue,
                            Err(error) => Some(self.fail(error)),
                        };
                    }
                    match self
                        .recovery
                        .recover(&error, &self.config, &self.token)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => return Some(self.fail(raw)),
                        Err(error) => return Some(self.fail(error)),
                    }
                }
            }
        }
    }

    /// Streaming is an optimisation. A stream that could not be read is still
    /// a request the provider can answer in one piece, and the consumer gets
    /// the same events either way, synthesised from the complete result.
    async fn fallback(&mut self, error: &ProviderError) -> Result<()> {
        self.config.notify(ResilienceNotice {
            kind: NoticeKind::Fallback,
            message: "stream unreadable, falling back to a single response".to_owned(),
            attempt: 1,
            delay_ms: None,
            error: error.clone(),
        });
        let inner = Arc::clone(&self.inner);
        let token = self.token.clone();
        let result = attempt(
            self.recovery.request.clone(),
            &self.config,
            &self.token,
            |req| {
                let inner = Arc::clone(&inner);
                let token = token.clone();
                async move { inner.chat(&req, &token).await }
            },
        )
        .await?;
        self.pending.extend(synthesise_stream(&result));
        self.finished = true;
        Ok(())
    }
}

/// Replays a non-streaming result as the events a streaming consumer expects.
pub fn synthesise_stream(result: &ChatResult) -> Vec<ChatStreamEvent> {
    let mut events = Vec::new();
    if let Some(reasoning) = result
        .message
        .reasoning
        .as_ref()
        .filter(|reasoning| !reasoning.is_empty())
    {
        events.push(ChatStreamEvent::Reasoning(reasoning.clone()));
    }
    for part in &result.message.content {
        if let ContentPart::Text(text) = part
            && !text.text.is_empty()
        {
            events.push(ChatStreamEvent::Text(text.text.clone()));
        }
    }
    events.push(ChatStreamEvent::Done(result.clone()));
    events
}
