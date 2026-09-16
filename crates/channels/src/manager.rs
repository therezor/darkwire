//! The bridge: the message bus on one side, the session hub on the other.
//!
//! A channel publishes an inbound message; this turns it into the same
//! `user.message` frame a browser sends and hands it to the hub. The hub answers
//! with the same `ServerMessage` stream a browser gets; this projects it back
//! into outbound messages and pushes them to the channel that asked. Both
//! directions go through the bus, so the queueing, the rate limit and the
//! bounded capacity are the ones `darkwire-core` already implements rather than a
//! second set per channel.
//!
//! What that buys is the point of the whole crate: a Telegram turn is not a
//! different code path from a web turn. It is the same hub, the same queue
//! discipline, the same approval gate, the same session store — so `@kb:`
//! parsing, `session_busy` and a stop mid-tool behave identically, and the
//! behaviours that only ever get exercised in a browser cannot quietly stop
//! working everywhere else.
//!
//! Three things here are load-bearing:
//!
//!  - **Session keys are namespaced by channel.** A channel names its own
//!    session (`telegram:4471`), and a channel that named `web:1` would be
//!    writing into a browser's conversation — reading its history back on the
//!    next turn and posting its replies to a stranger. Any key not already
//!    prefixed with the channel's id gets prefixed.
//!  - **One hub connection per `(channel, session)`, bounded and evicted.** A
//!    public channel has an unbounded supply of senders, and a connection the
//!    hub can see is a session the hub will not evict — so the bound has to be
//!    here. Eviction is least-recently-used and skips sessions with a turn in
//!    flight, which is the same rule the hub applies to its own rings.
//!  - **Ordering is per channel, not global.** Each channel gets its own
//!    delivery queue and its own task draining it, so a Telegram edit waiting on
//!    a `retry_after` cannot hold up a reply on Discord, and a `reply` still
//!    cannot overtake the `progress` that preceded it on the same channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use darkwire_core::clock::{Clock, SystemClock};
use darkwire_core::message_bus::{
    IdSource, InboundMessage, InboundMessageInput, MessageBus, MessageBusOptions, OutboundKind,
    OutboundMessage, OutboundMessageInput, PublishResult,
};
use darkwire_core::messages::text_part;
use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{
    Attachment, ClientMessage, ContentPart, ServerMessage, UserMessageRequest, UserMessageTag,
};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::channel::{
    Channel, ChannelContext, ChannelControl, ChannelFactory, ChannelInbound, DEFAULT_ACCEPTED_KINDS,
};
use crate::projection::{OutboundDraft, TurnProjection, TurnProjectionOptions};

/// One connection to the hub, as this crate needs it.
///
/// Structural rather than an import of the server's own session hub: depending
/// on the transport crate would put an HTTP router, an auth store and argon2id
/// behind every channel test, and would point an arrow from the channels at the
/// HTTP server that nothing in the design wants.
pub trait ChannelHubConnection: Send + Sync {
    /// Moves on `session.switch`; a channel connection never sends one.
    fn session_key(&self) -> String;
    /// Hands the hub one client frame.
    fn receive(&self, frame: ClientMessage);
    /// Drops the connection. Called once, and safe to call twice.
    fn close(&self);
}

/// Where a channel's events are delivered.
pub type SendEvent = Arc<dyn Fn(ServerMessage) + Send + Sync>;

/// What the manager asks the hub for.
#[derive(Clone)]
pub struct ChannelHubConnectOptions {
    /// Where the hub delivers this connection's events.
    pub send: SendEvent,
    /// The conversation to land in.
    pub session_key: Option<String>,
    /// Recorded as the session's origin, so a bridged turn is not labelled
    /// `web`.
    pub channel: Option<String>,
    /// The workspace a session created by this connection lands in.
    pub workspace_id: Option<String>,
    /// The agent a session created by this channel is bound to.
    ///
    /// Optional, like everything else here: this port is structural so a
    /// channel needs no import of the hub, and a required field would break
    /// every implementation of it.
    pub agent_id: Option<String>,
}

impl std::fmt::Debug for ChannelHubConnectOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelHubConnectOptions")
            .field("session_key", &self.session_key)
            .field("channel", &self.channel)
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}

/// The hub, as this crate needs it.
pub trait ChannelHub: Send + Sync {
    /// Opens one connection.
    fn connect(&self, options: ChannelHubConnectOptions) -> Arc<dyn ChannelHubConnection>;
}

/// How many `(channel, session)` pairs hold a hub connection.
///
/// High enough that a household's worth of chats never reaches it, low enough
/// that a public bot cannot turn one connection per sender into unbounded state
/// in a process that has no other reason to grow.
pub const DEFAULT_MAX_CHANNEL_SESSIONS: usize = 256;

/// Inputs to [`ChannelManager::new`].
pub struct ChannelManagerOptions {
    /// The hub every bridged conversation runs on.
    pub hub: Arc<dyn ChannelHub>,
    /// `config.channels`, whole.
    ///
    /// The two known flags drive the projection; every other key is a channel's
    /// own settings block, looked up by the channel's id. `enabled: false` in
    /// that block keeps a registered channel from starting, which is how a UI
    /// toggle turns one off without uninstalling it.
    pub channels: Map<String, Value>,
    /// Registered before `start()`.
    pub factories: Vec<ChannelFactory>,
    /// Shared with a caller that has one; otherwise the manager makes its own.
    pub bus: Option<Arc<MessageBus>>,
    /// Applied to the bus this manager creates. Ignored when one is supplied.
    pub bus_options: Option<MessageBusOptions>,
    /// Wall-clock and monotonic time, handed to every channel.
    pub clock: Arc<dyn Clock>,
    /// Ids for messages that arrive without one.
    ///
    /// Injected rather than minted here: a UUIDv7 needs a random source, which
    /// lives in `darkwire-security` — a crate below this one, and one this crate
    /// deliberately does not depend on. The composition root has both.
    pub new_id: IdSource,
    /// The ceiling on bridged `(channel, session)` pairs.
    pub max_sessions: usize,
    /// The workspace bridged conversations are created in.
    ///
    /// `None` leaves it to the hub, which is the right answer for a chat app: a
    /// person messaging a bot has no way to pick a workspace, and the default
    /// is the one that can see every other. An operator who wants a channel
    /// confined to its own workspace sets this and gets exactly that.
    ///
    /// Only ever creates — a session that already exists keeps the workspace it
    /// was born in, so changing this does not move existing conversations.
    pub workspace_id: Option<String>,
}

impl ChannelManagerOptions {
    /// Options over `hub` with no channels and no config.
    pub fn new(hub: Arc<dyn ChannelHub>, new_id: IdSource) -> ChannelManagerOptions {
        ChannelManagerOptions {
            hub,
            channels: Map::new(),
            factories: Vec::new(),
            bus: None,
            bus_options: None,
            clock: Arc::new(SystemClock),
            new_id,
            max_sessions: DEFAULT_MAX_CHANNEL_SESSIONS,
            workspace_id: None,
        }
    }
}

/// One `(channel, session)` pair: its hub connection and its turn state.
struct Bridged {
    channel_id: String,
    session_key: String,
    connection: Arc<dyn ChannelHubConnection>,
    projection: TurnProjection,
    /// Where replies go: the channel's own address for this conversation.
    target: String,
    /// Read from `session.status`, so eviction can leave a running turn alone.
    busy: bool,
}

fn is_record(value: &Value) -> bool {
    value.is_object()
}

fn settings_for(channels: &Map<String, Value>, id: &str) -> Map<String, Value> {
    match channels.get(id) {
        Some(block) if is_record(block) => block.as_object().cloned().unwrap_or_default(),
        _ => Map::new(),
    }
}

fn flag_of(source: &Map<String, Value>, key: &str) -> Option<bool> {
    source.get(key).and_then(Value::as_bool)
}

/// The text and the attachments a `user.message` frame carries.
///
/// A file part maps across one-for-one, because a frame attachment *is* a
/// workspace file — see the contract in `channel.rs`. An image part carrying
/// bytes has nowhere to go: a frame names a path, and this converter has no
/// workspace to write those bytes into. It becomes a note rather than
/// vanishing, so a channel author sees the omission instead of wondering why
/// their photo never reached the model.
fn to_frame_content(content: &[ContentPart]) -> (String, Vec<Attachment>) {
    let mut texts = Vec::new();
    let mut attachments = Vec::new();
    for part in content {
        match part {
            ContentPart::Text(text) => texts.push(text.text.clone()),
            ContentPart::File(file) => attachments.push(Attachment {
                path: file.path.clone(),
                mime_type: file.mime_type.clone(),
                name: file.name.clone(),
                size_bytes: file.size_bytes,
            }),
            ContentPart::Image(image) => texts.push(format!(
                "[image omitted: {} — publish a file part instead]",
                image.mime_type
            )),
        }
    }
    (texts.join("\n"), attachments)
}

/// A draft's metadata as the bus wants it.
///
/// The turn id is merged in last rather than living in the projection's own
/// metadata, because the manager is what knows it belongs there — the draft
/// carries it as a field precisely so a projection cannot forget to.
fn metadata_of(draft: &OutboundDraft) -> Map<String, Value> {
    let mut metadata = draft.metadata.clone();
    if let Some(turn_id) = &draft.turn_id {
        metadata.insert("turnId".to_owned(), Value::String(turn_id.clone()));
    }
    metadata
}

/// The reply address a channel gave us, if it gave one.
fn target_of(message: &InboundMessage) -> String {
    match message.metadata.get("target").and_then(Value::as_str) {
        Some(target) if !target.is_empty() => target.to_owned(),
        _ => message.sender_id.clone(),
    }
}

fn namespaced(channel_id: &str, session_key: &str) -> String {
    let prefix = format!("{channel_id}:");
    let key = if session_key.is_empty() {
        "default"
    } else {
        session_key
    };
    if key.starts_with(&prefix) {
        key.to_owned()
    } else {
        format!("{prefix}{key}")
    }
}

/// Everything the bridge between the bus and the hub needs, and nothing that
/// would make it own a channel.
///
/// Separate from the manager because a channel's context holds one of these:
/// the manager holds the channels, so a context that held the manager would be
/// a reference cycle that never drops. A connection holds a [`std::sync::Weak`]
/// back to it for the same reason.
struct Bridge {
    hub: Arc<dyn ChannelHub>,
    bus: Arc<MessageBus>,
    projection_options: TurnProjectionOptions,
    max_sessions: usize,
    workspace_id: Option<String>,
    table: Mutex<IndexMap<String, Bridged>>,
}

impl Bridge {
    fn key(channel_id: &str, session_key: &str) -> String {
        format!("{channel_id} {session_key}")
    }

    fn len(&self) -> usize {
        self.table.lock().len()
    }

    /// The `(channel, session)` pair, made if this is the first ask for it.
    ///
    /// Made rather than looked up, for both callers. A `tool.approve` always
    /// finds a live pair — a session with an approval parked on it is busy, and
    /// eviction leaves busy ones alone — but a re-run typed into a chat that
    /// has sat idle can arrive after eviction, and dropping it there would be a
    /// silent no-op with nothing to debug.
    ///
    /// The session key arrives already namespaced: both callers have to know
    /// the final key for themselves anyway, so passing it in beats namespacing
    /// twice.
    fn ensure(
        self: &Arc<Self>,
        channel_id: &str,
        session_key: &str,
        target: &str,
    ) -> Arc<dyn ChannelHubConnection> {
        let key = Bridge::key(channel_id, session_key);
        {
            let mut table = self.table.lock();
            if let Some(index) = table.get_index_of(&key) {
                // Moving the key to the end makes the map's iteration order
                // least-recently-used first, so eviction needs no second index.
                let last = table.len() - 1;
                table.move_index(index, last);
                if let Some(bridged) = table.get(&key) {
                    return Arc::clone(&bridged.connection);
                }
            }
        }

        // Outside the lock on purpose: a hub is free to deliver an event
        // synchronously from `connect`, and that event reaches `deliver`,
        // which takes this same lock. The entry is not in the table yet, so
        // such an event is dropped exactly as one for an evicted pair would be.
        let weak = Arc::downgrade(self);
        let for_send = key.clone();
        let connection = self.hub.connect(ChannelHubConnectOptions {
            session_key: Some(session_key.to_owned()),
            channel: Some(channel_id.to_owned()),
            workspace_id: self.workspace_id.clone(),
            agent_id: None,
            send: Arc::new(move |event| {
                if let Some(bridge) = weak.upgrade() {
                    bridge.deliver(&for_send, &event);
                }
            }),
        });

        let mut table = self.table.lock();
        if let Some(existing) = table.get(&key) {
            // Another caller won the race while the hub was connecting. Two
            // connections for one conversation would each answer half its
            // events, so the loser is closed rather than kept.
            let winner = Arc::clone(&existing.connection);
            drop(table);
            connection.close();
            return winner;
        }
        table.insert(
            key.clone(),
            Bridged {
                channel_id: channel_id.to_owned(),
                session_key: session_key.to_owned(),
                connection: Arc::clone(&connection),
                projection: TurnProjection::new(self.projection_options),
                target: target.to_owned(),
                busy: false,
            },
        );
        evict(&mut table, &key, self.max_sessions);
        connection
    }

    /// Where this conversation's replies go, as of the message just published.
    fn retarget(&self, channel_id: &str, session_key: &str, target: String) {
        if let Some(bridged) = self
            .table
            .lock()
            .get_mut(&Bridge::key(channel_id, session_key))
        {
            bridged.target = target;
        }
    }

    fn to_hub(self: &Arc<Self>, message: &InboundMessage) {
        let session_key = namespaced(&message.channel_id, &message.session_key);
        let target = target_of(message);
        let connection = self.ensure(&message.channel_id, &session_key, &target);
        self.retarget(&message.channel_id, &session_key, target);

        let (content, attachments) = to_frame_content(&message.content);
        connection.receive(ClientMessage::UserMessage(UserMessageRequest {
            tag: UserMessageTag,
            session_key,
            content,
            attachments,
            agent_id: None,
            // The channel's own id for the message, so a transport that
            // redelivers — every one of them, on a dropped connection — is
            // acked rather than running the same turn twice.
            client_message_id: Some(message.id.clone()),
        }));
    }

    /// Delivers one control frame on the channel's own connection.
    ///
    /// Straight to the connection rather than through the bus, because none of
    /// these is content: a `turn.stop` queued behind the turn it is stopping is
    /// a stop that arrives after the thing it was for.
    fn control(self: &Arc<Self>, channel_id: &str, command: ChannelControl) {
        let session_key = namespaced(channel_id, &command.session_key);
        let target = command.target.unwrap_or(command.session_key);
        let connection = self.ensure(channel_id, &session_key, &target);
        connection.receive(command.frame.with_session_key(&session_key).into());
    }

    fn deliver(&self, key: &str, event: &ServerMessage) {
        let published = {
            let mut table = self.table.lock();
            let Some(bridged) = table.get_mut(key) else {
                return;
            };
            if let ServerMessage::SessionStatus(status) = event {
                bridged.busy = status.event.busy;
            }
            let drafts = bridged.projection.project(event);
            if drafts.is_empty() {
                return;
            }
            (
                bridged.channel_id.clone(),
                bridged.session_key.clone(),
                bridged.target.clone(),
                drafts,
            )
        };

        let (channel_id, session_key, target, drafts) = published;
        for draft in drafts {
            let result = self.bus.publish_outbound(OutboundMessageInput {
                channel_id: channel_id.clone(),
                session_key: session_key.clone(),
                target: target.clone(),
                kind: draft.kind,
                content: vec![text_part(draft.text.clone())],
                metadata: metadata_of(&draft),
                id: None,
            });
            if !matches!(result, PublishResult::Accepted { .. }) {
                tracing::warn!(
                    channel = %channel_id,
                    kind = ?draft.kind,
                    result = ?result,
                    "outbound message dropped"
                );
            }
        }
    }

    fn close_all(&self) {
        let table = std::mem::take(&mut *self.table.lock());
        for (_, bridged) in table {
            bridged.connection.close();
        }
    }
}

/// Drops the least-recently-used idle connections until the cap holds.
fn evict(table: &mut IndexMap<String, Bridged>, exclude: &str, max_sessions: usize) {
    while table.len() > max_sessions {
        let victim = table
            .iter()
            .find(|(key, bridged)| key.as_str() != exclude && !bridged.busy)
            .map(|(key, _)| key.clone());
        // Every remaining pair has a turn in flight. Dropping one would lose
        // the reply it is about to produce, which is worse than being over the
        // cap until it finishes.
        let Some(victim) = victim else {
            return;
        };
        if let Some(bridged) = table.shift_remove(&victim) {
            bridged.connection.close();
            tracing::debug!(
                channel = %bridged.channel_id,
                session_key = %bridged.session_key,
                "evicted idle channel session"
            );
        }
    }
}

/// Everything the manager owns, behind one `Arc` so the pumps can hold it.
struct ManagerInner {
    bridge: Arc<Bridge>,
    bus: Arc<MessageBus>,
    owns_bus: bool,
    clock: Arc<dyn Clock>,
    channels_config: Map<String, Value>,
    factories: Mutex<IndexMap<String, ChannelFactory>>,
    channels: Mutex<IndexMap<String, Arc<dyn Channel>>>,
    /// Delivery queues, one per channel — see the module header.
    tails: Mutex<HashMap<String, mpsc::UnboundedSender<OutboundMessage>>>,
    /// The two bus pumps.
    pumps: Mutex<Vec<JoinHandle<()>>>,
    /// One delivery task per channel, each draining that channel's queue.
    forwarders: Mutex<Vec<JoinHandle<()>>>,
    lifetime: CancellationToken,
    started: AtomicBool,
}

/// The bridge between the bus and the hub, and the lifecycle of every channel.
pub struct ChannelManager {
    inner: Arc<ManagerInner>,
}

impl std::fmt::Debug for ChannelManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelManager")
            .field("channels", &self.channel_ids())
            .field("sessions", &self.session_count())
            .finish_non_exhaustive()
    }
}

impl ChannelManager {
    /// A manager with its factories registered and nothing started.
    pub fn new(options: ChannelManagerOptions) -> Result<ChannelManager> {
        let owns_bus = options.bus.is_none();
        let bus = options.bus.unwrap_or_else(|| {
            Arc::new(MessageBus::new(options.bus_options.unwrap_or_else(|| {
                MessageBusOptions::new(Arc::clone(&options.clock), options.new_id)
            })))
        });
        let projection_options = TurnProjectionOptions {
            send_progress: flag_of(&options.channels, "sendProgress").unwrap_or(true),
            send_tool_hints: flag_of(&options.channels, "sendToolHints").unwrap_or(false),
        };

        let manager = ChannelManager {
            inner: Arc::new(ManagerInner {
                bridge: Arc::new(Bridge {
                    hub: options.hub,
                    bus: Arc::clone(&bus),
                    projection_options,
                    max_sessions: options.max_sessions,
                    workspace_id: options.workspace_id,
                    table: Mutex::new(IndexMap::new()),
                }),
                bus,
                owns_bus,
                clock: options.clock,
                channels_config: options.channels,
                factories: Mutex::new(IndexMap::new()),
                channels: Mutex::new(IndexMap::new()),
                tails: Mutex::new(HashMap::new()),
                pumps: Mutex::new(Vec::new()),
                forwarders: Mutex::new(Vec::new()),
                lifetime: CancellationToken::new(),
                started: AtomicBool::new(false),
            }),
        };
        for factory in options.factories {
            manager.register(factory)?;
        }
        Ok(manager)
    }

    /// The queue both directions travel through. Shared with a scheduler later.
    pub fn bus(&self) -> &Arc<MessageBus> {
        &self.inner.bus
    }

    /// Live channel ids, in registration order.
    pub fn channel_ids(&self) -> Vec<String> {
        self.inner.channels.lock().keys().cloned().collect()
    }

    /// Live channels, in registration order.
    pub fn channels(&self) -> Vec<Arc<dyn Channel>> {
        self.inner
            .channels
            .lock()
            .values()
            .map(Arc::clone)
            .collect()
    }

    /// `(channel, session)` pairs currently holding a hub connection.
    pub fn session_count(&self) -> usize {
        self.inner.bridge.len()
    }

    /// One live channel by id.
    pub fn channel(&self, id: &str) -> Option<Arc<dyn Channel>> {
        self.inner.channels.lock().get(id).map(Arc::clone)
    }

    /// Adds a factory. Before `start()`, and never twice under one id.
    ///
    /// A duplicate id is refused rather than shadowed: two channels publishing
    /// under one id produce sessions neither of them can address, and the
    /// failure shows up as replies going to the wrong chat.
    pub fn register(&self, factory: ChannelFactory) -> Result<()> {
        if self.inner.started.load(Ordering::Acquire) {
            return Err(WireError::new(
                ErrorKind::Conflict,
                format!(
                    "Channel \"{}\" was registered after the manager started",
                    factory.id()
                ),
            ));
        }
        let mut factories = self.inner.factories.lock();
        if factories.contains_key(factory.id()) {
            return Err(WireError::new(
                ErrorKind::Conflict,
                format!("Channel \"{}\" is already registered", factory.id()),
            ));
        }
        factories.insert(factory.id().to_owned(), factory);
        Ok(())
    }

    /// Builds and starts every enabled channel, then the pumps.
    ///
    /// A channel that fails to build or to start fails the whole call, and
    /// anything already started is stopped again — a half-started manager is a
    /// process where some channels answer and the rest are silent, with nothing
    /// saying which is which.
    pub async fn start(&self) -> Result<()> {
        if self.inner.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        if let Err(error) = self.build_channels().await {
            self.stop().await;
            return Err(error);
        }

        let inbound = Arc::clone(&self.inner);
        let outbound = Arc::clone(&self.inner);
        let mut pumps = self.inner.pumps.lock();
        pumps.push(tokio::spawn(async move { inbound.pump_inbound().await }));
        pumps.push(tokio::spawn(async move { outbound.pump_outbound().await }));
        drop(pumps);

        tracing::info!(channels = ?self.channel_ids(), "channels started");
        Ok(())
    }

    async fn build_channels(&self) -> Result<()> {
        let factories: Vec<ChannelFactory> =
            self.inner.factories.lock().values().cloned().collect();
        for factory in factories {
            let settings = settings_for(&self.inner.channels_config, factory.id());
            if flag_of(&settings, "enabled") == Some(false) {
                tracing::info!(channel = factory.id(), "channel disabled by config");
                continue;
            }
            let channel = factory.create(self.context(factory.id(), settings))?;
            if channel.id() != factory.id() {
                return Err(WireError::new(
                    ErrorKind::Config,
                    format!(
                        "Channel factory \"{}\" created a channel with id \"{}\"",
                        factory.id(),
                        channel.id()
                    ),
                ));
            }
            channel.start().await?;
            self.inner
                .channels
                .lock()
                .insert(factory.id().to_owned(), channel);
        }
        Ok(())
    }

    /// Stops everything, in the order that lets replies already produced land.
    ///
    /// The token fires first so a channel's own in-flight request unwinds; the
    /// bus closes next, which ends the pumps once they have drained what was
    /// buffered; and only then are the channels told to disconnect.
    ///
    /// Idempotent: a transport that closed on its own and a manager shutting
    /// down both reach here, in either order.
    pub async fn stop(&self) {
        self.inner.lifetime.cancel();
        if self.inner.owns_bus {
            self.inner.bus.close();
        }

        // The pumps first, so everything the bus was holding has been handed to
        // a channel's queue before any of those queues is closed.
        let pumps = std::mem::take(&mut *self.inner.pumps.lock());
        for pump in pumps {
            // A pump that panicked has already logged; there is nothing this
            // can do about it that failing to shut down would improve.
            let _ = pump.await;
        }
        // Dropping the senders is what ends each forwarding task — and it has
        // to happen *before* they are awaited, because a task holding a live
        // sender waits for a message that will never come. Anything already
        // queued behind it still goes out: a closed receiver drains before it
        // reports the end.
        self.inner.tails.lock().clear();
        let forwarders = std::mem::take(&mut *self.inner.forwarders.lock());
        for forwarder in forwarders {
            let _ = forwarder.await;
        }

        let channels = std::mem::take(&mut *self.inner.channels.lock());
        for (id, channel) in channels {
            if let Err(error) = channel.stop().await {
                // A transport that cannot say goodbye is not a reason to leave
                // the rest of them running.
                tracing::warn!(channel = %id, error = %error.message, "channel stop failed");
            }
        }

        self.inner.bridge.close_all();
    }

    fn context(&self, id: &str, settings: Map<String, Value>) -> ChannelContext {
        let bus = Arc::clone(&self.inner.bus);
        let publish_id = id.to_owned();
        let bridge = Arc::clone(&self.inner.bridge);
        let control_id = id.to_owned();
        ChannelContext {
            id: id.to_owned(),
            settings,
            clock: Arc::clone(&self.inner.clock),
            token: self.inner.lifetime.clone(),
            // Bound to this id, so a channel can neither publish as another
            // channel nor reach the outbound queue it does not own.
            publish: Arc::new(move |message: ChannelInbound| {
                bus.publish_inbound(InboundMessageInput {
                    channel_id: publish_id.clone(),
                    session_key: message.session_key,
                    sender_id: message.sender_id,
                    content: message.content,
                    metadata: message.metadata,
                    id: message.id,
                })
            }),
            control: Arc::new(move |command: ChannelControl| {
                bridge.control(&control_id, command);
            }),
        }
    }
}

impl ManagerInner {
    async fn pump_inbound(self: Arc<Self>) {
        let consumer = self.bus.inbound();
        loop {
            tokio::select! {
                // Biased so everything already queued is bridged before the
                // shutdown branch can win: a message the bus accepted is owed a
                // turn even if the stop arrived in the same instant.
                biased;
                item = consumer.next() => match item {
                    Some(message) => self.bridge.to_hub(&message),
                    None => break,
                },
                () = self.lifetime.cancelled() => break,
            }
        }
    }

    async fn pump_outbound(self: Arc<Self>) {
        let consumer = self.bus.outbound();
        loop {
            tokio::select! {
                biased;
                item = consumer.next() => match item {
                    Some(message) => self.dispatch(message),
                    None => break,
                },
                () = self.lifetime.cancelled() => break,
            }
        }
    }

    /// Queues one message behind whatever that channel is already sending.
    fn dispatch(&self, message: OutboundMessage) {
        let Some(channel) = self
            .channels
            .lock()
            .get(&message.channel_id)
            .map(Arc::clone)
        else {
            tracing::warn!(
                channel = %message.channel_id,
                "outbound message for an unknown channel"
            );
            return;
        };
        if !accepts(channel.as_ref(), message.kind) {
            return;
        }

        let mut tails = self.tails.lock();
        let sender = tails.entry(message.channel_id.clone()).or_insert_with(|| {
            let (sender, receiver) = mpsc::unbounded_channel::<OutboundMessage>();
            self.forwarders
                .lock()
                .push(tokio::spawn(forward(channel, receiver)));
            sender
        });
        // The receiving task lives as long as the sender, so the only way this
        // fails is a send racing `stop()`, where the message has nowhere to go
        // anyway.
        let _ = sender.send(message);
    }
}

fn accepts(channel: &dyn Channel, kind: OutboundKind) -> bool {
    let declared = channel.accepts();
    let declared = if declared.is_empty() {
        DEFAULT_ACCEPTED_KINDS
    } else {
        declared
    };
    declared.contains(&kind)
}

/// One channel's delivery chain: strictly in order, and never another
/// channel's problem.
async fn forward(
    channel: Arc<dyn Channel>,
    mut receiver: mpsc::UnboundedReceiver<OutboundMessage>,
) {
    while let Some(message) = receiver.recv().await {
        let kind = message.kind;
        if let Err(error) = channel.send(message).await {
            // One failed send, not a dead pump: whether repeating it is safe is
            // a question only the transport's own API can answer.
            tracing::warn!(
                channel = %channel.id(),
                kind = ?kind,
                error = %error.message,
                "channel send failed"
            );
        }
    }
}
