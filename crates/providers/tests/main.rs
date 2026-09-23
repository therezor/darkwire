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

mod errors;
mod factory;
mod instances;
mod measure;
mod openai_chat;
mod registry;
mod resilience;
mod sse;
mod tokens;
mod types;
mod wire_encode;
mod wires;
