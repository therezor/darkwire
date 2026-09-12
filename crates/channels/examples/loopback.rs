//! The loopback channel: the contract, with the transport removed.
//!
//! Every other channel spends most of its code on someone else's API — long
//! polling, gateway reconnects, message-edit rate limits — and none of that is
//! the contract. This one has no network at all: `say()` is a user typing and
//! the transcript is the chat window. What is left is exactly the four things a
//! channel does, which is what makes it worth reading before writing a real one:
//!
//!  1. turn transport input into `context.publish(ChannelInbound { … })`;
//!  2. name the conversation, and use the same name every time so the session is
//!     the same session;
//!  3. say where a reply goes, via `metadata.target`;
//!  4. render an outbound message in `send()`.
//!
//! It is also what the conformance suite runs against, so the suite is checked
//! against a passing implementation rather than only against the channels that
//! come later. `tests/loopback.rs` is where that happens; this file is the
//! implementation and a two-line demonstration of it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ghostai_channels::channel::{
    BoxFuture, Channel, ChannelContext, ChannelFactory, ChannelInbound,
};
use ghostai_core::Result;
use ghostai_core::message_bus::{OutboundKind, OutboundMessage, PublishResult};
use ghostai_core::messages::text_part;
use ghostai_protocol::ContentPart;
use parking_lot::Mutex;
use serde_json::{Map, Value};

/// One line of the conversation, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopbackEntry {
    /// Which way it went.
    pub incoming: bool,
    /// What was said.
    pub text: String,
    /// `reply`, `notice`, `error` — and `progress`, since this channel renders
    /// it. Absent on the inbound half.
    pub kind: Option<OutboundKind>,
    /// The conversation it belongs to.
    pub session_key: String,
}

/// How one loopback channel is configured.
#[derive(Debug, Clone)]
pub struct LoopbackOptions {
    /// The id it publishes under. A second instance needs a second id.
    pub id: String,
    /// The conversation `say()` speaks into.
    pub conversation: String,
    /// Who is typing. The rate-limiting identity, so it is per user.
    pub sender_id: String,
}

impl Default for LoopbackOptions {
    fn default() -> LoopbackOptions {
        LoopbackOptions {
            id: "loopback".to_owned(),
            conversation: "default".to_owned(),
            sender_id: "local".to_owned(),
        }
    }
}

/// A channel with no network.
///
/// Built before the manager rather than by it, so a caller keeps a handle to
/// the same object the manager drives — there is no downcasting from
/// `Arc<dyn Channel>`, and a transcript nobody can read is not a transcript.
pub struct Loopback {
    id: String,
    conversation: String,
    sender_id: String,
    transcript: Mutex<Vec<LoopbackEntry>>,
    context: Mutex<Option<ChannelContext>>,
    open: AtomicBool,
}

/// Declared, unlike most channels: an in-memory transcript can hold the answer
/// as it is being written without anyone having to re-read it, which is the case
/// `progress` exists for. A channel that can only post should leave this out and
/// get replies alone.
const LOOPBACK_ACCEPTS: &[OutboundKind] = &[
    OutboundKind::Reply,
    OutboundKind::Notice,
    OutboundKind::Error,
    OutboundKind::Progress,
];

impl Loopback {
    /// A user typing. Returns what the bus made of it, rate limit included.
    pub fn say(&self, text: &str) -> PublishResult {
        self.say_into(text, None)
    }

    /// A user typing into a named conversation.
    pub fn say_into(&self, text: &str, conversation: Option<&str>) -> PublishResult {
        let context = self.context.lock().clone();
        // A transport that kept accepting input after `stop()` would produce
        // turns whose replies have nowhere to land. The manager's token says
        // the same thing a socket's close would.
        let Some(context) = context else {
            return PublishResult::Closed;
        };
        if !self.open.load(Ordering::Acquire) || context.token.is_cancelled() {
            return PublishResult::Closed;
        }

        let conversation = conversation.unwrap_or(&self.conversation).to_owned();
        self.transcript.lock().push(LoopbackEntry {
            incoming: true,
            text: text.to_owned(),
            kind: None,
            session_key: conversation.clone(),
        });

        let mut metadata = Map::new();
        // Where the answer goes. A real channel puts its chat id here.
        metadata.insert("target".to_owned(), Value::String(conversation.clone()));

        context.publish(ChannelInbound {
            // The conversation, not the message: a channel that minted a fresh
            // key per message would start a new session for every line the user
            // typed.
            session_key: conversation,
            sender_id: self.sender_id.clone(),
            content: vec![text_part(text)],
            metadata,
            id: None,
        })
    }

    /// Everything said, both directions.
    pub fn transcript(&self) -> Vec<LoopbackEntry> {
        self.transcript.lock().clone()
    }

    /// Just what the agent said, oldest first — what a test usually asserts on.
    pub fn replies(&self) -> Vec<String> {
        self.transcript
            .lock()
            .iter()
            .filter(|entry| !entry.incoming && entry.kind == Some(OutboundKind::Reply))
            .map(|entry| entry.text.clone())
            .collect()
    }

    /// Everything the channel put on its transport, oldest first.
    pub fn sent(&self) -> Vec<String> {
        self.transcript
            .lock()
            .iter()
            .filter(|entry| !entry.incoming)
            .map(|entry| entry.text.clone())
            .collect()
    }
}

impl Channel for Loopback {
    fn id(&self) -> &str {
        &self.id
    }

    fn accepts(&self) -> &[OutboundKind] {
        LOOPBACK_ACCEPTS
    }

    fn start(&self) -> BoxFuture<'_, Result<()>> {
        self.open.store(true, Ordering::Release);
        Box::pin(std::future::ready(Ok(())))
    }

    fn stop(&self) -> BoxFuture<'_, Result<()>> {
        self.open.store(false, Ordering::Release);
        Box::pin(std::future::ready(Ok(())))
    }

    fn send(&self, message: OutboundMessage) -> BoxFuture<'_, Result<()>> {
        let text: String = message
            .content
            .iter()
            .map(|part| match part {
                ContentPart::Text(text) => text.text.clone(),
                ContentPart::Image(image) => format!("[{}]", image.mime_type),
                ContentPart::File(file) => format!("[{}]", file.mime_type),
            })
            .collect();
        self.transcript.lock().push(LoopbackEntry {
            incoming: false,
            text,
            kind: Some(message.kind),
            session_key: message.session_key,
        });
        Box::pin(std::future::ready(Ok(())))
    }
}

/// The factory the manager registers, and a handle to what it will build.
///
/// A factory rather than an instance because the manager owns the lifecycle: it
/// decides when the channel is built, hands it the settings block and the
/// cancellation token, and is the only thing that can bind `publish` to this
/// channel's id.
pub fn loopback_channel(options: LoopbackOptions) -> (ChannelFactory, Arc<Loopback>) {
    let channel = Arc::new(Loopback {
        id: options.id.clone(),
        conversation: options.conversation,
        sender_id: options.sender_id,
        transcript: Mutex::new(Vec::new()),
        context: Mutex::new(None),
        open: AtomicBool::new(false),
    });
    let built = Arc::clone(&channel);
    let factory = ChannelFactory::new(
        options.id,
        Arc::new(move |context| {
            *built.context.lock() = Some(context);
            Ok(Arc::clone(&built) as Arc<dyn Channel>)
        }),
    );
    (factory, channel)
}

/// Puts one message through a manager and prints what came back.
#[cfg(not(test))]
#[tokio::main]
async fn main() -> Result<()> {
    use ghostai_channels::manager::{ChannelManager, ChannelManagerOptions};
    use ghostai_channels::testkit::{ScriptedHub, counter_ids, flush};

    let hub = ScriptedHub::new();
    let (factory, channel) = loopback_channel(LoopbackOptions::default());
    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![factory],
        ..ChannelManagerOptions::new(hub, counter_ids("demo-"))
    })?;

    manager.start().await?;
    channel.say("hello there");
    flush().await;
    manager.stop().await;

    for entry in channel.transcript() {
        let arrow = if entry.incoming { "→" } else { "←" };
        println!("{arrow} {}", entry.text);
    }
    Ok(())
}
