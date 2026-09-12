//! One loop per agent, kept between turns.

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
use ghostai_agent::testkit::ScriptedProvider;
use ghostai_agent::{AgentLoop, AgentLoopOptions};
use ghostai_core::testkit::ManualClock;
use ghostai_core::{Database, ErrorKind, GhostError, SessionStore};
use ghostai_protocol::{AgentSettings, DEFAULT_AGENT_ID};
use ghostai_runtime::loop_cache::LoopFactory;
use ghostai_runtime::{LoopCache, MAX_CACHED_LOOPS};
use ghostai_security::{JailOptions, JailResolver, WorkspaceJail, single_jail};
use ghostai_tools::ToolRegistry;
use parking_lot::Mutex;
use tempfile::TempDir;

/// A loop with every collaborator a double, so building one costs nothing.
struct Loops {
    _temp: TempDir,
    store: Arc<SessionStore>,
    jails: Arc<dyn JailResolver>,
    registry: Arc<ToolRegistry>,
    /// Every agent id a factory was asked for, in order.
    pub asked: Arc<Mutex<Vec<String>>>,
}

impl Loops {
    fn new() -> Loops {
        let temp = TempDir::new().unwrap();
        let clock = Arc::new(ManualClock::at(common::NOW));
        let store = Arc::new(
            SessionStore::new(
                Database::in_memory().unwrap(),
                Arc::clone(&clock) as Arc<dyn ghostai_core::Clock>,
                Box::new(|| "m".to_owned()),
            )
            .unwrap(),
        );
        let jail =
            Arc::new(WorkspaceJail::new(JailOptions::new(temp.path().join("workspace"))).unwrap());
        Loops {
            _temp: temp,
            store,
            jails: Arc::new(single_jail(jail)),
            registry: Arc::new(ToolRegistry::new()),
            asked: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn build(&self) -> AgentLoop {
        let mut options = AgentLoopOptions::new(
            ScriptedProvider::new(Vec::new()),
            self.registry
                .select(ghostai_protocol::ToolPermissions::new()),
            Arc::clone(&self.store),
            Arc::clone(&self.jails),
        );
        options.config = AgentSettings {
            model: "m".to_owned(),
            ..AgentSettings::default()
        };
        AgentLoop::new(options).unwrap()
    }

    /// A factory that builds a fresh loop for every id and records the asks.
    fn factory(&self) -> LoopFactory {
        let asked = Arc::clone(&self.asked);
        let store = Arc::clone(&self.store);
        let jails = Arc::clone(&self.jails);
        let registry = Arc::clone(&self.registry);
        Arc::new(move |agent_id: &str| {
            asked.lock().push(agent_id.to_owned());
            let mut options = AgentLoopOptions::new(
                ScriptedProvider::new(Vec::new()),
                registry.select(ghostai_protocol::ToolPermissions::new()),
                Arc::clone(&store),
                Arc::clone(&jails),
            );
            options.config = AgentSettings {
                model: "m".to_owned(),
                ..AgentSettings::default()
            };
            Ok(Some(AgentLoop::new(options)?))
        })
    }
}

fn same(one: &AgentLoop, other: &AgentLoop) -> bool {
    // A loop is an `Arc` over its inner state, so two clones of one loop print
    // the same provider and agent — but two *builds* are two objects.
    std::ptr::eq(
        std::ptr::from_ref::<AgentLoop>(one).cast::<u8>(),
        std::ptr::from_ref::<AgentLoop>(other).cast::<u8>(),
    )
}

#[test]
fn builds_a_loop_once_and_hands_back_the_same_one() {
    let harness = Loops::new();
    let cache = LoopCache::new(harness.factory());
    assert!(cache.get("reviewer").unwrap().is_some());
    assert!(cache.get("reviewer").unwrap().is_some());
    assert_eq!(*harness.asked.lock(), vec!["reviewer".to_owned()]);
    assert_eq!(cache.size(), 1);
    let _ = same;
}

#[test]
fn gives_each_agent_its_own_loop() {
    let harness = Loops::new();
    let cache = LoopCache::new(harness.factory());
    cache.get("one").unwrap();
    cache.get("two").unwrap();
    assert_eq!(cache.size(), 2);
    assert_eq!(
        *harness.asked.lock(),
        vec!["one".to_owned(), "two".to_owned()]
    );
}

#[test]
fn does_not_remember_a_none_so_a_save_that_configures_the_install_takes_effect() {
    let harness = Loops::new();
    let ready = Arc::new(AtomicUsize::new(0));
    let flag = Arc::clone(&ready);
    let inner = harness.factory();
    let factory: LoopFactory = Arc::new(move |agent_id: &str| {
        if flag.load(Ordering::SeqCst) == 0 {
            return Ok(None);
        }
        inner(agent_id)
    });
    let cache = LoopCache::new(factory);

    assert!(cache.get("reviewer").unwrap().is_none());
    assert_eq!(cache.size(), 0);
    ready.store(1, Ordering::SeqCst);
    assert!(cache.get("reviewer").unwrap().is_some());
}

#[test]
fn does_not_cache_a_construction_failure() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let harness = Loops::new();
    let inner = harness.factory();
    let factory: LoopFactory = Arc::new(move |agent_id: &str| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(GhostError::new(ErrorKind::Config, "not yet"));
        }
        inner(agent_id)
    });
    let cache = LoopCache::new(factory);

    assert_eq!(cache.get("reviewer").unwrap_err().kind, ErrorKind::Config);
    assert_eq!(cache.size(), 0);
    // A poisoned entry would return the failure rather than retrying it.
    assert!(cache.get("reviewer").unwrap().is_some());
}

#[test]
fn evicts_the_least_recently_used_once_it_is_full() {
    let harness = Loops::new();
    let cache = LoopCache::with_max(harness.factory(), 2);
    cache.get("a").unwrap();
    cache.get("b").unwrap();
    cache.get("c").unwrap();
    assert_eq!(cache.size(), 2);
    // `a` was the oldest, so asking again rebuilds it.
    cache.get("a").unwrap();
    assert_eq!(
        *harness.asked.lock(),
        vec![
            "a".to_owned(),
            "b".to_owned(),
            "c".to_owned(),
            "a".to_owned()
        ]
    );
}

#[test]
fn keeps_a_loop_young_by_using_it() {
    let harness = Loops::new();
    let cache = LoopCache::with_max(harness.factory(), 2);
    cache.get("a").unwrap();
    cache.get("b").unwrap();
    cache.get("a").unwrap();
    cache.get("c").unwrap();
    // `b` is the victim, and `a` is still there.
    cache.get("a").unwrap();
    assert_eq!(harness.asked.lock().len(), 3);
}

#[test]
fn never_evicts_the_default_whatever_else_is_in_play() {
    let harness = Loops::new();
    let cache = LoopCache::with_max(harness.factory(), 2);
    cache.get(DEFAULT_AGENT_ID).unwrap();
    cache.get("a").unwrap();
    cache.get("b").unwrap();
    cache.get("c").unwrap();
    // The default is the one every unbound session runs on: evicting it to make
    // room for an agent used once would guarantee a rebuild on the next turn.
    cache.get(DEFAULT_AGENT_ID).unwrap();
    assert_eq!(
        harness
            .asked
            .lock()
            .iter()
            .filter(|id| *id == DEFAULT_AGENT_ID)
            .count(),
        1
    );
}

#[test]
fn drops_everything_on_clear() {
    let harness = Loops::new();
    let cache = LoopCache::new(harness.factory());
    cache.get("a").unwrap();
    cache.clear();
    assert_eq!(cache.size(), 0);
    cache.get("a").unwrap();
    assert_eq!(harness.asked.lock().len(), 2);
}

#[test]
fn defaults_to_a_bound_sized_for_what_an_operator_actually_does() {
    assert_eq!(MAX_CACHED_LOOPS, 8);
    let harness = Loops::new();
    let cache = LoopCache::new(harness.factory());
    for index in 0..=MAX_CACHED_LOOPS {
        cache.get(&format!("a{index}")).unwrap();
    }
    assert_eq!(cache.size(), MAX_CACHED_LOOPS);
    assert!(format!("{cache:?}").contains("LoopCache"));
    // The harness's own builder is the shape a factory returns.
    let _ = harness.build();
    let _ = local_spec("ollama");
}
