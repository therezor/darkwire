//! The reference channel, against the suite every channel has to pass.
//!
//! The example is included by path rather than reimplemented, which is the
//! point: it proves the contract can be satisfied from *outside* the crate,
//! using only the public surface, and it fails to compile the moment that
//! surface stops being enough.
//!
//! The other half of `examples/loopback-channel` — the test that drove a real
//! hub, a real agent loop and a real store end to end — cannot live here,
//! because this crate must not depend on the agent or the server. It moves to
//! `ghostai-runtime`'s tests, where the composition root already has all three.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    dead_code,
    reason = "a fixture that cannot load is a failing test either way, and the example carries more surface than one suite uses"
)]

use std::sync::Arc;

use ghostai_channels::channel::BoxFuture;
use ghostai_channels::manager::{ChannelManager, ChannelManagerOptions};
use ghostai_channels::testkit::{
    ChannelConformanceOptions, ChannelProbe, ChannelUnderTest, ScriptedHub, channel_conformance,
    counter_ids, flush,
};
use ghostai_core::message_bus::PublishResult;

#[path = "../examples/loopback.rs"]
mod loopback;

use loopback::{Loopback, LoopbackOptions, loopback_channel};

/// The suite's view of a loopback channel: `say` in, transcript out.
struct LoopbackProbe(Arc<Loopback>);

impl ChannelProbe for LoopbackProbe {
    fn receive<'a>(&'a self, text: &'a str) -> BoxFuture<'a, ()> {
        self.0.say(text);
        Box::pin(std::future::ready(()))
    }

    fn sent(&self) -> Vec<String> {
        self.0.sent()
    }
}

fn under_test() -> ChannelUnderTest {
    let (factory, channel) = loopback_channel(LoopbackOptions::default());
    ChannelUnderTest {
        factory,
        probe: Arc::new(LoopbackProbe(channel)),
    }
}

#[tokio::test]
async fn the_reference_channel_satisfies_the_contract() {
    channel_conformance(&ChannelConformanceOptions::new(Arc::new(under_test))).await;
}

#[tokio::test]
async fn a_message_travels_from_the_transport_to_the_hub_and_back() {
    let hub = ScriptedHub::replying("the answer");
    let (factory, channel) = loopback_channel(LoopbackOptions::default());
    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![factory],
        ..ChannelManagerOptions::new(Arc::clone(&hub) as Arc<_>, counter_ids("loopback-"))
    })
    .expect("the manager takes the factory");
    manager.start().await.expect("the channel starts");

    let accepted = channel.say("what is the time?");
    assert!(matches!(accepted, PublishResult::Accepted { .. }));
    flush().await;
    flush().await;

    assert_eq!(channel.replies(), vec!["the answer".to_owned()]);
    // One hub connection, named by the manager rather than by the channel.
    assert_eq!(hub.only().session_key(), "loopback:default");

    manager.stop().await;
}

#[tokio::test]
async fn a_channel_that_has_not_started_refuses_input() {
    let (_factory, channel) = loopback_channel(LoopbackOptions::default());

    // Nothing has built it, so it has no context and nowhere to publish.
    assert_eq!(channel.say("hello"), PublishResult::Closed);
    assert!(channel.transcript().is_empty());
}

#[tokio::test]
async fn a_second_conversation_gets_a_second_session() {
    let hub = ScriptedHub::silent();
    let (factory, channel) = loopback_channel(LoopbackOptions::default());
    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![factory],
        ..ChannelManagerOptions::new(Arc::clone(&hub) as Arc<_>, counter_ids("loopback-"))
    })
    .expect("the manager takes the factory");
    manager.start().await.expect("the channel starts");

    channel.say("first");
    channel.say_into("second", Some("other"));
    flush().await;

    let keys: Vec<String> = hub
        .connections()
        .iter()
        .map(|connection| connection.session_key().to_owned())
        .collect();
    assert_eq!(keys, vec!["loopback:default", "loopback:other"]);
    assert_eq!(manager.session_count(), 2);

    manager.stop().await;
}
