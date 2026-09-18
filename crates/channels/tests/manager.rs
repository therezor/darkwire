//! The bridge between the message bus and the hub.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot be built is a failing test either way"
)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use common::{SESSION, session_status};
use darkwire_channels::channel::{
    BoxFuture, Channel, ChannelContext, ChannelControl, ChannelControlFrame, ChannelFactory,
    ChannelInbound, DEFAULT_ACCEPTED_KINDS,
};
use darkwire_channels::manager::{ChannelHub, ChannelManager, ChannelManagerOptions};
use darkwire_channels::testkit::{ReceivedFrame, ScriptedHub, counter_ids, flush};
use darkwire_core::message_bus::{OutboundKind, OutboundMessage, PublishResult};
use darkwire_core::messages::{FileDetails, ImageSource, file_part, image_part, text_part};
use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{
    ClientMessage, StopTurnMessage, StopTurnTag, ToolApproveMessage, ToolApproveTag,
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

// A channel that records what it was asked to do

#[derive(Default)]
struct Faults {
    start: bool,
    send: bool,
    stop: bool,
}

struct Recorder {
    id: String,
    accepts: Vec<OutboundKind>,
    sent: Mutex<Vec<OutboundMessage>>,
    context: Mutex<Option<ChannelContext>>,
    faults: Faults,
    send_delay: Option<Duration>,
    started: AtomicBool,
    stopped: AtomicUsize,
}

impl Recorder {
    fn context(&self) -> ChannelContext {
        self.context.lock().clone().expect("the manager built it")
    }

    fn publish(&self, session_key: &str, text: &str) -> PublishResult {
        self.publish_with(ChannelInbound {
            session_key: session_key.to_owned(),
            sender_id: "user-1".to_owned(),
            content: vec![text_part(text)],
            metadata: target("chat-1"),
            id: Some("m-1".to_owned()),
        })
    }

    fn publish_with(&self, message: ChannelInbound) -> PublishResult {
        self.context().publish(message)
    }

    fn control(&self, command: ChannelControl) {
        self.context().control(command);
    }

    fn sent_kinds(&self) -> Vec<OutboundKind> {
        self.sent
            .lock()
            .iter()
            .map(|message| message.kind)
            .collect()
    }

    fn sent_texts(&self) -> Vec<String> {
        self.sent
            .lock()
            .iter()
            .map(|message| {
                message
                    .content
                    .iter()
                    .map(|part| match part {
                        darkwire_protocol::ContentPart::Text(text) => text.text.clone(),
                        _ => String::new(),
                    })
                    .collect()
            })
            .collect()
    }
}

impl Channel for Recorder {
    fn id(&self) -> &str {
        &self.id
    }

    fn accepts(&self) -> &[OutboundKind] {
        &self.accepts
    }

    fn start(&self) -> BoxFuture<'_, Result<()>> {
        self.started.store(true, Ordering::Release);
        let fails = self.faults.start;
        Box::pin(async move {
            if fails {
                return Err(WireError::new(ErrorKind::Config, "the token is wrong"));
            }
            Ok(())
        })
    }

    fn stop(&self) -> BoxFuture<'_, Result<()>> {
        self.stopped.fetch_add(1, Ordering::AcqRel);
        let fails = self.faults.stop;
        Box::pin(async move {
            if fails {
                return Err(WireError::new(ErrorKind::Network, "the socket is gone"));
            }
            Ok(())
        })
    }

    fn send(&self, message: OutboundMessage) -> BoxFuture<'_, Result<()>> {
        let delay = self.send_delay;
        let fails = self.faults.send;
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            if fails {
                return Err(WireError::new(ErrorKind::Network, "the send failed"));
            }
            self.sent.lock().push(message);
            Ok(())
        })
    }
}

/// A recorder and the factory that hands it to the manager.
struct Built {
    factory: ChannelFactory,
    channel: Arc<Recorder>,
}

struct Spec {
    id: &'static str,
    /// The id the built channel claims, when it lies about it.
    claimed: Option<&'static str>,
    accepts: Vec<OutboundKind>,
    faults: Faults,
    send_delay: Option<Duration>,
}

impl Default for Spec {
    fn default() -> Spec {
        Spec {
            id: "loopback",
            claimed: None,
            accepts: DEFAULT_ACCEPTED_KINDS.to_vec(),
            faults: Faults::default(),
            send_delay: None,
        }
    }
}

fn build(spec: Spec) -> Built {
    let channel = Arc::new(Recorder {
        id: spec.claimed.unwrap_or(spec.id).to_owned(),
        accepts: spec.accepts,
        sent: Mutex::new(Vec::new()),
        context: Mutex::new(None),
        faults: spec.faults,
        send_delay: spec.send_delay,
        started: AtomicBool::new(false),
        stopped: AtomicUsize::new(0),
    });
    let built = Arc::clone(&channel);
    Built {
        factory: ChannelFactory::new(
            spec.id,
            Arc::new(move |context| {
                *built.context.lock() = Some(context);
                Ok(Arc::clone(&built) as Arc<dyn Channel>)
            }),
        ),
        channel,
    }
}

fn target(value: &str) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert("target".to_owned(), Value::String(value.to_owned()));
    metadata
}

fn options(hub: Arc<ScriptedHub>) -> ChannelManagerOptions {
    ChannelManagerOptions::new(hub as Arc<dyn ChannelHub>, counter_ids("manager-"))
}

/// A started manager over one recorder.
async fn started(hub: Arc<ScriptedHub>, spec: Spec) -> (ChannelManager, Arc<Recorder>) {
    let built = build(spec);
    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![built.factory],
        ..options(hub)
    })
    .expect("the manager takes the factory");
    manager.start().await.expect("the channel starts");
    (manager, built.channel)
}

// Inbound: channel → bus → hub

#[tokio::test]
async fn turns_a_published_message_into_the_frame_a_browser_would_have_sent() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish("chat-1", "hello");
    flush().await;

    let messages = hub.messages();
    let frame = messages.first().expect("one frame reached the hub");
    assert_eq!(frame.tag, "user.message");
    assert_eq!(frame.content.as_deref(), Some("hello"));
    // The channel's own id for the message, so a redelivered update is acked
    // rather than running the same turn twice.
    assert_eq!(frame.client_message_id.as_deref(), Some("m-1"));

    manager.stop().await;
}

#[tokio::test]
async fn namespaces_a_session_key_the_channel_chose() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish("4471", "hello");
    flush().await;

    assert_eq!(hub.only().session_key(), "loopback:4471");
    manager.stop().await;
}

#[tokio::test]
async fn leaves_a_key_that_already_carries_its_own_prefix_alone() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish("loopback:4471", "hello");
    flush().await;

    assert_eq!(hub.only().session_key(), "loopback:4471");
    manager.stop().await;
}

#[tokio::test]
async fn an_empty_key_becomes_the_channels_default_conversation() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish("", "hello");
    flush().await;

    assert_eq!(hub.only().session_key(), "loopback:default");
    manager.stop().await;
}

#[tokio::test]
async fn stamps_the_channel_id_so_a_channel_cannot_publish_as_another_one() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish("chat-1", "hello");
    flush().await;

    // The channel never names its own id: the manager binds it into `publish`.
    assert_eq!(hub.origins(), vec![Some("loopback".to_owned())]);
    manager.stop().await;
}

#[tokio::test]
async fn carries_a_file_part_through_as_an_attachment() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish_with(ChannelInbound {
        session_key: "chat-1".to_owned(),
        sender_id: "user-1".to_owned(),
        content: vec![
            text_part("look at this"),
            file_part(
                "uploads/cat.png",
                "image/png",
                FileDetails {
                    name: Some("cat.png".to_owned()),
                    size_bytes: Some(1024),
                },
            ),
        ],
        metadata: target("chat-1"),
        id: None,
    });
    flush().await;

    let messages = hub.messages();
    let frame = messages.first().expect("one frame reached the hub");
    assert_eq!(frame.content.as_deref(), Some("look at this"));
    assert_eq!(frame.attachments.len(), 1);
    assert_eq!(frame.attachments[0].path, "uploads/cat.png");
    assert_eq!(frame.attachments[0].mime_type, "image/png");
    assert_eq!(frame.attachments[0].name.as_deref(), Some("cat.png"));
    assert_eq!(frame.attachments[0].size_bytes, Some(1024));

    manager.stop().await;
}

#[tokio::test]
async fn notes_an_inline_image_rather_than_dropping_it_silently() {
    // A frame names a path, and this converter has no workspace to write bytes
    // into — so the author sees the omission instead of wondering why their
    // photo never reached the model.
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish_with(ChannelInbound {
        session_key: "chat-1".to_owned(),
        sender_id: "user-1".to_owned(),
        content: vec![image_part(
            "image/png",
            ImageSource::Data("AAAA".to_owned()),
        )],
        metadata: target("chat-1"),
        id: None,
    });
    flush().await;

    let messages = hub.messages();
    let content = messages[0].content.clone().unwrap_or_default();
    assert!(content.contains("image omitted"), "{content}");
    assert!(content.contains("image/png"), "{content}");
    assert!(messages[0].attachments.is_empty());

    manager.stop().await;
}

// Outbound: hub → bus → channel

#[tokio::test]
async fn delivers_the_answer_back_to_the_address_the_message_came_from() {
    let hub = ScriptedHub::replying("the answer");
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish_with(ChannelInbound {
        session_key: "4471".to_owned(),
        sender_id: "user-1".to_owned(),
        content: vec![text_part("hello")],
        metadata: target("chat-99"),
        id: None,
    });
    flush().await;
    flush().await;

    let sent = channel.sent.lock().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].target, "chat-99");
    assert_eq!(sent[0].session_key, "loopback:4471");
    assert_eq!(sent[0].kind, OutboundKind::Reply);

    manager.stop().await;
}

#[tokio::test]
async fn falls_back_to_the_sender_as_the_reply_address() {
    let hub = ScriptedHub::replying("the answer");
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish_with(ChannelInbound {
        session_key: "4471".to_owned(),
        sender_id: "user-7".to_owned(),
        content: vec![text_part("hello")],
        metadata: Map::new(),
        id: None,
    });
    flush().await;
    flush().await;

    assert_eq!(channel.sent.lock()[0].target, "user-7");
    manager.stop().await;
}

#[tokio::test]
async fn withholds_progress_from_a_channel_that_did_not_ask_for_it() {
    // A transport that can only post would repeat the whole answer twice, once
    // in pieces and once whole.
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish("chat-1", "hello");
    flush().await;
    let connection = hub.only();
    connection.emit(common::turn_start());
    connection.emit(common::delta("so far"));
    connection.emit(common::tool_call("c1", "read"));
    connection.emit(common::turn_end(darkwire_protocol::StopReason::Complete));
    flush().await;
    flush().await;

    assert_eq!(channel.sent_kinds(), vec![OutboundKind::Reply]);
    assert_eq!(channel.sent_texts(), vec!["so far"]);

    manager.stop().await;
}

#[tokio::test]
async fn delivers_progress_to_a_channel_that_renders_it() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(
        Arc::clone(&hub),
        Spec {
            accepts: vec![
                OutboundKind::Reply,
                OutboundKind::Notice,
                OutboundKind::Error,
                OutboundKind::Progress,
            ],
            ..Spec::default()
        },
    )
    .await;

    channel.publish("chat-1", "hello");
    flush().await;
    let connection = hub.only();
    connection.emit(common::turn_start());
    connection.emit(common::delta("so far"));
    connection.emit(common::tool_call("c1", "read"));
    connection.emit(common::turn_end(darkwire_protocol::StopReason::Complete));
    flush().await;
    flush().await;

    assert_eq!(
        channel.sent_kinds(),
        vec![OutboundKind::Progress, OutboundKind::Reply]
    );

    manager.stop().await;
}

#[tokio::test]
async fn carries_a_drafts_metadata_through_to_the_outbound_message() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish("chat-1", "hello");
    flush().await;
    let connection = hub.only();
    connection.emit(common::turn_start());
    connection.emit(common::approval_request(
        "call-3",
        "exec",
        1_700_000_000_000,
    ));
    flush().await;
    flush().await;

    let sent = channel.sent.lock().clone();
    let approval = sent[0].metadata["approval"].clone();
    assert_eq!(approval["callId"], json!("call-3"));
    assert_eq!(approval["name"], json!("exec"));
    // The manager merges the turn id in last, because it is what knows the
    // draft's turn belongs on the message.
    assert_eq!(sent[0].metadata["turnId"], json!(common::TURN));

    manager.stop().await;
}

#[tokio::test]
async fn keeps_one_channels_order_without_making_it_another_channels_problem() {
    let hub = ScriptedHub::silent();
    let slow = build(Spec {
        id: "slow",
        send_delay: Some(Duration::from_millis(80)),
        ..Spec::default()
    });
    let quick = build(Spec {
        id: "quick",
        ..Spec::default()
    });
    let slow_channel = Arc::clone(&slow.channel);
    let quick_channel = Arc::clone(&quick.channel);

    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![slow.factory, quick.factory],
        ..options(Arc::clone(&hub))
    })
    .expect("the manager takes both factories");
    manager.start().await.expect("both channels start");

    slow_channel.publish("a", "one");
    quick_channel.publish("b", "two");
    flush().await;

    let connections = hub.connections();
    for connection in &connections {
        connection.emit(common::turn_start());
        connection.emit(common::delta("first"));
        connection.emit(common::turn_end(darkwire_protocol::StopReason::Complete));
        connection.emit(common::turn_start());
        connection.emit(common::delta("second"));
        connection.emit(common::turn_end(darkwire_protocol::StopReason::Complete));
    }
    flush().await;

    // The quick channel is not queued behind the slow one's sleep.
    assert_eq!(quick_channel.sent_texts(), vec!["first", "second"]);
    assert!(slow_channel.sent_texts().len() < 2);

    manager.stop().await;
    // And the slow channel's own order held, with everything delivered.
    assert_eq!(slow_channel.sent_texts(), vec!["first", "second"]);
}

#[tokio::test]
async fn drops_an_outbound_message_addressed_to_a_channel_it_does_not_have() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    let result = manager
        .bus()
        .publish_outbound(darkwire_core::message_bus::OutboundMessageInput {
            channel_id: "nobody".to_owned(),
            session_key: "nobody:1".to_owned(),
            target: "chat-1".to_owned(),
            content: vec![text_part("into the void")],
            kind: OutboundKind::Reply,
            metadata: Map::new(),
            id: None,
        });
    assert!(matches!(result, PublishResult::Accepted { .. }));
    flush().await;

    assert!(channel.sent.lock().is_empty());
    manager.stop().await;
}

// Connections, eviction and the lifecycle

#[tokio::test]
async fn reuses_the_connection_for_a_session_it_has_already_seen() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish("chat-1", "one");
    flush().await;
    channel.publish("chat-1", "two");
    flush().await;

    assert_eq!(hub.connections().len(), 1);
    assert_eq!(manager.session_count(), 1);
    manager.stop().await;
}

#[tokio::test]
async fn evicts_the_least_recently_used_session_and_never_a_busy_one() {
    let hub = ScriptedHub::silent();
    let built = build(Spec::default());
    let channel = Arc::clone(&built.channel);
    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![built.factory],
        max_sessions: 2,
        ..options(Arc::clone(&hub))
    })
    .expect("the manager takes the factory");
    manager.start().await.expect("the channel starts");

    channel.publish("busy", "one");
    flush().await;
    // The oldest connection has a turn in flight, so eviction must skip it.
    hub.connections()[0].emit(session_status(true));
    flush().await;

    channel.publish("idle", "two");
    flush().await;
    channel.publish("fresh", "three");
    flush().await;

    assert_eq!(manager.session_count(), 2);
    let connections = hub.connections();
    assert!(
        !connections[0].is_closed(),
        "a busy session must not be evicted"
    );
    assert!(connections[1].is_closed(), "the idle session is the victim");
    assert!(!connections[2].is_closed());

    manager.stop().await;
}

#[tokio::test]
async fn stays_over_the_cap_rather_than_dropping_a_turn_in_flight() {
    let hub = ScriptedHub::silent();
    let built = build(Spec::default());
    let channel = Arc::clone(&built.channel);
    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![built.factory],
        max_sessions: 1,
        ..options(Arc::clone(&hub))
    })
    .expect("the manager takes the factory");
    manager.start().await.expect("the channel starts");

    channel.publish("busy", "one");
    flush().await;
    hub.connections()[0].emit(session_status(true));
    flush().await;
    channel.publish("second", "two");
    flush().await;

    // Every remaining pair has a turn in flight, so being over the cap beats
    // losing the reply one of them is about to produce.
    assert_eq!(manager.session_count(), 2);
    assert!(!hub.connections()[0].is_closed());

    manager.stop().await;
}

#[tokio::test]
async fn does_not_start_a_channel_its_config_disabled() {
    let built = build(Spec::default());
    let mut channels = Map::new();
    channels.insert("loopback".to_owned(), json!({ "enabled": false }));

    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![built.factory],
        channels,
        ..options(ScriptedHub::silent())
    })
    .expect("the manager takes the factory");
    manager.start().await.expect("nothing to start");

    assert!(manager.channels().is_empty());
    assert!(!built.channel.started.load(Ordering::Acquire));
    manager.stop().await;
}

#[tokio::test]
async fn hands_a_channel_only_its_own_settings_block() {
    let built = build(Spec::default());
    let mut channels = Map::new();
    channels.insert("loopback".to_owned(), json!({ "apiBase": "http://here" }));
    channels.insert("telegram".to_owned(), json!({ "token": "not yours" }));
    channels.insert("sendProgress".to_owned(), json!(false));

    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![built.factory],
        channels,
        ..options(ScriptedHub::silent())
    })
    .expect("the manager takes the factory");
    manager.start().await.expect("the channel starts");

    let settings = built.channel.context().settings;
    assert_eq!(settings["apiBase"], json!("http://here"));
    assert!(!settings.contains_key("token"));
    manager.stop().await;
}

#[tokio::test]
async fn reads_the_projection_flags_off_the_channels_config() {
    let hub = ScriptedHub::silent();
    let built = build(Spec {
        accepts: vec![
            OutboundKind::Reply,
            OutboundKind::Notice,
            OutboundKind::Error,
            OutboundKind::Progress,
        ],
        ..Spec::default()
    });
    let channel = Arc::clone(&built.channel);
    let mut channels = Map::new();
    channels.insert("sendProgress".to_owned(), json!(false));
    channels.insert("sendToolHints".to_owned(), json!(true));

    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![built.factory],
        channels,
        ..options(Arc::clone(&hub))
    })
    .expect("the manager takes the factory");
    manager.start().await.expect("the channel starts");

    channel.publish("chat-1", "hello");
    flush().await;
    let connection = hub.only();
    connection.emit(common::turn_start());
    connection.emit(common::delta("so far"));
    connection.emit(common::tool_call("c1", "read"));
    flush().await;
    flush().await;

    // Hints on, progress off.
    assert_eq!(channel.sent_texts(), vec!["Running read…"]);
    manager.stop().await;
}

#[tokio::test]
async fn refuses_a_duplicate_id_and_a_registration_after_start() {
    let first = build(Spec::default());
    let second = build(Spec::default());
    let third = build(Spec {
        id: "another",
        ..Spec::default()
    });

    let duplicate = ChannelManager::new(ChannelManagerOptions {
        factories: vec![first.factory, second.factory],
        ..options(ScriptedHub::silent())
    });
    let error = duplicate.expect_err("a duplicate id is refused");
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(
        error.message.contains("already registered"),
        "{}",
        error.message
    );

    let manager = ChannelManager::new(options(ScriptedHub::silent())).expect("an empty manager");
    manager.start().await.expect("nothing to start");
    let late = manager.register(third.factory).expect_err("refused");
    assert_eq!(late.kind, ErrorKind::Conflict);
    assert!(
        late.message.contains("after the manager started"),
        "{}",
        late.message
    );
    manager.stop().await;
}

#[tokio::test]
async fn fails_to_start_when_a_factory_builds_a_channel_under_another_id() {
    let built = build(Spec {
        id: "loopback",
        claimed: Some("impostor"),
        ..Spec::default()
    });
    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![built.factory],
        ..options(ScriptedHub::silent())
    })
    .expect("the manager takes the factory");

    let error = manager.start().await.expect_err("the mismatch is refused");
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("impostor"), "{}", error.message);
    assert!(manager.channels().is_empty());
}

#[tokio::test]
async fn stops_the_channels_it_already_started_when_a_later_one_fails() {
    // A half-started manager is a process where some channels answer and the
    // rest are silent, with nothing saying which is which.
    let good = build(Spec {
        id: "good",
        ..Spec::default()
    });
    let bad = build(Spec {
        id: "bad",
        faults: Faults {
            start: true,
            ..Faults::default()
        },
        ..Spec::default()
    });
    let good_channel = Arc::clone(&good.channel);

    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![good.factory, bad.factory],
        ..options(ScriptedHub::silent())
    })
    .expect("the manager takes both factories");

    let error = manager.start().await.expect_err("the bad channel fails");
    assert_eq!(error.kind, ErrorKind::Config);
    assert_eq!(good_channel.stopped.load(Ordering::Acquire), 1);
    assert!(manager.channels().is_empty());
}

#[tokio::test]
async fn closes_its_connections_and_stops_its_channels_on_stop() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish("chat-1", "hello");
    flush().await;
    let token = channel.context().token.clone();

    manager.stop().await;

    assert!(
        token.is_cancelled(),
        "the token fires before stop() is called"
    );
    assert!(hub.only().is_closed());
    assert_eq!(channel.stopped.load(Ordering::Acquire), 1);
    assert_eq!(manager.session_count(), 0);
    assert!(manager.channels().is_empty());
}

#[tokio::test]
async fn survives_a_channel_that_fails_on_send_and_on_stop() {
    let hub = ScriptedHub::replying("the answer");
    let (manager, channel) = started(
        Arc::clone(&hub),
        Spec {
            faults: Faults {
                send: true,
                stop: true,
                start: false,
            },
            ..Spec::default()
        },
    )
    .await;

    channel.publish("chat-1", "hello");
    flush().await;
    flush().await;

    // One failed send is not a dead pump: the next message still goes out.
    channel.publish("chat-2", "again");
    flush().await;
    assert_eq!(hub.messages().len(), 2);

    // And a transport that cannot say goodbye does not stop the shutdown.
    manager.stop().await;
    assert_eq!(manager.session_count(), 0);
}

#[tokio::test]
async fn stop_is_idempotent() {
    let (manager, _channel) = started(ScriptedHub::silent(), Spec::default()).await;

    manager.stop().await;
    manager.stop().await;
}

#[tokio::test]
async fn start_twice_is_a_no_op() {
    let (manager, channel) = started(ScriptedHub::silent(), Spec::default()).await;

    manager
        .start()
        .await
        .expect("the second start does nothing");

    assert_eq!(manager.channels().len(), 1);
    assert_eq!(channel.stopped.load(Ordering::Acquire), 0);
    manager.stop().await;
}

#[tokio::test]
async fn a_shared_bus_is_left_open_for_its_owner() {
    let bus = Arc::new(darkwire_core::message_bus::MessageBus::new(
        darkwire_core::message_bus::MessageBusOptions::new(
            Arc::new(darkwire_core::clock::SystemClock),
            counter_ids("shared-"),
        ),
    ));
    let built = build(Spec::default());
    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![built.factory],
        bus: Some(Arc::clone(&bus)),
        ..options(ScriptedHub::silent())
    })
    .expect("the manager takes the factory");
    manager.start().await.expect("the channel starts");

    manager.stop().await;

    assert!(
        !bus.closed(),
        "a bus the manager does not own is not its to close"
    );
}

// Control frames

fn stop_frame(session_key: &str) -> ChannelControlFrame {
    ChannelControlFrame::StopTurn(StopTurnMessage {
        tag: StopTurnTag,
        session_key: session_key.to_owned(),
    })
}

fn approve_frame() -> ChannelControlFrame {
    ChannelControlFrame::ToolApprove(ToolApproveMessage {
        tag: ToolApproveTag,
        call_id: "call-1".to_owned(),
        approved: true,
        scope: darkwire_protocol::ApprovalScope::Once,
    })
}

fn control_frames(
    connection: &darkwire_channels::testkit::ScriptedConnection,
) -> Vec<ReceivedFrame> {
    connection
        .frames()
        .into_iter()
        .filter(|frame| frame.tag != "user.message")
        .collect()
}

#[tokio::test]
async fn control_delivers_on_the_same_connection_publish_uses() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.publish("4471", "hello");
    flush().await;
    channel.control(ChannelControl {
        session_key: "4471".to_owned(),
        target: None,
        frame: stop_frame("4471"),
    });

    assert_eq!(hub.connections().len(), 1);
    let frames = control_frames(&hub.only());
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].tag, "turn.stop");

    manager.stop().await;
}

#[tokio::test]
async fn control_rewrites_the_session_key_into_the_frame() {
    // The hub reads the key off the frame rather than off the connection, so a
    // channel that wrote a bare id would otherwise address a session that is
    // not its own.
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.control(ChannelControl {
        session_key: "4471".to_owned(),
        target: None,
        frame: stop_frame("4471"),
    });

    let frames = control_frames(&hub.only());
    assert_eq!(frames[0].session_key.as_deref(), Some("loopback:4471"));
    manager.stop().await;
}

#[tokio::test]
async fn control_leaves_an_already_namespaced_key_alone() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.control(ChannelControl {
        session_key: "loopback:4471".to_owned(),
        target: None,
        frame: stop_frame("loopback:4471"),
    });

    let frames = control_frames(&hub.only());
    assert_eq!(frames[0].session_key.as_deref(), Some("loopback:4471"));
    manager.stop().await;
}

#[tokio::test]
async fn control_passes_through_a_frame_that_carries_no_session_of_its_own() {
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    channel.control(ChannelControl {
        session_key: "4471".to_owned(),
        target: None,
        frame: approve_frame(),
    });

    let frames = control_frames(&hub.only());
    assert_eq!(frames[0].tag, "tool.approve");
    let ClientMessage::ToolApprove(body) = &frames[0].frame else {
        panic!("the frame stays a tool.approve");
    };
    assert_eq!(body.call_id, "call-1");
    assert!(body.approved);
    // The connection it landed on is still the namespaced one.
    assert_eq!(hub.only().session_key(), "loopback:4471");

    manager.stop().await;
}

#[tokio::test]
async fn control_opens_a_connection_when_the_conversation_has_none_yet() {
    // A re-run typed into a chat that has sat idle can arrive after eviction,
    // and dropping it there would be a silent no-op with nothing to debug.
    let hub = ScriptedHub::silent();
    let (manager, channel) = started(Arc::clone(&hub), Spec::default()).await;

    assert_eq!(manager.session_count(), 0);
    channel.control(ChannelControl {
        session_key: "never-seen".to_owned(),
        target: Some("chat-9".to_owned()),
        frame: stop_frame("never-seen"),
    });

    assert_eq!(manager.session_count(), 1);
    assert_eq!(hub.only().session_key(), "loopback:never-seen");
    manager.stop().await;
}

#[tokio::test]
async fn control_binds_to_the_calling_channel() {
    // So one channel cannot drive another's conversation.
    let hub = ScriptedHub::silent();
    let first = build(Spec {
        id: "first",
        ..Spec::default()
    });
    let second = build(Spec {
        id: "second",
        ..Spec::default()
    });
    let first_channel = Arc::clone(&first.channel);
    let second_channel = Arc::clone(&second.channel);

    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![first.factory, second.factory],
        ..options(Arc::clone(&hub))
    })
    .expect("the manager takes both factories");
    manager.start().await.expect("both channels start");

    first_channel.publish("shared", "hello");
    flush().await;
    // The same bare key, from the other channel.
    second_channel.control(ChannelControl {
        session_key: "shared".to_owned(),
        target: None,
        frame: stop_frame("shared"),
    });

    let keys: Vec<String> = hub
        .connections()
        .iter()
        .map(|connection| connection.session_key().to_owned())
        .collect();
    assert_eq!(keys, vec!["first:shared", "second:shared"]);

    manager.stop().await;
}

#[tokio::test]
async fn the_session_fixture_names_the_conversation_the_projection_uses() {
    // A guard against the shared fixture drifting away from the manager's own
    // namespacing rule.
    assert!(SESSION.starts_with("loopback:"));
}
