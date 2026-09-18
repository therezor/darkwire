//! The `openai-chat` wire adapter.
//!
//! One adapter, ten providers: Ollama, LM Studio, llama.cpp, vLLM, OpenAI,
//! OpenRouter, DeepSeek, Groq, xAI and Gemini's compatibility endpoint all
//! speak `POST /chat/completions`. Everything that differs between them (base
//! URL, model-prefix handling, which name the token cap goes by) is a field in
//! the registry table, not a subtype here.
//!
//! The adapter is deliberately thin and does exactly three things: encode
//! canonical messages onto the wire, decode the wire back into a canonical
//! `AssistantMessage`, and turn a non-2xx response into a typed provider
//! error. It does not retry, does not degrade, does not truncate history and
//! does not repair anything. Those belong to `with_resilience` and to
//! `history_for_llm`, in one place each, rather than smeared across every
//! adapter that will follow.
//!
//! The first of those three lives next door, in `wire_encode`, because the
//! body's shape has a second reader: the context inspector prices what this
//! adapter would send, and the two must be the same function rather than two
//! descriptions of it. Decoding stays here; nothing else has any use for it.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use darkwire_core::{Clock, ErrorKind, Result, SystemClock, WireError};
use darkwire_protocol::{
    AssistantMessage, AssistantRole, ContentPart, ModelInfo, ReasoningEffort, TextPart, TextTag,
    ToolCall, Usage,
};
use darkwire_security::{
    AddressCategory, OsRandom, RandomSource, classify_address, parse_ip_literal,
};
use futures::stream::{BoxStream, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, Response, Url};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::errors::{
    ProviderError, ProviderErrorReason, TransportContext, WireErrorBody, classify_status,
    parse_retry_after, to_provider_error,
};
use crate::registry::{MaxTokensParam, ProviderSpec, model_override_for, resolve_model_id};
use crate::sse::{SseEvent, SseOptions, parse_sse};
use crate::types::{
    BoxFuture, ChatProvider, ChatRequest, ChatResult, ChatStreamEvent, FinishReason,
    WireAdapterOptions, empty_usage,
};
use crate::wire_encode::{encode_message, encode_tools};

/// Time to first byte. Generous, because a cold local model loads weights
/// first.
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 120_000;
/// Between chunks, not total: a long answer is not a hung connection.
const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 120_000;
/// TCP connect. Nothing legitimate takes longer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How much of an error body reaches the log line.
///
/// Bounded: an HTML error page from a proxy is not a useful log line, and the
/// full body would be one megabyte of it.
const ERROR_DETAIL_UNITS: usize = 500;

/// Rejects a configuration that would put an API key on the wire in
/// cleartext.
///
/// This is the one security decision the adapter owns. A base URL is operator
/// configuration and never model input, so it does not go through the SSRF
/// guard: that guard exists to stop the *model* from choosing a destination,
/// and blocking a local model server on loopback would be that guard
/// misfiring on the one host it is meant to trust.
///
/// What remains is worth catching: `http://` to a public address with an
/// `Authorization` header attached hands the key to every hop in between.
/// Plain HTTP stays allowed without a key (llama.cpp on a LAN) and to loopback
/// or private ranges (every local server), which covers the legitimate cases.
pub fn assert_usable_api_base(raw_base: &str, has_api_key: bool) -> Result<Url> {
    let url = Url::parse(raw_base).map_err(|error| {
        WireError::new(
            ErrorKind::Config,
            format!("Provider apiBase is not a URL: {raw_base}"),
        )
        .with_source(error)
    })?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(WireError::new(
            ErrorKind::Config,
            format!(
                "Provider apiBase must be http or https, got \"{}:\"",
                url.scheme()
            ),
        ));
    }
    if url.scheme() == "https" || !has_api_key {
        return Ok(url);
    }

    let host = url.host_str().unwrap_or_default().to_lowercase();
    if host == "localhost" {
        return Ok(url);
    }
    let category = parse_ip_literal(&host)
        .as_ref()
        .and_then(classify_address)
        .map(|range| range.category);
    if matches!(
        category,
        Some(AddressCategory::Loopback | AddressCategory::Private)
    ) {
        return Ok(url);
    }

    Err(WireError::new(
        ErrorKind::Config,
        format!(
            "Refusing to send an API key over plain HTTP to {host}. Use https, or configure the \
             provider without a key."
        ),
    ))
}

/// Trailing slashes are common in pasted base URLs and produce `//` on join.
fn join_path(base: &Url, path: &str) -> String {
    format!("{}/{path}", base.as_str().trim_end_matches('/'))
}

/// What "do not think" is, absent a `reasoning_off_body` on the spec.
///
/// OpenAI's own extension of `reasoning_effort`, and the closest thing the
/// OpenAI-compatible range has to a convention. An endpoint that has never
/// heard of it answers with an `unsupported_param` or a bare 400, which is
/// exactly the shape `drop_reasoning_effort` repairs.
fn default_reasoning_off_body() -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("reasoning_effort".into(), Value::String("none".into()));
    body
}

fn reasoning_effort_str(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Off => "off",
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
    }
}

/// The `chat/completions` body for `request` at `spec`.
///
/// Public because it is the contract the wire fixture pins; the adapter posts
/// exactly this.
pub fn build_body(spec: &ProviderSpec, request: &ChatRequest, stream: bool) -> Map<String, Value> {
    let model = resolve_model_id(spec, &request.model);
    let override_ = model_override_for(spec, &request.model);
    let max_tokens = override_.and_then(|o| o.max_tokens).or(request.max_tokens);
    let temperature = override_
        .and_then(|o| o.temperature)
        .or(request.temperature);

    let mut body = Map::new();
    body.insert("model".into(), Value::String(model));
    body.insert(
        "messages".into(),
        serde_json::to_value(
            request
                .messages
                .iter()
                .map(encode_message)
                .collect::<Vec<_>>(),
        )
        .unwrap_or(Value::Array(Vec::new())),
    );

    if let Some(max_tokens) = max_tokens {
        let param = match spec.max_tokens_param {
            MaxTokensParam::MaxTokens => "max_tokens",
            MaxTokensParam::MaxCompletionTokens => "max_completion_tokens",
        };
        body.insert(param.into(), Value::from(max_tokens.max(1)));
    }
    if let Some(temperature) = temperature {
        body.insert("temperature".into(), Value::from(temperature));
    }
    // `off` is a value this project made up, not one any wire accepts, so it
    // is translated rather than sent. Everything else is already the wire's
    // own vocabulary and goes through as it is.
    match request.reasoning_effort {
        Some(ReasoningEffort::Off) => {
            let off = spec
                .reasoning_off_body
                .clone()
                .unwrap_or_else(default_reasoning_off_body);
            body.extend(off);
        }
        Some(effort) => {
            body.insert(
                "reasoning_effort".into(),
                Value::String(reasoning_effort_str(effort).into()),
            );
        }
        None => {}
    }
    // Only where the table says the provider caches prompts. It is an optional
    // routing hint everywhere it is understood and an unknown field everywhere
    // else, and sending unknown fields to endpoints that have no use for them
    // is how the degradation ladder ends up doing work that need not happen.
    if spec.supports_prompt_caching
        && let Some(cache_key) = &request.cache_key
    {
        body.insert("prompt_cache_key".into(), Value::String(cache_key.clone()));
    }

    if !request.tools.is_empty() {
        body.insert(
            "tools".into(),
            serde_json::to_value(encode_tools(&request.tools)).unwrap_or(Value::Array(Vec::new())),
        );
        if let Some(choice) = request.tool_choice {
            body.insert("tool_choice".into(), Value::String(choice.as_str().into()));
        }
    }

    if stream {
        body.insert("stream".into(), Value::Bool(true));
        // Without this, a streamed turn reports no usage at all and the
        // session's token accounting silently only counts the non-streaming
        // calls.
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }

    body
}

// Decoding

/// `finish_reason` is unreliable in exactly one direction: several servers
/// report `stop` on a turn that emitted tool calls. The tool calls are the
/// fact; the label is a claim about them, so the fact wins.
fn decode_finish_reason(raw: Option<&str>, has_tool_calls: bool) -> FinishReason {
    if has_tool_calls {
        return FinishReason::ToolCalls;
    }
    match raw {
        Some("tool_calls" | "function_call") => FinishReason::ToolCalls,
        Some("length" | "max_tokens") => FinishReason::Length,
        Some("content_filter") => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

/// Content arrives as a string, or as parts from providers that mirror the
/// input shape.
fn decode_content(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn str_field<'a>(record: Option<&'a Value>, key: &str) -> Option<&'a str> {
    record?.get(key)?.as_str()
}

fn num_field(record: Option<&Value>, key: &str) -> Option<u64> {
    let number = record?.get(key)?;
    number.as_u64().or_else(|| {
        number
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            // Token counts are whole numbers; a float here is a provider being
            // loose with its types, not a fraction of a token.
            .map(|value| {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "guarded finite and non-negative above"
                )]
                let whole = value as u64;
                whole
            })
    })
}

/// DeepSeek and friends use `reasoning_content`; OpenRouter uses `reasoning`.
fn decode_reasoning(record: Option<&Value>) -> String {
    str_field(record, "reasoning_content")
        .or_else(|| str_field(record, "reasoning"))
        .unwrap_or_default()
        .to_owned()
}

fn decode_usage(record: Option<&Value>) -> Usage {
    let Some(record) = record.filter(|value| value.is_object()) else {
        return empty_usage();
    };
    let prompt = num_field(Some(record), "prompt_tokens").unwrap_or(0);
    let completion = num_field(Some(record), "completion_tokens").unwrap_or(0);
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: num_field(Some(record), "total_tokens").unwrap_or(prompt + completion),
        cached_tokens: num_field(record.get("prompt_tokens_details"), "cached_tokens"),
        reasoning_tokens: num_field(record.get("completion_tokens_details"), "reasoning_tokens"),
    }
}

fn assistant_of(text: String, reasoning: String, tool_calls: Vec<ToolCall>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole,
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![ContentPart::Text(TextPart { tag: TextTag, text })]
        },
        tool_calls,
        reasoning: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
    }
}

/// A tool-call id for a provider that sent none: `call_` plus the first
/// sixteen hex digits of a version-4 UUID drawn from `random`.
pub fn tool_call_id(random: &dyn RandomSource) -> String {
    let mut bytes = [0u8; 8];
    random.fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    let hex = bytes
        .iter()
        .fold(String::with_capacity(16), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
    format!("call_{hex}")
}

fn decode_tool_calls(raw: Option<&Value>, random: &dyn RandomSource) -> Vec<ToolCall> {
    let Some(entries) = raw.and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let function = entry.get("function");
            let name = str_field(function, "name").filter(|name| !name.is_empty())?;
            let arguments = function.and_then(|f| f.get("arguments"));
            Some(ToolCall {
                id: str_field(Some(entry), "id")
                    .map_or_else(|| tool_call_id(random), str::to_owned),
                name: name.to_owned(),
                // Verbatim. Some providers send an object here rather than a
                // string; re-serialising keeps the field's contract without
                // judging its contents.
                arguments_json: match arguments {
                    Some(Value::String(text)) => text.clone(),
                    Some(other) => serde_json::to_string(other).unwrap_or_else(|_| "{}".into()),
                    None => "{}".into(),
                },
            })
        })
        .collect()
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments_json: String,
}

/// Folds tool-call deltas into their accumulators.
///
/// `index` is what pairs a fragment with its call, and it is the only reliable
/// key: `id` and `name` arrive once, in the first fragment, and every fragment
/// after that carries nothing but a slice of the argument string. Keying on
/// `id` would put every continuation into its own bucket.
fn accumulate_tool_calls(partials: &mut BTreeMap<u64, PartialToolCall>, deltas: Option<&Value>) {
    let Some(entries) = deltas.and_then(Value::as_array) else {
        return;
    };
    for entry in entries {
        if !entry.is_object() {
            continue;
        }
        // A provider that omits `index` sends one call per delta, so appending
        // is the only reading that does not merge two distinct calls into one.
        let index = num_field(Some(entry), "index").unwrap_or(partials.len() as u64);
        let partial = partials.entry(index).or_default();
        if let Some(id) = str_field(Some(entry), "id").filter(|id| !id.is_empty()) {
            id.clone_into(&mut partial.id);
        }
        let function = entry.get("function");
        partial
            .name
            .push_str(str_field(function, "name").unwrap_or_default());
        partial
            .arguments_json
            .push_str(str_field(function, "arguments").unwrap_or_default());
    }
}

fn ms_of(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The first `n` UTF-16 units of `text`, on a character boundary.
fn head_units(text: &str, n: usize) -> String {
    let mut units = 0;
    text.chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= n
        })
        .collect()
}

// The adapter

/// The connection state one instance shares across every request.
struct Inner {
    spec: ProviderSpec,
    base: Url,
    headers: HeaderMap,
    request_timeout: Duration,
    stream_idle_timeout: Duration,
    random: Arc<dyn RandomSource>,
    clock: Arc<dyn Clock>,
    /// Built on first use and dropped by `close`, so an instance that never
    /// sends never opens a pool.
    client: Mutex<Option<reqwest::Client>>,
}

/// The `openai-chat` adapter for one endpoint.
pub struct OpenAiChatProvider {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for OpenAiChatProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiChatProvider")
            .field("spec", &self.inner.spec.id)
            .field("base", &self.inner.base)
            .finish_non_exhaustive()
    }
}

/// Builds an [`OpenAiChatProvider`]. The `WireAdapter` for `openai-chat`.
pub fn create_openai_chat_provider(options: WireAdapterOptions) -> Result<Arc<dyn ChatProvider>> {
    Ok(Arc::new(OpenAiChatProvider::new(options)?))
}

fn header_pair(name: &str, value: &str) -> Result<(HeaderName, HeaderValue)> {
    let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
        WireError::new(ErrorKind::Config, format!("Invalid header name: {name}")).with_source(error)
    })?;
    let value = HeaderValue::from_str(value).map_err(|error| {
        WireError::new(
            ErrorKind::Config,
            format!("Invalid value for header {name}"),
        )
        .with_source(error)
    })?;
    Ok((name, value))
}

impl OpenAiChatProvider {
    /// One provider on `options`. A `config` error for a base URL that is
    /// missing, unparseable or would leak the key.
    pub fn new(options: WireAdapterOptions) -> Result<OpenAiChatProvider> {
        let spec = options.spec;
        let api_key = options.api_key.filter(|key| !key.is_empty());
        let raw_base = options
            .api_base
            .clone()
            .or_else(|| spec.default_api_base.clone())
            .unwrap_or_default();
        if raw_base.is_empty() {
            return Err(WireError::new(
                ErrorKind::Config,
                format!(
                    "Provider \"{}\" has no apiBase; set one in configuration.",
                    spec.id
                ),
            ));
        }
        let base = assert_usable_api_base(&raw_base, api_key.is_some())?;

        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        );
        for (name, value) in spec
            .default_headers
            .iter()
            .chain(options.extra_headers.iter())
        {
            let (name, value) = header_pair(name, value)?;
            headers.insert(name, value);
        }
        if let Some(key) = &api_key {
            let mut value = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|error| {
                WireError::new(ErrorKind::Config, "API key is not a valid header value")
                    .with_source(error)
            })?;
            value.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }

        Ok(OpenAiChatProvider {
            inner: Arc::new(Inner {
                spec,
                base,
                headers,
                request_timeout: Duration::from_millis(
                    options
                        .request_timeout_ms
                        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
                ),
                stream_idle_timeout: Duration::from_millis(
                    options
                        .stream_idle_timeout_ms
                        .unwrap_or(DEFAULT_STREAM_IDLE_TIMEOUT_MS),
                ),
                random: options.random.unwrap_or_else(|| Arc::new(OsRandom)),
                clock: options.clock.unwrap_or_else(|| Arc::new(SystemClock)),
                client: Mutex::new(None),
            }),
        })
    }
}

impl Inner {
    fn context(&self, url: &str) -> TransportContext {
        TransportContext {
            url: Some(url.to_owned()),
            label: Some(self.spec.display_name.clone()),
        }
    }

    fn client(&self) -> Result<reqwest::Client> {
        let mut slot = self
            .client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(client) = slot.as_ref() {
            return Ok(client.clone());
        }
        // Both timeouts are *idle* timeouts. A wall-clock cap on a turn
        // belongs to the agent loop, which is the only layer that knows what
        // the turn is for.
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(self.stream_idle_timeout)
            .build()
            .map_err(|error| {
                WireError::new(ErrorKind::Internal, "Could not build the HTTP client")
                    .with_source(error)
            })?;
        *slot = Some(client.clone());
        Ok(client)
    }

    fn aborted(&self) -> WireError {
        ProviderError::new(ProviderErrorReason::Aborted, "Request aborted")
            .with_provider(self.spec.id.clone())
            .into_wire()
    }

    /// Turns a non-2xx into a typed error, reading the provider's own error
    /// object.
    async fn failure(&self, response: Response, url: &str) -> WireError {
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let text = response.text().await.unwrap_or_default();
        let parsed: Option<Value> = serde_json::from_str(&text).ok();
        let error = parsed
            .as_ref()
            .filter(|value| value.is_object())
            .map(|body| body.get("error").filter(|e| e.is_object()).unwrap_or(body));
        let wire = error.map(WireErrorBody::from_value).unwrap_or_default();
        let reason = classify_status(status, Some(&wire));
        let retry_after_ms = parse_retry_after(retry_after.as_deref(), self.clock.now_ms());
        let detail = wire
            .message
            .clone()
            .unwrap_or_else(|| head_units(&text, ERROR_DETAIL_UNITS));
        let message = if detail.is_empty() {
            format!("{} request failed ({status})", self.spec.id)
        } else {
            format!("{} request failed ({status}): {detail}", self.spec.id)
        };
        ProviderError::new(reason, message)
            .with_provider(self.spec.id.clone())
            .with_status(status)
            .with_code(wire.code)
            .with_param(wire.param)
            .with_retry_after_ms(retry_after_ms)
            .with_detail("url", url)
            .into_wire()
    }

    /// Sends one request and returns a 2xx response, or the typed failure.
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
        token: &CancellationToken,
    ) -> Result<(Response, String)> {
        let url = join_path(&self.base, path);
        let context = self.context(&url);
        let client = self.client()?;
        let mut request = client.request(method, &url).headers(self.headers.clone());
        if let Some(body) = body {
            request = request.body(body);
        }
        if token.is_cancelled() {
            return Err(self.aborted());
        }
        let sent = tokio::select! {
            () = token.cancelled() => return Err(self.aborted()),
            sent = tokio::time::timeout(self.request_timeout, request.send()) => sent,
        };
        let response = match sent {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                return Err(to_provider_error(&error, &self.spec.id, &context).into_wire());
            }
            Err(_elapsed) => {
                return Err(ProviderError::new(
                    ProviderErrorReason::Timeout,
                    format!(
                        "Could not reach {} at {}. It accepted the request and never replied.",
                        self.spec.display_name,
                        self.base.origin().ascii_serialization()
                    ),
                )
                .with_provider(self.spec.id.clone())
                .with_detail("url", url.as_str())
                .into_wire());
            }
        };
        if !response.status().is_success() {
            return Err(self.failure(response, &url).await);
        }
        Ok((response, url))
    }

    async fn chat(&self, request: &ChatRequest, token: &CancellationToken) -> Result<ChatResult> {
        let body = serde_json::to_string(&build_body(&self.spec, request, false))
            .unwrap_or_else(|_| "{}".into());
        let (response, url) = self
            .send(Method::POST, "chat/completions", Some(body), token)
            .await?;
        let status = response.status();
        // A body that fails mid-download is a transport failure, not a bad
        // response; `to_provider_error` is what makes the difference visible
        // to the caller.
        let text = tokio::select! {
            () = token.cancelled() => return Err(self.aborted()),
            text = response.text() => text.map_err(|error| {
                to_provider_error(&error, &self.spec.id, &self.context(&url)).into_wire()
            })?,
        };
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let choice = parsed
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .filter(|choice| choice.is_object());
        let Some(choice) = choice else {
            // A 200 with no choice is a provider-side glitch, not a bad
            // request: classified as `server` so it is retried rather than
            // surfaced.
            return Err(ProviderError::new(
                ProviderErrorReason::Server,
                format!("{} returned no choices", self.spec.id),
            )
            .with_provider(self.spec.id.clone())
            .with_status(status.as_u16())
            .with_detail("body", head_units(&text, ERROR_DETAIL_UNITS))
            .into_wire());
        };

        let message = choice.get("message");
        let tool_calls = decode_tool_calls(
            message.and_then(|m| m.get("tool_calls")),
            self.random.as_ref(),
        );
        let has_tool_calls = !tool_calls.is_empty();
        Ok(ChatResult {
            message: assistant_of(
                decode_content(message.and_then(|m| m.get("content"))),
                decode_reasoning(message),
                tool_calls,
            ),
            finish_reason: decode_finish_reason(
                str_field(Some(choice), "finish_reason"),
                has_tool_calls,
            ),
            usage: decode_usage(parsed.get("usage")),
            model: str_field(Some(&parsed), "model")
                .map_or_else(|| request.model.clone(), str::to_owned),
            generation_ms: None,
            first_token_ms: None,
        })
    }

    async fn list_models(&self, token: &CancellationToken) -> Result<Vec<ModelInfo>> {
        let (response, url) = self.send(Method::GET, "models", None, token).await?;
        let text = response.text().await.map_err(|error| {
            to_provider_error(&error, &self.spec.id, &self.context(&url)).into_wire()
        })?;
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok(parsed
            .get("data")
            .and_then(Value::as_array)
            .map(|data| {
                data.iter()
                    .filter_map(|entry| str_field(Some(entry), "id"))
                    .filter(|id| !id.is_empty())
                    .map(|id| ModelInfo {
                        id: id.to_owned(),
                        provider_id: self.spec.id.clone(),
                        provider_type: None,
                        display_name: None,
                        context_window_tokens: None,
                        supports_tools: None,
                        supports_vision: None,
                        supports_reasoning: None,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Opens the stream: the request, then the SSE reader over its body.
    async fn open_stream(
        self: &Arc<Inner>,
        request: &ChatRequest,
        token: &CancellationToken,
    ) -> Result<BoxStream<'static, Result<SseEvent>>> {
        let body = serde_json::to_string(&build_body(&self.spec, request, true))
            .unwrap_or_else(|_| "{}".into());
        let (response, url) = self
            .send(Method::POST, "chat/completions", Some(body), token)
            .await?;
        let inner = Arc::clone(self);
        let context = self.context(&url);
        // Everything the socket can raise while the stream is being read (a
        // reset connection, an idle timeout) arrives as the client's error.
        // The adapter's contract is that it only ever raises a typed provider
        // error, so the conversion happens once, on every chunk.
        let bytes = response.bytes_stream().map(move |chunk| {
            chunk.map_err(|error| to_provider_error(&error, &inner.spec.id, &context).into_wire())
        });
        Ok(parse_sse(
            bytes,
            SseOptions {
                provider_id: Some(self.spec.id.clone()),
                max_frame_chars: None,
            },
        ))
    }
}

/// Everything a streaming turn accumulates between the first frame and
/// `Done`.
struct StreamState {
    inner: Arc<Inner>,
    request: ChatRequest,
    token: CancellationToken,
    /// `None` until the request has been sent.
    events: Option<BoxStream<'static, Result<SseEvent>>>,
    pending: VecDeque<ChatStreamEvent>,
    finished: bool,
    requested_at: Duration,
    text: String,
    reasoning: String,
    finish_reason: Option<String>,
    usage: Usage,
    model: String,
    partials: BTreeMap<u64, PartialToolCall>,
    /// When the first frame carrying real content arrived, and the last.
    first_content_at: Option<Duration>,
    last_content_at: Option<Duration>,
}

impl StreamState {
    fn new(inner: Arc<Inner>, request: ChatRequest, token: CancellationToken) -> StreamState {
        // Before the request, not after its headers land. What
        // `first_token_ms` is for is the wait before anything is generated,
        // and on a cold local server most of that wait is weight loading that
        // happens while this call is outstanding. Starting the clock on the
        // response would measure everything except the part worth measuring.
        let requested_at = inner.clock.monotonic();
        let model = request.model.clone();
        StreamState {
            inner,
            request,
            token,
            events: None,
            pending: VecDeque::new(),
            finished: false,
            requested_at,
            text: String::new(),
            reasoning: String::new(),
            finish_reason: None,
            usage: empty_usage(),
            model,
            partials: BTreeMap::new(),
            first_content_at: None,
            last_content_at: None,
        }
    }

    /// Marks a frame that carried something the model generated.
    ///
    /// Guarded on *content* rather than called per chunk, because an
    /// OpenAI-compatible server routinely opens with a role-only frame
    /// (`delta: {role: "assistant"}`) and closes with a usage-only one. Timing
    /// from the opening frame would put the entire prompt-eval wait inside the
    /// generation window: the same mistake this whole measurement exists to
    /// undo, just one layer down.
    fn mark_content(&mut self) {
        let now = self.inner.clock.monotonic();
        self.last_content_at = Some(now);
        self.first_content_at.get_or_insert(now);
    }

    fn fail(&mut self, error: WireError) -> Result<ChatStreamEvent> {
        self.finished = true;
        self.events = None;
        Err(error)
    }

    /// The next event, or `None` once `Done` has gone out.
    async fn next(&mut self) -> Option<Result<ChatStreamEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.finished {
                return None;
            }
            if self.events.is_none() {
                match self.inner.open_stream(&self.request, &self.token).await {
                    Ok(events) => self.events = Some(events),
                    Err(error) => return Some(self.fail(error)),
                }
            }
            let events = self.events.as_mut()?;
            let frame = tokio::select! {
                () = self.token.cancelled() => {
                    let aborted = self.inner.aborted();
                    return Some(self.fail(aborted));
                }
                frame = events.next() => frame,
            };
            match frame {
                None => self.complete(),
                Some(Err(error)) => return Some(self.fail(error)),
                Some(Ok(event)) if event.data == "[DONE]" => self.complete(),
                Some(Ok(event)) => {
                    if let Err(error) = self.consume(&event.data) {
                        return Some(self.fail(error));
                    }
                }
            }
        }
    }

    fn complete(&mut self) {
        self.finished = true;
        self.events = None;
        let tool_calls: Vec<ToolCall> = std::mem::take(&mut self.partials)
            .into_values()
            .map(|partial| ToolCall {
                id: if partial.id.is_empty() {
                    tool_call_id(self.inner.random.as_ref())
                } else {
                    partial.id
                },
                name: partial.name,
                arguments_json: partial.arguments_json,
            })
            .collect();
        let has_tool_calls = !tool_calls.is_empty();
        // Omitted rather than zeroed when no frame carried content, so a
        // caller can tell "nothing was generated" from "generated instantly".
        // Fractional on purpose: the caller sums these across a turn and
        // rounds once, and rounding here would lose most of a millisecond per
        // request to no one's benefit.
        let timings = self.first_content_at.zip(self.last_content_at);
        self.pending.push_back(ChatStreamEvent::Done(ChatResult {
            message: assistant_of(
                std::mem::take(&mut self.text),
                std::mem::take(&mut self.reasoning),
                tool_calls,
            ),
            finish_reason: decode_finish_reason(self.finish_reason.as_deref(), has_tool_calls),
            usage: self.usage,
            model: std::mem::take(&mut self.model),
            generation_ms: timings.map(|(first, last)| ms_of(last.saturating_sub(first))),
            first_token_ms: timings
                .map(|(first, _)| ms_of(first.saturating_sub(self.requested_at))),
        }));
    }

    /// One frame's payload, folded into the accumulators.
    fn consume(&mut self, data: &str) -> Result<()> {
        let spec_id = self.inner.spec.id.clone();
        let chunk: Value = serde_json::from_str(data).unwrap_or(Value::Null);
        if !chunk.is_object() {
            return Err(ProviderError::new(
                ProviderErrorReason::StreamParse,
                format!("{spec_id} sent a stream frame that is not JSON"),
            )
            .with_provider(spec_id)
            .with_detail("frame", head_units(data, 200))
            .into_wire());
        }
        // An error can arrive *inside* a 200 stream: providers do this when
        // the failure is discovered after the headers are already on the
        // wire.
        if let Some(inline) = chunk.get("error").filter(|e| e.is_object()) {
            // `code` is a string enum in OpenAI's schema and an HTTP status in
            // OpenRouter's. Both readings are attempted, neither is guessed at.
            let wire = WireErrorBody {
                message: None,
                kind: None,
                code: str_field(Some(inline), "code").map(str::to_owned),
                param: str_field(Some(inline), "param").map(str::to_owned),
            };
            let status = num_field(Some(inline), "code")
                .and_then(|code| u16::try_from(code).ok())
                .unwrap_or(400);
            return Err(ProviderError::new(
                classify_status(status, Some(&wire)),
                format!(
                    "{spec_id} stream error: {}",
                    str_field(Some(inline), "message").unwrap_or("unknown")
                ),
            )
            .with_provider(spec_id)
            .with_code(wire.code)
            .with_param(wire.param)
            .into_wire());
        }

        if let Some(model) = str_field(Some(&chunk), "model") {
            model.clone_into(&mut self.model);
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            self.usage = decode_usage(Some(usage));
        }

        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .filter(|choice| choice.is_object());
        let Some(choice) = choice else {
            return Ok(());
        };
        if let Some(reason) = str_field(Some(choice), "finish_reason") {
            self.finish_reason = Some(reason.to_owned());
        }

        let delta = choice.get("delta");
        let delta_text = decode_content(delta.and_then(|d| d.get("content")));
        if !delta_text.is_empty() {
            self.mark_content();
            self.text.push_str(&delta_text);
            self.pending.push_back(ChatStreamEvent::Text(delta_text));
        }
        let delta_reasoning = decode_reasoning(delta);
        if !delta_reasoning.is_empty() {
            self.mark_content();
            self.reasoning.push_str(&delta_reasoning);
            self.pending
                .push_back(ChatStreamEvent::Reasoning(delta_reasoning));
        }
        // Marked even though nothing is yielded for it. A reply that is
        // nothing but a tool call streams its JSON like any other output and
        // is charged for as completion tokens, but `ChatStreamEvent` has no
        // shape to carry it, so a consumer counting deltas sees an instant
        // response. Timing it here is what keeps those tokens from being
        // divided by somebody else's window.
        let delta_tool_calls = delta.and_then(|d| d.get("tool_calls"));
        if delta_tool_calls
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty())
        {
            self.mark_content();
        }
        accumulate_tool_calls(&mut self.partials, delta_tool_calls);
        Ok(())
    }
}

impl ChatProvider for OpenAiChatProvider {
    fn id(&self) -> &str {
        &self.inner.spec.id
    }

    fn spec(&self) -> &ProviderSpec {
        &self.inner.spec
    }

    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
        token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<ChatResult>> {
        Box::pin(self.inner.chat(request, token))
    }

    fn stream(
        &self,
        request: ChatRequest,
        token: CancellationToken,
    ) -> BoxStream<'static, Result<ChatStreamEvent>> {
        let state = StreamState::new(Arc::clone(&self.inner), request, token);
        futures::stream::unfold(state, |mut state| async move {
            state.next().await.map(|event| (event, state))
        })
        .boxed()
    }

    fn list_models<'a>(
        &'a self,
        token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<ModelInfo>>> {
        Box::pin(self.inner.list_models(token))
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Dropping the client drops its pool; the next request builds a
            // fresh one, so closing is safe at any point.
            *self
                .inner
                .client
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        })
    }
}
