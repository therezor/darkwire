//! The built-in tools, mirroring `src/builtin/`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod automation;
mod common;
mod conformance;
mod edit;
mod exec;
mod find;
mod grep;
mod ls;
mod memory;
mod read;
mod set;
mod shared;
mod todo;
mod tool_search;
mod write;
