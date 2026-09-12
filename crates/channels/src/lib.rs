//! Channels — every way into the agent that is not a browser.
//!
//! The crate is three things: a contract a transport implements, a manager that
//! bridges the message bus to the session hub, and one channel over that
//! contract — Telegram, which ships in the box.
//!
//! That third part is worth stating plainly, because it is the property the
//! whole arrangement rests on: **the built-in consumes exactly the
//! [`ChannelFactory`] contract an extension would**. It is registered by the
//! composition root like any other factory, it reaches nothing an extension
//! could not reach, and [`testkit::channel_conformance`] — the suite written for
//! implementations outside this repository — runs against it. The contract
//! cannot rot into "whatever the built-in needed", because the built-in is held
//! to the contract by the same test everyone else is.
//!
//! The two things Telegram needs that the contract does not give every channel
//! are stated where they are used rather than smuggled in here:
//! [`ChannelContext::control`] is a member because *any* transport that can
//! answer an approval needs it, and [`telegram::console::TelegramConsole`] is a
//! factory option, not a context member, because a channel never sees a session
//! store.
//!
//! It depends on `ghostai-protocol` and `ghostai-core` alone. The hub is stated
//! as a structural port ([`ChannelHub`]), so nothing here imports the HTTP
//! server, and a channel therefore has no path to the agent loop, the session
//! store or a router — only the `publish` and `control` functions it was handed.
//!
//! The conformance suite sits behind the `testkit` cargo feature rather than in
//! the default build: it exists for implementors, a channel is the one
//! implementation that will routinely live outside this repository, and a
//! contract an external channel cannot run against is a contract that only
//! holds for the channels that were already here.
#![forbid(unsafe_code)]

pub mod channel;
pub mod manager;
pub mod projection;
pub mod telegram;

#[cfg(feature = "testkit")]
pub mod testkit;

pub use channel::{
    BoxFuture, CHANNEL_CONTROL_TAGS, Channel, ChannelBuilder, ChannelContext, ChannelControl,
    ChannelControlFrame, ChannelFactory, ChannelInbound, ControlFn, DEFAULT_ACCEPTED_KINDS,
    PublishFn,
};
pub use manager::{
    ChannelHub, ChannelHubConnectOptions, ChannelHubConnection, ChannelManager,
    ChannelManagerOptions, DEFAULT_MAX_CHANNEL_SESSIONS, SendEvent,
};
pub use projection::{
    APPROVAL_METADATA_KEY, ApprovalDraftDetail, OutboundDraft, TurnProjection,
    TurnProjectionOptions,
};
pub use telegram::{TelegramChannelOptions, TelegramConsole, TelegramSettings, telegram_channel};
