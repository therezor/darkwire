//! How a subsystem that owns a changing set of tools hands them to a registry.
//!
//! One method, and it **replaces** rather than adds: an MCP server that
//! reconnects with a shorter list must lose the tools it no longer has, and an
//! extension that reloads must lose the ones its new code stopped registering.
//! Expressing that as add-plus-remove would put the bookkeeping in two places
//! that can disagree.
//!
//! `unregister_by_source` is the wrong grain for both of them. It is exact by
//! *source* — `mcp`, `extension` — and one server reconnecting or one
//! extension reloading would take every sibling's tools with it. `owner_id` is
//! the finer key the implementation remembers names under.
//!
//! Declared here because two crates consume it and neither may depend on the
//! other; this is the one place both already depend on, and the interface is
//! about a registry rather than about MCP.

use crate::tool::AnyTool;

/// A registry as its owners see it.
pub trait ToolSink: Send + Sync {
    /// Replaces everything `owner_id` currently holds.
    ///
    /// Returns the names it could not register — a collision with a built-in, a
    /// container program or another owner. Returned rather than failed: one clash
    /// must not cost an owner its other thirty-nine tools, and the caller is the
    /// one that knows where to report it.
    fn replace(&self, owner_id: &str, tools: Vec<AnyTool>) -> Vec<String>;
}
