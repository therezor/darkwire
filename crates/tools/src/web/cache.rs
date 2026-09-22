//! What a turn does not have to fetch twice.
//!
//! The reason this exists is narrow and real: a model that searches, fails to
//! read a result, and searches again must not pay for the search twice. It is
//! not a general-purpose store, and it is deliberately in memory and
//! short-lived. A fetched page is worth minutes, not restarts, and putting one
//! in the session database makes pruning it somebody's job.
//!
//! **Every key carries the agent id, and that is a security property rather
//! than a nicety.** One resolver serves every agent on the install. Keyed by URL
//! alone, an agent whose egress is open could fetch an internal host and an
//! agent restricted to an allow-list that does not name it would be served the
//! cached copy: an egress bypass through the cache. Keying by agent means two
//! agents cannot share an entry at all, which is a coarser answer than
//! fingerprinting the policy and a much harder one to get subtly wrong.

use std::num::NonZeroUsize;
use std::sync::Arc;

use darkwire_core::Clock;
use lru::LruCache;
use parking_lot::Mutex;

/// One cached value and the moment it stops being usable.
struct Entry<T> {
    value: T,
    expires_at_ms: i64,
}

/// A bounded, expiring store keyed by agent and request.
pub struct WebCache<T> {
    entries: Mutex<LruCache<(String, String), Entry<T>>>,
    clock: Arc<dyn Clock>,
    ttl_ms: i64,
}

impl<T: Clone> WebCache<T> {
    /// A cache holding `capacity` entries for `ttl_ms` each. A capacity of zero
    /// disables it, which is how an operator turns it off.
    pub fn new(capacity: usize, ttl_ms: u64, clock: Arc<dyn Clock>) -> WebCache<T> {
        let size = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        WebCache {
            entries: Mutex::new(if capacity == 0 {
                LruCache::new(NonZeroUsize::MIN)
            } else {
                LruCache::new(size)
            }),
            clock,
            ttl_ms: i64::try_from(ttl_ms).unwrap_or(i64::MAX),
        }
    }

    fn disabled(&self) -> bool {
        self.ttl_ms == 0
    }

    /// The value stored for this agent and request, if it has not expired.
    pub fn get(&self, agent_id: &str, request: &str) -> Option<T> {
        if self.disabled() {
            return None;
        }
        let key = (agent_id.to_owned(), request.to_owned());
        let mut entries = self.entries.lock();
        let entry = entries.get(&key)?;
        if entry.expires_at_ms <= self.clock.now_ms() {
            entries.pop(&key);
            return None;
        }
        Some(entry.value.clone())
    }

    /// Stores a value against this agent and request.
    pub fn put(&self, agent_id: &str, request: &str, value: T) {
        if self.disabled() {
            return;
        }
        let expires_at_ms = self.clock.now_ms().saturating_add(self.ttl_ms);
        self.entries.lock().put(
            (agent_id.to_owned(), request.to_owned()),
            Entry {
                value,
                expires_at_ms,
            },
        );
    }
}

impl<T> std::fmt::Debug for WebCache<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebCache")
            .field("entries", &self.entries.lock().len())
            .field("ttlMs", &self.ttl_ms)
            .finish_non_exhaustive()
    }
}
