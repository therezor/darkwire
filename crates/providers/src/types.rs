//! The provider interface every wire adapter implements.
//!
//! Two shapes matter, and both are chosen so the agent loop needs no
//! translation layer of its own:
//!
//! - A result carries an `AssistantMessage` from `ghostai-protocol`, the same
//!   canonical shape the session store persists. The loop appends it directly.
//!   An adapter-specific response type would mean every consumer converting,
//!   and conversions are where tool-call ids get lost.
//! - Streaming is a `Stream` ending in a `Done` event that carries the same
//!   complete result. Dropping the stream is how a consumer stops consuming,
//!   and an adapter releases its connection when that happens; a callback
//!   would make every one of those the adapter's problem instead.
//!
//! There is deliberately no `chat_with_retry` here. Resilience is a decorator
//! over this trait (`with_resilience`), so an adapter is only responsible for
//! speaking its wire correctly, and retry semantics exist once rather than once
//! per adapter and once per streaming variant.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::stream::BoxStream;
use ghostai_core::{Clock, Result};
use ghostai_protocol::{
    AssistantMessage, ChatMessage, ModelInfo, ReasoningEffort, ToolDefinition, Usage,
};
use ghostai_security::RandomSource;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::registry::ProviderSpec;

/// A boxed future, the return type of every async trait method here.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Whether the model may call tools this turn.
///
/// Not a specific-tool object: forcing one named tool is a single-purpose
/// feature (the heartbeat's `skip|run` decision) and it belongs to the caller
/// that needs it, once that caller exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// The model decides.
    Auto,
    /// No tool calls this turn.
    None,
    /// The model must call a tool.
    Required,
}

impl ToolChoice {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolChoice::Auto => "auto",
            ToolChoice::None => "none",
            ToolChoice::Required => "required",
        }
    }
}

/// Why the provider stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The model finished its answer.
    Stop,
    /// The model asked for tools to run.
    ToolCalls,
    /// The token cap was reached.
    Length,
    /// The provider's content filter intervened.
    ContentFilter,
}

/// A request, as the loop assembles it.
///
/// Every optional field is an `Option`, and that is what the degradation
/// ladder relies on: its whole job is to *remove* a parameter, and `None` is
/// the natural expression of that.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    /// The model id as configured, provider prefix included.
    pub model: String,
    /// Already windowed and legal: `history_for_llm` has run. An adapter
    /// encodes what it is given and never edits history, because the
    /// alignment rules that keep tool results paired live in one place and
    /// this is not it.
    pub messages: Vec<ChatMessage>,
    /// The tools on offer. Empty means none are sent.
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    /// Whether the model may use them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// The completion cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// How hard to ask the model to think.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// A stable identifier for the conversation this request belongs to.
    ///
    /// A hint for the provider's prompt cache, not a correctness input:
    /// caching itself works off the prefix bytes, and this only tells a load
    /// balancer to send requests that share a prefix to the machine already
    /// holding it. The loop passes the session key, so every iteration of
    /// every turn on one conversation routes together.
    ///
    /// Omitted rather than empty when there is nothing to say, and removable
    /// by the degradation ladder for endpoints that reject fields they do not
    /// know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_key: Option<String>,
}

impl ChatRequest {
    /// A request for `model` over `messages`, nothing else set.
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            messages,
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            reasoning_effort: None,
            cache_key: None,
        }
    }
}

/// What one request produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatResult {
    /// Canonical, and ready to append to history as-is.
    pub message: AssistantMessage,
    /// Why generation stopped.
    pub finish_reason: FinishReason,
    /// Token accounting, as the provider reported it.
    pub usage: Usage,
    /// As reported by the provider, which may differ from what was requested.
    pub model: String,
    /// Time spent emitting tokens: the first delta that carried content to the
    /// last one.
    ///
    /// Reported here rather than measured by the caller because this is the
    /// only scope that sees every kind of delta. A reply that is nothing but a
    /// tool call yields no text and no reasoning, so a consumer of
    /// [`ChatStreamEvent`] observes no deltas at all, while the provider spent
    /// real time generating the tool-call JSON and charged real completion
    /// tokens for it. Measured outside, those tokens would have no time under
    /// them and would inflate whatever rate was derived from the pair.
    ///
    /// Absent when there was nothing to measure: a non-streaming request, or a
    /// stream replayed from one. `0` is its own answer, a stream whose content
    /// arrived in a single frame, and means the same thing to a caller as
    /// absence, since neither is a measurement you can divide by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_ms: Option<f64>,
    /// Request to first content delta: queueing, weight loading and prompt
    /// eval.
    ///
    /// On a local server this is dominated by the model load, which is why it
    /// is worth reporting separately rather than folding into the figure
    /// above. A cold start puts tens of seconds here and changes nothing about
    /// the rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<f64>,
}

/// A streaming event.
///
/// `Text` and `Reasoning` are deltas: the consumer appends. `Done` arrives
/// exactly once, last, and carries the assembled result including tool calls
/// and usage, so a consumer that only wants the final message can ignore the
/// deltas entirely and still be correct.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatStreamEvent {
    /// A slice of the answer.
    Text(String),
    /// A slice of the model's thinking.
    Reasoning(String),
    /// The whole result. Exactly one, last.
    Done(ChatResult),
}

/// A provider: one endpoint, answering many requests.
///
/// Every method takes the turn's [`CancellationToken`]. The same token threads
/// from the transport through the loop, this request, tool execution and any
/// child process, so a stop anywhere stops everything.
pub trait ChatProvider: Send + Sync {
    /// The provider id, which is the spec's.
    fn id(&self) -> &str;
    /// The table entry this provider speaks for.
    fn spec(&self) -> &ProviderSpec;
    /// One request, one complete answer.
    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
        token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<ChatResult>>;
    /// One request, streamed. Ends with exactly one [`ChatStreamEvent::Done`]
    /// or with an error; dropping the stream releases the connection.
    fn stream(
        &self,
        request: ChatRequest,
        token: CancellationToken,
    ) -> BoxStream<'static, Result<ChatStreamEvent>>;
    /// The endpoint's model catalogue. Empty when it has none; an error only
    /// when asking failed.
    fn list_models<'a>(
        &'a self,
        token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<ModelInfo>>>;
    /// Releases the connection pool. Idempotent.
    fn close(&self) -> BoxFuture<'_, ()>;
}

impl std::fmt::Debug for dyn ChatProvider + '_ {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatProvider")
            .field("id", &self.id())
            .finish()
    }
}

/// What every wire adapter is handed.
///
/// The union of what a connection needs and nothing about a conversation: the
/// adapter is constructed once per provider instance and then answers many
/// requests, so anything that varies per turn belongs on [`ChatRequest`].
#[derive(Clone)]
pub struct WireAdapterOptions {
    /// The table entry.
    pub spec: ProviderSpec,
    /// From the credential vault, never from config. Absent for local servers.
    pub api_key: Option<String>,
    /// Overrides `spec.default_api_base`. Operator configuration, not model
    /// input.
    pub api_base: Option<String>,
    /// Headers every request carries, layered over the spec's own.
    pub extra_headers: IndexMap<String, String>,
    /// Time to first response header, in milliseconds. Not a cap on
    /// generation.
    pub request_timeout_ms: Option<u64>,
    /// Longest gap between stream chunks, in milliseconds, before the
    /// connection is considered dead.
    pub stream_idle_timeout_ms: Option<u64>,
    /// Tool-call ids for providers that omit them. Injected so tests are
    /// stable.
    pub random: Option<Arc<dyn RandomSource>>,
    /// What `generation_ms` and `first_token_ms` are read off. Injected so
    /// tests are stable, the same way `random` is: a spaced-out stream can
    /// then be asserted exactly instead of approximately.
    pub clock: Option<Arc<dyn Clock>>,
}

impl WireAdapterOptions {
    /// Options for `spec` with every connection setting left to its default.
    pub fn new(spec: ProviderSpec) -> WireAdapterOptions {
        WireAdapterOptions {
            spec,
            api_key: None,
            api_base: None,
            extra_headers: IndexMap::new(),
            request_timeout_ms: None,
            stream_idle_timeout_ms: None,
            random: None,
            clock: None,
        }
    }
}

impl std::fmt::Debug for WireAdapterOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireAdapterOptions")
            .field("spec", &self.spec.id)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("api_base", &self.api_base)
            .field("extra_headers", &self.extra_headers)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("stream_idle_timeout_ms", &self.stream_idle_timeout_ms)
            .finish_non_exhaustive()
    }
}

/// A wire protocol, as code.
///
/// [`ProviderSpec`] says which wire a provider speaks; this is the thing that
/// speaks it. Separating them is what lets a provider be *data*, the claim the
/// whole crate is organised around, and it is also the seam an extension
/// reaches to add a wire this build does not ship.
pub type WireAdapter =
    Arc<dyn Fn(WireAdapterOptions) -> Result<Arc<dyn ChatProvider>> + Send + Sync>;

/// A zeroed [`Usage`].
pub fn empty_usage() -> Usage {
    Usage::default()
}
