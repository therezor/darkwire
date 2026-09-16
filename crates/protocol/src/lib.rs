//! The wire contract, mirrored from `packages/protocol`.
//!
//! `packages/protocol` (TypeScript, zod) stays the source of truth because the
//! web UI parses these schemas in the browser. This crate mirrors every schema
//! as a serde type deriving `JsonSchema`; `tests/drift.rs` compares the two
//! after normalisation, so a field that exists on one side and not the other
//! fails a test rather than reaching a component three renders later.
//!
//! Beyond the shapes, the crate carries the handful of pure functions whose
//! behaviour must be identical wherever they run — the browser and the server
//! both mint ids, render prompt templates and compute token rates, and two
//! implementations of a rule whose whole job is agreement is not a rule. They
//! are pinned by the fixtures under `fixtures/`, which the TypeScript suite
//! writes and `tests/fixtures.rs` reads. Zero I/O.
#![forbid(unsafe_code)]

pub mod automation;
pub mod config;
pub mod environment;
pub mod extension;
pub mod ids;
pub mod json;
pub mod messages;
pub mod prompt;
pub mod rest;
pub mod schemas;
pub mod subagent;
pub mod tools;
pub mod uuid;
pub mod ws;

pub use automation::*;
pub use config::*;
pub use environment::*;
pub use extension::*;
pub use ids::*;
pub use messages::*;
pub use prompt::*;
pub use rest::*;
pub use schemas::{PROTOCOL_SCHEMAS, RegisteredSchema, protocol_generator, registered};
pub use subagent::*;
pub use tools::*;
pub use uuid::*;
pub use ws::*;
