//! LLM providers.
//!
//! The crate is organised around a claim: **a provider is data, and only a
//! wire protocol is code.** `PROVIDERS` is a table; `openai-chat` is the single
//! adapter that serves every entry in it today: Ollama, LM Studio, llama.cpp,
//! vLLM, OpenAI, OpenRouter, DeepSeek, Groq, xAI, and Gemini through its
//! compatibility endpoint. Adding a provider is a table entry. Adding a
//! *protocol* is an adapter, and there are only three left worth writing.
//!
//! Three rules the rest of the tree depends on:
//!
//! - **Failures are typed.** A provider error carries a `reason`, an HTTP
//!   status and the parameter the provider blamed, in the structured details
//!   of the one `WireError` every crate returns. Nothing searches an error
//!   message for "429" or "rate limit": a model that writes those words in
//!   its answer must not trigger a retry, and a provider that phrases its
//!   rejection differently must still be understood.
//! - **Retry and degradation live in one decorator.** `with_resilience` wraps
//!   both call styles over one ladder of `DegradationStep`s, so the streaming
//!   and non-streaming paths cannot drift apart, and each repair is a pure
//!   function testable without a provider at all.
//! - **Nothing here touches the network without being asked.** Every request
//!   goes to the base URL an operator configured, through one pooled client
//!   per provider instance.
//!
//! The provider base URL deliberately does *not* go through the SSRF guard.
//! That guard exists to stop the model from choosing a destination; a base URL
//! is operator configuration, and the common case is a model server on
//! loopback, the one host the guard is built to refuse. What is enforced
//! instead is narrower and real: an API key never goes over plain HTTP to a
//! public address.
#![forbid(unsafe_code)]

pub mod errors;
pub mod factory;
pub mod instances;
pub mod measure;
pub mod openai_chat;
pub mod registry;
pub mod resilience;
pub mod sse;
pub mod tokens;
pub mod types;
pub mod wire_encode;
pub mod wires;

#[cfg(feature = "testkit")]
pub mod testkit;

pub use errors::{
    ProviderError, ProviderErrorReason, TransportContext, TransportFault, WireErrorBody,
    classify_status, parse_retry_after, to_provider_error, transport_error,
};
pub use factory::{
    Connection, CreateProviderOptions, ProviderRef, Resilience, create_provider, resolve_connection,
};
pub use instances::{
    ProviderInstance, ResolveInstanceOptions, describe_instance, find_instance, instance_label,
    list_instances, next_instance_id, resolve_instance,
};
pub use measure::{estimate_message_tokens, estimate_tool_tokens};
pub use openai_chat::{
    OpenAiChatProvider, assert_usable_api_base, build_body, create_openai_chat_provider,
    tool_call_id,
};
pub use registry::{
    GatewayHints, MaxTokensParam, ModelOverride, PROVIDERS, ProviderSpec, ResolveProviderOptions,
    WIRE_PROTOCOLS, WireProtocol, describe_provider, find_builtin, find_gateway, find_provider,
    find_provider_by_model, is_provider_id, model_override_for, provider_ids, resolve_model_id,
    resolve_provider,
};
pub use resilience::{
    BackoffOptions, DEFAULT_DEGRADATION_STEPS, DegradationStep, JitterFn, NoticeFn, NoticeKind,
    ResilienceNotice, ResilienceOptions, Resilient, backoff_delay_ms, synthesise_stream,
    truncate_oldest_turns, with_resilience,
};
pub use sse::{MAX_SSE_FRAME_CHARS, SseEvent, SseOptions, SseParser, parse_sse, parse_sse_chunks};
pub use tokens::estimate_tokens;
pub use types::{
    BoxFuture, ChatProvider, ChatRequest, ChatResult, ChatStreamEvent, FinishReason, ToolChoice,
    WireAdapter, WireAdapterOptions, empty_usage,
};
pub use wires::{BUILTIN_WIRES, WireAdapters, builtin_wire, wire_adapter_for};
