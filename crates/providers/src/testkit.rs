//! Test doubles for providers, behind the `testkit` feature.
//!
//! Three things, each replacing a socket with a script:
//!
//! - [`ScriptedProvider`]: a `ChatProvider` whose every call is a queued
//!   result, error or event list, so an attempt sequence is a value.
//! - [`ScriptedServer`]: a real TCP listener answering `openai-chat` requests
//!   from a queue of scripted responses, including a body that stops
//!   mid-stream and one that hangs until the client goes away, which is what
//!   a truncated event stream and a Stop button actually look like on the
//!   wire and what an HTTP mock cannot produce.
//! - [`provider_conformance`]: the scenarios every provider that is supposed
//!   to work must pass, run against whatever `create` builds on the server.
//!
//! The queues are strict. An unscripted request fails rather than returning a
//! default, because a test that accidentally makes a second call is a test
//! whose subject is doing something unexpected, and a permissive mock would
//! report that as a pass.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::too_many_lines,
    reason = "a fixture that cannot be built is a failing test either way, and a conformance suite is one long list of assertions"
)]

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use darkwire_core::messages::{
    AssistantOptions, ImageSource, ToolOptions, assistant_message, image_part, system_message,
    text_part, tool_message, user_message,
};
use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ChatMessage, ModelInfo, ReasoningEffort, ToolCall};
use futures::stream::{BoxStream, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::errors::{ProviderError, ProviderErrorReason};
use crate::registry::{MaxTokensParam, ProviderSpec};
use crate::resilience::{NoticeKind, ResilienceNotice, ResilienceOptions, with_resilience};
use crate::types::{
    BoxFuture, ChatProvider, ChatRequest, ChatResult, ChatStreamEvent, FinishReason, empty_usage,
};

// A scripted provider

/// What one scripted call does.
#[derive(Debug)]
pub enum ScriptedStep {
    /// `chat` returns this; `stream` yields it as the one `Done`.
    Result(ChatResult),
    /// Both call styles fail with this.
    Error(WireError),
    /// `stream` yields these in order; `chat` returns the `Done` among them.
    Events(Vec<Result<ChatStreamEvent>>),
}

/// A provider whose every call is scripted.
#[derive(Debug)]
pub struct ScriptedProvider {
    spec: ProviderSpec,
    steps: Mutex<VecDeque<ScriptedStep>>,
    seen: Mutex<Vec<ChatRequest>>,
    models: Vec<ModelInfo>,
    list_calls: Mutex<u32>,
    close_calls: Mutex<u32>,
}

impl ScriptedProvider {
    /// A provider for `spec` with `steps` queued.
    pub fn new(spec: ProviderSpec, steps: Vec<ScriptedStep>) -> Arc<ScriptedProvider> {
        Arc::new(ScriptedProvider {
            spec,
            steps: Mutex::new(steps.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
            models: Vec::new(),
            list_calls: Mutex::new(0),
            close_calls: Mutex::new(0),
        })
    }

    /// The same, answering `list_models` with `models`.
    pub fn with_models(
        spec: ProviderSpec,
        steps: Vec<ScriptedStep>,
        models: Vec<ModelInfo>,
    ) -> Arc<ScriptedProvider> {
        let mut provider =
            Arc::try_unwrap(ScriptedProvider::new(spec, steps)).expect("freshly built");
        provider.models = models;
        Arc::new(provider)
    }

    /// Every request seen so far, in order.
    pub fn seen(&self) -> Vec<ChatRequest> {
        self.seen.lock().unwrap().clone()
    }

    /// How many times `list_models` was called.
    pub fn list_calls(&self) -> u32 {
        *self.list_calls.lock().unwrap()
    }

    /// How many times `close` was called.
    pub fn close_calls(&self) -> u32 {
        *self.close_calls.lock().unwrap()
    }

    fn next(&self, request: &ChatRequest) -> Result<ScriptedStep> {
        let mut seen = self.seen.lock().unwrap();
        seen.push(request.clone());
        let call = seen.len();
        self.steps
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| WireError::new(ErrorKind::Internal, format!("unscripted call {call}")))
    }
}

impl ChatProvider for ScriptedProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }

    fn spec(&self) -> &ProviderSpec {
        &self.spec
    }

    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
        _token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<ChatResult>> {
        let step = self.next(request);
        Box::pin(async move {
            match step? {
                ScriptedStep::Result(result) => Ok(result),
                ScriptedStep::Error(error) => Err(error),
                ScriptedStep::Events(events) => {
                    for event in events {
                        if let Ok(ChatStreamEvent::Done(result)) = event {
                            return Ok(result);
                        }
                    }
                    Ok(result_of(""))
                }
            }
        })
    }

    fn stream(
        &self,
        request: ChatRequest,
        _token: CancellationToken,
    ) -> BoxStream<'static, Result<ChatStreamEvent>> {
        let events: Vec<Result<ChatStreamEvent>> = match self.next(&request) {
            Err(error) | Ok(ScriptedStep::Error(error)) => vec![Err(error)],
            Ok(ScriptedStep::Result(result)) => vec![Ok(ChatStreamEvent::Done(result))],
            Ok(ScriptedStep::Events(events)) => events,
        };
        futures::stream::iter(events).boxed()
    }

    fn list_models<'a>(
        &'a self,
        _token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<ModelInfo>>> {
        *self.list_calls.lock().unwrap() += 1;
        Box::pin(async move { Ok(self.models.clone()) })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        *self.close_calls.lock().unwrap() += 1;
        Box::pin(async {})
    }
}

/// A complete result carrying `text` and nothing else.
pub fn result_of(text: &str) -> ChatResult {
    ChatResult {
        message: assistant_message(text, AssistantOptions::default()),
        finish_reason: FinishReason::Stop,
        usage: empty_usage(),
        model: "test-model".to_owned(),
        generation_ms: None,
        first_token_ms: None,
    }
}

/// A provider error with `reason`, as the one `WireError`.
pub fn provider_error(reason: ProviderErrorReason, message: &str) -> WireError {
    ProviderError::new(reason, message).into_wire()
}

// Fixtures: the `openai-chat` wire

/// A tool call in a scripted completion.
#[derive(Debug, Clone)]
pub struct FixtureToolCall {
    /// The id.
    pub id: String,
    /// The name.
    pub name: String,
    /// The arguments, verbatim.
    pub arguments_json: String,
}

impl FixtureToolCall {
    /// A call.
    pub fn new(id: &str, name: &str, arguments_json: &str) -> FixtureToolCall {
        FixtureToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments_json: arguments_json.to_owned(),
        }
    }
}

/// What a scripted non-streaming completion carries.
#[derive(Debug, Clone, Default)]
pub struct CompletionOptions {
    /// The answer. `None` sends `content: null`.
    pub text: Option<String>,
    /// `reasoning_content`.
    pub reasoning: Option<String>,
    /// Tool calls.
    pub tool_calls: Vec<FixtureToolCall>,
    /// Overrides the inferred `finish_reason`.
    pub finish_reason: Option<String>,
    /// Overrides the default usage block.
    pub usage: Option<Value>,
    /// Overrides the model.
    pub model: Option<String>,
}

impl CompletionOptions {
    /// A completion answering `text`.
    pub fn text(text: &str) -> CompletionOptions {
        CompletionOptions {
            text: Some(text.to_owned()),
            ..CompletionOptions::default()
        }
    }
}

/// A non-streaming completion body.
pub fn completion(options: CompletionOptions) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": options.text,
    });
    if let Some(reasoning) = options.reasoning {
        message["reasoning_content"] = Value::String(reasoning);
    }
    if !options.tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(
            options
                .tool_calls
                .iter()
                .enumerate()
                .map(|(index, call)| {
                    json!({
                        "index": index,
                        "id": call.id,
                        "type": "function",
                        "function": {"name": call.name, "arguments": call.arguments_json},
                    })
                })
                .collect(),
        );
    }
    let finish_reason = options.finish_reason.unwrap_or_else(|| {
        if options.tool_calls.is_empty() {
            "stop".to_owned()
        } else {
            "tool_calls".to_owned()
        }
    });
    json!({
        "id": "chatcmpl-fixture",
        "model": options.model.unwrap_or_else(|| "test-model".to_owned()),
        "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}],
        "usage": options.usage.unwrap_or_else(|| json!({
            "prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18
        })),
    })
}

/// `GET /models`: the one endpoint whose shape is the same everywhere.
pub fn models_body(ids: &[&str]) -> Value {
    json!({
        "object": "list",
        "data": ids.iter().map(|id| json!({"id": id, "object": "model"})).collect::<Vec<_>>(),
    })
}

/// An error body in the shape every OpenAI-compatible endpoint returns.
pub fn error_body(error: &Value) -> Value {
    json!({"error": error})
}

/// Wraps frames as SSE `data:` events, terminated the way providers terminate.
pub fn sse_body(frames: &[Value], done: bool) -> String {
    let mut body = String::new();
    for frame in frames {
        let text = match frame {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let _ = write!(body, "data: {text}\n\n");
    }
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    body
}

/// A `chat.completion.chunk` carrying a text delta.
pub fn text_chunk(text: &str) -> Value {
    json!({"model": "test-model", "choices": [{"index": 0, "delta": {"content": text}}]})
}

/// A chunk carrying a reasoning delta.
pub fn reasoning_chunk(text: &str) -> Value {
    json!({"choices": [{"index": 0, "delta": {"reasoning_content": text}}]})
}

/// A tool-call fragment. Splitting `arguments` across chunks is the normal
/// case.
pub fn tool_call_chunk(
    index: u64,
    id: Option<&str>,
    name: Option<&str>,
    arguments_json: Option<&str>,
) -> Value {
    let mut call = json!({"index": index, "function": {}});
    if let Some(id) = id {
        call["id"] = Value::String(id.to_owned());
    }
    if let Some(name) = name {
        call["function"]["name"] = Value::String(name.to_owned());
    }
    if let Some(arguments) = arguments_json {
        call["function"]["arguments"] = Value::String(arguments.to_owned());
    }
    json!({"choices": [{"index": 0, "delta": {"tool_calls": [call]}}]})
}

/// The chunk that closes a choice.
pub fn finish_chunk(reason: &str) -> Value {
    json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
}

/// The usage-only trailer `stream_options: {include_usage: true}` asks for.
pub fn usage_chunk(usage: &Value) -> Value {
    json!({"choices": [], "usage": usage})
}

// A scripted HTTP server

/// One request the server saw.
#[derive(Debug, Clone)]
pub struct RecordedCall {
    /// `POST`, `GET`.
    pub method: String,
    /// The request target, `/v1/chat/completions`.
    pub path: String,
    /// Header names lowercased.
    pub headers: HashMap<String, String>,
    /// The parsed JSON body, or `null` when there was none.
    pub body: Value,
}

/// How a streamed body ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// The terminating chunk: a complete response.
    Finish,
    /// The socket closes with the body unfinished: a dropped connection.
    Close,
    /// Nothing more is sent until the client goes away: a stall the client
    /// has to abort.
    Hang,
}

enum ScriptedBody {
    Whole(Vec<u8>),
    Chunked {
        chunks: Vec<Vec<u8>>,
        ending: Ending,
    },
}

/// One scripted HTTP response.
pub struct ScriptedResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: ScriptedBody,
    before: Option<Box<dyn FnOnce() + Send>>,
}

impl std::fmt::Debug for ScriptedResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedResponse")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl ScriptedResponse {
    /// A response with `body` and its content type.
    pub fn raw(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> ScriptedResponse {
        ScriptedResponse {
            status,
            headers: vec![("content-type".to_owned(), content_type.to_owned())],
            body: ScriptedBody::Whole(body.into()),
            before: None,
        }
    }

    /// A JSON response.
    pub fn json(status: u16, body: &Value) -> ScriptedResponse {
        ScriptedResponse::raw(status, "application/json", body.to_string())
    }

    /// A complete SSE response, `[DONE]` included when `done`.
    pub fn sse(frames: &[Value], done: bool) -> ScriptedResponse {
        ScriptedResponse::raw(200, "text/event-stream", sse_body(frames, done))
    }

    /// An SSE response delivered as separate chunks, ending as `ending` says.
    pub fn streaming(chunks: Vec<String>, ending: Ending) -> ScriptedResponse {
        ScriptedResponse {
            status: 200,
            headers: vec![("content-type".to_owned(), "text/event-stream".to_owned())],
            body: ScriptedBody::Chunked {
                chunks: chunks.into_iter().map(String::into_bytes).collect(),
                ending,
            },
            before: None,
        }
    }

    /// A response with no body at all.
    pub fn empty(status: u16) -> ScriptedResponse {
        ScriptedResponse {
            status,
            headers: Vec::new(),
            body: ScriptedBody::Whole(Vec::new()),
            before: None,
        }
    }

    /// Adds a response header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> ScriptedResponse {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Runs `hook` after the request is read and before the response is
    /// written: where a test advances a clock to stand in for a model
    /// loading its weights.
    #[must_use]
    pub fn before(mut self, hook: impl FnOnce() + Send + 'static) -> ScriptedResponse {
        self.before = Some(Box::new(hook));
        self
    }
}

struct ServerState {
    queue: Mutex<VecDeque<ScriptedResponse>>,
    calls: Mutex<Vec<RecordedCall>>,
}

/// A TCP server answering from a queue of [`ScriptedResponse`]s.
pub struct ScriptedServer {
    port: u16,
    state: Arc<ServerState>,
    accept: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for ScriptedServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedServer")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl Drop for ScriptedServer {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

impl ScriptedServer {
    /// Binds a listener on loopback and starts answering.
    pub async fn start() -> ScriptedServer {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let state = Arc::new(ServerState {
            queue: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
        });
        let accepting = Arc::clone(&state);
        let accept = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve_connection(socket, Arc::clone(&accepting)));
            }
        });
        ScriptedServer {
            port,
            state,
            accept,
        }
    }

    /// `http://127.0.0.1:<port>/v1`, the base URL a provider is pointed at.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    /// The port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Queues one response. Calls are served in the order they were queued.
    pub fn push(&self, response: ScriptedResponse) -> &ScriptedServer {
        self.state.queue.lock().unwrap().push_back(response);
        self
    }

    /// Every request seen so far, in order.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.state.calls.lock().unwrap().clone()
    }
}

/// Reads one HTTP/1.1 request off `socket`, answers it, closes.
async fn serve_connection(mut socket: TcpStream, state: Arc<ServerState>) {
    let Some(call) = read_request(&mut socket).await else {
        return;
    };
    let method = call.method.clone();
    let path = call.path.clone();
    state.calls.lock().unwrap().push(call);
    let response = state.queue.lock().unwrap().pop_front();
    let Some(mut response) = response else {
        let body = format!("Unscripted request: {method} {path}");
        let _ = write_head(
            &mut socket,
            500,
            &[("content-type", "text/plain")],
            Some(body.len()),
        )
        .await;
        let _ = socket.write_all(body.as_bytes()).await;
        let _ = socket.shutdown().await;
        return;
    };
    if let Some(before) = response.before.take() {
        before();
    }
    let headers: Vec<(&str, &str)> = response
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    match response.body {
        ScriptedBody::Whole(body) => {
            if write_head(&mut socket, response.status, &headers, Some(body.len()))
                .await
                .is_err()
            {
                return;
            }
            let _ = socket.write_all(&body).await;
            let _ = socket.shutdown().await;
        }
        ScriptedBody::Chunked { chunks, ending } => {
            if write_head(&mut socket, response.status, &headers, None)
                .await
                .is_err()
            {
                return;
            }
            for chunk in chunks {
                let framed = format!("{:x}\r\n", chunk.len());
                if socket.write_all(framed.as_bytes()).await.is_err()
                    || socket.write_all(&chunk).await.is_err()
                    || socket.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
                let _ = socket.flush().await;
            }
            match ending {
                Ending::Finish => {
                    let _ = socket.write_all(b"0\r\n\r\n").await;
                    let _ = socket.shutdown().await;
                }
                Ending::Close => {
                    let _ = socket.shutdown().await;
                }
                Ending::Hang => {
                    // Nothing more goes out; wait for the client to give up.
                    let mut sink = [0u8; 256];
                    while let Ok(read) = socket.read(&mut sink).await {
                        if read == 0 {
                            break;
                        }
                    }
                }
            }
        }
    }
}

async fn write_head(
    socket: &mut TcpStream,
    status: u16,
    headers: &[(&str, &str)],
    content_length: Option<usize>,
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status));
    for (name, value) in headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    match content_length {
        Some(length) => {
            let _ = write!(head, "content-length: {length}\r\n");
        }
        None => head.push_str("transfer-encoding: chunked\r\n"),
    }
    head.push_str("connection: close\r\n\r\n");
    socket.write_all(head.as_bytes()).await
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Scripted",
    }
}

/// Parses the request line, headers and a `content-length` body.
async fn read_request(socket: &mut TcpStream) -> Option<RecordedCall> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_lowercase(), value.trim().to_owned());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    Some(RecordedCall {
        method,
        path,
        headers,
        body,
    })
}

// Conformance

/// Builds the provider under test against a running server.
pub type CreateProvider = dyn Fn(&ScriptedServer) -> Arc<dyn ChatProvider> + Send + Sync;

/// The deltas and the single terminal result of a stream.
#[derive(Debug, Default)]
pub struct Collected {
    /// Text deltas, in order.
    pub text: Vec<String>,
    /// Reasoning deltas, in order.
    pub reasoning: Vec<String>,
    /// The `Done` event's result.
    pub done: Option<ChatResult>,
}

/// Drains a stream, keeping the deltas and the result; fails on an error.
pub async fn collect(mut events: BoxStream<'static, Result<ChatStreamEvent>>) -> Result<Collected> {
    let mut collected = Collected::default();
    while let Some(event) = events.next().await {
        match event? {
            ChatStreamEvent::Text(text) => collected.text.push(text),
            ChatStreamEvent::Reasoning(text) => collected.reasoning.push(text),
            ChatStreamEvent::Done(result) => collected.done = Some(result),
        }
    }
    Ok(collected)
}

/// The reason a result failed with, or a sentinel when it did not.
pub fn reason_of<T>(result: &Result<T>) -> Option<ProviderErrorReason> {
    result.as_ref().err().map(ProviderError::reason_of)
}

fn hello() -> Vec<ChatMessage> {
    vec![
        ChatMessage::System(system_message("You are DarkWire.")),
        ChatMessage::User(user_message("hello")),
    ]
}

fn base_request(model: &str) -> ChatRequest {
    ChatRequest {
        max_tokens: Some(256),
        temperature: Some(0.1),
        ..ChatRequest::new(model, hello())
    }
}

/// The provider wrapped in resilience with a pinned schedule, and the notices
/// it emitted.
fn resilient(
    provider: Arc<dyn ChatProvider>,
) -> (Arc<dyn ChatProvider>, Arc<Mutex<Vec<ResilienceNotice>>>) {
    let notices = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&notices);
    let wrapped = with_resilience(
        provider,
        ResilienceOptions {
            // `jitter = 1` pins full-jitter backoff to its ceiling, so a
            // schedule is a value a test can state rather than a range it has
            // to tolerate; a tiny `max_delay_ms` keeps the real wait short.
            jitter: Some(Arc::new(|| 1.0)),
            max_delay_ms: Some(10),
            base_delay_ms: Some(1),
            on_notice: Some(Arc::new(move |notice| sink.lock().unwrap().push(notice))),
            ..ResilienceOptions::default()
        },
    );
    (wrapped, notices)
}

/// The provider conformance suite.
///
/// One suite, run against every provider that is supposed to work. That is
/// what makes "one adapter covers ten providers" a checked claim rather than
/// an assertion in a README: the same scenarios run against Ollama's spec,
/// OpenAI's and a gateway's, and a table entry that quietly needs different
/// behaviour fails here rather than in someone's session.
///
/// It also fixes the *contract* rather than the implementation. Streaming ends
/// with exactly one `Done`; a rejected parameter is dropped and retried; an
/// abort raises rather than returning a partial answer. A future adapter
/// passes this suite or it is not a provider.
pub async fn provider_conformance(model: &str, create: &CreateProvider) {
    let base = base_request(model);

    // Sends the model, the messages and the token cap.
    {
        let server = ScriptedServer::start().await;
        server.push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions::text("hi")),
        ));
        let provider = create(&server);
        provider
            .chat(&base, &CancellationToken::new())
            .await
            .expect("chat");
        let call = &server.calls()[0];
        assert_eq!(call.method, "POST");
        assert!(call.path.ends_with("/chat/completions"), "{}", call.path);
        let param = match provider.spec().max_tokens_param {
            MaxTokensParam::MaxTokens => "max_tokens",
            MaxTokensParam::MaxCompletionTokens => "max_completion_tokens",
        };
        assert_eq!(call.body[param], json!(256));
        assert_eq!(call.body["messages"].as_array().map(Vec::len), Some(2));
        assert!(call.body.get("stream").is_none());
    }

    // Returns text, finish reason and usage.
    {
        let server = ScriptedServer::start().await;
        server.push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions {
                usage: Some(
                    json!({"prompt_tokens": 30, "completion_tokens": 5, "total_tokens": 35}),
                ),
                ..CompletionOptions::text("the answer")
            }),
        ));
        let result = create(&server)
            .chat(&base, &CancellationToken::new())
            .await
            .expect("chat");
        assert_eq!(result.message.content, vec![text_part("the answer")]);
        assert_eq!(result.finish_reason, FinishReason::Stop);
        assert_eq!(result.usage.prompt_tokens, 30);
        assert_eq!(result.usage.completion_tokens, 5);
        assert_eq!(result.usage.total_tokens, 35);
    }

    // Returns parallel tool calls with their arguments verbatim. The second
    // call's arguments are deliberately not valid JSON: parsing is the tool
    // registry's job, and a transport that repairs them hides a model defect
    // the registry would have reported as a retryable tool error.
    {
        let server = ScriptedServer::start().await;
        server.push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions {
                tool_calls: vec![
                    FixtureToolCall::new("call_1", "read", "{\"path\":\"a.txt\"}"),
                    FixtureToolCall::new("call_2", "ls", "{\"path\": "),
                ],
                ..CompletionOptions::default()
            }),
        ));
        let result = create(&server)
            .chat(&base, &CancellationToken::new())
            .await
            .expect("chat");
        assert_eq!(result.finish_reason, FinishReason::ToolCalls);
        assert_eq!(
            result.message.tool_calls,
            vec![
                ToolCall {
                    id: "call_1".into(),
                    name: "read".into(),
                    arguments_json: "{\"path\":\"a.txt\"}".into(),
                },
                ToolCall {
                    id: "call_2".into(),
                    name: "ls".into(),
                    arguments_json: "{\"path\": ".into(),
                },
            ]
        );
    }

    // Reports tool calls even when the provider labels the turn "stop".
    {
        let server = ScriptedServer::start().await;
        server.push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions {
                finish_reason: Some("stop".into()),
                tool_calls: vec![FixtureToolCall::new("call_1", "ls", "{}")],
                ..CompletionOptions::default()
            }),
        ));
        let result = create(&server)
            .chat(&base, &CancellationToken::new())
            .await
            .expect("chat");
        assert_eq!(result.finish_reason, FinishReason::ToolCalls);
    }

    // Streams deltas in order and ends with one assembled result.
    {
        let server = ScriptedServer::start().await;
        server.push(ScriptedResponse::sse(
            &[
                reasoning_chunk("thinking"),
                text_chunk("Hel"),
                text_chunk("lo"),
                finish_chunk("stop"),
                usage_chunk(
                    &json!({"prompt_tokens": 9, "completion_tokens": 2, "total_tokens": 11}),
                ),
            ],
            true,
        ));
        let collected = collect(create(&server).stream(base.clone(), CancellationToken::new()))
            .await
            .expect("stream");
        assert_eq!(collected.text, vec!["Hel", "lo"]);
        assert_eq!(collected.reasoning, vec!["thinking"]);
        let done = collected.done.expect("done");
        assert_eq!(done.message.content, vec![text_part("Hello")]);
        assert_eq!(done.message.reasoning.as_deref(), Some("thinking"));
        assert_eq!(done.usage.total_tokens, 11);
        assert_eq!(server.calls()[0].body["stream"], json!(true));
    }

    // Reassembles tool-call arguments split across stream frames.
    {
        let server = ScriptedServer::start().await;
        server.push(ScriptedResponse::sse(
            &[
                tool_call_chunk(0, Some("call_a"), Some("read"), None),
                tool_call_chunk(0, None, None, Some("{\"path\":")),
                tool_call_chunk(1, Some("call_b"), Some("ls"), Some("{}")),
                tool_call_chunk(0, None, None, Some("\"a.txt\"}")),
                finish_chunk("tool_calls"),
            ],
            true,
        ));
        let collected = collect(create(&server).stream(base.clone(), CancellationToken::new()))
            .await
            .expect("stream");
        let done = collected.done.expect("done");
        assert_eq!(
            done.message.tool_calls,
            vec![
                ToolCall {
                    id: "call_a".into(),
                    name: "read".into(),
                    arguments_json: "{\"path\":\"a.txt\"}".into(),
                },
                ToolCall {
                    id: "call_b".into(),
                    name: "ls".into(),
                    arguments_json: "{}".into(),
                },
            ]
        );
        assert_eq!(done.finish_reason, FinishReason::ToolCalls);
    }

    // Raises on a mid-stream abort and keeps what was already delivered.
    {
        let server = ScriptedServer::start().await;
        server.push(ScriptedResponse::streaming(
            vec![sse_body(&[text_chunk("Hel")], false)],
            Ending::Hang,
        ));
        let token = CancellationToken::new();
        let mut events = create(&server).stream(base.clone(), token.clone());
        let mut seen = Vec::new();
        let mut failure = None;
        while let Some(event) = events.next().await {
            match event {
                Ok(ChatStreamEvent::Text(text)) => {
                    seen.push(text);
                    token.cancel();
                }
                Ok(_) => {}
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        assert_eq!(seen, vec!["Hel"]);
        let failure = failure.expect("the abort is raised");
        assert_eq!(
            ProviderError::reason_of(&failure),
            ProviderErrorReason::Aborted
        );
    }

    // Retries a rate limit after the delay the provider asked for.
    {
        let server = ScriptedServer::start().await;
        server
            .push(
                ScriptedResponse::json(
                    429,
                    &error_body(&json!({"message": "slow down", "type": "rate_limit_error"})),
                )
                .header("retry-after", "2"),
            )
            .push(ScriptedResponse::json(
                200,
                &completion(CompletionOptions::text("second time")),
            ));
        let (provider, notices) = resilient(create(&server));
        let result = provider
            .chat(&base, &CancellationToken::new())
            .await
            .expect("chat");
        assert_eq!(result.message.content, vec![text_part("second time")]);
        let notices = notices.lock().unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, NoticeKind::Retry);
        // The header was read as two seconds; the wait itself is clamped by
        // the suite's ceiling so the test does not spend them.
        assert_eq!(notices[0].error.retry_after_ms, Some(2000));
        assert_eq!(notices[0].delay_ms, Some(10));
        assert_eq!(server.calls().len(), 2);
    }

    // Does not retry an authentication failure.
    {
        let server = ScriptedServer::start().await;
        server.push(ScriptedResponse::json(
            401,
            &error_body(&json!({"message": "bad key", "code": "invalid_api_key"})),
        ));
        let (provider, _) = resilient(create(&server));
        let result = provider.chat(&base, &CancellationToken::new()).await;
        let error = ProviderError::of(&result.expect_err("401 fails"));
        assert_eq!(error.reason, ProviderErrorReason::Auth);
        assert_eq!(error.status, Some(401));
        assert_eq!(server.calls().len(), 1);
    }

    // Drops a rejected parameter and retries without it.
    {
        let server = ScriptedServer::start().await;
        server
            .push(ScriptedResponse::json(
                400,
                &error_body(&json!({
                    "message": "Unsupported parameter: reasoning_effort",
                    "code": "unsupported_parameter",
                    "param": "reasoning_effort",
                })),
            ))
            .push(ScriptedResponse::json(
                200,
                &completion(CompletionOptions::text("degraded but answered")),
            ));
        let (provider, notices) = resilient(create(&server));
        let result = provider
            .chat(
                &ChatRequest {
                    reasoning_effort: Some(ReasoningEffort::High),
                    ..base.clone()
                },
                &CancellationToken::new(),
            )
            .await
            .expect("chat");
        assert_eq!(
            result.message.content,
            vec![text_part("degraded but answered")]
        );
        let calls = server.calls();
        assert_eq!(calls[0].body["reasoning_effort"], json!("high"));
        assert!(calls[1].body.get("reasoning_effort").is_none());
        // A degradation is a repair, not a transient failure: it must not
        // spend the retry budget, and it must not wait before trying the fixed
        // request.
        let kinds: Vec<NoticeKind> = notices.lock().unwrap().iter().map(|n| n.kind).collect();
        assert_eq!(kinds, vec![NoticeKind::Degraded]);
    }

    // Drops the oldest turns when the request exceeds the context window.
    {
        let mut long = vec![ChatMessage::System(system_message("You are DarkWire."))];
        for index in 0..12 {
            long.push(ChatMessage::User(user_message(
                format!("question {index} ").repeat(40),
            )));
        }
        let server = ScriptedServer::start().await;
        server
            .push(ScriptedResponse::json(
                400,
                &error_body(&json!({"message": "too long", "code": "context_length_exceeded"})),
            ))
            .push(ScriptedResponse::json(
                200,
                &completion(CompletionOptions::text("fits now")),
            ));
        let (provider, _) = resilient(create(&server));
        let result = provider
            .chat(
                &ChatRequest {
                    messages: long,
                    ..base.clone()
                },
                &CancellationToken::new(),
            )
            .await
            .expect("chat");
        assert_eq!(result.message.content, vec![text_part("fits now")]);
        let calls = server.calls();
        let before = calls[0].body["messages"].as_array().unwrap().len();
        let after = calls[1].body["messages"].as_array().unwrap();
        assert!(after.len() < before);
        // The system prompt is instructions, not conversation; it survives.
        assert_eq!(after[0]["role"], json!("system"));
    }

    // Falls back to a single response when the event stream cannot be read.
    {
        let server = ScriptedServer::start().await;
        server
            .push(ScriptedResponse::sse(
                &[json!("{\"choices\": ["), json!("still not json")],
                false,
            ))
            .push(ScriptedResponse::json(
                200,
                &completion(CompletionOptions::text("answered without streaming")),
            ));
        let (provider, _) = resilient(create(&server));
        let collected = collect(provider.stream(base.clone(), CancellationToken::new()))
            .await
            .expect("stream");
        assert_eq!(collected.text, vec!["answered without streaming"]);
        assert_eq!(
            collected.done.expect("done").message.content,
            vec![text_part("answered without streaming")]
        );
        assert!(server.calls()[1].body.get("stream").is_none());
    }

    // Encodes an image as a content part beside its text.
    {
        let server = ScriptedServer::start().await;
        server.push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions::text("a cat")),
        ));
        create(&server)
            .chat(
                &ChatRequest {
                    messages: vec![ChatMessage::User(user_message(vec![
                        text_part("what is this?"),
                        image_part("image/png", ImageSource::Data("aGk=".into())),
                    ]))],
                    ..base.clone()
                },
                &CancellationToken::new(),
            )
            .await
            .expect("chat");
        assert_eq!(
            server.calls()[0].body["messages"][0]["content"],
            json!([
                {"type": "text", "text": "what is this?"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,aGk="}},
            ])
        );
    }

    // Round-trips a tool result back into the next request. An assistant turn
    // with only tool calls carries `null` content, and the tool result carries
    // the id that pairs it: the two facts every provider returns a 400 for
    // when they are wrong.
    {
        let server = ScriptedServer::start().await;
        server.push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions::text("the file says hi")),
        ));
        create(&server)
            .chat(
                &ChatRequest {
                    messages: vec![
                        ChatMessage::User(user_message("read a.txt")),
                        ChatMessage::Assistant(assistant_message(
                            "",
                            AssistantOptions {
                                tool_calls: vec![ToolCall {
                                    id: "call_1".into(),
                                    name: "read".into(),
                                    arguments_json: "{}".into(),
                                }],
                                reasoning: None,
                            },
                        )),
                        ChatMessage::Tool(tool_message(
                            "call_1",
                            "read",
                            "hi",
                            ToolOptions::default(),
                        )),
                    ],
                    ..base.clone()
                },
                &CancellationToken::new(),
            )
            .await
            .expect("chat");
        let messages = server.calls()[0].body["messages"].clone();
        assert_eq!(messages[1]["role"], json!("assistant"));
        assert_eq!(messages[1]["content"], Value::Null);
        assert_eq!(messages[2]["role"], json!("tool"));
        assert_eq!(messages[2]["tool_call_id"], json!("call_1"));
        assert_eq!(messages[2]["content"], json!("hi"));
    }

    // Lists models, and reports the failure when it cannot.
    {
        let server = ScriptedServer::start().await;
        server
            .push(ScriptedResponse::json(
                200,
                &models_body(&["model-a", "model-b"]),
            ))
            .push(ScriptedResponse::json(
                500,
                &error_body(&json!({"message": "boom"})),
            ));
        let provider = create(&server);
        let token = CancellationToken::new();
        let models = provider.list_models(&token).await.expect("models");
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["model-a", "model-b"]);
        assert!(
            models
                .iter()
                .all(|model| model.provider_id == provider.id())
        );
        let failure = provider.list_models(&token).await;
        assert_eq!(reason_of(&failure), Some(ProviderErrorReason::Server));
    }
}
