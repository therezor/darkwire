//! Retry, the degradation ladder and the streaming fallback, on a paused clock.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use crate::common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use darkwire_core::Result;
use darkwire_core::messages::{ImageSource, image_part, text_part};
use darkwire_protocol::{ChatMessage, ModelInfo, ReasoningEffort};
use darkwire_providers::testkit::{
    ScriptedProvider, ScriptedStep, collect, provider_error, result_of,
};
use darkwire_providers::{
    BackoffOptions, BoxFuture, ChatProvider, ChatRequest, ChatResult, ChatStreamEvent,
    DEFAULT_DEGRADATION_STEPS, MAX_RETRY_AFTER_MS, ProviderError, ProviderErrorReason,
    ProviderSpec, ResilienceOptions, ToolChoice, backoff_delay_ms, synthesise_stream,
    with_resilience,
};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

fn spec() -> ProviderSpec {
    common::spec_of("ollama")
}

fn request() -> ChatRequest {
    ChatRequest::new("test-model", vec![common::user("hi")])
}

fn err(reason: ProviderErrorReason, message: &str) -> ScriptedStep {
    ScriptedStep::Error(provider_error(reason, message))
}

fn err_param(reason: ProviderErrorReason, param: &str) -> ScriptedStep {
    ScriptedStep::Error(
        ProviderError::new(reason, "no")
            .with_param(Some(param.into()))
            .into_wire(),
    )
}

fn ok(text: &str) -> ScriptedStep {
    ScriptedStep::Result(result_of(text))
}

/// Wraps with a pinned schedule: `jitter = 1` puts full-jitter backoff at its
/// ceiling, so the waits are values rather than ranges, read off the paused
/// clock.
fn wrap(inner: Arc<ScriptedProvider>) -> Arc<dyn ChatProvider> {
    with_resilience(
        inner,
        ResilienceOptions {
            jitter: Some(Arc::new(|| 1.0)),
            ..ResilienceOptions::default()
        },
    )
}

#[tokio::test(start_paused = true)]
async fn backs_off_exponentially_and_gives_up_after_the_attempt_cap() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err(ProviderErrorReason::Server, "boom"),
            err(ProviderErrorReason::Server, "boom"),
            err(ProviderErrorReason::Server, "boom"),
        ],
    );
    let provider = wrap(Arc::clone(&inner));
    let started = tokio::time::Instant::now();
    let error = provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.message.contains("boom"));
    // Three attempts, two waits, 500 then 1000, on the paused clock.
    assert_eq!(started.elapsed(), Duration::from_millis(1500));
    assert_eq!(inner.seen().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn stops_immediately_on_an_error_repeating_cannot_fix() {
    let inner = ScriptedProvider::new(spec(), vec![err(ProviderErrorReason::Auth, "bad key")]);
    let provider = wrap(Arc::clone(&inner));
    let error = provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.message.contains("bad key"));
    assert_eq!(inner.seen().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn never_retries_an_abort() {
    let inner = ScriptedProvider::new(spec(), vec![err(ProviderErrorReason::Aborted, "cancelled")]);
    let provider = wrap(Arc::clone(&inner));
    let error = provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(
        ProviderError::reason_of(&error),
        ProviderErrorReason::Aborted
    );
    assert_eq!(inner.seen().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn classifies_an_untyped_failure_before_deciding() {
    // A bare network error is a transport failure, which is retryable, but
    // only once it has been classified.
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            ScriptedStep::Error(darkwire_core::WireError::new(
                darkwire_core::ErrorKind::Network,
                "connection reset",
            )),
            ok("recovered"),
        ],
    );
    let provider = wrap(inner);
    let result = provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.message.content, vec![text_part("recovered")]);
}

#[test]
fn honours_retry_after_over_its_own_schedule_up_to_the_ceiling() {
    let options = BackoffOptions {
        base_delay_ms: 500,
        max_delay_ms: 8000,
        jitter: Arc::new(|| 1.0),
    };
    let rate_limited = |retry_after| {
        ProviderError::new(ProviderErrorReason::RateLimit, "x")
            .with_retry_after_ms(Some(retry_after))
    };
    let server = ProviderError::new(ProviderErrorReason::Server, "x");
    // Asked for explicitly, a wait past the jittered ceiling is honoured, up
    // to its own.
    assert_eq!(backoff_delay_ms(1, &rate_limited(2000), &options), 2000);
    assert_eq!(backoff_delay_ms(1, &rate_limited(20_000), &options), 20_000);
    assert_eq!(
        backoff_delay_ms(1, &rate_limited(90_000), &options),
        MAX_RETRY_AFTER_MS
    );
    assert_eq!(backoff_delay_ms(4, &server, &options), 4000);
    assert_eq!(backoff_delay_ms(9, &server, &options), 8000);
    let quarter = BackoffOptions {
        base_delay_ms: 1000,
        jitter: Arc::new(|| 0.25),
        ..options
    };
    assert_eq!(backoff_delay_ms(1, &server, &quarter), 250);
}

#[tokio::test(start_paused = true)]
async fn the_default_jitter_lands_in_the_upper_half() {
    // Without an injected jitter the default draws from the random source,
    // floored at half so a retry never fires immediately.
    let inner = ScriptedProvider::new(
        spec(),
        vec![err(ProviderErrorReason::Server, "boom"), ok("ok")],
    );
    let provider = with_resilience(
        inner,
        ResilienceOptions {
            random: Some(Arc::new(darkwire_security::testkit::FixedRandom::constant(
                0,
            ))),
            ..ResilienceOptions::default()
        },
    );
    let started = tokio::time::Instant::now();
    provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(started.elapsed(), Duration::from_millis(250));
    assert!(format!("{:?}", ResilienceOptions::default()).contains("ResilienceOptions"));
}

#[tokio::test(start_paused = true)]
async fn aborting_during_backoff_ends_the_turn_rather_than_retrying() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![err(ProviderErrorReason::Server, "boom"), ok("never")],
    );
    let provider = wrap(Arc::clone(&inner));
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let error = provider.chat(&request(), &token).await.unwrap_err();
    assert!(error.is_aborted());
    assert_eq!(inner.seen().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn drops_reasoning_effort_without_spending_a_retry_or_a_wait() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err_param(ProviderErrorReason::UnsupportedParam, "reasoning_effort"),
            ok("ok"),
        ],
    );
    let provider = wrap(Arc::clone(&inner));
    let started = tokio::time::Instant::now();
    let result = provider
        .chat(
            &ChatRequest {
                reasoning_effort: Some(ReasoningEffort::High),
                tool_choice: Some(ToolChoice::Auto),
                ..request()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let seen = inner.seen();
    assert_eq!(seen[1].reasoning_effort, None);
    // Only the one thing the provider objected to.
    assert_eq!(seen[1].tool_choice, Some(ToolChoice::Auto));
    assert_eq!(started.elapsed(), Duration::ZERO);
    assert_eq!(
        result.degraded,
        ["This model does not take a reasoning effort, so that setting was left out."]
    );
}

#[tokio::test(start_paused = true)]
async fn drops_the_effort_under_its_other_name_and_skips_a_step_blamed_elsewhere() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err_param(ProviderErrorReason::UnsupportedParam, "reasoning"),
            ok("ok"),
        ],
    );
    let provider = wrap(Arc::clone(&inner));
    provider
        .chat(
            &ChatRequest {
                reasoning_effort: Some(ReasoningEffort::Off),
                ..request()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(inner.seen()[1].reasoning_effort, None);

    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err_param(ProviderErrorReason::UnsupportedParam, "tool_choice"),
            ok("ok"),
        ],
    );
    let provider = wrap(Arc::clone(&inner));
    provider
        .chat(
            &ChatRequest {
                reasoning_effort: Some(ReasoningEffort::High),
                tool_choice: Some(ToolChoice::Required),
                ..request()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let seen = inner.seen();
    assert_eq!(seen[1].reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(seen[1].tool_choice, None);
}

#[tokio::test(start_paused = true)]
async fn walks_the_whole_ladder_on_a_bare_400() {
    // Every local inference server. Each step removes only something the
    // request carried, so the ladder degrades in order and then gives up.
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err(ProviderErrorReason::InvalidRequest, "400"),
            err(ProviderErrorReason::InvalidRequest, "400"),
            err(ProviderErrorReason::InvalidRequest, "400"),
            err(ProviderErrorReason::InvalidRequest, "400"),
        ],
    );
    let provider = wrap(Arc::clone(&inner));
    let error = provider
        .chat(
            &ChatRequest {
                reasoning_effort: Some(ReasoningEffort::Low),
                tool_choice: Some(ToolChoice::Auto),
                messages: vec![common::user_parts(vec![
                    text_part("look"),
                    image_part("image/png", ImageSource::Data("aGk=".into())),
                ])],
                ..request()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("400"));
    let seen = inner.seen();
    assert_eq!(seen.len(), 4);
    assert_eq!(seen[1].reasoning_effort, None);
    assert_eq!(seen[2].tool_choice, None);
    match &seen[3].messages[0] {
        ChatMessage::User(user) => assert_eq!(user.content, vec![text_part("look")]),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn drops_the_cache_key_before_anything_that_costs_the_answer() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err(
                ProviderErrorReason::UnsupportedParam,
                "unknown field prompt_cache_key",
            ),
            ok("ok"),
        ],
    );
    let provider = wrap(Arc::clone(&inner));
    provider
        .chat(
            &ChatRequest {
                cache_key: Some("web:1".into()),
                reasoning_effort: Some(ReasoningEffort::High),
                ..request()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let seen = inner.seen();
    assert_eq!(seen[1].cache_key, None);
    assert_eq!(seen[1].reasoning_effort, Some(ReasoningEffort::High));
}

#[tokio::test(start_paused = true)]
async fn merges_a_trailing_user_turn_for_a_strict_alternation_endpoint() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err(
                ProviderErrorReason::InvalidRequest,
                "messages must alternate",
            ),
            ok("ok"),
        ],
    );
    let provider = wrap(Arc::clone(&inner));
    provider
        .chat(
            &ChatRequest {
                messages: vec![
                    common::system("rules"),
                    common::user("hello"),
                    common::user("live"),
                ],
                ..request()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let merged = &inner.seen()[1].messages;
    assert_eq!(merged.len(), 2);
    match &merged[1] {
        ChatMessage::User(user) => {
            assert_eq!(user.content, vec![text_part("hello"), text_part("live")]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_ladder_is_ordered_and_every_step_declines_an_empty_request() {
    let ids: Vec<&str> = DEFAULT_DEGRADATION_STEPS.iter().map(|s| s.id).collect();
    assert_eq!(
        ids,
        vec![
            "drop_prompt_cache_key",
            "merge_trailing_user",
            "drop_reasoning_effort",
            "drop_tool_choice",
            "strip_images",
        ]
    );
    for step in &DEFAULT_DEGRADATION_STEPS {
        assert!((step.apply)(&request()).is_none(), "{}", step.id);
        assert!(!step.notice.is_empty());
    }
    let merge = DEFAULT_DEGRADATION_STEPS
        .iter()
        .find(|s| s.id == "merge_trailing_user")
        .unwrap();
    assert!(
        (merge.apply)(&ChatRequest {
            messages: vec![common::user("hello"), common::assistant("hi")],
            ..request()
        })
        .is_none()
    );
    assert!(format!("{:?}", DEFAULT_DEGRADATION_STEPS[0]).contains("drop_prompt_cache_key"));
}

#[tokio::test(start_paused = true)]
async fn does_not_degrade_an_error_no_repair_addresses() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![err(ProviderErrorReason::ContentFilter, "refused")],
    );
    let provider = wrap(Arc::clone(&inner));
    let error = provider
        .chat(
            &ChatRequest {
                reasoning_effort: Some(ReasoningEffort::High),
                ..request()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("refused"));
    assert_eq!(inner.seen().len(), 1);
}

/// A provider that logs when it emits, so interleaving is observable.
struct Interleaving {
    spec: ProviderSpec,
    order: Arc<Mutex<Vec<String>>>,
    fail_after_first: bool,
    chat_calls: Arc<Mutex<u32>>,
}

impl ChatProvider for Interleaving {
    fn id(&self) -> &str {
        &self.spec.id
    }

    fn spec(&self) -> &ProviderSpec {
        &self.spec
    }

    fn chat<'a>(
        &'a self,
        _request: &'a ChatRequest,
        _token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<ChatResult>> {
        *self.chat_calls.lock().unwrap() += 1;
        Box::pin(async { Ok(result_of("should not be called")) })
    }

    fn stream(
        &self,
        _request: ChatRequest,
        _token: CancellationToken,
    ) -> BoxStream<'static, Result<ChatStreamEvent>> {
        let order = Arc::clone(&self.order);
        let fail = self.fail_after_first;
        futures::stream::unfold(0u8, move |step| {
            let order = Arc::clone(&order);
            async move {
                match (step, fail) {
                    (0, _) => {
                        order.lock().unwrap().push("emit:a".into());
                        Some((Ok(ChatStreamEvent::Text("a".into())), 1))
                    }
                    (1, true) => Some((
                        Err(provider_error(
                            ProviderErrorReason::Server,
                            "died mid-answer",
                        )),
                        3,
                    )),
                    (1, false) => {
                        order.lock().unwrap().push("emit:b".into());
                        Some((Ok(ChatStreamEvent::Text("b".into())), 2))
                    }
                    (2, _) => Some((Ok(ChatStreamEvent::Done(Box::new(result_of("ab")))), 3)),
                    _ => None,
                }
            }
        })
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

#[tokio::test]
async fn forwards_deltas_as_they_arrive_rather_than_buffering_the_turn() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let inner = Arc::new(Interleaving {
        spec: spec(),
        order: Arc::clone(&order),
        fail_after_first: false,
        chat_calls: Arc::new(Mutex::new(0)),
    });
    let provider = with_resilience(inner, ResilienceOptions::default());
    let mut events = provider.stream(request(), CancellationToken::new());
    while let Some(event) = events.next().await {
        if let ChatStreamEvent::Text(text) = event.unwrap() {
            order.lock().unwrap().push(format!("consume:{text}"));
        }
    }
    assert_eq!(
        *order.lock().unwrap(),
        vec!["emit:a", "consume:a", "emit:b", "consume:b"]
    );
}

#[tokio::test]
async fn raises_rather_than_restarting_a_stream_that_already_emitted() {
    let chat_calls = Arc::new(Mutex::new(0));
    let inner = Arc::new(Interleaving {
        spec: spec(),
        order: Arc::new(Mutex::new(Vec::new())),
        fail_after_first: true,
        chat_calls: Arc::clone(&chat_calls),
    });
    let provider = with_resilience(inner, ResilienceOptions::default());
    let mut events = provider.stream(request(), CancellationToken::new());
    let mut seen = Vec::new();
    let mut failure = None;
    while let Some(event) = events.next().await {
        match event {
            Ok(ChatStreamEvent::Text(text)) => seen.push(text),
            Ok(_) => {}
            Err(error) => failure = Some(error),
        }
    }
    assert_eq!(seen, vec!["a"]);
    assert!(failure.unwrap().message.contains("died mid-answer"));
    // Restarting would replay text the user has already read.
    assert_eq!(*chat_calls.lock().unwrap(), 0);
}

#[tokio::test(start_paused = true)]
async fn retries_a_stream_that_failed_before_saying_anything() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err(ProviderErrorReason::Overloaded, "busy"),
            ScriptedStep::Events(vec![Ok(ChatStreamEvent::Done(Box::new(result_of(
                "second",
            ))))]),
        ],
    );
    let provider = wrap(inner);
    let started = tokio::time::Instant::now();
    let collected = collect(provider.stream(request(), CancellationToken::new()))
        .await
        .unwrap();
    assert!(collected.text.is_empty());
    assert_eq!(
        collected.done.unwrap().message.content,
        vec![text_part("second")]
    );
    assert_eq!(started.elapsed(), Duration::from_millis(500));
}

#[tokio::test(start_paused = true)]
async fn degrades_a_stream_the_same_way_as_a_single_request() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err_param(ProviderErrorReason::UnsupportedParam, "reasoning_effort"),
            ScriptedStep::Events(vec![Ok(ChatStreamEvent::Done(Box::new(result_of("ok"))))]),
        ],
    );
    let provider = wrap(Arc::clone(&inner));
    collect(provider.stream(
        ChatRequest {
            reasoning_effort: Some(ReasoningEffort::High),
            ..request()
        },
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    assert_eq!(inner.seen()[1].reasoning_effort, None);
}

#[tokio::test(start_paused = true)]
async fn says_when_the_request_that_succeeded_was_not_the_one_built() {
    // Its usage then describes a smaller request, so a caller comparing the
    // count with its own estimate must not.
    let degraded = ScriptedProvider::new(
        spec(),
        vec![
            err_param(ProviderErrorReason::UnsupportedParam, "reasoning_effort"),
            ok("ok"),
        ],
    );
    let provider = wrap(degraded);
    let effort = ChatRequest {
        reasoning_effort: Some(ReasoningEffort::High),
        ..request()
    };
    let result = provider
        .chat(&effort, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.degraded.len(), 1);

    let streamed = ScriptedProvider::new(
        spec(),
        vec![
            err_param(ProviderErrorReason::UnsupportedParam, "reasoning_effort"),
            ScriptedStep::Events(vec![Ok(ChatStreamEvent::Done(Box::new(result_of("ok"))))]),
        ],
    );
    let provider = wrap(streamed);
    let collected = collect(provider.stream(effort, CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(collected.done.unwrap().degraded.len(), 1);

    // A retry sends the same request again, so it is not a degradation.
    let retried = ScriptedProvider::new(
        spec(),
        vec![err(ProviderErrorReason::RateLimit, "slow down"), ok("ok")],
    );
    let provider = wrap(retried);
    let result = provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap();
    assert!(result.degraded.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_exhausts_recovery_raises_the_error() {
    let inner = ScriptedProvider::new(spec(), vec![err(ProviderErrorReason::Auth, "bad key")]);
    let provider = wrap(inner);
    let error = collect(provider.stream(request(), CancellationToken::new()))
        .await
        .unwrap_err();
    assert_eq!(ProviderError::reason_of(&error), ProviderErrorReason::Auth);
}

#[tokio::test(start_paused = true)]
async fn falls_back_to_a_single_response_when_the_stream_cannot_be_parsed() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err(ProviderErrorReason::StreamParse, "garbage"),
            ok("whole answer"),
        ],
    );
    let provider = wrap(Arc::clone(&inner));
    let mut events: Vec<ChatStreamEvent> = Vec::new();
    let mut stream = provider.stream(request(), CancellationToken::new());
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }
    assert_eq!(events[0], ChatStreamEvent::Text("whole answer".into()));
    assert!(matches!(events[1], ChatStreamEvent::Done(_)));
    assert_eq!(events.len(), 2);
    assert_eq!(inner.seen().len(), 2);

    // And the fallback's own failure is the error the consumer sees.
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err(ProviderErrorReason::StreamParse, "garbage"),
            err(ProviderErrorReason::Auth, "bad key"),
        ],
    );
    let provider = wrap(inner);
    let error = collect(provider.stream(request(), CancellationToken::new()))
        .await
        .unwrap_err();
    assert_eq!(ProviderError::reason_of(&error), ProviderErrorReason::Auth);
}

#[tokio::test]
async fn passes_list_models_and_close_straight_through() {
    let inner = ScriptedProvider::with_models(
        spec(),
        Vec::new(),
        vec![ModelInfo {
            id: "m".into(),
            provider_id: "ollama".into(),
            provider_type: None,
            display_name: None,
            context_window_tokens: None,
            supports_tools: None,
            supports_vision: None,
            supports_reasoning: None,
        }],
    );
    let provider = wrap(Arc::clone(&inner));
    assert_eq!(provider.id(), inner.id());
    assert_eq!(provider.spec(), inner.spec());
    let models = provider
        .list_models(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    provider.close().await;
    assert_eq!(inner.list_calls(), 1);
    assert_eq!(inner.close_calls(), 1);
    assert!(format!("{provider:?}").contains("ollama"));
}

#[test]
fn synthesise_stream_replays_a_result_as_events() {
    let result = ChatResult {
        message: match common::assistant_with("answer", vec![], Some("because")) {
            ChatMessage::Assistant(message) => message,
            other => panic!("{other:?}"),
        },
        ..result_of("")
    };
    assert_eq!(
        synthesise_stream(&result),
        vec![
            ChatStreamEvent::Reasoning("because".into()),
            ChatStreamEvent::Text("answer".into()),
            ChatStreamEvent::Done(Box::new(result.clone())),
        ]
    );

    let tool_only = ChatResult {
        message: match common::assistant_with("", vec![common::call("c", "ls", "{}")], None) {
            ChatMessage::Assistant(message) => message,
            other => panic!("{other:?}"),
        },
        ..result_of("")
    };
    assert_eq!(
        synthesise_stream(&tool_only),
        vec![ChatStreamEvent::Done(Box::new(tool_only.clone()))]
    );
}

#[tokio::test(start_paused = true)]
async fn a_request_too_long_for_the_model_fails_without_a_second_try() {
    // The loop already windows history to the configured context, so this is
    // a wrong setting, and cutting more would hide it.
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err(ProviderErrorReason::ContextLength, "too long"),
            ok("never"),
        ],
    );
    let provider = wrap(Arc::clone(&inner));
    let error = provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(
        ProviderError::reason_of(&error),
        ProviderErrorReason::ContextLength
    );
    assert_eq!(inner.seen().len(), 1);
}
