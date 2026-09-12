//! The Telegram channel, as the composition root consumes it.
//!
//! A narrow surface on purpose: a factory, the port it needs filled, and the
//! settings type. Everything else — the Bot API, the command table, the
//! callback store, the renderer — is this module's own business, which is what
//! keeps "shipped in the box" from meaning "part of the contract".

pub mod access;
pub mod api;
pub mod channel;
pub mod chats;
pub mod commands;
pub mod console;
pub mod format;
pub mod menus;
pub mod render;
pub mod settings;

pub use channel::{Telegram, TelegramChannelOptions, telegram_channel};
pub use console::{MemoryState, SkillSummary, SkillsState, TelegramConsole};
pub use settings::{TelegramSettings, parse_telegram_settings};
