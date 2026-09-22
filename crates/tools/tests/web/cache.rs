//! What a turn does not fetch twice, and what one agent must never see of
//! another's.

use std::sync::Arc;
use std::time::Duration;

use darkwire_core::Clock;
use darkwire_core::testkit::ManualClock;
use darkwire_tools::web::WebCache;

fn cache(ttl_ms: u64) -> (Arc<ManualClock>, WebCache<String>) {
    let clock = Arc::new(ManualClock::at(1_700_000_000_000));
    let cache = WebCache::new(8, ttl_ms, clock.clone() as Arc<dyn Clock>);
    (clock, cache)
}

#[test]
fn a_stored_value_comes_back() {
    let (_clock, cache) = cache(60_000);
    assert_eq!(cache.get("main", "https://x.example/"), None);
    cache.put("main", "https://x.example/", "page".to_owned());
    assert_eq!(
        cache.get("main", "https://x.example/"),
        Some("page".to_owned())
    );
}

/// One resolver serves every agent. Keyed by URL alone, an agent with open
/// egress could fetch an internal host and an agent restricted to an allow-list
/// that does not name it would be handed the copy: an egress bypass through the
/// cache.
#[test]
fn one_agent_never_sees_another_agents_page() {
    let (_clock, cache) = cache(60_000);
    cache.put("open", "https://internal.example/", "secret".to_owned());
    assert_eq!(cache.get("boxed", "https://internal.example/"), None);
    assert_eq!(
        cache.get("open", "https://internal.example/"),
        Some("secret".to_owned())
    );
}

#[test]
fn an_entry_expires_on_the_injected_clock() {
    let (clock, cache) = cache(60_000);
    cache.put("main", "q", "hit".to_owned());
    clock.advance(Duration::from_secs(59));
    assert_eq!(cache.get("main", "q"), Some("hit".to_owned()));
    clock.advance(Duration::from_secs(2));
    assert_eq!(cache.get("main", "q"), None);
}

/// Zero is how an operator turns it off, and off has to mean off rather than
/// "one entry".
#[test]
fn a_zero_ttl_stores_nothing() {
    let (_clock, cache) = cache(0);
    cache.put("main", "q", "hit".to_owned());
    assert_eq!(cache.get("main", "q"), None);
}

#[test]
fn the_oldest_entry_falls_out_when_the_bound_is_reached() {
    let clock = Arc::new(ManualClock::at(1_700_000_000_000));
    let cache: WebCache<String> = WebCache::new(2, 60_000, clock as Arc<dyn Clock>);
    for key in ["a", "b", "c"] {
        cache.put("main", key, key.to_owned());
    }
    assert_eq!(cache.get("main", "a"), None);
    assert_eq!(cache.get("main", "c"), Some("c".to_owned()));
}
