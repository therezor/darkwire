//! Configured endpoints, as distinct from provider types.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_protocol::{ProviderConfig, ProvidersConfig};
use darkwire_providers::{
    PROVIDERS, ProviderSpec, ResolveInstanceOptions, WireProtocol, describe_instance,
    find_instance, instance_label, list_instances, next_instance_id, resolve_instance,
};
use indexmap::IndexMap;

fn instance(kind: &str) -> ProviderConfig {
    ProviderConfig {
        kind: kind.to_owned(),
        label: String::new(),
        api_base: None,
        extra_headers: IndexMap::new(),
        models: Vec::new(),
        enabled: true,
    }
}

fn providers(entries: Vec<(&str, ProviderConfig)>) -> ProvidersConfig {
    entries
        .into_iter()
        .map(|(id, config)| (id.to_owned(), config))
        .collect()
}

/// Two Ollama servers, the case one-entry-per-provider could not express.
fn two_ollamas() -> ProvidersConfig {
    providers(vec![
        (
            "ollama",
            ProviderConfig {
                api_base: Some("http://127.0.0.1:11434/v1".into()),
                ..instance("ollama")
            },
        ),
        (
            "ollama-gpu",
            ProviderConfig {
                label: "GPU box".into(),
                api_base: Some("http://gpu.lan:11434/v1".into()),
                ..instance("ollama")
            },
        ),
    ])
}

#[test]
fn list_instances_resolves_in_config_order_and_skips_unknown_types() {
    let listed = list_instances(&two_ollamas(), &PROVIDERS);
    let ids: Vec<&str> = listed.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec!["ollama", "ollama-gpu"]);
    assert!(listed.iter().all(|i| i.spec.id == "ollama"));

    // A typo in one instance must not take the other nine down with it.
    let mixed = list_instances(
        &providers(vec![
            ("good", instance("ollama")),
            ("bad", instance("ollamaa")),
        ]),
        &PROVIDERS,
    );
    assert_eq!(mixed.len(), 1);
    assert_eq!(mixed[0].id, "good");
}

#[test]
fn instance_label_prefers_the_label() {
    let listed = list_instances(&two_ollamas(), &PROVIDERS);
    assert_eq!(instance_label(&listed[0]), "Ollama");
    assert_eq!(instance_label(&listed[1]), "GPU box");
}

#[test]
fn find_instance_is_none_for_unknown_ids_and_types() {
    let two = two_ollamas();
    assert!(find_instance(&two, "nope", &PROVIDERS).is_none());
    assert_eq!(
        find_instance(&two, "ollama-gpu", &PROVIDERS).unwrap().id,
        "ollama-gpu"
    );
    let bad = providers(vec![("bad", instance("ollamaa"))]);
    assert!(find_instance(&bad, "bad", &PROVIDERS).is_none());
}

#[test]
fn next_instance_id_numbers_upward_past_what_is_taken() {
    assert_eq!(next_instance_id("ollama", []), "ollama");
    assert_eq!(next_instance_id("ollama", ["ollama"]), "ollama-2");
    assert_eq!(
        next_instance_id("ollama", ["ollama", "ollama-2", "ollama-3"]),
        "ollama-4"
    );
}

#[test]
fn describe_instance_reports_the_effective_base_and_the_given_flag() {
    let plain = list_instances(&providers(vec![("ollama", instance("ollama"))]), &PROVIDERS);
    assert_eq!(
        describe_instance(&plain[0], false).api_base,
        "http://127.0.0.1:11434/v1"
    );

    let gpu = list_instances(&two_ollamas(), &PROVIDERS).remove(1);
    let described = describe_instance(&gpu, true);
    assert_eq!(described.id, "ollama-gpu");
    assert_eq!(described.kind, "ollama");
    assert_eq!(described.display_name, "GPU box");
    assert_eq!(described.api_base, "http://gpu.lan:11434/v1");
    assert!(described.credentials_present);
    assert!(described.supports_model_listing);
    assert!(described.is_local);
    assert!(described.enabled);
}

fn resolve<'a>(
    providers: &'a ProvidersConfig,
    provider: Option<&'a str>,
    model: Option<&'a str>,
) -> Option<String> {
    resolve_instance(&ResolveInstanceOptions {
        providers,
        provider,
        model,
        has_credential: None,
        specs: None,
    })
    .map(|instance| instance.id)
}

#[test]
fn resolve_prefers_an_exact_instance_then_the_first_of_a_type() {
    let two = two_ollamas();
    assert_eq!(
        resolve(&two, Some("ollama-gpu"), None).as_deref(),
        Some("ollama-gpu")
    );
    assert_eq!(
        resolve(&two, Some("ollama"), None).as_deref(),
        Some("ollama")
    );
    // A name that is neither an instance nor a type is a typo.
    assert_eq!(resolve(&two, Some("ollamaa"), None), None);
}

#[test]
fn resolve_synthesises_an_instance_for_an_unconfigured_type() {
    let none = ProvidersConfig::new();
    let resolved = resolve_instance(&ResolveInstanceOptions {
        providers: &none,
        provider: Some("ollama"),
        model: None,
        has_credential: None,
        specs: None,
    })
    .unwrap();
    assert_eq!(resolved.id, "ollama");
    assert_eq!(resolved.config.api_base, None);
    assert!(resolved.config.enabled);
}

#[test]
fn resolve_is_blind_to_a_disabled_instance() {
    let config = providers(vec![
        (
            "off",
            ProviderConfig {
                enabled: false,
                ..instance("ollama")
            },
        ),
        ("on", instance("lmstudio")),
    ]);
    assert_eq!(resolve(&config, Some("off"), None), None);
    assert_eq!(resolve(&config, None, None).as_deref(), Some("on"));
}

#[test]
fn resolve_under_auto_uses_the_model_then_the_gateway_then_credentials() {
    let by_model = providers(vec![
        ("local", instance("ollama")),
        ("cloud", instance("openai")),
    ]);
    assert_eq!(
        resolve(&by_model, Some("auto"), Some("gpt-4o")).as_deref(),
        Some("cloud")
    );

    let with_gateway = providers(vec![
        ("first", instance("openai")),
        (
            "router",
            ProviderConfig {
                api_base: Some("https://openrouter.ai/api/v1".into()),
                ..instance("openrouter")
            },
        ),
    ]);
    assert_eq!(
        resolve(&with_gateway, None, None).as_deref(),
        Some("router")
    );

    let two = providers(vec![
        ("first", instance("ollama")),
        ("second", instance("lmstudio")),
    ]);
    let has_credential = |id: &str| id == "second";
    let resolved = resolve_instance(&ResolveInstanceOptions {
        providers: &two,
        provider: None,
        model: None,
        has_credential: Some(&has_credential),
        specs: None,
    });
    assert_eq!(resolved.map(|i| i.id).as_deref(), Some("second"));

    assert_eq!(
        resolve(&two_ollamas(), None, None).as_deref(),
        Some("ollama")
    );
    assert_eq!(resolve(&ProvidersConfig::new(), None, None), None);
}

#[test]
fn resolve_sees_a_provider_type_an_extension_contributed() {
    let spec = ProviderSpec {
        default_api_base: Some("https://llm.corp.invalid/v1".into()),
        ..ProviderSpec::new("corp-llm", "Corp LLM", WireProtocol::OpenaiChat, &["corp"])
    };
    let house = providers(vec![("house", instance("corp-llm"))]);
    assert_eq!(resolve(&house, Some("house"), None), None);
    let mut specs: Vec<ProviderSpec> = PROVIDERS.clone();
    specs.push(spec);
    let resolved = resolve_instance(&ResolveInstanceOptions {
        providers: &house,
        provider: Some("house"),
        model: None,
        has_credential: None,
        specs: Some(&specs),
    });
    assert_eq!(resolved.map(|i| i.spec.id).as_deref(), Some("corp-llm"));

    // And its keywords answer an auto resolution.
    let both = providers(vec![
        ("local", instance("ollama")),
        ("house", instance("corp-llm")),
    ]);
    let resolved = resolve_instance(&ResolveInstanceOptions {
        providers: &both,
        provider: Some("auto"),
        model: Some("corp-large"),
        has_credential: None,
        specs: Some(&specs),
    });
    assert_eq!(resolved.map(|i| i.id).as_deref(), Some("house"));
    assert!(
        format!(
            "{:?}",
            ResolveInstanceOptions {
                providers: &both,
                provider: None,
                model: None,
                has_credential: None,
                specs: None
            }
        )
        .contains("ResolveInstanceOptions")
    );
}
