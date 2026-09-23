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

mod common;

mod agent_loop;
mod approval;
mod attachments;
mod context;
mod dispatch;
mod events;
mod history_window;
mod length_cut;
mod memory_contributor;
mod prompt;
mod skills;
mod skills_contributor;
mod steering;
mod subagent;
mod tasks_contributor;
mod text_tool_call;
