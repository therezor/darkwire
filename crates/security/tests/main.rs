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

mod allow;
mod egress;
mod environment;
mod exec_guard;
mod exec_rules;
mod extension;
mod extension_store;
mod fetch;
mod ip;
mod jail;
mod keychain;
mod nonce;
mod policy_store;
mod random;
mod vault;
