//! Provider instances kept between reconfigurations, and the key that holds
//! them.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::local_spec;
use ghostai_core::{ErrorKind, GhostError};
use ghostai_providers::testkit::ScriptedProvider;
use ghostai_providers::{ChatProvider, CreateProviderOptions, ProviderSpec};
use ghostai_runtime::provider_cache::{ProviderFactory, ProviderRequest};
use ghostai_runtime::{MAX_CACHED_PROVIDERS, ProviderCache, provider_cache_key};
use indexmap::IndexMap;

const SECRET: &str = "sk-do-not-print-me";

fn request(model: &str) -> ProviderRequest {
    ProviderRequest {
        instance_id: "ollama".to_owned(),
        spec: local_spec("ollama"),
        model: model.to_owned(),
        api_base: "http://ollama.test/v1".to_owned(),
        extra_headers: IndexMap::new(),
        api_key: None,
        wires: None,
    }
}

fn headers(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

/// A cache that counts constructions and never opens a socket.
fn counting(max: usize) -> (Arc<ProviderCache>, Arc<AtomicUsize>) {
    let built = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&built);
    let factory: ProviderFactory = Arc::new(move |options: CreateProviderOptions| {
        counter.fetch_add(1, Ordering::SeqCst);
        let spec = match &options.provider {
            ghostai_providers::ProviderRef::Spec(spec) => (**spec).clone(),
            ghostai_providers::ProviderRef::Id(id) => local_spec(id),
        };
        Ok(ScriptedProvider::new(spec, Vec::new()) as Arc<dyn ChatProvider>)
    });
    (Arc::new(ProviderCache::with_factory(max, factory)), built)
}

mod cache_key {
    use super::*;

    #[test]
    fn never_carries_the_credential_in_the_clear() {
        let mut with_key = request("m");
        with_key.api_key = Some(SECRET.to_owned());
        let key = provider_cache_key(&with_key);
        assert!(!key.contains(SECRET), "{key}");
        assert!(!key.contains("sk-"), "{key}");
        // And it is still *identity*: a different credential is a different key.
        assert_ne!(key, provider_cache_key(&request("m")));
    }

    #[test]
    fn is_stable_across_header_ordering() {
        let mut one = request("m");
        one.extra_headers = headers(&[("A", "1"), ("B", "2")]);
        let mut other = request("m");
        other.extra_headers = headers(&[("B", "2"), ("A", "1")]);
        assert_eq!(provider_cache_key(&one), provider_cache_key(&other));
    }

    #[test]
    fn distinguishes_an_absent_credential_from_an_empty_one() {
        let mut empty = request("m");
        empty.api_key = Some(String::new());
        assert_ne!(
            provider_cache_key(&request("m")),
            provider_cache_key(&empty)
        );
    }

    #[test]
    fn cannot_be_collided_by_moving_a_character_between_headers() {
        let mut one = request("m");
        one.extra_headers = headers(&[("X", "ab"), ("Y", "c")]);
        let mut other = request("m");
        other.extra_headers = headers(&[("X", "a"), ("Y", "bc")]);
        assert_ne!(provider_cache_key(&one), provider_cache_key(&other));
    }

    #[test]
    fn separates_two_instances_of_one_provider_type() {
        let mut second = request("m");
        second.instance_id = "gpu".to_owned();
        assert_ne!(
            provider_cache_key(&request("m")),
            provider_cache_key(&second)
        );
    }
}

#[test]
fn returns_the_same_adapter_when_nothing_about_the_connection_changed() {
    let (cache, built) = counting(8);
    let first = cache.get(&request("m")).unwrap();
    let second = cache.get(&request("m")).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(built.load(Ordering::SeqCst), 1);
    assert_eq!(cache.size(), 1);
}

#[test]
fn builds_a_new_adapter_when_any_part_of_the_identity_moves() {
    for edit in [
        |request: &mut ProviderRequest| request.model = "other".to_owned(),
        |request: &mut ProviderRequest| request.api_base = "http://elsewhere/v1".to_owned(),
        |request: &mut ProviderRequest| request.instance_id = "gpu".to_owned(),
        |request: &mut ProviderRequest| request.extra_headers = headers(&[("X", "1")]),
        |request: &mut ProviderRequest| request.api_key = Some(SECRET.to_owned()),
        |request: &mut ProviderRequest| request.spec = local_spec("lmstudio"),
    ] {
        let (cache, built) = counting(8);
        cache.get(&request("m")).unwrap();
        let mut moved = request("m");
        edit(&mut moved);
        cache.get(&moved).unwrap();
        assert_eq!(built.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn builds_a_new_adapter_when_the_credential_changes() {
    // The case the whole digest exists for: a key saved in the settings UI has
    // to be usable on the next turn without a restart.
    let (cache, built) = counting(8);
    let mut before = request("m");
    before.api_key = Some("old".to_owned());
    cache.get(&before).unwrap();
    let mut after = request("m");
    after.api_key = Some("new".to_owned());
    cache.get(&after).unwrap();
    assert_eq!(built.load(Ordering::SeqCst), 2);
}

#[test]
fn ignores_the_wire_adapters_which_are_not_part_of_the_identity() {
    let (cache, built) = counting(8);
    cache.get(&request("m")).unwrap();
    let mut with_wires = request("m");
    with_wires.wires = Some(ghostai_providers::WireAdapters::new());
    cache.get(&with_wires).unwrap();
    assert_eq!(built.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn evicts_the_least_recently_used_past_its_bound_and_closes_it() {
    let closed = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&closed);
    let factory: ProviderFactory = Arc::new(move |_| {
        Ok(Arc::new(Closing(Arc::clone(&counter), local_spec("ollama"))) as Arc<dyn ChatProvider>)
    });
    let cache = ProviderCache::with_factory(2, factory);

    let first = cache.get(&request("a")).unwrap();
    cache.get(&request("b")).unwrap();
    // Re-using `a` makes `b` the oldest.
    cache.get(&request("a")).unwrap();
    cache.get(&request("c")).unwrap();
    assert_eq!(cache.size(), 2);
    // `a` was re-used, so it survived; the eviction released its connection
    // pool rather than only dropping the map entry.
    assert!(Arc::ptr_eq(&first, &cache.get(&request("a")).unwrap()));
    assert!(
        common::eventually(std::time::Duration::from_secs(5), || closed
            .load(Ordering::SeqCst)
            >= 1)
        .await
    );
}

#[tokio::test]
async fn closes_everything_on_clear() {
    let closed = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&closed);
    let factory: ProviderFactory = Arc::new(move |_| {
        Ok(Arc::new(Closing(Arc::clone(&counter), local_spec("ollama"))) as Arc<dyn ChatProvider>)
    });
    let cache = ProviderCache::with_factory(8, factory);
    cache.get(&request("a")).unwrap();
    cache.get(&request("b")).unwrap();
    cache.clear();
    assert_eq!(cache.size(), 0);
    assert!(
        common::eventually(std::time::Duration::from_secs(5), || closed
            .load(Ordering::SeqCst)
            == 2)
        .await
    );
}

#[test]
fn clearing_off_a_runtime_drops_the_adapters_rather_than_failing() {
    // No tokio runtime here: there is nothing to spawn the graceful close onto,
    // and dropping the last reference releases the sockets anyway.
    let (cache, _) = counting(8);
    cache.get(&request("a")).unwrap();
    cache.clear();
    assert_eq!(cache.size(), 0);
}

#[test]
fn does_not_cache_a_construction_that_failed() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let factory: ProviderFactory = Arc::new(move |_| {
        // Fails once, as a wire with no adapter does, and then succeeds once the
        // operator has fixed the config.
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(GhostError::new(
                ErrorKind::Config,
                "no adapter for that wire",
            ));
        }
        Ok(ScriptedProvider::new(local_spec("ollama"), Vec::new()) as Arc<dyn ChatProvider>)
    });
    let cache = ProviderCache::with_factory(8, factory);

    assert_eq!(
        cache.get(&request("m")).unwrap_err().kind,
        ErrorKind::Config
    );
    assert_eq!(cache.size(), 0);
    // A cached failure would survive the operator fixing it, which is the whole
    // reason construction happens outside the map.
    assert!(cache.get(&request("m")).is_ok());
}

#[test]
fn builds_a_real_provider_through_the_default_factory() {
    // The real factory, which constructs a client and reaches nothing.
    let cache = ProviderCache::new();
    assert_eq!(cache.size(), 0);
    let provider = cache.get(&request("m")).unwrap();
    assert_eq!(provider.id(), "ollama");
    assert_eq!(cache.size(), 1);
    assert!(format!("{cache:?}").contains("ProviderCache"));
}

#[test]
fn the_default_bound_is_sized_for_one_adapter_per_instance() {
    assert_eq!(MAX_CACHED_PROVIDERS, 16);
    let cache = ProviderCache::default();
    assert_eq!(cache.size(), 0);
}

#[test]
fn a_request_never_prints_its_credential() {
    let mut with_key = request("m");
    with_key.api_key = Some(SECRET.to_owned());
    let shown = format!("{with_key:?}");
    assert!(!shown.contains(SECRET), "{shown}");
    assert!(shown.contains("ollama"), "{shown}");
}

/// A provider that counts how many times its pool was released.
struct Closing(Arc<AtomicUsize>, ProviderSpec);

impl ChatProvider for Closing {
    fn id(&self) -> &str {
        &self.1.id
    }

    fn spec(&self) -> &ProviderSpec {
        &self.1
    }

    fn chat<'a>(
        &'a self,
        request: &'a ghostai_providers::ChatRequest,
        token: &'a tokio_util::sync::CancellationToken,
    ) -> ghostai_providers::BoxFuture<'a, ghostai_core::Result<ghostai_providers::ChatResult>> {
        let _ = (request, token);
        Box::pin(async { Err(GhostError::new(ErrorKind::Internal, "not scripted")) })
    }

    fn stream(
        &self,
        request: ghostai_providers::ChatRequest,
        token: tokio_util::sync::CancellationToken,
    ) -> futures::stream::BoxStream<'static, ghostai_core::Result<ghostai_providers::ChatStreamEvent>>
    {
        let _ = (request, token);
        Box::pin(futures::stream::empty())
    }

    fn list_models<'a>(
        &'a self,
        token: &'a tokio_util::sync::CancellationToken,
    ) -> ghostai_providers::BoxFuture<'a, ghostai_core::Result<Vec<ghostai_protocol::ModelInfo>>>
    {
        let _ = token;
        Box::pin(async { Ok(Vec::new()) })
    }

    fn close(&self) -> ghostai_providers::BoxFuture<'_, ()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }
}
