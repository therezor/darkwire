//! The Telegram channel, mirroring `src/telegram/`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

#[allow(dead_code, reason = "each suite drives a different part of the double")]
mod console_double;
#[allow(dead_code, reason = "each suite drives a different part of the double")]
mod fake_bot_api;

mod access;
mod api;
mod approvals;
mod channel;
mod chats;
mod commands;
mod conformance;
mod format;
mod menus;
mod render;
mod settings;
