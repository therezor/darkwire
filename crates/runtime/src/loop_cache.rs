//! One [`AgentLoop`] per agent, kept between turns.
//!
//! A loop is cheap to hold and not free to build: it resolves a provider
//! instance, pulls an adapter out of the provider cache and freezes a tool
//! scope. Rebuilding one per turn would do all of that on every message, and —
//! worse — would make "a turn keeps the loop it started on" untrue, which is the
//! property that lets a settings save land safely while a turn is running.
//!
//! Three decisions, each mirroring [`crate::JailCache`] because the problem is
//! the same shape:
//!
//!  - **The default agent is built eagerly and never evicted.** The runtime's
//!    build computes everything able to fail before it mutates anything, and the
//!    default agent's provider resolution is what `configured`, `model` and
//!    `instance` all mean. Building it lazily would move that failure to the
//!    first turn after a save, which is exactly where it must not be.
//!
//!  - **Construction happens outside the map.** A factory that fails must not
//!    leave a poisoned entry behind, because the next call would return the
//!    failure rather than retrying it.
//!
//!  - **Eviction is a plain removal.** A loop owns no handles — the provider
//!    adapter it points at belongs to [`crate::ProviderCache`], which closes it
//!    on its own eviction — so dropping the reference is the whole of closing
//!    one.

use std::sync::Arc;

use ghostai_agent::AgentLoop;
use ghostai_core::Result;
use ghostai_protocol::DEFAULT_AGENT_ID;
use indexmap::IndexMap;
use parking_lot::Mutex;

/// Beyond this many live loops the least-recently-used is dropped.
///
/// Sized for what an operator does: a handful of agents in play across a working
/// session. A miss costs one provider-cache lookup and one object.
pub const MAX_CACHED_LOOPS: usize = 8;

/// Builds the loop for an agent id.
///
/// `None` means nothing can run — no provider resolved, or no model — which is a
/// state rather than an error, exactly as it is on the runtime.
pub type LoopFactory = Arc<dyn Fn(&str) -> Result<Option<AgentLoop>> + Send + Sync>;

/// Live loops, keyed by agent id.
pub struct LoopCache {
    create: LoopFactory,
    max: usize,
    /// Insertion-ordered, which is what makes the first key the LRU victim.
    loops: Mutex<IndexMap<String, AgentLoop>>,
}

impl std::fmt::Debug for LoopCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopCache")
            .field("size", &self.loops.lock().len())
            .field("max", &self.max)
            .finish_non_exhaustive()
    }
}

impl LoopCache {
    /// A cache of [`MAX_CACHED_LOOPS`] over `create`.
    pub fn new(create: LoopFactory) -> LoopCache {
        LoopCache::with_max(create, MAX_CACHED_LOOPS)
    }

    /// A cache with its own bound.
    pub fn with_max(create: LoopFactory, max: usize) -> LoopCache {
        LoopCache {
            create,
            max,
            loops: Mutex::new(IndexMap::new()),
        }
    }

    /// The loop for an agent, built on first use.
    ///
    /// `None` propagates from the factory and is deliberately *not* cached: an
    /// unconfigured install becomes configured by a settings save, and that save
    /// rebuilds the runtime and drops this cache — but a `None` memoised here
    /// would also have to be invalidated by anything else that could change the
    /// answer. Nothing is gained by remembering "no".
    pub fn get(&self, agent_id: &str) -> Result<Option<AgentLoop>> {
        {
            let mut loops = self.loops.lock();
            if let Some(hit) = loops.shift_remove(agent_id) {
                // Re-inserted so the working set stays at the young end.
                loops.insert(agent_id.to_owned(), hit.clone());
                return Ok(Some(hit));
            }
        }

        // Outside the map on purpose: a failure here must not be remembered.
        let Some(built) = (self.create)(agent_id)? else {
            return Ok(None);
        };

        let mut loops = self.loops.lock();
        loops.insert(agent_id.to_owned(), built.clone());
        if loops.len() > self.max {
            // Never the default: it is the one every unbound session runs on, so
            // evicting it to make room for an agent used once would guarantee a
            // rebuild on the very next turn.
            let victim = loops
                .keys()
                .find(|key| key.as_str() != DEFAULT_AGENT_ID)
                .cloned();
            if let Some(victim) = victim {
                loops.shift_remove(&victim);
            }
        }
        Ok(Some(built))
    }

    /// How many loops are live. For tests and for a status page.
    pub fn size(&self) -> usize {
        self.loops.lock().len()
    }

    /// Drops every loop.
    pub fn clear(&self) {
        self.loops.lock().clear();
    }
}
