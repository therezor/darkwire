//! `enabledTools` — which of a server's tools this install advertises at all.
//!
//! Applied to the **upstream** names, before flattening, and that is the whole
//! of the design: the names an operator reads in a server's own documentation
//! are the names they type here. Matching on `mcp_github_create_issue` would
//! make the config a function of this client's naming scheme.
//!
//! Note the layering against the per-agent permission map. This narrows what
//! the *install* holds; `agents.list.<id>.tools` decides who may call what is
//! left. A tool filtered out here occupies no name, appears in no agent editor,
//! and costs no prompt tokens — which is the point for a server advertising
//! forty tools when two of them are wanted.

use crate::session::McpToolDescriptor;

/// The schema's own default: everything the server advertises.
const WILDCARD: &str = "*";

/// What `enabledTools` kept, and what it asked for that was not there.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolSelection {
    /// The descriptors that matched, in the order the server advertised them.
    pub selected: Vec<McpToolDescriptor>,
    /// Entries that matched nothing — a warning, never a failure.
    pub unmatched: Vec<String>,
}

/// A trailing `*` and nothing more elaborate.
///
/// A server grouping its tools by prefix (`repo_`, `issue_`) is common enough
/// to be worth one character of syntax; a full glob would be a second
/// mini-language to specify, validate and document for a field an operator
/// writes once.
fn matches(pattern: &str, name: &str) -> bool {
    if pattern == WILDCARD {
        return true;
    }
    match pattern.strip_suffix(WILDCARD) {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// Applies `enabled_tools` to what a server advertised.
///
/// An empty list selects nothing, which is a real answer: the convention here
/// is the opposite of an agent's `exec.allowedBinaries`, because this is a narrowing
/// of a server the operator already added.
pub fn select_tools<S: AsRef<str>>(
    advertised: &[McpToolDescriptor],
    enabled_tools: &[S],
) -> ToolSelection {
    if enabled_tools
        .iter()
        .any(|pattern| pattern.as_ref() == WILDCARD)
    {
        return ToolSelection {
            selected: advertised.to_vec(),
            unmatched: Vec::new(),
        };
    }

    let selected = advertised
        .iter()
        .filter(|descriptor| {
            enabled_tools
                .iter()
                .any(|pattern| matches(pattern.as_ref(), &descriptor.name))
        })
        .cloned()
        .collect();

    // Reported so an operator who mistyped a tool name finds out from the MCP
    // servers row rather than from a model that never reaches for it.
    let unmatched = enabled_tools
        .iter()
        .map(AsRef::as_ref)
        .filter(|pattern| {
            !advertised
                .iter()
                .any(|descriptor| matches(pattern, &descriptor.name))
        })
        .map(str::to_owned)
        .collect();

    ToolSelection {
        selected,
        unmatched,
    }
}
