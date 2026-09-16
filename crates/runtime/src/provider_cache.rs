//! Provider instances, kept between reconfigurations.
//!
//! A [`ChatProvider`] is not a value: the adapter underneath it holds an HTTP
//! client with keep-alive sockets. Building a new one on every settings save —
//! and the settings panel saves a panel at a time — would leak a connection pool
//! per keystroke and pay a fresh TLS handshake on the next turn. So a runtime
//! asks the cache, and gets the same instance back whenever nothing about the
//! connection changed.
//!
//! Two decisions worth stating:
//!
//!  - **The credential is part of the identity, and never part of the key.** A
//!    key saved in the settings UI has to be usable on the next turn without a
//!    restart, so a cache keyed on `(provider, model, api_base)` alone would
//!    hand back the adapter that still carries the old one — or none. It is
//!    folded in as a SHA-256 digest instead, because a map key is the kind of
//!    thing that ends up in a heap dump or a debug log, and an API key should be
//!    in neither.
//!
//!  - **The cache is bounded, evicts least-recently-used, and closes what it
//!    evicts.** A session flipping between models must not accumulate one
//!    connection pool per model it ever touched. `close()` is graceful and
//!    idempotent, so an eviction cannot cut off a turn that is still streaming
//!    through the adapter it evicted — and the turn holds its own reference, so
//!    the adapter outlives the map entry for as long as it is in use.

use std::sync::Arc;

use darkwire_core::Result;
use darkwire_providers::{
    ChatProvider, CreateProviderOptions, ProviderSpec, Resilience, WireAdapters, create_provider,
};
use indexmap::IndexMap;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

/// Beyond this many live adapters the least-recently-used is dropped.
///
/// Sized for what a person does, not for what a script can: a few models across
/// a couple of providers is a normal working set, and the cost of a miss is one
/// adapter construction. Sized for one adapter *per instance* rather than per
/// provider type — enumerating models asks every configured endpoint at once.
pub const MAX_CACHED_PROVIDERS: usize = 16;

/// What one adapter is built from, and what it is keyed on.
#[derive(Clone)]
pub struct ProviderRequest {
    /// The instance this adapter belongs to.
    ///
    /// Part of the key so two instances of one type stay two adapters even when
    /// their connection details coincide — a shared client between them would
    /// make disabling or re-keying one silently affect the other.
    pub instance_id: String,
    /// The provider type.
    pub spec: ProviderSpec,
    /// Part of the key even though [`create_provider`] does not take it: a
    /// spec's `model_overrides` and `max_tokens_param` are per-model, so two
    /// models on one provider are two adapters as soon as either is used.
    pub model: String,
    /// The effective base URL.
    pub api_base: String,
    /// Headers every request carries.
    pub extra_headers: IndexMap<String, String>,
    /// From the vault or the environment. Digested into the key, never spelled
    /// into it.
    pub api_key: Option<String>,
    /// Wire adapters an extension contributed, and deliberately **not** part of
    /// the key.
    ///
    /// A wire is a property of the spec, and `spec.id` is already in the key, so
    /// two adapters for one wire cannot both be reachable. What that costs is
    /// the case where an extension is reloaded with a *different* adapter for
    /// the same wire: the cached provider keeps the old one until the process
    /// restarts.
    pub wires: Option<WireAdapters>,
}

impl std::fmt::Debug for ProviderRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The credential is deliberately absent: this type is printed in logs
        // and in a test failure, and the key it produces already hides it.
        f.debug_struct("ProviderRequest")
            .field("instance_id", &self.instance_id)
            .field("provider", &self.spec.id)
            .field("model", &self.model)
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

/// A stable digest of the parts that must not appear in the key in the clear.
fn digest(parts: &[Option<&str>]) -> String {
    let mut hash = Sha256::new();
    // Length-prefixed so `["ab", "c"]` and `["a", "bc"]` cannot collide, and an
    // absent value is distinguishable from an empty one.
    for part in parts {
        match part {
            None => hash.update(b"-"),
            Some(value) => {
                hash.update(b"+");
                hash.update(value.len().to_le_bytes());
                hash.update(value.as_bytes());
            }
        }
    }
    let out = hash.finalize();
    out.iter().take(8).fold(String::new(), |mut text, byte| {
        use std::fmt::Write as _;
        // Infallible: writing to a `String` cannot fail.
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// The cache key for one connection.
///
/// Exported for the test that asserts a credential never appears in it.
pub fn provider_cache_key(request: &ProviderRequest) -> String {
    let mut headers: Vec<(&str, &str)> = request
        .extra_headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    headers.sort_unstable();
    let mut parts: Vec<Option<&str>> = Vec::with_capacity(headers.len() * 2 + 1);
    for (name, value) in headers {
        parts.push(Some(name));
        parts.push(Some(value));
    }
    parts.push(request.api_key.as_deref());
    format!(
        "{} {} {} {} {}",
        request.instance_id,
        request.spec.id,
        request.model,
        request.api_base,
        digest(&parts)
    )
}

/// Builds an adapter. Injected by tests, which count constructions rather than
/// opening sockets.
pub type ProviderFactory =
    Arc<dyn Fn(CreateProviderOptions) -> Result<Arc<dyn ChatProvider>> + Send + Sync>;

/// Live adapters, keyed by connection identity.
pub struct ProviderCache {
    /// Insertion order is the recency order: a hit is removed and re-inserted,
    /// so the first entry is always the least recently used.
    entries: Mutex<IndexMap<String, Arc<dyn ChatProvider>>>,
    max: usize,
    create: ProviderFactory,
}

impl std::fmt::Debug for ProviderCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderCache")
            .field("size", &self.entries.lock().len())
            .field("max", &self.max)
            .finish_non_exhaustive()
    }
}

impl Default for ProviderCache {
    fn default() -> Self {
        ProviderCache::new()
    }
}

impl ProviderCache {
    /// A cache of [`MAX_CACHED_PROVIDERS`] over the real factory.
    pub fn new() -> ProviderCache {
        ProviderCache::with_factory(MAX_CACHED_PROVIDERS, Arc::new(create_provider))
    }

    /// A cache with its own bound and factory.
    pub fn with_factory(max: usize, create: ProviderFactory) -> ProviderCache {
        ProviderCache {
            entries: Mutex::new(IndexMap::new()),
            max,
            create,
        }
    }

    /// How many adapters are live.
    pub fn size(&self) -> usize {
        self.entries.lock().len()
    }

    /// The adapter for one connection, built on first use.
    pub fn get(&self, request: &ProviderRequest) -> Result<Arc<dyn ChatProvider>> {
        let key = provider_cache_key(request);
        {
            let mut entries = self.entries.lock();
            if let Some(hit) = entries.shift_remove(&key) {
                entries.insert(key, Arc::clone(&hit));
                return Ok(hit);
            }
        }

        // Deliberately outside the lock and outside the map: construction fails
        // on a wire with no adapter, and caching that would turn one config
        // error into a permanent one that survives the operator fixing it.
        let mut options = CreateProviderOptions::new(request.spec.clone());
        options.api_key.clone_from(&request.api_key);
        options.api_base = Some(request.api_base.clone());
        options.extra_headers.clone_from(&request.extra_headers);
        options.wires.clone_from(&request.wires);
        options.resilience = Resilience::Default;
        let provider = (self.create)(options)?;

        let evicted = {
            let mut entries = self.entries.lock();
            entries.insert(key, Arc::clone(&provider));
            let mut evicted: Vec<Arc<dyn ChatProvider>> = Vec::new();
            while entries.len() > self.max {
                if let Some((_, old)) = entries.shift_remove_index(0) {
                    evicted.push(old);
                }
            }
            evicted
        };
        for old in evicted {
            release(old);
        }
        Ok(provider)
    }

    /// Closes every adapter and empties the cache.
    pub fn clear(&self) {
        let drained: Vec<Arc<dyn ChatProvider>> =
            self.entries.lock().drain(..).map(|(_, one)| one).collect();
        for provider in drained {
            release(provider);
        }
    }
}

/// Returns the adapter's connection pool, without waiting for it.
///
/// `close()` is idempotent and graceful, and a pool that refuses to drain is not
/// something the caller could act on. The alternative is an async eviction,
/// which would make every settings save — and every runtime shutdown — a future
/// the CLI has to await in a `finally`.
///
/// Off a runtime there is nothing to spawn onto and dropping the last reference
/// releases the sockets anyway, which is what a synchronous caller gets.
fn release(provider: Arc<dyn ChatProvider>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move { provider.close().await });
    }
}
