//! The loop's test doubles, behind the `testkit` feature.
//!
//! A scripted provider was written for the loop's own tests and the end-to-end
//! suite is its second consumer, in another crate. The alternative was a second
//! implementation of the same event shaping over there — which would let the
//! model a browser test drives behave differently from the model every loop
//! test asserts against, and the whole point of a scripted provider is that
//! those are the same thing.
//!
//! Nothing here depends on a test framework, so this module does not pull one
//! into anyone's graph.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot be built is a failing test either way"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use darkwire_core::messages::{AssistantOptions, assistant_message};
use darkwire_core::{Clock, ErrorKind, Result, WireError};
use darkwire_protocol::{ModelInfo, ToolCall, Usage};
use darkwire_providers::{
    BoxFuture, ChatProvider, ChatRequest, ChatResult, ChatStreamEvent, FinishReason, PROVIDERS,
    ProviderSpec, empty_usage,
};
use darkwire_security::RandomSource;
use futures::stream::{self, BoxStream, StreamExt as _};
use tokio_util::sync::CancellationToken;

/// A clock that reads tokio's timer.
///
/// The loop's wall-clock cap is measured on the injected clock while its
/// heartbeat and approval deadline are tokio sleeps, so a test that paused one
/// and advanced the other would be measuring two different times.
/// `tokio::time::advance` moves both here, which is what lets one test assert a
/// cap and a cadence together.
#[derive(Debug)]
pub struct TokioClock {
    epoch_ms: i64,
    origin: tokio::time::Instant,
}

impl TokioClock {
    /// A clock reading `now_ms` at the epoch given, monotonic origin now.
    pub fn at(now_ms: i64) -> Arc<TokioClock> {
        Arc::new(TokioClock {
            epoch_ms: now_ms,
            origin: tokio::time::Instant::now(),
        })
    }

    /// The default epoch every scripted test starts from.
    pub fn new() -> Arc<TokioClock> {
        TokioClock::at(1_700_000_000_000)
    }

    fn elapsed(&self) -> Duration {
        tokio::time::Instant::now().saturating_duration_since(self.origin)
    }
}

impl Clock for TokioClock {
    fn now_ms(&self) -> i64 {
        self.epoch_ms + i64::try_from(self.elapsed().as_millis()).unwrap_or(i64::MAX)
    }

    fn monotonic(&self) -> Duration {
        self.elapsed()
    }
}

/// A random source that is not random.
///
/// Fixed so a turn's nonce — and therefore its tool-output delimiter and every
/// byte of the prompt that names it — is the same on every run. Never
/// constructed outside a test: a predictable delimiter is a guessable one.
#[derive(Debug, Clone, Copy)]
pub struct FixedRandom(pub u8);

impl Default for FixedRandom {
    fn default() -> FixedRandom {
        FixedRandom(0xa1)
    }
}

impl RandomSource for FixedRandom {
    fn fill(&self, buf: &mut [u8]) {
        for (index, byte) in buf.iter_mut().enumerate() {
            *byte = self.0.wrapping_add(u8::try_from(index % 256).unwrap_or(0));
        }
    }
}

/// Ids that count, so a test asserts on stable values.
#[derive(Debug, Default)]
pub struct CountingIds {
    prefix: String,
    next: Mutex<u64>,
}

impl CountingIds {
    /// Ids of the form `<prefix>-1`, `<prefix>-2`, …
    pub fn new(prefix: impl Into<String>) -> Arc<CountingIds> {
        Arc::new(CountingIds {
            prefix: prefix.into(),
            next: Mutex::new(1),
        })
    }

    /// The next id.
    ///
    /// # Panics
    ///
    /// If another thread panicked while holding the counter.
    pub fn next_id(&self) -> String {
        let mut next = self.next.lock().unwrap();
        let id = format!("{}-{}", self.prefix, *next);
        *next += 1;
        id
    }
}

/// What the model does on one request.
#[derive(Debug, Clone, Default)]
pub struct ScriptedTurn {
    /// Text deltas, in order. Their concatenation is the message content.
    pub deltas: Vec<String>,
    /// Reasoning deltas, emitted before the text.
    pub reasoning: Vec<String>,
    /// The calls this reply asks for.
    pub tool_calls: Vec<ToolCall>,
    /// What the provider reported it cost.
    pub usage: Option<Usage>,
    /// What the wire adapter would have measured for this request.
    ///
    /// Stated rather than timed, because the measurement itself belongs to the
    /// adapter and is tested there against a real event stream. What the loop
    /// owes is the arithmetic over several of these — summing one, keeping the
    /// first of the other — and that is clearest when the inputs are written
    /// down.
    pub generation_ms: Option<f64>,
    /// Request to first content delta.
    pub first_token_ms: Option<f64>,
    /// Failed instead of streaming.
    ///
    /// The kind and the wording rather than the error itself, because a
    /// `WireError` carries a source and is deliberately not `Clone`, and a
    /// script that repeats its last turn hands the same failure back twice.
    pub error: Option<ScriptedError>,
    /// Ends the stream without its completion — a truncated transport.
    pub omit_done: bool,
    /// How long the request takes before its first event, on tokio's timer.
    ///
    /// The seam a test uses to hold a turn open while something else happens to
    /// it: a stop, a steer, a wall-clock cap.
    pub delay_ms: u64,
}

impl ScriptedTurn {
    /// A reply that is nothing but `text`.
    pub fn text(text: &str) -> ScriptedTurn {
        ScriptedTurn {
            deltas: vec![text.to_owned()],
            ..ScriptedTurn::default()
        }
    }

    /// A reply that is nothing but tool calls.
    pub fn calls(calls: Vec<ToolCall>) -> ScriptedTurn {
        ScriptedTurn {
            tool_calls: calls,
            ..ScriptedTurn::default()
        }
    }

    /// A request that fails with `kind`.
    pub fn failing(kind: ErrorKind, message: &str) -> ScriptedTurn {
        ScriptedTurn {
            error: Some(ScriptedError {
                kind,
                message: message.to_owned(),
            }),
            ..ScriptedTurn::default()
        }
    }

    /// The same reply, taking `delay_ms` on tokio's timer before it starts.
    #[must_use]
    pub fn after(mut self, delay_ms: u64) -> ScriptedTurn {
        self.delay_ms = delay_ms;
        self
    }

    fn result(&self, model: &str) -> ChatResult {
        let text = self.deltas.concat();
        let reasoning = self.reasoning.concat();
        ChatResult {
            message: assistant_message(
                text,
                AssistantOptions {
                    tool_calls: self.tool_calls.clone(),
                    reasoning: (!reasoning.is_empty()).then_some(reasoning),
                    reasoning_ms: None,
                },
            ),
            finish_reason: if self.tool_calls.is_empty() {
                FinishReason::Stop
            } else {
                FinishReason::ToolCalls
            },
            usage: self.usage.unwrap_or_else(empty_usage),
            model: model.to_owned(),
            // Absent rather than zero when the script says nothing, so the
            // default scripted turn looks like a request nothing could measure.
            generation_ms: self.generation_ms,
            first_token_ms: self.first_token_ms,
        }
    }
}

/// A failure a script hands back, as a value it can repeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedError {
    /// Which kind the loop should map onto a wire code.
    pub kind: ErrorKind,
    /// What the turn's stats row records.
    pub message: String,
}

impl ScriptedError {
    fn build(&self) -> WireError {
        WireError::new(self.kind, self.message.clone())
    }
}

/// A provider that reads from a script.
///
/// The loop's behaviour is almost entirely a function of what the model does —
/// answer, call one tool, call three, fail, stall — and a scripted provider is
/// how each of those becomes one line of a test instead of a mocked transport.
/// The real wire adapter is tested against a socket in `darkwire-providers`;
/// nothing here asserts anything about HTTP.
///
/// Requests are recorded, because half of what the loop must get right is in
/// the request rather than in the response: the system prompt's static prefix
/// staying byte-identical across iterations, the tool definitions being the
/// same list every time, history arriving already aligned.
///
/// Running past the end repeats the last turn rather than failing: a test for
/// the iteration cap is a test about a model that never stops calling tools,
/// and writing that as forty identical script entries would only obscure it.
#[derive(Debug)]
pub struct ScriptedProvider {
    spec: ProviderSpec,
    turns: Vec<ScriptedTurn>,
    seen: Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    /// A provider answering from `turns`.
    pub fn new(turns: Vec<ScriptedTurn>) -> Arc<ScriptedProvider> {
        let spec = PROVIDERS
            .iter()
            .find(|spec| spec.id == "ollama")
            .cloned()
            .unwrap_or_else(|| PROVIDERS[0].clone());
        Arc::new(ScriptedProvider {
            spec,
            turns,
            seen: Mutex::new(Vec::new()),
        })
    }

    /// Every request, in order.
    ///
    /// # Panics
    ///
    /// If another thread panicked while holding the log.
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.seen.lock().unwrap().clone()
    }

    /// How many requests have been made.
    ///
    /// # Panics
    ///
    /// If another thread panicked while holding the log.
    pub fn request_count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn next(&self, request: &ChatRequest) -> ScriptedTurn {
        let mut seen = self.seen.lock().unwrap();
        seen.push(request.clone());
        let index = seen.len() - 1;
        self.turns
            .get(index.min(self.turns.len().saturating_sub(1)))
            .cloned()
            .unwrap_or_default()
    }
}

impl ChatProvider for ScriptedProvider {
    fn id(&self) -> &'static str {
        "scripted"
    }

    fn spec(&self) -> &ProviderSpec {
        &self.spec
    }

    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
        _token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<ChatResult>> {
        let turn = self.next(request);
        let model = request.model.clone();
        Box::pin(async move {
            match turn.error {
                Some(error) => Err(error.build()),
                None => Ok(turn.result(&model)),
            }
        })
    }

    fn stream(
        &self,
        request: ChatRequest,
        token: CancellationToken,
    ) -> BoxStream<'static, Result<ChatStreamEvent>> {
        let turn = self.next(&request);
        let model = request.model;
        stream::once(async move {
            if turn.delay_ms > 0 {
                tokio::select! {
                    () = token.cancelled() => {
                        return vec![Err(WireError::aborted("Provider request"))];
                    }
                    () = tokio::time::sleep(Duration::from_millis(turn.delay_ms)) => {}
                }
            }
            if let Some(error) = turn.error {
                return vec![Err(error.build())];
            }
            if token.is_cancelled() {
                return vec![Err(WireError::aborted("Provider request"))];
            }

            let mut events: Vec<Result<ChatStreamEvent>> = Vec::new();
            for text in &turn.reasoning {
                events.push(Ok(ChatStreamEvent::Reasoning(text.clone())));
            }
            for text in &turn.deltas {
                events.push(Ok(ChatStreamEvent::Text(text.clone())));
            }
            if !turn.omit_done {
                events.push(Ok(ChatStreamEvent::Done(turn.result(&model))));
            }
            events
        })
        .flat_map(stream::iter)
        .boxed()
    }

    fn list_models<'a>(
        &'a self,
        _token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// A tool call in the shape the adapters produce.
pub fn tool_call(id: &str, name: &str, args: &serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments_json: args.to_string(),
    }
}

/// A tool call whose arguments are whatever the model wrote, valid or not.
pub fn raw_tool_call(id: &str, name: &str, arguments_json: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments_json: arguments_json.to_owned(),
    }
}
