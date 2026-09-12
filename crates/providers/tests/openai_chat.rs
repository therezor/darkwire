//! The `openai-chat` adapter against the scripted server and wiremock.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use ghostai_core::ErrorKind;
use ghostai_core::messages::{FileDetails, ImageSource, file_part, image_part, text_part};
use ghostai_core::testkit::ManualClock;
use ghostai_protocol::ReasoningEffort;
use ghostai_providers::testkit::{
    CompletionOptions, Ending, ScriptedResponse, ScriptedServer, collect, completion, error_body,
    finish_chunk, provider_conformance, reasoning_chunk, sse_body, text_chunk, tool_call_chunk,
    usage_chunk,
};
use ghostai_providers::{
    ChatProvider, ChatRequest, ChatResult, ChatStreamEvent, FinishReason, OpenAiChatProvider,
    ProviderError, ProviderErrorReason, ProviderSpec, ToolChoice, WireAdapterOptions, WireProtocol,
    assert_usable_api_base, create_openai_chat_provider, tool_call_id,
};
use ghostai_security::testkit::FixedRandom;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider_on(
    server: &ScriptedServer,
    spec: &str,
    api_key: Option<&str>,
) -> Arc<dyn ChatProvider> {
    create_openai_chat_provider(WireAdapterOptions {
        api_base: Some(server.base_url()),
        api_key: api_key.map(str::to_owned),
        ..WireAdapterOptions::new(common::spec_of(spec))
    })
    .unwrap()
}

fn base() -> ChatRequest {
    ChatRequest::new(
        "test-model",
        vec![common::system("sys"), common::user("hi")],
    )
}

fn token() -> CancellationToken {
    CancellationToken::new()
}

// The claim the crate makes is that one adapter serves every OpenAI-compatible
// provider. Running the same suite against a local server, a direct cloud
// provider whose table entry renames the token cap, and a gateway that
// rewrites model ids is what turns that from a comment into a test.

#[tokio::test]
async fn conformance_ollama() {
    provider_conformance("test-model", &|server| provider_on(server, "ollama", None)).await;
}

#[tokio::test]
async fn conformance_openai() {
    provider_conformance("gpt-4o", &|server| {
        provider_on(server, "openai", Some("sk-test"))
    })
    .await;
}

#[tokio::test]
async fn conformance_openrouter() {
    provider_conformance("anthropic/claude-sonnet-4", &|server| {
        provider_on(server, "openrouter", Some("sk-or-test"))
    })
    .await;
}

#[test]
fn assert_usable_api_base_accepts_https_and_local_http() {
    assert_eq!(
        assert_usable_api_base("https://api.openai.com/v1", true)
            .unwrap()
            .scheme(),
        "https"
    );
    assert_eq!(
        assert_usable_api_base("https://api.openai.com/v1", false)
            .unwrap()
            .scheme(),
        "https"
    );
    for base in [
        "http://127.0.0.1:11434/v1",
        "http://localhost:1234/v1",
        "http://192.168.1.5:8000/v1",
        "http://[::1]:8000/v1",
    ] {
        assert_eq!(
            assert_usable_api_base(base, true).unwrap().scheme(),
            "http",
            "{base}"
        );
    }
    assert_eq!(
        assert_usable_api_base("http://example.com/v1", false)
            .unwrap()
            .host_str(),
        Some("example.com")
    );
}

#[test]
fn assert_usable_api_base_refuses_a_key_over_plain_http_to_a_public_host() {
    let refused = assert_usable_api_base("http://example.com/v1", true).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Config);
    assert!(refused.message.contains("plain HTTP"));
    // Decimal-encoded 8.8.8.8: the point of routing through the address
    // classifier rather than a string test for "127." or "192.168.".
    let decimal = assert_usable_api_base("http://134744072/v1", true).unwrap_err();
    assert!(
        decimal.message.contains("plain HTTP"),
        "{}",
        decimal.message
    );
}

#[test]
fn assert_usable_api_base_rejects_a_non_url_and_a_non_http_scheme() {
    assert!(
        assert_usable_api_base("not a url", false)
            .unwrap_err()
            .message
            .contains("not a URL")
    );
    assert!(
        assert_usable_api_base("file:///etc/passwd", false)
            .unwrap_err()
            .message
            .contains("http or https")
    );
    assert!(
        assert_usable_api_base("ftp://example.com", false)
            .unwrap_err()
            .message
            .contains("http or https")
    );
}

#[test]
fn requires_a_base_url_when_the_table_has_no_default() {
    let spec = ProviderSpec::new("custom", "Custom", WireProtocol::OpenaiChat, &[]);
    let error = OpenAiChatProvider::new(WireAdapterOptions::new(spec)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("no apiBase"));
}

#[test]
fn rejects_a_bad_base_url_and_bad_headers_as_configuration_errors() {
    let bad_base = OpenAiChatProvider::new(WireAdapterOptions {
        api_base: Some("nonsense".into()),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap_err();
    assert_eq!(bad_base.kind, ErrorKind::Config);
    assert!(!ProviderError::is_provider_error(&bad_base));

    let bad_header = OpenAiChatProvider::new(WireAdapterOptions {
        extra_headers: [("bad header".to_owned(), "x".to_owned())]
            .into_iter()
            .collect(),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap_err();
    assert_eq!(bad_header.kind, ErrorKind::Config);
    let bad_value = OpenAiChatProvider::new(WireAdapterOptions {
        extra_headers: [("x-ok".to_owned(), "line\nbreak".to_owned())]
            .into_iter()
            .collect(),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap_err();
    assert_eq!(bad_value.kind, ErrorKind::Config);
    let bad_key = OpenAiChatProvider::new(WireAdapterOptions {
        api_key: Some("line\nbreak".into()),
        api_base: Some("https://x.test/v1".into()),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap_err();
    assert_eq!(bad_key.kind, ErrorKind::Config);
}

#[tokio::test]
async fn sends_the_bearer_token_and_the_table_headers() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(
        200,
        &completion(CompletionOptions::text("ok")),
    ));
    let provider = create_openai_chat_provider(WireAdapterOptions {
        api_base: Some(server.base_url()),
        api_key: Some("sk-or-secret".into()),
        extra_headers: [("x-custom".to_owned(), "yes".to_owned())]
            .into_iter()
            .collect(),
        ..WireAdapterOptions::new(common::spec_of("openrouter"))
    })
    .unwrap();
    provider.chat(&base(), &token()).await.unwrap();
    let headers = &server.calls()[0].headers;
    assert_eq!(
        headers.get("authorization").map(String::as_str),
        Some("Bearer sk-or-secret")
    );
    assert_eq!(headers.get("x-title").map(String::as_str), Some("GhostAI"));
    assert_eq!(headers.get("x-custom").map(String::as_str), Some("yes"));
    assert_eq!(
        headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    assert!(format!("{provider:?}").contains("openrouter"));
}

#[tokio::test]
async fn omits_authorization_for_a_keyless_local_server_and_joins_paths_cleanly() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(
        200,
        &completion(CompletionOptions::text("ok")),
    ));
    let provider = create_openai_chat_provider(WireAdapterOptions {
        api_base: Some(format!("{}/", server.base_url())),
        api_key: Some(String::new()),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap();
    provider.chat(&base(), &token()).await.unwrap();
    let call = &server.calls()[0];
    assert!(!call.headers.contains_key("authorization"));
    assert_eq!(call.path, "/v1/chat/completions");
}

#[tokio::test]
async fn encodes_images_files_prefixes_tools_and_efforts() {
    let server = ScriptedServer::start().await;
    for _ in 0..6 {
        server.push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions::text("ok")),
        ));
    }
    let provider = provider_on(&server, "ollama", None);
    let t = token();

    provider
        .chat(
            &ChatRequest::new(
                "test-model",
                vec![common::user_parts(vec![
                    text_part("what is this?"),
                    image_part("image/png", ImageSource::Data("AAA".into())),
                    file_part("uploads/ab12-notes.csv", "text/csv", FileDetails::default()),
                ])],
            ),
            &t,
        )
        .await
        .unwrap();
    let content = &server.calls()[0].body["messages"][0]["content"];
    assert_eq!(
        content[1],
        json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}})
    );
    assert!(
        content[2]["text"]
            .as_str()
            .unwrap()
            .contains("uploads/ab12-notes.csv")
    );

    // tools and tool_choice only when tools are present.
    provider
        .chat(
            &ChatRequest {
                tool_choice: Some(ToolChoice::Required),
                ..base()
            },
            &t,
        )
        .await
        .unwrap();
    assert!(server.calls()[1].body.get("tools").is_none());
    assert!(server.calls()[1].body.get("tool_choice").is_none());
    provider
        .chat(
            &ChatRequest {
                tool_choice: Some(ToolChoice::Required),
                tools: vec![common::tool_definition("t", "d", json!({"type": "object"}))],
                ..base()
            },
            &t,
        )
        .await
        .unwrap();
    assert_eq!(server.calls()[2].body["tool_choice"], json!("required"));
    assert_eq!(
        server.calls()[2].body["tools"][0]["function"]["name"],
        json!("t")
    );

    // An effort goes straight through; `xhigh` is Qwen3.8's own top rung.
    for (effort, expected) in [
        (ReasoningEffort::High, "high"),
        (ReasoningEffort::Xhigh, "xhigh"),
        (ReasoningEffort::Off, "none"),
    ] {
        provider
            .chat(
                &ChatRequest {
                    reasoning_effort: Some(effort),
                    ..base()
                },
                &t,
            )
            .await
            .unwrap();
        assert_eq!(
            server.calls().last().unwrap().body["reasoning_effort"],
            json!(expected)
        );
    }
}

#[tokio::test]
async fn spells_off_the_way_the_table_says_and_handles_model_prefixes() {
    let server = ScriptedServer::start().await;
    server
        .push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions::text("ok")),
        ))
        .push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions::text("ok")),
        ));
    let openrouter = provider_on(&server, "openrouter", Some("sk-or-test"));
    openrouter
        .chat(
            &ChatRequest {
                reasoning_effort: Some(ReasoningEffort::Off),
                model: "anthropic/claude-sonnet-4".into(),
                ..base()
            },
            &token(),
        )
        .await
        .unwrap();
    let body = &server.calls()[0].body;
    assert_eq!(body["reasoning"], json!({"enabled": false}));
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["model"], json!("anthropic/claude-sonnet-4"));

    let openai = provider_on(&server, "openai", Some("k"));
    openai
        .chat(
            &ChatRequest {
                model: "openai/gpt-4o".into(),
                ..base()
            },
            &token(),
        )
        .await
        .unwrap();
    assert_eq!(server.calls()[1].body["model"], json!("gpt-4o"));
}

#[tokio::test]
async fn classifies_a_200_with_no_choices_as_a_server_glitch() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(200, &json!({"choices": []})));
    let error = provider_on(&server, "ollama", None)
        .chat(&base(), &token())
        .await
        .unwrap_err();
    let provider_error = ProviderError::of(&error);
    assert_eq!(provider_error.reason, ProviderErrorReason::Server);
    assert!(provider_error.retryable);
    assert_eq!(provider_error.status, Some(200));
}

#[tokio::test]
async fn reports_a_refused_connection_as_network_not_a_provider_rejection() {
    let port = common::refused_port();
    let provider = create_openai_chat_provider(WireAdapterOptions {
        api_base: Some(format!("http://127.0.0.1:{port}/v1")),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap();
    let error = provider.chat(&base(), &token()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    let provider_error = ProviderError::of(&error);
    assert_eq!(provider_error.reason, ProviderErrorReason::Transport);
    assert!(
        provider_error.message.starts_with(&format!(
            "Could not reach Ollama at http://127.0.0.1:{port} — "
        )),
        "{}",
        provider_error.message
    );
    assert!(
        provider_error
            .message
            .contains("nothing is listening there")
    );
    assert!(!provider_error.retryable);

    // The streaming and listing paths go through the same door.
    let stream_error = collect(provider.stream(base(), token())).await.unwrap_err();
    assert_eq!(
        ProviderError::reason_of(&stream_error),
        ProviderErrorReason::Transport
    );
    let list_error = provider.list_models(&token()).await.unwrap_err();
    assert_eq!(
        ProviderError::reason_of(&list_error),
        ProviderErrorReason::Transport
    );
}

#[tokio::test]
async fn surfaces_an_error_delivered_inside_a_200_stream() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::sse(
        &[
            text_chunk("par"),
            json!({"error": {"message": "upstream died", "code": "server_error"}}),
        ],
        false,
    ));
    let mut events = provider_on(&server, "ollama", None).stream(base(), token());
    let mut seen = Vec::new();
    let mut failure = None;
    while let Some(event) = events.next().await {
        match event {
            Ok(ChatStreamEvent::Text(text)) => seen.push(text),
            Ok(_) => {}
            Err(error) => failure = Some(error),
        }
    }
    assert_eq!(seen, vec!["par"]);
    let failure = failure.unwrap();
    assert!(failure.message.contains("upstream died"));
    assert_eq!(
        ProviderError::reason_of(&failure),
        ProviderErrorReason::InvalidRequest
    );

    // OpenRouter spells the code as an HTTP status.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::sse(
        &[json!({"error": {"message": "too many", "code": 429}})],
        false,
    ));
    let error = collect(provider_on(&server, "ollama", None).stream(base(), token()))
        .await
        .unwrap_err();
    assert_eq!(
        ProviderError::reason_of(&error),
        ProviderErrorReason::RateLimit
    );
}

#[tokio::test]
async fn rejects_a_truncated_stream_and_an_empty_one() {
    // No terminating newline: the frame never completes, which is what a
    // connection dropped mid-write actually looks like.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::raw(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"par",
    ));
    let error = collect(provider_on(&server, "ollama", None).stream(base(), token()))
        .await
        .unwrap_err();
    assert_eq!(
        ProviderError::reason_of(&error),
        ProviderErrorReason::StreamParse
    );

    // No body at all: the stream ends before saying anything, which is a
    // complete, empty answer rather than a parse failure.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::empty(200));
    let collected = collect(provider_on(&server, "ollama", None).stream(base(), token()))
        .await
        .unwrap();
    assert!(collected.done.unwrap().message.content.is_empty());
}

#[tokio::test]
async fn raises_when_the_socket_closes_mid_stream() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::streaming(
        vec![sse_body(&[text_chunk("Hel")], false)],
        Ending::Close,
    ));
    let mut events = provider_on(&server, "ollama", None).stream(base(), token());
    let mut seen = Vec::new();
    let mut failure = None;
    while let Some(event) = events.next().await {
        match event {
            Ok(ChatStreamEvent::Text(text)) => seen.push(text),
            Ok(_) => {}
            Err(error) => failure = Some(error),
        }
    }
    assert_eq!(seen, vec!["Hel"]);
    let failure = ProviderError::of(&failure.unwrap());
    assert_eq!(failure.reason, ProviderErrorReason::Transport);
    assert!(failure.retryable);
}

#[tokio::test]
async fn a_stalled_stream_hits_the_idle_timeout() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::streaming(
        vec![sse_body(&[text_chunk("Hel")], false)],
        Ending::Hang,
    ));
    let provider = create_openai_chat_provider(WireAdapterOptions {
        api_base: Some(server.base_url()),
        stream_idle_timeout_ms: Some(100),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap();
    let mut events = provider.stream(base(), token());
    let mut failure = None;
    while let Some(event) = events.next().await {
        if let Err(error) = event {
            failure = Some(error);
        }
    }
    assert_eq!(
        ProviderError::reason_of(&failure.unwrap()),
        ProviderErrorReason::Timeout
    );
}

#[tokio::test]
async fn a_request_that_is_already_cancelled_never_leaves() {
    let server = ScriptedServer::start().await;
    let provider = provider_on(&server, "ollama", None);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let error = provider.chat(&base(), &cancelled).await.unwrap_err();
    assert!(error.is_aborted());
    assert_eq!(ProviderError::of(&error).provider_id, "ollama");
    assert!(server.calls().is_empty());
}

#[tokio::test]
async fn reads_a_non_json_error_body_without_losing_the_status() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::raw(
        502,
        "text/html",
        "<html>502 Bad Gateway</html>",
    ));
    let error = provider_on(&server, "ollama", None)
        .chat(&base(), &token())
        .await
        .unwrap_err();
    let provider_error = ProviderError::of(&error);
    assert_eq!(provider_error.reason, ProviderErrorReason::Server);
    assert_eq!(provider_error.status, Some(502));
    assert!(provider_error.message.contains("502 Bad Gateway"));

    // A JSON error with a code and a param carries both.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(
        400,
        &error_body(&json!({"message": "bad", "code": "unsupported_parameter", "param": "x"})),
    ));
    let error = provider_on(&server, "ollama", None)
        .chat(&base(), &token())
        .await
        .unwrap_err();
    let provider_error = ProviderError::of(&error);
    assert_eq!(
        provider_error.code.as_deref(),
        Some("unsupported_parameter")
    );
    assert_eq!(provider_error.param.as_deref(), Some("x"));
    assert_eq!(provider_error.message, "ollama request failed (400): bad");

    // An empty error body still names the status.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::empty(500));
    let error = provider_on(&server, "ollama", None)
        .chat(&base(), &token())
        .await
        .unwrap_err();
    assert_eq!(error.message, "ollama request failed (500)");
}

#[tokio::test]
async fn generates_a_tool_call_id_when_the_provider_omits_one() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(
        200,
        &json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{"type": "function", "function": {"name": "list_dir", "arguments": "{}"}}],
                },
                "finish_reason": "tool_calls",
            }]
        }),
    ));
    let provider = create_openai_chat_provider(WireAdapterOptions {
        api_base: Some(server.base_url()),
        random: Some(Arc::new(FixedRandom::constant(0xab))),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap();
    let result = provider.chat(&base(), &token()).await.unwrap();
    let expected = tool_call_id(&FixedRandom::constant(0xab));
    assert_eq!(expected, "call_abababababab4bab");
    assert_eq!(result.message.tool_calls[0].id, expected);
    assert_eq!(result.message.tool_calls[0].name, "list_dir");
    // A v4 UUID's version nibble, whatever the bytes.
    assert_eq!(&tool_call_id(&FixedRandom::constant(0xff))[17..18], "4");
}

#[tokio::test]
async fn re_serialises_tool_arguments_a_provider_sent_as_an_object() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(
        200,
        &json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}],
                    "reasoning": "hmm",
                    "tool_calls": [
                        {"id": "c1", "function": {"name": "read_file", "arguments": {"path": "a.txt"}}},
                        {"id": "c2", "function": {"name": "no_args"}},
                        {"id": "c3", "function": {}},
                    ],
                },
                "finish_reason": "length",
            }],
            "usage": {
                "prompt_tokens": 1.0, "completion_tokens": 2,
                "prompt_tokens_details": {"cached_tokens": 1},
                "completion_tokens_details": {"reasoning_tokens": 1},
            },
        }),
    ));
    let result = provider_on(&server, "ollama", None)
        .chat(&base(), &token())
        .await
        .unwrap();
    assert_eq!(result.message.tool_calls.len(), 2);
    assert_eq!(
        result.message.tool_calls[0].arguments_json,
        "{\"path\":\"a.txt\"}"
    );
    assert_eq!(result.message.tool_calls[1].arguments_json, "{}");
    assert_eq!(result.message.content, vec![text_part("a\nb")]);
    assert_eq!(result.message.reasoning.as_deref(), Some("hmm"));
    // Tool calls are the fact; the label is a claim about them.
    assert_eq!(result.finish_reason, FinishReason::ToolCalls);
    assert_eq!(result.usage.total_tokens, 3);
    assert_eq!(result.usage.cached_tokens, Some(1));
    assert_eq!(result.usage.reasoning_tokens, Some(1));
    assert_eq!(result.model, "test-model");
}

#[tokio::test]
async fn decodes_every_finish_reason_spelling() {
    let server = ScriptedServer::start().await;
    for reason in [
        "length",
        "max_tokens",
        "content_filter",
        "function_call",
        "weird",
    ] {
        server.push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions {
                finish_reason: Some(reason.into()),
                ..CompletionOptions::text("x")
            }),
        ));
    }
    let provider = provider_on(&server, "ollama", None);
    let mut seen = Vec::new();
    for _ in 0..5 {
        seen.push(
            provider
                .chat(&base(), &token())
                .await
                .unwrap()
                .finish_reason,
        );
    }
    assert_eq!(
        seen,
        vec![
            FinishReason::Length,
            FinishReason::Length,
            FinishReason::ContentFilter,
            FinishReason::ToolCalls,
            FinishReason::Stop
        ]
    );
}

#[tokio::test]
async fn closing_is_safe_at_any_point() {
    let server = ScriptedServer::start().await;
    server
        .push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions::text("one")),
        ))
        .push(ScriptedResponse::json(
            200,
            &completion(CompletionOptions::text("two")),
        ));
    let provider = provider_on(&server, "ollama", None);
    provider.close().await;
    provider.chat(&base(), &token()).await.unwrap();
    provider.close().await;
    provider.close().await;
    // The pool is rebuilt on the next request.
    provider.chat(&base(), &token()).await.unwrap();
    assert_eq!(server.calls().len(), 2);
}

// Everything above goes through the crate's own scripted server. These two
// drive the same adapter through wiremock, so the ordinary request path is
// covered by a second, independent HTTP implementation.

#[tokio::test]
async fn completes_a_turn_through_wiremock() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "qwen3",
            "choices": [{"message": {"role": "assistant", "content": "from wiremock"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2, "total_tokens": 6},
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "qwen3"}, {"id": ""}, {"object": "model"}]
        })))
        .mount(&server)
        .await;
    let provider = create_openai_chat_provider(WireAdapterOptions {
        api_base: Some(format!("{}/v1", server.uri())),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap();
    let result = provider
        .chat(
            &ChatRequest::new("qwen3", vec![common::user("hi")]),
            &token(),
        )
        .await
        .unwrap();
    assert_eq!(result.message.content, vec![text_part("from wiremock")]);
    assert_eq!(result.usage.total_tokens, 6);
    assert_eq!(result.model, "qwen3");
    assert_eq!(result.generation_ms, None);
    assert_eq!(result.first_token_ms, None);
    let models = provider.list_models(&token()).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "qwen3");
    assert_eq!(models[0].provider_id, "ollama");
}

#[tokio::test]
async fn streams_through_wiremock_and_surfaces_a_real_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "7")
                .set_body_json(json!({"error": {"message": "slow down"}})),
        )
        .mount(&server)
        .await;
    let provider = create_openai_chat_provider(WireAdapterOptions {
        api_base: Some(format!("{}/v1", server.uri())),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap();
    let error = collect(provider.stream(base(), token())).await.unwrap_err();
    let provider_error = ProviderError::of(&error);
    assert_eq!(provider_error.reason, ProviderErrorReason::RateLimit);
    assert_eq!(provider_error.status, Some(429));
    assert_eq!(provider_error.retry_after_ms, Some(7000));
}

// What the turn-stats rate divides by. Time is moved in two places, matching
// where it goes in life: `load_ms` is advanced inside the request handler,
// which is where a local server loads its weights, while the POST is
// outstanding; `step_ms` is advanced by the consumer between events, and
// time that moves while the stream is suspended is time it reads on the next
// frame it parses.
async fn drain(frames: &[Value], load_ms: u64, step_ms: u64) -> Option<ChatResult> {
    let clock = Arc::new(ManualClock::at(1_700_000_000_000));
    let server = ScriptedServer::start().await;
    let loading = Arc::clone(&clock);
    server.push(
        ScriptedResponse::sse(frames, true)
            .before(move || loading.advance(Duration::from_millis(load_ms))),
    );
    let provider = create_openai_chat_provider(WireAdapterOptions {
        api_base: Some(server.base_url()),
        clock: Some(clock.clone()),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap();
    let mut events = provider.stream(base(), token());
    let mut result = None;
    while let Some(event) = events.next().await {
        if let ChatStreamEvent::Done(done) = event.unwrap() {
            result = Some(done);
        }
        clock.advance(Duration::from_millis(step_ms));
    }
    result
}

fn usage(prompt: u64, completion: u64) -> Value {
    usage_chunk(&json!({"prompt_tokens": prompt, "completion_tokens": completion}))
}

#[tokio::test]
async fn times_generation_from_the_first_content_frame_not_the_request() {
    // Five seconds of weight loading and 100 ms of generation. Charging the
    // load to the rate is what reported a fast local model as a slow one.
    let result = drain(
        &[text_chunk("one"), text_chunk("two"), usage(10, 2)],
        5000,
        100,
    )
    .await
    .unwrap();
    assert_eq!(result.generation_ms, Some(100.0));
    assert_eq!(result.first_token_ms, Some(5000.0));
}

#[tokio::test]
async fn opens_a_window_for_a_reply_that_is_nothing_but_a_tool_call() {
    // Nothing is yielded for a tool-call frame, so a consumer counting deltas
    // sees an instant response, while the provider streamed the JSON and
    // charged completion tokens for it. The assertion is that a window
    // *exists*.
    let result = drain(
        &[
            tool_call_chunk(0, Some("c1"), Some("read_file"), None),
            tool_call_chunk(0, None, None, Some("{\"path\":\"a\"}")),
            finish_chunk("tool_calls"),
            usage(10, 12),
        ],
        5000,
        100,
    )
    .await
    .unwrap();
    assert_eq!(result.first_token_ms, Some(5000.0));
    assert_eq!(result.generation_ms, Some(0.0));
    assert_eq!(
        result.message.tool_calls[0].arguments_json,
        "{\"path\":\"a\"}"
    );
}

#[tokio::test]
async fn reports_nothing_for_a_stream_that_carried_no_content() {
    // A role-only opening frame and a usage trailer are the two frames an
    // OpenAI-compatible server sends around the content, and neither is
    // content.
    let result = drain(
        &[
            json!({"choices": [{"index": 0, "delta": {"role": "assistant"}}]}),
            usage(10, 0),
        ],
        5000,
        100,
    )
    .await
    .unwrap();
    assert_eq!(result.generation_ms, None);
    assert_eq!(result.first_token_ms, None);
    assert_eq!(result.usage.prompt_tokens, 10);
}

#[tokio::test]
async fn reports_a_zero_window_for_one_frame_and_opens_on_reasoning() {
    let result = drain(&[text_chunk("all of it"), usage(10, 3)], 5000, 100)
        .await
        .unwrap();
    assert_eq!(result.generation_ms, Some(0.0));
    assert_eq!(result.first_token_ms, Some(5000.0));

    // A model that thinks before it answers is generating throughout.
    let result = drain(
        &[reasoning_chunk("hmm"), text_chunk("answer"), usage(10, 5)],
        5000,
        100,
    )
    .await
    .unwrap();
    assert_eq!(result.generation_ms, Some(100.0));
    assert_eq!(result.first_token_ms, Some(5000.0));
    assert_eq!(result.message.reasoning.as_deref(), Some("hmm"));
}

#[tokio::test]
async fn a_tool_call_delta_without_an_index_is_its_own_call() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::sse(
        &[
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                {"id": "a", "function": {"name": "one", "arguments": "{}"}}
            ]}}]}),
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                {"function": {"name": "two", "arguments": "{}"}}, "not an object"
            ]}}]}),
        ],
        true,
    ));
    let provider = create_openai_chat_provider(WireAdapterOptions {
        api_base: Some(server.base_url()),
        random: Some(Arc::new(FixedRandom::constant(1))),
        ..WireAdapterOptions::new(common::spec_of("ollama"))
    })
    .unwrap();
    let done = collect(provider.stream(base(), token()))
        .await
        .unwrap()
        .done
        .unwrap();
    assert_eq!(done.message.tool_calls.len(), 2);
    assert_eq!(done.message.tool_calls[0].id, "a");
    assert!(done.message.tool_calls[1].id.starts_with("call_"));
    assert_eq!(done.message.tool_calls[1].name, "two");
}
