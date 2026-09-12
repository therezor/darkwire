//! The channel ⇄ agent bus and its rate limiter.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ghostai_core::message_bus::{
    IdSource, InboundMessage, InboundMessageInput, MessageBus, MessageBusOptions, OutboundKind,
    OutboundMessageInput, PublishResult, RateLimitOptions, RateLimiter,
};
use ghostai_core::messages::text_part;
use ghostai_core::testkit::ManualClock;
use ghostai_protocol::ContentPart;
use serde_json::{Map, json};

const NOW: i64 = 1_700_000_000_000;

fn counter_ids() -> IdSource {
    let n = AtomicU64::new(0);
    Arc::new(move || format!("id{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

fn hello() -> Vec<ContentPart> {
    vec![text_part("hello")]
}

fn clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::at(NOW))
}

fn options() -> MessageBusOptions {
    MessageBusOptions::new(clock(), counter_ids())
}

fn make_bus() -> MessageBus {
    MessageBus::new(options())
}

fn inbound_input() -> InboundMessageInput {
    InboundMessageInput {
        channel_id: "telegram".to_owned(),
        session_key: "telegram:42".to_owned(),
        sender_id: "user-1".to_owned(),
        content: hello(),
        metadata: Map::new(),
        id: None,
    }
}

fn with_id(id: &str) -> InboundMessageInput {
    InboundMessageInput {
        id: Some(id.to_owned()),
        ..inbound_input()
    }
}

fn outbound_input() -> OutboundMessageInput {
    OutboundMessageInput {
        channel_id: "telegram".to_owned(),
        session_key: "telegram:42".to_owned(),
        target: "42".to_owned(),
        content: hello(),
        kind: OutboundKind::default(),
        metadata: Map::new(),
        id: None,
    }
}

fn accepted(id: &str) -> PublishResult {
    PublishResult::Accepted { id: id.to_owned() }
}

async fn soon<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("the future should settle promptly")
}

mod publishing_inbound {
    use super::*;

    #[tokio::test]
    async fn accepts_a_message_and_stamps_it() {
        let bus = make_bus();
        assert_eq!(bus.publish_inbound(inbound_input()), accepted("id1"));
        let received = soon(bus.inbound().next()).await.unwrap();
        assert_eq!(
            received,
            InboundMessage {
                id: "id1".to_owned(),
                channel_id: "telegram".to_owned(),
                session_key: "telegram:42".to_owned(),
                sender_id: "user-1".to_owned(),
                content: hello(),
                received_at_ms: NOW,
                metadata: Map::new(),
            }
        );
    }

    #[test]
    fn keeps_a_channel_supplied_id_for_idempotency() {
        let bus = make_bus();
        assert_eq!(bus.publish_inbound(with_id("tg-991")), accepted("tg-991"));
    }

    #[tokio::test]
    async fn carries_channel_metadata_through() {
        let bus = make_bus();
        let mut metadata = Map::new();
        metadata.insert("topicId".to_owned(), json!(7));
        bus.publish_inbound(InboundMessageInput {
            metadata: metadata.clone(),
            ..inbound_input()
        });
        let received = soon(bus.inbound().next()).await.unwrap();
        assert_eq!(received.metadata, metadata);
    }

    #[test]
    fn refuses_once_the_queue_is_full() {
        let bus = MessageBus::new(MessageBusOptions {
            capacity: 2,
            ..options()
        });
        assert!(matches!(
            bus.publish_inbound(inbound_input()),
            PublishResult::Accepted { .. }
        ));
        assert!(matches!(
            bus.publish_inbound(inbound_input()),
            PublishResult::Accepted { .. }
        ));
        assert_eq!(
            bus.publish_inbound(inbound_input()),
            PublishResult::QueueFull { queued: 2 }
        );
        assert_eq!(bus.inbound_size(), 2);
    }

    #[test]
    fn refuses_after_close() {
        let bus = make_bus();
        bus.close();
        assert_eq!(bus.publish_inbound(inbound_input()), PublishResult::Closed);
        assert!(bus.closed());
        assert!(format!("{bus:?}").contains("closed: true"));
    }
}

mod publishing_outbound {
    use super::*;

    #[tokio::test]
    async fn defaults_to_a_reply() {
        let bus = make_bus();
        bus.publish_outbound(outbound_input());
        let received = soon(bus.outbound().next()).await.unwrap();
        assert_eq!(received.kind, OutboundKind::Reply);
        assert_eq!(received.target, "42");
        assert_eq!(received.created_at_ms, NOW);
        assert_eq!(received.id, "id1");
    }

    #[tokio::test]
    async fn carries_an_explicit_kind() {
        let bus = make_bus();
        bus.publish_outbound(OutboundMessageInput {
            channel_id: "web".to_owned(),
            session_key: "web:1".to_owned(),
            target: "socket-1".to_owned(),
            kind: OutboundKind::Progress,
            ..outbound_input()
        });
        let received = soon(bus.outbound().next()).await.unwrap();
        assert_eq!(received.kind, OutboundKind::Progress);
    }

    #[test]
    fn is_not_rate_limited_because_pacing_belongs_to_the_channel() {
        let bus = MessageBus::new(MessageBusOptions {
            rate_limit: RateLimitOptions {
                per_minute: 1.0,
                burst: Some(1),
            },
            ..options()
        });
        assert!(matches!(
            bus.publish_outbound(outbound_input()),
            PublishResult::Accepted { .. }
        ));
        assert!(matches!(
            bus.publish_outbound(outbound_input()),
            PublishResult::Accepted { .. }
        ));
        assert_eq!(bus.outbound_size(), 2);
    }

    #[test]
    fn refuses_after_close_and_when_full() {
        let bus = MessageBus::new(MessageBusOptions {
            capacity: 1,
            ..options()
        });
        bus.publish_outbound(outbound_input());
        assert_eq!(
            bus.publish_outbound(outbound_input()),
            PublishResult::QueueFull { queued: 1 }
        );
        bus.close();
        assert_eq!(
            bus.publish_outbound(outbound_input()),
            PublishResult::Closed
        );
    }
}

mod consuming {
    use super::*;

    #[tokio::test]
    async fn delivers_a_message_published_after_the_consumer_started_waiting() {
        let bus = Arc::new(make_bus());
        let consumer = bus.inbound();
        let pending = tokio::spawn(async move { consumer.next().await });
        tokio::task::yield_now().await;
        bus.publish_inbound(inbound_input());
        assert!(soon(pending).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delivers_each_message_to_exactly_one_of_several_consumers() {
        let bus = Arc::new(make_bus());
        let first = bus.inbound();
        let second = bus.inbound();
        let first = tokio::spawn(async move { first.next().await });
        let second = tokio::spawn(async move { second.next().await });
        tokio::task::yield_now().await;

        bus.publish_inbound(with_id("a"));
        bus.publish_inbound(with_id("b"));

        let mut ids = vec![
            soon(first).await.unwrap().unwrap().id,
            soon(second).await.unwrap().unwrap().id,
        ];
        ids.sort();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(bus.inbound_size(), 0);
    }

    #[tokio::test]
    async fn drains_buffered_messages_after_close_then_ends() {
        let bus = make_bus();
        bus.publish_inbound(with_id("a"));
        bus.close();
        let consumer = bus.inbound();
        assert_eq!(soon(consumer.next()).await.unwrap().id, "a");
        assert!(soon(consumer.next()).await.is_none());
    }

    #[tokio::test]
    async fn releases_a_waiting_consumer_on_close_so_shutdown_does_not_hang() {
        let bus = Arc::new(make_bus());
        let consumer = bus.inbound();
        let pending = tokio::spawn(async move { consumer.next().await });
        tokio::task::yield_now().await;
        bus.close();
        assert!(soon(pending).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn ends_a_loop_when_the_bus_closes() {
        let bus = make_bus();
        bus.publish_inbound(with_id("a"));
        bus.publish_inbound(with_id("b"));
        bus.close();
        let consumer = bus.inbound();
        let mut seen = Vec::new();
        while let Some(message) = soon(consumer.next()).await {
            seen.push(message.id);
        }
        assert_eq!(seen, ["a", "b"]);
    }

    #[tokio::test]
    async fn lets_one_consumer_stop_without_disturbing_the_queue() {
        let bus = make_bus();
        bus.publish_inbound(with_id("a"));
        bus.publish_inbound(with_id("b"));
        {
            let consumer = bus.inbound();
            assert_eq!(soon(consumer.next()).await.unwrap().id, "a");
            assert!(format!("{consumer:?}").contains("Consumer"));
        }
        // `b` is still queued and still deliverable to the next consumer.
        assert_eq!(bus.inbound_size(), 1);
        assert_eq!(soon(bus.inbound().next()).await.unwrap().id, "b");
    }

    #[test]
    fn keeps_the_two_directions_independent() {
        let bus = make_bus();
        bus.publish_inbound(inbound_input());
        assert_eq!(bus.inbound_size(), 1);
        assert_eq!(bus.outbound_size(), 0);
    }
}

mod rate_limiting {
    use super::*;

    fn limiter(per_minute: f64, burst: Option<u32>, clock: Arc<ManualClock>) -> RateLimiter {
        RateLimiter::new(RateLimitOptions { per_minute, burst }, clock)
    }

    #[test]
    fn is_disabled_by_default() {
        let mut limiter = RateLimiter::new(RateLimitOptions::default(), clock());
        assert!(!limiter.enabled());
        for _ in 0..100 {
            assert_eq!(limiter.consume("user-1"), None);
        }
        assert!(format!("{limiter:?}").contains("RateLimiter"));
    }

    #[test]
    fn allows_a_burst_and_then_refuses() {
        let mut limiter = limiter(60.0, Some(3), clock());
        assert_eq!(limiter.consume("u"), None);
        assert_eq!(limiter.consume("u"), None);
        assert_eq!(limiter.consume("u"), None);
        assert!(limiter.consume("u").unwrap() > 0);
    }

    #[test]
    fn reports_how_long_until_the_next_token() {
        let mut limiter = limiter(60.0, Some(1), clock());
        limiter.consume("u");
        // 60/minute is one per second, and the bucket was just emptied.
        assert_eq!(limiter.consume("u"), Some(1_000));
    }

    #[test]
    fn refills_over_time() {
        let clock = clock();
        let mut limiter = limiter(60.0, Some(1), Arc::clone(&clock));
        limiter.consume("u");
        assert!(limiter.consume("u").unwrap() > 0);
        clock.advance(Duration::from_secs(1));
        assert_eq!(limiter.consume("u"), None);
    }

    #[test]
    fn never_refills_past_the_burst_ceiling() {
        let clock = clock();
        let mut limiter = limiter(60.0, Some(2), Arc::clone(&clock));
        clock.advance(Duration::from_mins(10));
        assert_eq!(limiter.consume("u"), None);
        assert_eq!(limiter.consume("u"), None);
        assert!(limiter.consume("u").unwrap() > 0);
    }

    #[test]
    fn meters_each_sender_separately() {
        let mut limiter = limiter(60.0, Some(1), clock());
        assert_eq!(limiter.consume("a"), None);
        assert_eq!(limiter.consume("b"), None);
        assert!(limiter.consume("a").unwrap() > 0);
    }

    #[test]
    fn defaults_the_burst_to_the_rate_capped_at_ten() {
        let mut generous = limiter(600.0, None, clock());
        for _ in 0..10 {
            assert_eq!(generous.consume("u"), None);
        }
        assert!(generous.consume("u").unwrap() > 0);
    }

    #[test]
    fn allows_at_least_one_message_even_at_a_rate_below_one_per_minute() {
        let mut limiter = limiter(0.5, None, clock());
        assert_eq!(limiter.consume("u"), None);
        assert!(limiter.consume("u").unwrap() > 0);
    }

    #[test]
    fn treats_a_negative_rate_as_disabled_rather_than_as_a_lockout() {
        let mut limiter = limiter(-5.0, None, clock());
        assert!(!limiter.enabled());
        assert_eq!(limiter.consume("u"), None);
    }

    #[test]
    fn reclaims_refilled_buckets_once_the_map_grows_past_the_ceiling() {
        let clock = clock();
        let mut limiter = limiter(60_000.0, Some(2), Arc::clone(&clock));
        for i in 0..=RateLimiter::MAX_TRACKED_SENDERS {
            clock.advance(Duration::from_millis(10));
            limiter.consume(&format!("sender-{i}"));
        }
        // Full buckets are indistinguishable from absent ones, so almost all of
        // them are reclaimable and the map collapses rather than merely capping.
        assert!(limiter.tracked_senders() < 10);
    }

    #[test]
    fn stays_bounded_even_when_nothing_is_reclaimable() {
        // Time frozen, so no bucket ever refills: the LRU pass is the only thing
        // between a flood of distinct senders and unbounded memory.
        let mut limiter = limiter(60.0, Some(2), clock());
        for i in 0..RateLimiter::MAX_TRACKED_SENDERS + 500 {
            limiter.consume(&format!("sender-{i}"));
        }
        assert_eq!(limiter.tracked_senders(), RateLimiter::MAX_TRACKED_SENDERS);
    }

    #[test]
    fn evicts_the_least_recently_used_sender_not_the_most_recent() {
        let mut limiter = limiter(60.0, Some(2), clock());
        limiter.consume("early");
        for i in 0..RateLimiter::MAX_TRACKED_SENDERS {
            limiter.consume(&format!("sender-{i}"));
        }
        // `early` was evicted, so its bucket starts full again: fail-open by
        // design, and the alternative would let a flood lock out real users.
        assert_eq!(limiter.consume("early"), None);
        assert_eq!(limiter.consume("early"), None);
    }

    #[test]
    fn surfaces_a_refusal_through_the_bus_as_a_value() {
        let bus = MessageBus::new(MessageBusOptions {
            rate_limit: RateLimitOptions {
                per_minute: 60.0,
                burst: Some(1),
            },
            ..options()
        });
        assert!(matches!(
            bus.publish_inbound(inbound_input()),
            PublishResult::Accepted { .. }
        ));
        match bus.publish_inbound(inbound_input()) {
            PublishResult::RateLimited { retry_after_ms } => assert!(retry_after_ms > 0),
            other => panic!("expected a refusal, got {other:?}"),
        }
        // Refused messages must not reach the agent.
        assert_eq!(bus.inbound_size(), 1);
    }

    #[test]
    fn options_have_a_debug_form() {
        assert!(format!("{:?}", options()).contains("capacity: 1000"));
    }
}
