//! The provider table and the resolution order over it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::collections::HashSet;

use darkwire_providers::{
    GatewayHints, ModelOverride, PROVIDERS, ProviderSpec, ResolveProviderOptions, WIRE_PROTOCOLS,
    WireProtocol, describe_provider, find_builtin, find_gateway, find_provider,
    find_provider_by_model, is_provider_id, model_override_for, provider_ids, resolve_model_id,
    resolve_provider,
};

#[test]
fn the_table_has_unique_ids_and_display_names() {
    let ids: HashSet<&str> = PROVIDERS.iter().map(|spec| spec.id.as_str()).collect();
    assert_eq!(ids.len(), PROVIDERS.len());
    assert_eq!(PROVIDERS.len(), 12);
    assert!(PROVIDERS.iter().all(|spec| !spec.display_name.is_empty()));
}

#[test]
fn the_table_names_only_wires_that_exist() {
    for spec in PROVIDERS.iter() {
        assert!(WIRE_PROTOCOLS.contains(&spec.wire));
        assert_eq!(WireProtocol::parse(spec.wire.as_str()), Some(spec.wire));
    }
    assert_eq!(WireProtocol::parse("telepathy"), None);
    assert_eq!(WireProtocol::OpenaiChat.to_string(), "openai-chat");
}

#[test]
fn every_non_local_provider_has_a_way_to_be_reached() {
    // A cloud provider with no default base URL and no way to detect it is an
    // entry nothing can select.
    for spec in PROVIDERS.iter() {
        if spec.is_local || spec.id == "custom" {
            continue;
        }
        assert!(spec.default_api_base.is_some(), "{}", spec.id);
    }
}

#[test]
fn gateways_come_before_the_direct_providers_they_front() {
    let gateway = PROVIDERS.iter().position(|spec| spec.is_gateway).unwrap();
    let direct = PROVIDERS
        .iter()
        .position(|spec| !spec.is_gateway && !spec.is_local)
        .unwrap();
    assert!(gateway < direct);
}

#[test]
fn provider_ids_come_from_the_table() {
    let expected: Vec<&str> = PROVIDERS.iter().map(|spec| spec.id.as_str()).collect();
    assert_eq!(provider_ids(), expected);
    assert!(is_provider_id("ollama"));
    assert!(!is_provider_id("not-a-provider"));
    assert_eq!(
        find_builtin("groq").map(|spec| spec.display_name.as_str()),
        Some("Groq")
    );
    assert!(find_provider("groq", &[]).is_none());
}

#[test]
fn the_table_round_trips_through_json() {
    let json = serde_json::to_string(&*PROVIDERS).unwrap();
    let back: Vec<ProviderSpec> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, *PROVIDERS);
}

#[test]
fn find_by_model_honours_an_explicit_prefix_over_any_keyword() {
    // "gpt" is OpenAI's keyword and appears in the model name, but the prefix
    // is an assertion by whoever wrote it and outranks the guess.
    assert_eq!(
        find_provider_by_model("deepseek/gpt-style-model", &PROVIDERS).map(|s| s.id.as_str()),
        Some("deepseek")
    );
}

#[test]
fn find_by_model_matches_on_a_keyword() {
    let by = |model: &str| find_provider_by_model(model, &PROVIDERS).map(|s| s.id.clone());
    assert_eq!(by("gpt-4o").as_deref(), Some("openai"));
    assert_eq!(by("claude-sonnet-4").as_deref(), Some("anthropic"));
    assert_eq!(by("gemini-2.0-flash").as_deref(), Some("gemini"));
    assert_eq!(by("grok-2").as_deref(), Some("xai"));
    // Hyphen and underscore are the same character.
    assert_eq!(by("DeepSeek_R1").as_deref(), Some("deepseek"));
    // Both would match everything and nothing respectively.
    assert_eq!(by("openrouter/whatever"), None);
    assert_eq!(by("ollama"), None);
    assert_eq!(by("some-unknown-model"), None);
}

#[test]
fn find_gateway_recognises_keys_bases_and_names() {
    let id = |hints: GatewayHints<'_>| find_gateway(&hints).map(|s| s.id.clone());
    assert_eq!(
        id(GatewayHints {
            api_key: Some("sk-or-v1-abc"),
            ..GatewayHints::default()
        })
        .as_deref(),
        Some("openrouter")
    );
    assert_eq!(
        id(GatewayHints {
            api_key: Some("gsk_abc"),
            ..GatewayHints::default()
        })
        .as_deref(),
        Some("groq")
    );
    assert_eq!(
        id(GatewayHints {
            api_base: Some("http://127.0.0.1:11434/v1"),
            ..GatewayHints::default()
        })
        .as_deref(),
        Some("ollama")
    );
    assert_eq!(
        id(GatewayHints {
            provider_id: Some("vllm"),
            ..GatewayHints::default()
        })
        .as_deref(),
        Some("vllm")
    );
    assert_eq!(
        id(GatewayHints {
            provider_id: Some("openai"),
            ..GatewayHints::default()
        }),
        None
    );
    // An unrecognised base is not a local server: a DeepSeek key behind a
    // corporate proxy must not be sent to whatever the proxy is.
    assert_eq!(
        id(GatewayHints {
            api_base: Some("https://proxy.corp.example/deepseek/v1"),
            ..GatewayHints::default()
        }),
        None
    );
}

#[test]
fn resolve_provider_follows_the_one_order() {
    let id = |options: ResolveProviderOptions<'_>| resolve_provider(&options).map(|s| s.id.clone());
    assert_eq!(
        id(ResolveProviderOptions {
            provider: Some("groq"),
            model: Some("claude-sonnet-4"),
            ..ResolveProviderOptions::default()
        })
        .as_deref(),
        Some("groq")
    );
    assert_eq!(
        id(ResolveProviderOptions {
            provider: Some("auto"),
            api_key: Some("sk-or-x"),
            ..ResolveProviderOptions::default()
        })
        .as_deref(),
        Some("openrouter")
    );
    assert_eq!(
        id(ResolveProviderOptions {
            provider: Some("auto"),
            model: Some("gpt-4o"),
            ..ResolveProviderOptions::default()
        })
        .as_deref(),
        Some("openai")
    );
    // An unknown id falls through rather than failing outright.
    assert_eq!(
        id(ResolveProviderOptions {
            provider: Some("typo"),
            model: Some("claude-3"),
            ..ResolveProviderOptions::default()
        })
        .as_deref(),
        Some("anthropic")
    );
    // The key is what the request is authenticated with; it beats the model.
    assert_eq!(
        id(ResolveProviderOptions {
            api_key: Some("sk-or-x"),
            model: Some("claude-sonnet-4"),
            ..ResolveProviderOptions::default()
        })
        .as_deref(),
        Some("openrouter")
    );
    assert_eq!(id(ResolveProviderOptions::default()), None);
    assert_eq!(
        id(ResolveProviderOptions {
            provider: Some(""),
            model: Some(""),
            ..ResolveProviderOptions::default()
        }),
        None
    );
}

fn spec(id: &str) -> ProviderSpec {
    ProviderSpec::new(id, "X", WireProtocol::OpenaiChat, &[])
}

#[test]
fn resolve_model_id_applies_the_three_rules() {
    assert_eq!(resolve_model_id(&spec("openai"), "openai/gpt-4o"), "gpt-4o");
    assert_eq!(
        resolve_model_id(&spec("openrouter"), "anthropic/claude-sonnet-4"),
        "anthropic/claude-sonnet-4"
    );
    let gateway = ProviderSpec {
        strip_model_prefix: true,
        ..spec("gw")
    };
    assert_eq!(
        resolve_model_id(&gateway, "openrouter/anthropic/claude"),
        "claude"
    );
    let preserving = ProviderSpec {
        preserve_model_prefix: true,
        ..spec("nv")
    };
    assert_eq!(resolve_model_id(&preserving, "nv/model"), "nv/model");
    assert_eq!(resolve_model_id(&spec("openai"), "gpt-4o"), "gpt-4o");
    assert_eq!(resolve_model_id(&spec("openai"), "/weird"), "/weird");
}

#[test]
fn model_override_matches_a_substring_case_insensitively() {
    let moonshot = ProviderSpec {
        model_overrides: vec![ModelOverride {
            matches: "kimi-k2".into(),
            temperature: Some(1.0),
            max_tokens: None,
        }],
        ..spec("moonshot")
    };
    assert_eq!(
        model_override_for(&moonshot, "Kimi-K2-Instruct").and_then(|o| o.temperature),
        Some(1.0)
    );
    assert!(model_override_for(&moonshot, "kimi-k1").is_none());
    assert!(model_override_for(&spec("x"), "m").is_none());
}

#[test]
fn describe_provider_projects_a_table_entry() {
    let info = describe_provider(find_builtin("ollama").unwrap());
    assert_eq!(info.id, "ollama");
    assert_eq!(info.display_name, "Ollama");
    assert_eq!(info.wire, "openai-chat");
    assert!(info.is_local);
    assert!(!info.is_gateway);
    assert!(!info.is_o_auth);
    assert_eq!(
        info.default_api_base.as_deref(),
        Some("http://127.0.0.1:11434/v1")
    );
    assert_eq!(info.env_key, None);
    assert!(info.supports_model_listing);
    assert_eq!(
        PROVIDERS.iter().map(describe_provider).count(),
        PROVIDERS.len()
    );
}
