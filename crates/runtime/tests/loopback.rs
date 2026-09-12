//! A message through a real loop, a real store and the channel contract.
//!
//! This is the half of `crates/channels`' loopback suite that could not live
//! there: that crate must not depend on the agent or the server, so it can prove
//! the *contract* is satisfiable but not that the turn behind it is the turn a
//! browser gets. The composition root has all three, so the proof lands here.
//!
//! What it establishes, and why each half matters:
//!
//!  - **One turn, two doors.** The hub below is the only thing a transport adds
//!    over [`ghostai_runtime::GhostRuntime`]: it opens a connection, hands the
//!    loop a [`TurnInput`], and stamps a `seq` on each event. A browser's
//!    WebSocket does exactly that and nothing more, so a channel reply and a
//!    browser reply come out of the same `AgentLoop::run`.
//!  - **The conversation is real.** The session the channel names is a row in
//!    the runtime's own `SessionStore`, created by the loop, with the channel
//!    recorded as its origin — so the same conversation is readable from the web
//!    UI afterwards, which is the whole point of bridging rather than forking.
//!
//! The reference channel is included by path rather than reimplemented, for the
//! reason its own suite gives: it proves the contract can be satisfied from
//! outside the crate, using only the public surface.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    dead_code,
    reason = "a fixture that cannot load is a failing test either way, and the example carries more surface than one suite uses"
)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use common::{Install, configured};
use ghostai_agent::testkit::{ScriptedProvider, ScriptedTurn};
use ghostai_agent::{AgentLoop, TurnInput};
use ghostai_channels::manager::{
    ChannelHub, ChannelHubConnectOptions, ChannelHubConnection, ChannelManager,
    ChannelManagerOptions, SendEvent,
};
use ghostai_channels::testkit::{counter_ids, flush};
use ghostai_core::Clock;
use ghostai_protocol::{ClientMessage, ServerMessage};
use ghostai_providers::ChatProvider;
use ghostai_runtime::provider_cache::{ProviderCache, ProviderFactory};
use ghostai_runtime::{GhostRuntime, RuntimeOptions, create_runtime};
use tokio_util::sync::CancellationToken;

#[path = "../../channels/examples/loopback.rs"]
mod loopback;

use loopback::{Loopback, LoopbackOptions, loopback_channel};

/// The whole of what a transport adds over a runtime.
///
/// Three methods, no HTTP: open a connection for a conversation, put a frame
/// into the loop, and stamp a `seq` on every event coming back. A WebSocket adds
/// a socket, a replay ring and an approval gate to this and changes none of it,
/// which is what makes "the same turn" a claim rather than a hope.
struct RuntimeHub {
    runtime: Arc<GhostRuntime>,
}

impl ChannelHub for RuntimeHub {
    fn connect(&self, options: ChannelHubConnectOptions) -> Arc<dyn ChannelHubConnection> {
        Arc::new(RuntimeConnection {
            session_key: options.session_key.unwrap_or_else(|| "loopback".to_owned()),
            channel: options.channel,
            agent_id: options.agent_id,
            workspace_id: options.workspace_id,
            send: options.send,
            runtime: Arc::clone(&self.runtime),
            seq: AtomicU64::new(0),
        })
    }
}

struct RuntimeConnection {
    session_key: String,
    channel: Option<String>,
    agent_id: Option<String>,
    workspace_id: Option<String>,
    send: SendEvent,
    runtime: Arc<GhostRuntime>,
    seq: AtomicU64,
}

impl ChannelHubConnection for RuntimeConnection {
    fn session_key(&self) -> String {
        self.session_key.clone()
    }

    fn receive(&self, frame: ClientMessage) {
        let ClientMessage::UserMessage(message) = frame else {
            return;
        };
        let Ok(agent_loop) = self.runtime.require_loop() else {
            return;
        };
        let input = TurnInput {
            channel: self.channel.clone(),
            agent_id: self.agent_id.clone(),
            workspace_id: self.workspace_id.clone(),
            ..TurnInput::new(message.session_key, message.content)
        };
        let send = Arc::clone(&self.send);
        // The `seq` is the transport's, exactly as it is for a browser: the loop
        // emits events and the thing holding the connection numbers them.
        let counter = AtomicU64::new(self.seq.load(Ordering::SeqCst));
        tokio::spawn(async move {
            let mut turn = AgentLoop::run(&agent_loop, input, &CancellationToken::new());
            while let Some(event) = turn.next_event().await {
                let seq = counter.fetch_add(1, Ordering::SeqCst) + 1;
                send(event.sequenced(seq));
            }
            let _ = turn.finish().await;
        });
    }

    fn close(&self) {}
}

/// A provider that answers one fixed sentence and reaches no network.
fn answering(text: &'static str) -> ProviderFactory {
    Arc::new(move |_| {
        Ok(ScriptedProvider::new(vec![ScriptedTurn::text(text)]) as Arc<dyn ChatProvider>)
    })
}

/// A runtime whose every turn answers `text`.
fn runtime(install: &Install, text: &'static str) -> Arc<GhostRuntime> {
    create_runtime(RuntimeOptions {
        providers: Some(Arc::new(ProviderCache::with_factory(8, answering(text)))),
        ..install.options()
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channel_turn_is_the_turn_a_browser_gets() {
    let install = Install::with(&configured("llama3"));
    let runtime = runtime(&install, "Hello from the model.");
    let (factory, channel) = loopback_channel(LoopbackOptions::default());

    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![factory],
        clock: Arc::clone(&install.clock) as Arc<dyn Clock>,
        ..ChannelManagerOptions::new(
            Arc::new(RuntimeHub {
                runtime: Arc::clone(&runtime),
            }),
            counter_ids("msg"),
        )
    })
    .unwrap();
    manager.start().await.unwrap();

    // A person typing into the channel's own transport.
    channel.say("Are you there?");

    assert!(
        common::eventually(Duration::from_secs(10), || channel
            .sent()
            .iter()
            .any(|line| line.contains("Hello from the model.")))
        .await,
        "transcript: {:?}",
        channel.transcript()
    );

    // The conversation is a row in the runtime's own store, so the same
    // conversation opens in the web UI afterwards.
    // The *outbound* half carries the hub's key: a channel names the
    // conversation in its own vocabulary and the manager maps it to a session.
    let key = channel
        .transcript()
        .iter()
        .find(|entry| !entry.incoming)
        .unwrap()
        .session_key
        .clone();
    let session = runtime.store().get_session(&key).unwrap().unwrap();
    assert_eq!(session.origin, "loopback");
    // And the turn really went through the loop: the user's message and the
    // model's answer are both appended, in order.
    let history = runtime
        .store()
        .messages(&key, &ghostai_core::session_store::ReadMessages::default())
        .unwrap();
    let texts: Vec<String> = history
        .iter()
        .map(|record| ghostai_core::text_of(&record.message))
        .collect();
    assert!(
        texts.iter().any(|text| text.contains("Are you there?")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("Hello from the model.")),
        "{texts:?}"
    );

    manager.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_messages_in_one_conversation_share_the_session_the_loop_created() {
    let install = Install::with(&configured("llama3"));
    let runtime = runtime(&install, "Noted.");
    let (factory, channel) = loopback_channel(LoopbackOptions::default());
    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![factory],
        clock: Arc::clone(&install.clock) as Arc<dyn Clock>,
        ..ChannelManagerOptions::new(
            Arc::new(RuntimeHub {
                runtime: Arc::clone(&runtime),
            }),
            counter_ids("msg"),
        )
    })
    .unwrap();
    manager.start().await.unwrap();

    channel.say("first");
    assert!(
        common::eventually(Duration::from_secs(10), || channel
            .sent()
            .iter()
            .filter(|line| line.contains("Noted."))
            .count()
            >= 1)
        .await
    );
    channel.say("second");
    assert!(
        common::eventually(Duration::from_secs(10), || channel
            .sent()
            .iter()
            .filter(|line| line.contains("Noted."))
            .count()
            >= 2)
        .await,
        "transcript: {:?}",
        channel.transcript()
    );

    // Naming the conversation the same way every time is the channel's second
    // job, and it is what makes the session the same session.
    let keys: std::collections::BTreeSet<String> = channel
        .transcript()
        .iter()
        .filter(|entry| !entry.incoming)
        .map(|entry| entry.session_key.clone())
        .collect();
    assert_eq!(keys.len(), 1, "{keys:?}");
    let key = keys.into_iter().next().unwrap();
    let history = runtime
        .store()
        .messages(&key, &ghostai_core::session_store::ReadMessages::default())
        .unwrap();
    assert!(
        history.len() >= 4,
        "one row per message, both turns: {history:?}"
    );

    manager.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unconfigured_install_leaves_the_channel_running_and_answers_nothing() {
    // The unconfigured state end to end: `ghostai serve` comes up on a bare
    // machine, the channel starts, and only the *turn* is missing.
    let install = Install::bare();
    let runtime = install.runtime().unwrap();
    assert!(!runtime.configured());
    let (factory, channel) = loopback_channel(LoopbackOptions::default());
    let manager = ChannelManager::new(ChannelManagerOptions {
        factories: vec![factory],
        clock: Arc::clone(&install.clock) as Arc<dyn Clock>,
        ..ChannelManagerOptions::new(
            Arc::new(RuntimeHub {
                runtime: Arc::clone(&runtime),
            }),
            counter_ids("msg"),
        )
    })
    .unwrap();
    manager.start().await.unwrap();

    channel.say("anyone there?");
    flush().await;
    assert!(channel.sent().is_empty(), "{:?}", channel.transcript());
    // The inbound half still happened, which is what a channel owes regardless.
    assert!(!channel.transcript().is_empty());

    manager.stop().await;
    let _: Option<ServerMessage> = None;
    let _ = Loopback::say;
}
