//! The conformance suite and the hub it runs on, behind the `testkit` feature.
//!
//! A channel is easy to write and easy to write *almost* right: one that
//! publishes a session key it made up, or renders `progress` it never declared,
//! or keeps answering after `stop()`, looks fine in a manual test and breaks a
//! property something else depends on. Those properties are stated here once and
//! checked against every implementation — the loopback reference channel, the
//! Telegram channel that ships in the box, and whatever an extension registers
//! after that.
//!
//! The suite drives the channel through a real [`ChannelManager`] against a
//! scripted hub, because the contract is about what the manager sees and what
//! the transport shows, not about a channel's internals.
//!
//! It is a cargo feature rather than part of the default build for the reason
//! the crate header gives: a channel is the one implementation that will
//! routinely live outside this repository, and a contract an external channel
//! cannot run against is a contract that only holds for the channels that were
//! already here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    reason = "a conformance suite is one long list of assertions, and a fixture that cannot be built is a failing test either way"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use ghostai_core::message_bus::IdSource;
use ghostai_protocol::{
    AssistantDelta, AssistantDeltaTag, Attachment, ClientMessage, Sequenced, ServerMessage,
    StopReason, TurnEnd, TurnEndTag, TurnStart, TurnStartTag,
};
use parking_lot::Mutex;
use serde_json::{Map, Value};

use crate::channel::{BoxFuture, Channel, ChannelFactory};
use crate::manager::{
    ChannelHub, ChannelHubConnectOptions, ChannelHubConnection, ChannelManager,
    ChannelManagerOptions, SendEvent,
};

/// One client frame, as the manager wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivedFrame {
    /// The wire tag.
    pub tag: &'static str,
    /// The conversation it addresses, for the frames that carry one.
    pub session_key: Option<String>,
    /// The text, for a `user.message`.
    pub content: Option<String>,
    /// The workspace files it names.
    pub attachments: Vec<Attachment>,
    /// The channel's own idempotency key.
    pub client_message_id: Option<String>,
    /// The frame itself, for an assertion the fields above do not cover.
    pub frame: ClientMessage,
}

impl ReceivedFrame {
    fn of(frame: ClientMessage) -> ReceivedFrame {
        let tag = frame.tag();
        let (session_key, content, attachments, client_message_id) = match &frame {
            ClientMessage::UserMessage(body) => (
                Some(body.session_key.clone()),
                Some(body.content.clone()),
                body.attachments.clone(),
                body.client_message_id.clone(),
            ),
            ClientMessage::StopTurn(body) => {
                (Some(body.session_key.clone()), None, Vec::new(), None)
            }
            ClientMessage::Steer(body) => (
                Some(body.session_key.clone()),
                Some(body.content.clone()),
                Vec::new(),
                None,
            ),
            ClientMessage::Regenerate(body) => (
                Some(body.session_key.clone()),
                None,
                Vec::new(),
                body.client_message_id.clone(),
            ),
            ClientMessage::Edit(body) => (
                Some(body.session_key.clone()),
                Some(body.content.clone()),
                body.attachments.clone(),
                body.client_message_id.clone(),
            ),
            _ => (None, None, Vec::new(), None),
        };
        ReceivedFrame {
            tag,
            session_key,
            content,
            attachments,
            client_message_id,
            frame,
        }
    }
}

/// What one scripted turn says, given the frame that started it.
pub type ScriptedReply = Arc<dyn Fn(&ReceivedFrame) -> String + Send + Sync>;

/// What the scripted turn answers with.
#[derive(Clone, Default)]
pub struct ScriptedHubOptions {
    /// What the turn answers with. `None` echoes the message back.
    pub reply: Option<ScriptedReply>,
    /// Suppresses the scripted turn, leaving the test to emit events by hand.
    pub silent: bool,
}

impl std::fmt::Debug for ScriptedHubOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedHubOptions")
            .field("silent", &self.silent)
            .finish_non_exhaustive()
    }
}

/// One connection to the scripted hub.
pub struct ScriptedConnection {
    session_key: String,
    frames: Mutex<Vec<ReceivedFrame>>,
    closed: AtomicBool,
    turns: AtomicU64,
    send: SendEvent,
    options: ScriptedHubOptions,
}

impl std::fmt::Debug for ScriptedConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedConnection")
            .field("session_key", &self.session_key)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl ScriptedConnection {
    /// Everything this connection was handed, oldest first.
    pub fn frames(&self) -> Vec<ReceivedFrame> {
        self.frames.lock().clone()
    }

    /// Whether the manager has closed it.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// The conversation the manager opened it on.
    pub fn session_key(&self) -> &str {
        &self.session_key
    }

    /// One turn, in the order and shape the agent loop yields one.
    pub fn turn(&self, text: &str) {
        let turn_id = format!("turn-{}", self.turns.fetch_add(1, Ordering::AcqRel));
        self.emit(ServerMessage::TurnStart(Sequenced {
            seq: 0,
            event: TurnStart {
                tag: TurnStartTag,
                session_key: self.session_key.clone(),
                turn_id: turn_id.clone(),
                first_seq: None,
                agent_id: "default".to_owned(),
                model: "scripted".to_owned(),
                provider: "scripted".to_owned(),
            },
        }));
        if !text.is_empty() {
            self.emit(ServerMessage::AssistantDelta(Sequenced {
                seq: 0,
                event: AssistantDelta {
                    tag: AssistantDeltaTag,
                    turn_id: turn_id.clone(),
                    text: text.to_owned(),
                },
            }));
        }
        self.emit(ServerMessage::TurnEnd(Sequenced {
            seq: 0,
            event: TurnEnd {
                tag: TurnEndTag,
                turn_id,
                stop_reason: StopReason::Complete,
                usage: None,
                iterations: 1,
                elapsed_ms: None,
                generation_ms: None,
                generation_tokens: None,
                first_token_ms: None,
                first_seq: None,
                last_seq: None,
            },
        }));
    }

    /// Anything else the hub could send this connection.
    pub fn emit(&self, message: ServerMessage) {
        if !self.is_closed() {
            (self.send)(message);
        }
    }
}

impl ChannelHubConnection for ScriptedConnection {
    fn session_key(&self) -> String {
        self.session_key.clone()
    }

    fn receive(&self, frame: ClientMessage) {
        let received = ReceivedFrame::of(frame);
        let is_message = received.tag == "user.message";
        self.frames.lock().push(received.clone());
        if !is_message || self.options.silent {
            return;
        }
        let text = self.options.reply.as_ref().map_or_else(
            || format!("echo: {}", received.content.clone().unwrap_or_default()),
            |reply| reply(&received),
        );
        self.turn(&text);
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

/// A hub that answers.
///
/// A real session hub needs a config, a store, an approval gate and a loop; a
/// channel test needs none of those and would be testing them by accident. This
/// is the same three-method port the manager states, backed by a scripted turn
/// — so a channel under test gets a real turn's event *shape* without a
/// provider anywhere near it.
#[derive(Debug, Default)]
pub struct ScriptedHub {
    connections: Mutex<Vec<Arc<ScriptedConnection>>>,
    origins: Mutex<Vec<Option<String>>>,
    options: ScriptedHubOptions,
}

impl ScriptedHub {
    /// A hub whose every turn echoes the message back.
    pub fn new() -> Arc<ScriptedHub> {
        Arc::new(ScriptedHub::default())
    }

    /// A hub with the scripted turn suppressed.
    pub fn silent() -> Arc<ScriptedHub> {
        ScriptedHub::with(ScriptedHubOptions {
            reply: None,
            silent: true,
        })
    }

    /// A hub whose every turn answers with `text`.
    pub fn replying(text: &'static str) -> Arc<ScriptedHub> {
        ScriptedHub::with(ScriptedHubOptions {
            reply: Some(Arc::new(move |_| text.to_owned())),
            silent: false,
        })
    }

    /// A hub with these options.
    pub fn with(options: ScriptedHubOptions) -> Arc<ScriptedHub> {
        Arc::new(ScriptedHub {
            connections: Mutex::new(Vec::new()),
            origins: Mutex::new(Vec::new()),
            options,
        })
    }

    /// Every connection opened, in order.
    pub fn connections(&self) -> Vec<Arc<ScriptedConnection>> {
        self.connections.lock().clone()
    }

    /// The `channel` each connection was opened with, in order.
    pub fn origins(&self) -> Vec<Option<String>> {
        self.origins.lock().clone()
    }

    /// The single connection, when a test expects exactly one.
    pub fn only(&self) -> Arc<ScriptedConnection> {
        let connections = self.connections();
        assert_eq!(
            connections.len(),
            1,
            "expected one hub connection, found {}",
            connections.len()
        );
        Arc::clone(&connections[0])
    }

    /// Every `user.message` this hub was handed, across every connection.
    pub fn messages(&self) -> Vec<ReceivedFrame> {
        self.connections()
            .iter()
            .flat_map(|connection| connection.frames())
            .filter(|frame| frame.tag == "user.message")
            .collect()
    }
}

impl ChannelHub for ScriptedHub {
    fn connect(&self, options: ChannelHubConnectOptions) -> Arc<dyn ChannelHubConnection> {
        let connection = Arc::new(ScriptedConnection {
            session_key: options.session_key.unwrap_or_else(|| "scripted".to_owned()),
            frames: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            turns: AtomicU64::new(0),
            send: options.send,
            options: self.options.clone(),
        });
        self.connections.lock().push(Arc::clone(&connection));
        self.origins.lock().push(options.channel);
        connection
    }
}

/// Deterministic ids, so nothing under test depends on a random source.
pub fn counter_ids(prefix: &'static str) -> IdSource {
    let next = Arc::new(AtomicU64::new(0));
    Arc::new(move || format!("{prefix}{}", next.fetch_add(1, Ordering::AcqRel) + 1))
}

/// Long enough for both of the manager's pumps and a channel's own send to run.
///
/// A duration rather than a count of yields: how many times the runtime has to
/// be pumped before a message has travelled bus → hub → bus → channel is not a
/// fixed number, and a count that passes on an idle machine fails on a loaded
/// one.
pub async fn flush() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}

/// How the suite drives one implementation's transport.
pub trait ChannelProbe: Send + Sync {
    /// A user speaking on the channel's own transport.
    ///
    /// Whatever the channel's real input is — a webhook body, a gateway event,
    /// a vector push — this is the test's way of producing one. It must lead to
    /// a `context.publish`, which is the whole of what is being checked.
    fn receive<'a>(&'a self, text: &'a str) -> BoxFuture<'a, ()>;

    /// Everything the channel has put on its transport, oldest first.
    fn sent(&self) -> Vec<String>;
}

/// One implementation, built fresh.
pub struct ChannelUnderTest {
    /// The factory the manager registers.
    pub factory: ChannelFactory,
    /// How the suite speaks to, and reads from, that channel's transport.
    pub probe: Arc<dyn ChannelProbe>,
}

/// Builds a fresh channel and its probe.
///
/// Called once per scenario, because a transcript that carried over from the
/// previous one would make "renders the answer back" pass on last scenario's
/// answer.
pub type MakeChannel = Arc<dyn Fn() -> ChannelUnderTest + Send + Sync>;

/// What [`channel_conformance`] needs.
#[derive(Clone)]
pub struct ChannelConformanceOptions {
    /// Builds the implementation under test.
    pub make: MakeChannel,
    /// The settings block the factory is handed.
    pub settings: Map<String, Value>,
}

impl ChannelConformanceOptions {
    /// Options with an empty settings block.
    pub fn new(make: MakeChannel) -> ChannelConformanceOptions {
        ChannelConformanceOptions {
            make,
            settings: Map::new(),
        }
    }
}

struct Running {
    manager: ChannelManager,
    probe: Arc<dyn ChannelProbe>,
    hub: Arc<ScriptedHub>,
}

async fn start(hub: Arc<ScriptedHub>, options: &ChannelConformanceOptions) -> Running {
    let under_test = (options.make)();
    let id = under_test.factory.id().to_owned();
    let mut channels = Map::new();
    channels.insert(id.clone(), Value::Object(options.settings.clone()));

    let manager = ChannelManager::new(ChannelManagerOptions {
        channels,
        factories: vec![under_test.factory],
        ..ChannelManagerOptions::new(
            Arc::clone(&hub) as Arc<dyn ChannelHub>,
            counter_ids("conformance-"),
        )
    })
    .expect("the manager takes the factory");
    manager.start().await.expect("the channel starts");
    assert!(
        manager.channel(&id).is_some(),
        "\"{id}\" did not start under its own id"
    );
    Running {
        manager,
        probe: under_test.probe,
        hub,
    }
}

fn only_channel(running: &Running) -> Arc<dyn Channel> {
    let channels = running.manager.channels();
    assert_eq!(channels.len(), 1, "expected exactly one live channel");
    Arc::clone(&channels[0])
}

/// Runs the contract against one implementation.
///
/// Every assertion is a property something else depends on, and each says which
/// in its message. Call it from a `#[tokio::test]`.
pub async fn channel_conformance(options: &ChannelConformanceOptions) {
    creates_a_channel_under_the_factorys_own_id(options).await;
    publishes_what_it_receives_under_its_own_id(options).await;
    renders_the_answer_back_onto_its_transport(options).await;
    keeps_one_conversation_in_one_session(options).await;
    renders_an_error_rather_than_swallowing_it(options).await;
    says_nothing_more_once_it_has_been_stopped(options).await;
    stops_cleanly_when_it_never_received_anything(options).await;
}

async fn creates_a_channel_under_the_factorys_own_id(options: &ChannelConformanceOptions) {
    let running = start(ScriptedHub::new(), options).await;
    let channel = only_channel(&running);
    let expected = running.manager.channel_ids();
    assert_eq!(
        vec![channel.id().to_owned()],
        expected,
        "a channel must publish under the id its factory registered"
    );
    running.manager.stop().await;
}

async fn publishes_what_it_receives_under_its_own_id(options: &ChannelConformanceOptions) {
    let running = start(ScriptedHub::silent(), options).await;
    let id = only_channel(&running).id().to_owned();

    running.probe.receive("hello there").await;
    flush().await;

    assert_eq!(
        running.hub.origins(),
        vec![Some(id.clone())],
        "the hub must record the channel as the session's origin"
    );
    let messages = running.hub.messages();
    let message = messages.first().expect("one user.message reached the hub");
    assert_eq!(message.tag, "user.message");
    assert_eq!(message.content.as_deref(), Some("hello there"));
    // Namespaced by the manager, whatever the channel chose.
    let session_key = running.hub.only().session_key().to_owned();
    assert!(
        session_key.starts_with(&format!("{id}:")),
        "the manager must namespace the session key by channel, got {session_key:?}"
    );
    // An idempotency key, so a transport that redelivers is acked rather than
    // running the turn twice.
    assert!(
        message
            .client_message_id
            .as_ref()
            .is_some_and(|id| !id.is_empty()),
        "a published message must carry an idempotency key"
    );
    running.manager.stop().await;
}

async fn renders_the_answer_back_onto_its_transport(options: &ChannelConformanceOptions) {
    let running = start(ScriptedHub::replying("the answer"), options).await;

    running.probe.receive("a question").await;
    flush().await;
    flush().await;

    assert!(
        running.probe.sent().iter().any(|sent| sent == "the answer"),
        "the answer must reach the transport, got {:?}",
        running.probe.sent()
    );
    running.manager.stop().await;
}

async fn keeps_one_conversation_in_one_session(options: &ChannelConformanceOptions) {
    let running = start(ScriptedHub::new(), options).await;

    running.probe.receive("first").await;
    flush().await;
    running.probe.receive("second").await;
    flush().await;

    assert_eq!(
        running.hub.connections().len(),
        1,
        "two messages from one conversation must share one hub connection"
    );
    let said: Vec<Option<String>> = running
        .hub
        .messages()
        .into_iter()
        .map(|frame| frame.content)
        .collect();
    assert_eq!(
        said,
        vec![Some("first".to_owned()), Some("second".to_owned())]
    );
    running.manager.stop().await;
}

async fn renders_an_error_rather_than_swallowing_it(options: &ChannelConformanceOptions) {
    let running = start(ScriptedHub::silent(), options).await;

    running.probe.receive("a question").await;
    flush().await;
    running
        .hub
        .only()
        .emit(ServerMessage::Error(ghostai_protocol::ErrorEvent {
            tag: ghostai_protocol::ErrorTag,
            code: ghostai_protocol::ErrorCode::ProviderError,
            message: "the model is unreachable".to_owned(),
            retryable: true,
            turn_id: None,
        }));
    flush().await;
    flush().await;

    assert!(
        running
            .probe
            .sent()
            .join("\n")
            .contains("the model is unreachable"),
        "an error must be rendered rather than dropped, got {:?}",
        running.probe.sent()
    );
    running.manager.stop().await;
}

async fn says_nothing_more_once_it_has_been_stopped(options: &ChannelConformanceOptions) {
    let running = start(ScriptedHub::silent(), options).await;

    running.probe.receive("a question").await;
    flush().await;
    let connection = running.hub.only();
    running.manager.stop().await;

    let before = running.probe.sent();
    connection.turn("too late");
    flush().await;
    flush().await;

    assert_eq!(
        running.probe.sent(),
        before,
        "a stopped channel must render nothing further"
    );
    assert!(
        connection.is_closed(),
        "stopping the manager must close its hub connections"
    );
}

async fn stops_cleanly_when_it_never_received_anything(options: &ChannelConformanceOptions) {
    let running = start(ScriptedHub::new(), options).await;

    running.manager.stop().await;
    // Idempotent: a transport that closed on its own and a manager shutting
    // down both call this, and in either order.
    running.manager.stop().await;
}
