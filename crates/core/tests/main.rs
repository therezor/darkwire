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

mod capture;
mod common;

mod clock;
mod config;
mod cron;
mod db;
mod ddl_parity;
mod errors;
mod frontmatter;
mod history;
mod ids;
mod logger;
mod memory;
mod message_bus;
mod messages;
mod paths;
mod session_store;
mod session_title;
mod sqlite_row;
mod workspace_files;
mod workspace_store;
