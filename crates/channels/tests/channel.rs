//! The contract itself: what a channel declares, and what it may send.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot be built is a failing test either way"
)]

use std::sync::Arc;

use ghostai_channels::channel::{
    BoxFuture, CHANNEL_CONTROL_TAGS, Channel, ChannelControlFrame, ChannelFactory, ChannelInbound,
    DEFAULT_ACCEPTED_KINDS,
};
use ghostai_core::Result;
use ghostai_core::message_bus::{OutboundKind, OutboundMessage};
use ghostai_protocol::{
    ApprovalScope, ClientMessage, EditMessage, EditTag, RegenerateMessage, RegenerateTag,
    SteerMessage, SteerTag, StopTurnMessage, StopTurnTag, ToolApproveMessage, ToolApproveTag,
};

/// A channel that declares nothing beyond what it must.
struct Bare;

impl Channel for Bare {
    fn id(&self) -> &'static str {
        "bare"
    }

    fn send(&self, _: OutboundMessage) -> BoxFuture<'_, Result<()>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

// accepts

#[test]
fn the_default_accepts_everything_but_progress() {
    // A transport that can only post would repeat the whole answer twice, once
    // in pieces and once whole, so `progress` has to be opted into.
    assert_eq!(
        DEFAULT_ACCEPTED_KINDS,
        &[
            OutboundKind::Reply,
            OutboundKind::Notice,
            OutboundKind::Error
        ]
    );
    assert!(!DEFAULT_ACCEPTED_KINDS.contains(&OutboundKind::Progress));
}

#[test]
fn a_channel_that_says_nothing_gets_the_default() {
    assert_eq!(Bare.accepts(), DEFAULT_ACCEPTED_KINDS);
}

#[tokio::test]
async fn start_and_stop_are_optional() {
    // A transport with nothing to connect should not have to say so.
    Bare.start().await.expect("the default start succeeds");
    Bare.stop().await.expect("the default stop succeeds");
}

// ChannelControlFrame

fn every_frame() -> Vec<ChannelControlFrame> {
    vec![
        ChannelControlFrame::ToolApprove(ToolApproveMessage {
            tag: ToolApproveTag,
            call_id: "call-1".to_owned(),
            approved: true,
            scope: ApprovalScope::Once,
        }),
        ChannelControlFrame::StopTurn(StopTurnMessage {
            tag: StopTurnTag,
            session_key: "bare:1".to_owned(),
        }),
        ChannelControlFrame::Steer(SteerMessage {
            tag: SteerTag,
            session_key: "bare:1".to_owned(),
            content: "try the other file".to_owned(),
        }),
        ChannelControlFrame::Regenerate(RegenerateMessage {
            tag: RegenerateTag,
            session_key: "bare:1".to_owned(),
            seq: Some(3),
            client_message_id: None,
        }),
        ChannelControlFrame::Edit(EditMessage {
            tag: EditTag,
            session_key: "bare:1".to_owned(),
            seq: 3,
            content: "actually, this".to_owned(),
            attachments: Vec::new(),
            agent_id: None,
            client_message_id: None,
        }),
    ]
}

#[test]
fn a_channel_may_send_exactly_five_kinds_of_frame() {
    let tags: Vec<&str> = every_frame().iter().map(ChannelControlFrame::tag).collect();

    assert_eq!(tags, CHANNEL_CONTROL_TAGS);
    assert_eq!(tags.len(), 5);
}

#[test]
fn the_frames_that_move_a_connection_are_excluded() {
    // A channel that sent one would move where its events arrive while its next
    // message still went to the old conversation, and the two halves would
    // disagree with nothing to say so.
    for excluded in ["session.new", "session.switch", "session.resume"] {
        assert!(
            !CHANNEL_CONTROL_TAGS.contains(&excluded),
            "{excluded} moves the connection and must not be a channel's to send"
        );
        // It is a real client frame, so this is an exclusion rather than a typo.
        assert!(ClientMessage::VALUES.contains(&excluded));
    }
}

#[test]
fn ping_is_excluded_because_there_is_no_socket_to_keep_alive() {
    assert!(!CHANNEL_CONTROL_TAGS.contains(&"ping"));
    assert!(ClientMessage::VALUES.contains(&"ping"));
}

#[test]
fn every_control_frame_is_a_client_message() {
    for frame in every_frame() {
        let tag = frame.tag();
        let client: ClientMessage = frame.into();
        assert_eq!(client.tag(), tag);
    }
}

#[test]
fn addressing_rewrites_the_key_into_every_frame_that_carries_one() {
    for frame in every_frame() {
        let tag = frame.tag();
        let addressed = frame.with_session_key("bare:rewritten");
        let client: ClientMessage = addressed.into();
        match client {
            // `tool.approve` carries no session of its own and is passed
            // through as it stands.
            ClientMessage::ToolApprove(body) => assert_eq!(body.call_id, "call-1"),
            ClientMessage::StopTurn(body) => assert_eq!(body.session_key, "bare:rewritten"),
            ClientMessage::Steer(body) => assert_eq!(body.session_key, "bare:rewritten"),
            ClientMessage::Regenerate(body) => assert_eq!(body.session_key, "bare:rewritten"),
            ClientMessage::Edit(body) => assert_eq!(body.session_key, "bare:rewritten"),
            other => panic!("{tag} became {}", other.tag()),
        }
    }
}

#[test]
fn addressing_leaves_the_rest_of_a_frame_alone() {
    let ChannelControlFrame::Edit(edited) = every_frame()
        .into_iter()
        .nth(4)
        .expect("the edit frame")
        .with_session_key("bare:rewritten")
    else {
        panic!("an edit stays an edit");
    };

    assert_eq!(edited.seq, 3);
    assert_eq!(edited.content, "actually, this");
}

// The factory

#[test]
fn a_factory_names_the_id_before_anything_is_built() {
    // The manager needs it first: it is the key of the settings block, and
    // `enabled: false` there means the channel is never built at all.
    let factory = ChannelFactory::new("bare", Arc::new(|_| Ok(Arc::new(Bare) as Arc<dyn Channel>)));

    assert_eq!(factory.id(), "bare");
}

#[test]
fn a_factory_failure_is_a_value() {
    let factory = ChannelFactory::new(
        "bare",
        Arc::new(|_| {
            Err(ghostai_core::GhostError::new(
                ghostai_core::ErrorKind::Config,
                "the settings block is wrong",
            ))
        }),
    );

    assert_eq!(factory.id(), "bare");
    assert!(format!("{factory:?}").contains("bare"));
}

#[test]
fn an_inbound_message_defaults_to_nothing_the_manager_has_to_undo() {
    let inbound = ChannelInbound::default();

    assert!(inbound.session_key.is_empty());
    assert!(inbound.content.is_empty());
    assert!(inbound.metadata.is_empty());
    // No idempotency key means the bus mints one, rather than the channel
    // inventing a constant that would collapse every message into one.
    assert_eq!(inbound.id, None);
}
