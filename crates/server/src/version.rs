//! The version this build reports.
//!
//! One version for the whole workspace, read from the crate metadata rather
//! than written out again here. The root `package.json` must agree with it, and
//! a test in the binary crate asserts that, so there is nothing left to forget
//! to bump.

/// The server version, as `GET /api/status` and `GET /api/health` report it.
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
