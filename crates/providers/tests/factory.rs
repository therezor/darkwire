//! From configuration to a working provider.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::sync::Arc;

use ghostai_core::ErrorKind;
use ghostai_protocol::ProviderConfig;
use ghostai_providers::testkit::{
    CompletionOptions, ScriptedProvider, ScriptedStep, completion, error_body, provider_error,
    result_of,
};
use ghostai_providers::{
    ChatRequest, CreateProviderOptions, ProviderError, ProviderErrorReason, ProviderRef,
    ProviderSpec, Resilience, ResilienceOptions, WireAdapters, WireProtocol, create_provider,
    resolve_connection,
};
use indexmap::IndexMap;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn base() -> ChatRequest {
    ChatRequest::new("test-model", vec![common::user("hi")])
}

fn options(provider: impl Into<ProviderRef>, api_base: Option<String>) -> CreateProviderOptions {
    CreateProviderOptions {
        api_base,
        ..CreateProviderOptions::new(provider)
    }
}

#[test]
fn builds_a_provider_from_a_table_id_or_a_spec() {
    let provider = create_provider(options("ollama", None)).unwrap();
    assert_eq!(provider.id(), "ollama");
    assert_eq!(provider.spec().wire, WireProtocol::OpenaiChat);

    let spec = ProviderSpec {
        default_api_base: Some("https://example.invalid/v1".into()),
        ..ProviderSpec::new(
            "extension-provider",
            "From an extension",
            WireProtocol::OpenaiChat,
            &[],
        )
    };
    assert_eq!(
        create_provider(options(spec, None)).unwrap().id(),
        "extension-provider"
    );
}

#[test]
fn names_the_unknown_provider_and_refuses_a_wire_with_no_adapter() {
    let unknown = create_provider(options("nope", None)).unwrap_err();
    assert_eq!(unknown.kind, ErrorKind::Config);
    assert!(unknown.message.contains("nope"));

    // Silently falling back to the OpenAI shape would surface as a 404 in the
    // middle of a turn, which reads as "the model is gone".
    let anthropic = create_provider(CreateProviderOptions {
        api_key: Some("k".into()),
        ..CreateProviderOptions::new("anthropic")
    })
    .unwrap_err();
    assert_eq!(anthropic.kind, ErrorKind::Config);
    assert!(anthropic.message.contains("anthropic-messages"));
    assert!(anthropic.message.contains("extension"));
}

#[test]
fn takes_a_wire_adapter_an_extension_supplied_but_never_a_replacement() {
    let calls = Arc::new(std::sync::Mutex::new(0));
    let counted = Arc::clone(&calls);
    let mut wires = WireAdapters::new();
    let adapter: ghostai_providers::WireAdapter = Arc::new(move |options| {
        *counted.lock().unwrap() += 1;
        Ok(ScriptedProvider::new(options.spec, Vec::new()) as _)
    });
    wires.insert(WireProtocol::AnthropicMessages, Arc::clone(&adapter));
    wires.insert(WireProtocol::OpenaiChat, adapter);

    let provider = create_provider(CreateProviderOptions {
        api_key: Some("k".into()),
        resilience: Resilience::Disabled,
        wires: Some(wires.clone()),
        ..CreateProviderOptions::new("anthropic")
    })
    .unwrap();
    assert_eq!(provider.id(), "anthropic");
    assert_eq!(*calls.lock().unwrap(), 1);

    // `openai-chat` is what every local provider speaks; it stays built in.
    create_provider(CreateProviderOptions {
        wires: Some(wires),
        ..CreateProviderOptions::new("ollama")
    })
    .unwrap();
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn wraps_an_extension_provider_in_resilience_like_any_other() {
    // The reason the seam is here rather than "hand us a ChatProvider": retry
    // and degradation are inherited rather than reimplemented.
    let mut wires = WireAdapters::new();
    wires.insert(
        WireProtocol::AnthropicMessages,
        Arc::new(|options| {
            Ok(ScriptedProvider::new(
                options.spec,
                vec![
                    ScriptedStep::Error(provider_error(ProviderErrorReason::Overloaded, "busy")),
                    ScriptedStep::Result(result_of("second try")),
                ],
            ) as _)
        }),
    );
    let provider = create_provider(CreateProviderOptions {
        api_key: Some("k".into()),
        wires: Some(wires),
        resilience: Resilience::Configured(ResilienceOptions {
            jitter: Some(Arc::new(|| 1.0)),
            base_delay_ms: Some(1),
            ..ResilienceOptions::default()
        }),
        ..CreateProviderOptions::new("anthropic")
    })
    .unwrap();
    let result = provider
        .chat(&base(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        result.message.content,
        vec![ghostai_core::messages::text_part("second try")]
    );
}

async fn overloaded_then_ok() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(error_body(&json!({"message": "overloaded"}))),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(completion(CompletionOptions::text("second try"))),
        )
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn wraps_with_resilience_by_default() {
    let server = overloaded_then_ok().await;
    let provider = create_provider(CreateProviderOptions {
        resilience: Resilience::Configured(ResilienceOptions {
            jitter: Some(Arc::new(|| 1.0)),
            base_delay_ms: Some(1),
            ..ResilienceOptions::default()
        }),
        ..options("ollama", Some(format!("{}/v1", server.uri())))
    })
    .unwrap();
    let result = provider
        .chat(&base(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        result.message.content,
        vec![ghostai_core::messages::text_part("second try")]
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);

    // The default configuration is also a wrapper; its first backoff is
    // 500 ms at most, which is what this run spends.
    let server = overloaded_then_ok().await;
    let provider =
        create_provider(options("ollama", Some(format!("{}/v1", server.uri())))).unwrap();
    provider
        .chat(&base(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn returns_the_bare_adapter_when_resilience_is_switched_off() {
    let server = overloaded_then_ok().await;
    let provider = create_provider(CreateProviderOptions {
        resilience: Resilience::Disabled,
        ..options("ollama", Some(format!("{}/v1", server.uri())))
    })
    .unwrap();
    let error = provider
        .chat(&base(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.message.contains("503"), "{}", error.message);
    assert_eq!(
        ProviderError::of(&error).reason,
        ProviderErrorReason::Overloaded
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert!(format!("{:?}", options("ollama", None)).contains("CreateProviderOptions"));
}

fn config(kind: &str, api_base: Option<&str>, headers: &[(&str, &str)]) -> ProviderConfig {
    ProviderConfig {
        kind: kind.to_owned(),
        label: String::new(),
        api_base: api_base.map(str::to_owned),
        extra_headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        models: Vec::new(),
        enabled: true,
    }
}

#[test]
fn resolve_connection_layers_config_over_the_table() {
    let ollama = common::spec_of("ollama");
    assert_eq!(
        resolve_connection(&ollama, None).api_base,
        "http://127.0.0.1:11434/v1"
    );
    // A cleared text box is unset, not an empty URL.
    assert_eq!(
        resolve_connection(&ollama, Some(&config("ollama", Some("   "), &[]))).api_base,
        "http://127.0.0.1:11434/v1"
    );
    assert_eq!(
        resolve_connection(
            &ollama,
            Some(&config("ollama", Some("http://gpu.local:11434/v1"), &[]))
        )
        .api_base,
        "http://gpu.local:11434/v1"
    );
    let openrouter = common::spec_of("openrouter");
    let headers = resolve_connection(
        &openrouter,
        Some(&config(
            "openrouter",
            None,
            &[("x-mine", "1"), ("X-Title", "Mine")],
        )),
    )
    .extra_headers;
    let expected: IndexMap<String, String> = [("X-Title", "Mine"), ("x-mine", "1")]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    assert_eq!(headers, expected);
    assert_eq!(
        resolve_connection(&common::spec_of("custom"), None).api_base,
        ""
    );
}
