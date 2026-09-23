//! Every integration test for this crate, as one binary.
//!
//! One binary per crate rather than one per file: each binary links the whole
//! dependency tree, so a file each cost minutes of linking and gigabytes of
//! `target/` for no isolation that nextest does not already give per test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that does not hold is a failing test either way"
)]

mod agent;
mod app;
mod approval;
mod ask;
mod bottom_pane;
mod chat;
mod chat_widget;
mod commands;
mod extension;
mod header;
mod help_parity;
mod history_cell;
mod i18n;
mod init;
mod live_area;
mod log_line;
mod menu;
mod messages;
mod models;
mod pickers_agents;
mod pickers_effort;
mod pickers_models;
mod pickers_palette;
mod pickers_rows;
mod pickers_sessions;
mod program;
mod render;
mod run;
mod runtime;
mod sandbox_service;
mod select_overlay;
mod serve;
mod server_runtime;
mod stream;
mod telegram;
mod transcript_overlay;
mod version;
