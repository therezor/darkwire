//! A remote tool's name, made safe to advertise: `mcp_{server}_{tool}`.
//!
//! The arithmetic — the character class, the 64-character cap, the digest tail
//! that keeps two truncated names apart — is `namespaced_tool_name` in
//! `ghostai-tools`, because the extension host needs exactly the same rule
//! under a different prefix and a copy would drift the first time the cap
//! moved. What is MCP's here is the prefix and nothing else; the prefix stays
//! a parameter so the extension host can hand in `ext` through the same bridge.

pub use ghostai_tools::names::NamespacedNames as FlattenedNames;
pub use ghostai_tools::names::is_advertisable_name;
use ghostai_tools::names::{namespaced_tool_name, namespaced_tool_names};

/// The prefix every MCP server's tools carry.
pub const MCP_TOOL_PREFIX: &str = "mcp";

/// `{prefix}_{owner}_{tool}`, always matching the provider pattern.
pub fn flatten_tool_name(prefix: &str, owner_id: &str, tool_name: &str) -> String {
    namespaced_tool_name(prefix, owner_id, tool_name)
}

/// `mcp_{server}_{tool}`.
pub fn flatten_mcp_tool_name(server_id: &str, tool_name: &str) -> String {
    flatten_tool_name(MCP_TOOL_PREFIX, server_id, tool_name)
}

/// The final names for one owner's tools, with within-owner clashes broken.
///
/// Insertion order decides who keeps the plain name, and the caller hands these
/// in the order the server advertised them: that is the only ordering the
/// operator can see in the server's own documentation.
pub fn flatten_tool_names(prefix: &str, owner_id: &str, tool_names: &[String]) -> FlattenedNames {
    namespaced_tool_names(prefix, owner_id, tool_names)
}
