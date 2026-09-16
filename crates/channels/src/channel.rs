//! What a channel is.
//!
//! A channel owns one transport — a Telegram bot, a Discord gateway, an SMTP
//! mailbox, a loopback vector in a test — and nothing else. It turns whatever
//! arrives there into an inbound message, and renders outbound ones back onto
//! it. It never sees the agent loop, never sees a session store and never sees
//! the hub: everything it can reach is on the [`ChannelContext`] it is handed.
//!
//! Two decisions in the shape are worth stating, because both close a hole an
//! extension channel would otherwise be able to walk through:
//!
//!  - **A channel publishes through [`ChannelContext::publish`], not through
//!    the bus.** Handing a channel the `MessageBus` would hand it
//!    `publish_outbound` — the ability to speak as the agent — and `outbound()`,
//!    whose consumer is *competing*: a channel that drained it would silently
//!    take another channel's replies out of the queue. `publish` stamps the
//!    channel's own id, so a channel cannot forge one either.
//!
//!  - **A channel declares the kinds it renders.** `progress` carries the
//!    answer as it is being written, for a transport that can edit a message in
//!    place; a transport that can only post would repeat the whole answer
//!    twice, once in pieces and once whole. Rather than trusting every channel
//!    author to know that, [`Channel::accepts`] states it and the manager
//!    filters — so the default is the one that cannot look broken.
//!
//! ## What a context holds, and why that list is short
//!
//! Three injected capabilities — `publish`, `control`, `clock` — plus the id it
//! publishes under, its own settings block, and the cancellation token that
//! fires at shutdown. Logging is ambient (`tracing`), so it is not a member and
//! not a capability anything has to be granted.
//!
//! ## Attachments
//!
//! **An attachment is a file in the workspace.** A channel that receives one
//! writes the bytes there and publishes a `ContentPart::File` naming the path;
//! it does not publish inline bytes and it does not publish a URL. That is the
//! same thing a browser upload produces, so a photo sent to a bot and a photo
//! dropped on the web composer travel one code path from here down — and a
//! path, unlike a URL, is still resolvable when the conversation is replayed
//! next month.
//!
//! The gap this leaves is deliberate and **not built**: a [`ChannelContext`]
//! has no filesystem, so a channel *cannot* write to the workspace today. When
//! one needs to, the seam is a fourth capability here, supplied by the manager
//! against a jail exactly as `publish` is supplied against the bus — which
//! keeps a channel as far from the filesystem as it is from the bus.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use darkwire_core::Result;
use darkwire_core::clock::Clock;
use darkwire_core::message_bus::{OutboundKind, OutboundMessage, PublishResult};
use darkwire_protocol::{
    ClientMessage, ContentPart, EditMessage, RegenerateMessage, SteerMessage, StopTurnMessage,
    ToolApproveMessage,
};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// A boxed future, the return type of every async method on [`Channel`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An inbound message as a channel writes it: everything but the channel id,
/// which the manager stamps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChannelInbound {
    /// The conversation it belongs to, before the manager namespaces it.
    pub session_key: String,
    /// The rate-limiting identity. Per *user*, not per session.
    pub sender_id: String,
    /// The message.
    pub content: Vec<ContentPart>,
    /// Channel-specific context: message ids, topic ids, reply targets.
    pub metadata: Map<String, Value>,
    /// The channel's own idempotency key, when it has one.
    pub id: Option<String>,
}

/// The frames a channel may send that are not a message somebody typed.
///
/// Deliberately five, rather than "everything except `user.message`". The three
/// that are missing — `session.new`, `session.switch`, `session.resume` — move
/// the *connection*, and the manager derives a session from what the channel
/// publishes: a channel that sent one would move where its events arrive while
/// its next message still went to the old conversation, and the two halves
/// would disagree with nothing to say so. A channel changes conversation by
/// publishing a different session key, which is the same thing the loopback
/// channel's `conversation` option does.
///
/// `ping` is absent because it means nothing here: there is no socket to keep
/// alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelControlFrame {
    /// Decide a tool call.
    ToolApprove(ToolApproveMessage),
    /// Stop the running turn.
    StopTurn(StopTurnMessage),
    /// Steer the running turn.
    Steer(SteerMessage),
    /// Run a turn again.
    Regenerate(RegenerateMessage),
    /// Replace a message and re-run from it.
    Edit(EditMessage),
}

/// The five `ClientMessage` tags a channel may send, in declaration order.
pub const CHANNEL_CONTROL_TAGS: &[&str] = &[
    "tool.approve",
    "turn.stop",
    "turn.steer",
    "turn.regenerate",
    "user.edit",
];

impl ChannelControlFrame {
    /// The wire tag this frame carries.
    pub fn tag(&self) -> &'static str {
        match self {
            ChannelControlFrame::ToolApprove(_) => "tool.approve",
            ChannelControlFrame::StopTurn(_) => "turn.stop",
            ChannelControlFrame::Steer(_) => "turn.steer",
            ChannelControlFrame::Regenerate(_) => "turn.regenerate",
            ChannelControlFrame::Edit(_) => "user.edit",
        }
    }

    /// The frame addressed to `session_key`.
    ///
    /// The manager namespaces the key and rewrites it in here, which is the
    /// load-bearing half: the hub reads the key off the frame rather than off
    /// the connection, so a channel that wrote a bare id — or another channel's
    /// — would otherwise address a session that is not its own. `tool.approve`
    /// carries no key of its own and is passed through as it stands.
    #[must_use]
    pub fn with_session_key(self, session_key: &str) -> ChannelControlFrame {
        match self {
            ChannelControlFrame::ToolApprove(frame) => ChannelControlFrame::ToolApprove(frame),
            ChannelControlFrame::StopTurn(mut frame) => {
                session_key.clone_into(&mut frame.session_key);
                ChannelControlFrame::StopTurn(frame)
            }
            ChannelControlFrame::Steer(mut frame) => {
                session_key.clone_into(&mut frame.session_key);
                ChannelControlFrame::Steer(frame)
            }
            ChannelControlFrame::Regenerate(mut frame) => {
                session_key.clone_into(&mut frame.session_key);
                ChannelControlFrame::Regenerate(frame)
            }
            ChannelControlFrame::Edit(mut frame) => {
                session_key.clone_into(&mut frame.session_key);
                ChannelControlFrame::Edit(frame)
            }
        }
    }
}

impl From<ChannelControlFrame> for ClientMessage {
    fn from(frame: ChannelControlFrame) -> ClientMessage {
        match frame {
            ChannelControlFrame::ToolApprove(body) => ClientMessage::ToolApprove(body),
            ChannelControlFrame::StopTurn(body) => ClientMessage::StopTurn(body),
            ChannelControlFrame::Steer(body) => ClientMessage::Steer(body),
            ChannelControlFrame::Regenerate(body) => ClientMessage::Regenerate(body),
            ChannelControlFrame::Edit(body) => ClientMessage::Edit(body),
        }
    }
}

/// A control frame, addressed to one of this channel's own conversations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelControl {
    /// The conversation it is about, namespaced by the manager exactly as the
    /// one on `publish` is.
    ///
    /// Required even for `tool.approve`, which carries no session of its own:
    /// the manager delivers on a connection, and this is what names which.
    pub session_key: String,
    /// Where a reply this produces goes. Defaults to `session_key`.
    pub target: Option<String>,
    /// The frame.
    pub frame: ChannelControlFrame,
}

/// Hands a user message to the agent, already bound to one channel's id.
pub type PublishFn = Arc<dyn Fn(ChannelInbound) -> PublishResult + Send + Sync>;

/// Delivers a control frame on one channel's own hub connection.
pub type ControlFn = Arc<dyn Fn(ChannelControl) + Send + Sync>;

/// What the manager gives a channel, and the whole of what a channel may reach.
#[derive(Clone)]
pub struct ChannelContext {
    /// The id this channel publishes under. Matches its factory.
    pub id: String,
    /// This channel's block of `config.channels`, unparsed.
    ///
    /// The channels config is deliberately a loose object, so that installing
    /// an extension that carries a channel does not require a schema change in
    /// `darkwire-protocol`: the channel parses its own settings and reports a
    /// bad block by refusing to start.
    pub settings: Map<String, Value>,
    /// Wall-clock and monotonic time.
    pub clock: Arc<dyn Clock>,
    /// Fires when the manager stops, before [`Channel::stop`] is called.
    ///
    /// A long poll or a reconnect backoff should hang off this rather than off
    /// a flag the channel sets in `stop()` — by then it is already too late for
    /// a request that is in flight.
    pub token: CancellationToken,
    /// Hands a user message to the agent. Rate limiting is applied here.
    ///
    /// A field rather than a method so a channel can clone it out — it is
    /// already bound to this channel's id, and that binding is the whole point.
    pub publish: PublishFn,
    /// The frames a browser sends that are not messages, on this channel's own
    /// conversation. This is what lets a transport answer an approval, stop a
    /// turn, or re-run one.
    ///
    /// Supplied the same way `publish` is: the manager namespaces the session
    /// key and rewrites it into the frame, so a channel can no more drive
    /// another channel's conversation through here than it can forge a channel
    /// id through `publish`. That symmetry is the whole reason this is a member
    /// rather than a hub handed over at construction.
    ///
    /// Does not go through the bus. The bus queues *content* — a message and a
    /// reply — and applies a per-sender rate limit to it; a stop is not
    /// content, and a stop that queued behind the turn it is trying to stop
    /// would never arrive in time to do anything.
    pub control: ControlFn,
}

impl std::fmt::Debug for ChannelContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelContext")
            .field("id", &self.id)
            .field("settings", &self.settings)
            .field("cancelled", &self.token.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl ChannelContext {
    /// Publishes one inbound message under this channel's id.
    pub fn publish(&self, message: ChannelInbound) -> PublishResult {
        (self.publish)(message)
    }

    /// Delivers one control frame on this channel's own hub connection.
    pub fn control(&self, command: ChannelControl) {
        (self.control)(command);
    }
}

/// Kinds delivered to a channel that does not say which it renders.
pub const DEFAULT_ACCEPTED_KINDS: &[OutboundKind] = &[
    OutboundKind::Reply,
    OutboundKind::Notice,
    OutboundKind::Error,
];

/// One transport.
pub trait Channel: Send + Sync {
    /// Matches the factory's id and the channel id of everything it publishes.
    fn id(&self) -> &str;

    /// The outbound kinds this transport renders. Defaults to everything but
    /// `progress` — see the module header for why that one is opt-in.
    fn accepts(&self) -> &[OutboundKind] {
        DEFAULT_ACCEPTED_KINDS
    }

    /// Connects.
    ///
    /// Failing here fails the manager's `start()`, which is what makes a bad
    /// token a startup error rather than a channel that is silently dead.
    fn start(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(std::future::ready(Ok(())))
    }

    /// Renders one message on the transport.
    ///
    /// A failure is logged and dropped: one Telegram edit that 429s must not
    /// stop the pump that feeds every other channel. Retrying is the channel's
    /// own decision, because only it knows what its API says about repeating a
    /// send.
    fn send(&self, message: OutboundMessage) -> BoxFuture<'_, Result<()>>;

    /// Disconnects. Called once, and after the context's token has fired.
    fn stop(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// Builds one channel over the context the manager hands it.
///
/// Synchronous, because connecting is [`Channel::start`]'s job: a factory that
/// opened a socket would do it before the manager had anything to stop.
pub type ChannelBuilder = Arc<dyn Fn(ChannelContext) -> Result<Arc<dyn Channel>> + Send + Sync>;

/// How a channel is built.
///
/// The indirection is what lets the manager own the lifecycle — settings,
/// clock, cancellation token, the publish function bound to this id — and it is
/// the same contract an extension's channel registration hands over. The
/// built-in channels consume it too, so it cannot rot.
///
/// The id is a field rather than something read off the built channel because
/// the manager needs it *first*: it is the key of the settings block, and
/// `enabled: false` in that block means the channel is never built at all.
#[derive(Clone)]
pub struct ChannelFactory {
    id: String,
    build: ChannelBuilder,
}

impl std::fmt::Debug for ChannelFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelFactory")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl ChannelFactory {
    /// A factory registering under `id`.
    pub fn new(id: impl Into<String>, build: ChannelBuilder) -> ChannelFactory {
        ChannelFactory {
            id: id.into(),
            build,
        }
    }

    /// The id this factory registers under.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Builds the channel.
    pub fn create(&self, context: ChannelContext) -> Result<Arc<dyn Channel>> {
        (self.build)(context)
    }
}
