//! Retry, the degradation ladder and the streaming fallback, on a paused clock.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

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
    DEFAULT_DEGRADATION_STEPS, MAX_TRUNCATIONS, NoticeKind, ProviderError, ProviderErrorReason,
    ProviderSpec, ResilienceNotice, ResilienceOptions, ToolChoice, backoff_delay_ms,
    synthesise_stream, truncate_oldest_turns, with_resilience,
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

type Notices = Arc<Mutex<Vec<ResilienceNotice>>>;

/// Wraps with a pinned schedule: `jitter = 1` puts full-jitter backoff at its
/// ceiling, so the delays are values rather than ranges.
fn wrap(inner: Arc<ScriptedProvider>) -> (Arc<dyn ChatProvider>, Notices) {
    let notices: Notices = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&notices);
    let provider = with_resilience(
        inner,
        ResilienceOptions {
            jitter: Some(Arc::new(|| 1.0)),
            on_notice: Some(Arc::new(move |notice| sink.lock().unwrap().push(notice))),
            ..ResilienceOptions::default()
        },
    );
    (provider, notices)
}

fn delays(notices: &Notices) -> Vec<u64> {
    notices
        .lock()
        .unwrap()
        .iter()
        .filter_map(|notice| notice.delay_ms)
        .collect()
}

fn kinds(notices: &Notices) -> Vec<NoticeKind> {
    notices.lock().unwrap().iter().map(|n| n.kind).collect()
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
    let (provider, notices) = wrap(Arc::clone(&inner));
    let started = tokio::time::Instant::now();
    let error = provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.message.contains("boom"));
    // Three attempts, two waits: 500 then 1000, and the paused clock shows
    // they were actually waited.
    assert_eq!(delays(&notices), vec![500, 1000]);
    assert_eq!(started.elapsed(), Duration::from_millis(1500));
    assert_eq!(inner.seen().len(), 3);
    assert!(notices.lock().unwrap().iter().all(|n| n.attempt >= 1));
}

#[tokio::test(start_paused = true)]
async fn stops_immediately_on_an_error_repeating_cannot_fix() {
    let inner = ScriptedProvider::new(spec(), vec![err(ProviderErrorReason::Auth, "bad key")]);
    let (provider, notices) = wrap(Arc::clone(&inner));
    let error = provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.message.contains("bad key"));
    assert!(delays(&notices).is_empty());
    assert_eq!(inner.seen().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn never_retries_an_abort() {
    let inner = ScriptedProvider::new(spec(), vec![err(ProviderErrorReason::Aborted, "cancelled")]);
    let (provider, notices) = wrap(Arc::clone(&inner));
    let error = provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(
        ProviderError::reason_of(&error),
        ProviderErrorReason::Aborted
    );
    assert!(delays(&notices).is_empty());
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
    let (provider, _) = wrap(inner);
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
    assert_eq!(backoff_delay_ms(1, &rate_limited(2000), &options), 2000);
    assert_eq!(backoff_delay_ms(1, &rate_limited(90_000), &options), 8000);
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
    let notices: Notices = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&notices);
    let provider = with_resilience(
        inner,
        ResilienceOptions {
            random: Some(Arc::new(darkwire_security::testkit::FixedRandom::constant(
                0,
            ))),
            on_notice: Some(Arc::new(move |notice| sink.lock().unwrap().push(notice))),
            ..ResilienceOptions::default()
        },
    );
    provider
        .chat(&request(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(delays(&notices), vec![250]);
    assert!(format!("{:?}", ResilienceOptions::default()).contains("ResilienceOptions"));
}

#[tokio::test(start_paused = true)]
async fn aborting_during_backoff_ends_the_turn_rather_than_retrying() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![err(ProviderErrorReason::Server, "boom"), ok("never")],
    );
    let (provider, _) = wrap(Arc::clone(&inner));
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
    let (provider, notices) = wrap(Arc::clone(&inner));
    provider
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
    assert!(delays(&notices).is_empty());
    assert_eq!(kinds(&notices), vec![NoticeKind::Degraded]);
    assert_eq!(
        notices.lock().unwrap()[0].message,
        "retrying without reasoning_effort"
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
    let (provider, _) = wrap(Arc::clone(&inner));
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
    let (provider, _) = wrap(Arc::clone(&inner));
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
    let (provider, _) = wrap(Arc::clone(&inner));
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
    let (provider, _) = wrap(Arc::clone(&inner));
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
    let (provider, _) = wrap(Arc::clone(&inner));
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
            "truncate_turns",
        ]
    );
    for step in &DEFAULT_DEGRADATION_STEPS {
        assert!((step.apply)(&request()).is_none(), "{}", step.id);
        assert!(!step.description.is_empty());
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
    let truncate = DEFAULT_DEGRADATION_STEPS
        .iter()
        .find(|s| s.id == "truncate_turns")
        .unwrap();
    let too_long = ProviderError::new(ProviderErrorReason::ContextLength, "x");
    assert!((truncate.applies)(&too_long, &request()));
    assert!(!(truncate.applies)(
        &ProviderError::new(ProviderErrorReason::Server, "x"),
        &request()
    ));
    assert!(format!("{:?}", DEFAULT_DEGRADATION_STEPS[0]).contains("drop_prompt_cache_key"));
}

#[tokio::test(start_paused = true)]
async fn does_not_degrade_an_error_no_repair_addresses() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![err(ProviderErrorReason::ContentFilter, "refused")],
    );
    let (provider, _) = wrap(Arc::clone(&inner));
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

fn long(marker: &str) -> ChatMessage {
    common::user(&format!("{marker} ").repeat(60))
}

#[test]
fn truncate_keeps_the_system_prompt_and_the_newest_turn() {
    let messages = vec![
        common::system("rules"),
        long("a"),
        long("b"),
        long("c"),
        long("d"),
    ];
    let kept = truncate_oldest_turns(&messages).unwrap();
    assert_eq!(kept[0], common::system("rules"));
    assert_eq!(kept.last(), messages.last());
    assert!(kept.len() < messages.len());
}

#[test]
fn truncate_cuts_the_same_history_whether_or_not_turns_carry_reasoning() {
    let thought = "x".repeat(4000);
    let plain = vec![
        common::system("rules"),
        long("a"),
        common::assistant("answered"),
        long("b"),
        common::assistant("answered"),
        long("c"),
    ];
    let thinking: Vec<ChatMessage> = plain
        .iter()
        .map(|message| match message {
            ChatMessage::Assistant(_) => common::assistant_with("answered", vec![], Some(&thought)),
            other => other.clone(),
        })
        .collect();
    assert_eq!(
        truncate_oldest_turns(&thinking).map(|k| k.len()),
        truncate_oldest_turns(&plain).map(|k| k.len())
    );
}

#[test]
fn truncate_keeps_the_question_and_the_trailing_runtime_turn() {
    let question = common::user("what did the migration change?");
    let runtime = common::user("<system-reminder>\n## Live state\n</system-reminder>");
    let kept = truncate_oldest_turns(&[
        common::system("rules"),
        long("a"),
        long("b"),
        long("c"),
        question.clone(),
        runtime.clone(),
    ])
    .unwrap();
    assert!(kept.contains(&question));
    assert_eq!(kept.last(), Some(&runtime));

    let kept = truncate_oldest_turns(&[
        common::system("rules"),
        long("a"),
        long("b"),
        long("c"),
        runtime.clone(),
    ])
    .unwrap();
    assert_eq!(kept.last(), Some(&runtime));
}

#[test]
fn truncate_keeps_the_question_on_a_later_iteration_of_its_turn() {
    // The bulk sits after the question, so only the floor stops the cut
    // from reaching it.
    let question = common::user("what did the migration change?");
    let runtime = common::user("<system-reminder>\n## Live state\n</system-reminder>");
    let messages = vec![
        common::system("rules"),
        common::user("hi"),
        common::assistant("hello"),
        question.clone(),
        common::assistant_with("", vec![common::call("c1", "read", "{}")], None),
        common::tool("c1", "read", &"x".repeat(4000)),
        runtime.clone(),
    ];
    let kept = truncate_oldest_turns(&messages).unwrap();
    assert!(kept.contains(&question));
    assert_eq!(kept.last(), Some(&runtime));
    assert!(kept.len() < messages.len());

    // The same without the trailing runtime turn.
    let kept = truncate_oldest_turns(&messages[..messages.len() - 1]).unwrap();
    assert!(kept.contains(&question));
}

#[test]
fn truncate_never_leaves_a_tool_result_without_its_assistant() {
    let messages = vec![
        long("old"),
        common::assistant_with("", vec![common::call("c1", "read", "{}")], None),
        common::tool("c1", "read", &"x".repeat(400)),
        long("newer"),
        long("newest"),
    ];
    let kept = truncate_oldest_turns(&messages).unwrap();
    let mut declared = std::collections::HashSet::new();
    for message in &kept {
        match message {
            ChatMessage::Assistant(assistant) => {
                declared.extend(assistant.tool_calls.iter().map(|c| c.id.clone()));
            }
            ChatMessage::Tool(tool) => assert!(declared.contains(&tool.tool_call_id)),
            _ => {}
        }
    }
}

#[test]
fn truncate_declines_when_there_is_nothing_to_drop() {
    assert!(truncate_oldest_turns(&[]).is_none());
    assert!(truncate_oldest_turns(&[common::user("only")]).is_none());
    assert!(truncate_oldest_turns(&[common::system("rules"), common::user("only")]).is_none());
    // The survivors would all be orphaned tool results.
    assert!(
        truncate_oldest_turns(&[
            common::user(&"x".repeat(500)),
            common::tool("orphan", "read", &"y".repeat(500)),
            common::tool("orphan2", "read", &"z".repeat(500)),
        ])
        .is_none()
    );
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
                    (2, _) => Some((Ok(ChatStreamEvent::Done(result_of("ab"))), 3)),
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
            ScriptedStep::Events(vec![Ok(ChatStreamEvent::Done(result_of("second")))]),
        ],
    );
    let (provider, notices) = wrap(inner);
    let collected = collect(provider.stream(request(), CancellationToken::new()))
        .await
        .unwrap();
    assert!(collected.text.is_empty());
    assert_eq!(
        collected.done.unwrap().message.content,
        vec![text_part("second")]
    );
    assert_eq!(delays(&notices), vec![500]);
}

#[tokio::test(start_paused = true)]
async fn degrades_a_stream_the_same_way_as_a_single_request() {
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err_param(ProviderErrorReason::UnsupportedParam, "reasoning_effort"),
            ScriptedStep::Events(vec![Ok(ChatStreamEvent::Done(result_of("ok")))]),
        ],
    );
    let (provider, _) = wrap(Arc::clone(&inner));
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
async fn a_stream_that_exhausts_recovery_raises_the_error() {
    let inner = ScriptedProvider::new(spec(), vec![err(ProviderErrorReason::Auth, "bad key")]);
    let (provider, _) = wrap(inner);
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
    let (provider, notices) = wrap(Arc::clone(&inner));
    let mut events: Vec<ChatStreamEvent> = Vec::new();
    let mut stream = provider.stream(request(), CancellationToken::new());
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }
    assert_eq!(events[0], ChatStreamEvent::Text("whole answer".into()));
    assert!(matches!(events[1], ChatStreamEvent::Done(_)));
    assert_eq!(events.len(), 2);
    assert_eq!(kinds(&notices), vec![NoticeKind::Fallback]);

    // And the fallback's own failure is the error the consumer sees.
    let inner = ScriptedProvider::new(
        spec(),
        vec![
            err(ProviderErrorReason::StreamParse, "garbage"),
            err(ProviderErrorReason::Auth, "bad key"),
        ],
    );
    let (provider, _) = wrap(inner);
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
    let (provider, _) = wrap(Arc::clone(&inner));
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
            ChatStreamEvent::Done(result.clone()),
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
        vec![ChatStreamEvent::Done(tool_only.clone())]
    );
}

/// A long conversation: the system prompt, `turns` answered questions, and
/// the question being asked now.
fn conversation(turns: usize) -> Vec<ChatMessage> {
    let mut messages = vec![common::system("rules")];
    for index in 0..turns {
        messages.push(long(&format!("q{index}")));
        messages.push(common::assistant("answered"));
    }
    messages.push(common::user("the question"));
    messages
}

#[tokio::test(start_paused = true)]
async fn truncation_repeats_shrinking_the_request_each_time() {
    let too_long = || err(ProviderErrorReason::ContextLength, "too long");
    let inner = ScriptedProvider::new(spec(), vec![too_long(), too_long(), ok("fits")]);
    let (provider, notices) = wrap(Arc::clone(&inner));
    let result = provider
        .chat(
            &ChatRequest {
                messages: conversation(40),
                ..request()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.message.content, vec![text_part("fits")]);

    let sizes: Vec<usize> = inner.seen().iter().map(|r| r.messages.len()).collect();
    assert_eq!(sizes.len(), 3);
    assert!(sizes[0] > sizes[1] && sizes[1] > sizes[2], "{sizes:?}");
    // Two cuts, one notice: the second says nothing the first did not.
    assert_eq!(kinds(&notices), vec![NoticeKind::Degraded]);
    for request in inner.seen() {
        assert_eq!(request.messages.first(), Some(&common::system("rules")));
        assert_eq!(request.messages.last(), Some(&common::user("the question")));
    }
}

#[tokio::test(start_paused = true)]
async fn truncation_stops_at_its_cap_on_a_window_that_never_fits() {
    let steps = (0..=MAX_TRUNCATIONS)
        .map(|_| err(ProviderErrorReason::ContextLength, "too long"))
        .collect();
    let inner = ScriptedProvider::new(spec(), steps);
    let (provider, notices) = wrap(Arc::clone(&inner));
    let error = provider
        .chat(
            &ChatRequest {
                messages: conversation(200),
                ..request()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        ProviderError::reason_of(&error),
        ProviderErrorReason::ContextLength
    );
    assert_eq!(inner.seen().len(), 1 + MAX_TRUNCATIONS as usize);
    assert_eq!(kinds(&notices), vec![NoticeKind::Degraded]);
}

#[tokio::test(start_paused = true)]
async fn truncation_stops_at_the_current_turn_before_its_cap() {
    let steps = (0..=MAX_TRUNCATIONS)
        .map(|_| err(ProviderErrorReason::ContextLength, "too long"))
        .collect();
    let inner = ScriptedProvider::new(spec(), steps);
    let (provider, _) = wrap(Arc::clone(&inner));
    let error = provider
        .chat(
            &ChatRequest {
                messages: conversation(1),
                ..request()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        ProviderError::reason_of(&error),
        ProviderErrorReason::ContextLength
    );
    let seen = inner.seen();
    assert!(seen.len() < 1 + MAX_TRUNCATIONS as usize, "{}", seen.len());
    // The floor held: the last request still asks the question.
    assert_eq!(
        seen.last().unwrap().messages.last(),
        Some(&common::user("the question"))
    );
}
