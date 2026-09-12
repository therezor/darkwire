//! The built-in tools, mirroring `src/builtin/`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod automation;
mod conformance;
mod exec;
mod fs;
mod memory;
mod set;
mod shared;
