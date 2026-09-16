//! A tool name from somewhere else, made safe to advertise.
//!
//! `{prefix}_{owner}_{tool}` is the scheme [`TOOL_NAME_PATTERN`] names in
//! advance, and it does two jobs at once:
//!
//!  - **Keeping the name legal.** Providers speaking the OpenAI wire accept
//!    `[A-Za-z0-9_-]{1,64}` and reject anything else *mid-turn*, as a 400 that
//!    reads like the model is broken. An MCP server is free to advertise
//!    `search files` and an extension is free to call a tool `send message`;
//!    the model is not free to be told about either under that name.
//!  - **Keeping the registry flat.** One shared registry holds the built-ins,
//!    every server's tools and every extension's, and the
//!    registry treats a duplicate as a `conflict` rather than letting load
//!    order decide which one a call reaches. Qualifying by owner is what makes
//!    two of them that both advertise `search` able to coexist.
//!
//! The 64-character cap is where this stops being a pure rename. Truncating
//! alone would map two long names onto one, so the tail becomes a digest of
//! the name *before* truncation — stable across restarts (the model's prompt
//! cache keys on these) and distinct for names sharing a prefix.
//!
//! It lives here because the MCP client and the extension host need exactly
//! the same arithmetic under different prefixes, and the alternative was a
//! copy that would drift the first time the cap moved.
//!
//! Lengths are UTF-16 code units, the unit the pattern's cap was written in.

use std::collections::HashSet;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use crate::tool::is_tool_name;

/// The cap in [`TOOL_NAME_PATTERN`], restated because the arithmetic needs it.
const MAX_NAME_LENGTH: usize = 64;

/// `_` plus eight hex characters.
const DIGEST_LENGTH: usize = 9;

static UNSAFE_CHARS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("[^A-Za-z0-9-]+").unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// FNV-1a, 32-bit, over UTF-16 code units.
///
/// A non-cryptographic hash is the right tool: this is a collision *avoidance*
/// measure between names an operator can see and rename, not a security
/// boundary.
fn digest(value: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in value.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(16_777_619);
    }
    format!("{hash:08x}")
}

/// Underscore is the separator, so it cannot survive inside a segment.
///
/// Otherwise an owner called `a_b` holding `c` and an owner called `a` holding
/// `b_c` would both flatten to `mcp_a_b_c`, and the collision would be silent
/// rather than merely possible.
fn sanitise(value: &str) -> String {
    UNSAFE_CHARS.replace_all(value, "-").into_owned()
}

fn take_units(value: &str, units: usize) -> String {
    let taken: Vec<u16> = value.encode_utf16().take(units).collect();
    String::from_utf16_lossy(&taken)
}

fn unit_length(value: &str) -> usize {
    value.encode_utf16().count()
}

/// `{prefix}_{owner}_{tool}`, always matching [`TOOL_NAME_PATTERN`].
pub fn namespaced_tool_name(prefix: &str, owner_id: &str, tool_name: &str) -> String {
    let full = format!("{prefix}_{}_{}", sanitise(owner_id), sanitise(tool_name));
    if unit_length(&full) <= MAX_NAME_LENGTH {
        return full;
    }
    // The digest goes on the end and the *tool* segment is what gives way: the
    // owner prefix is how an operator recognises the row in the agent editor,
    // and losing it would make every truncated name look alike.
    let budget = MAX_NAME_LENGTH - DIGEST_LENGTH;
    format!("{}_{}", take_units(&full, budget), digest(&full))
}

/// The final names for one owner's tools, with within-owner clashes broken.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NamespacedNames {
    /// Upstream name → advertised name, in the order given.
    pub names: IndexMap<String, String>,
    /// Upstream names that had to be numbered to stay distinct.
    pub collisions: Vec<String>,
}

/// The final names for one owner's tools, with within-owner clashes broken.
///
/// Two upstream names can flatten to one — `read file` and `read-file` both
/// become `read-file` — and the registry would refuse the second as a conflict,
/// losing a tool for a reason nothing reports. Numbering the loser keeps both,
/// and the caller raises a warning naming what happened.
///
/// Insertion order decides who keeps the plain name, and the caller hands these
/// in the order the owner advertised them: for an MCP server that is the only
/// ordering visible in the server's own documentation, and for an extension it
/// is the order its `activate` registered them in.
pub fn namespaced_tool_names(
    prefix: &str,
    owner_id: &str,
    tool_names: &[String],
) -> NamespacedNames {
    let mut names = IndexMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut collisions = Vec::new();

    for tool_name in tool_names {
        let base = namespaced_tool_name(prefix, owner_id, tool_name);
        let mut candidate = base.clone();
        let mut suffix = 2u32;
        while taken.contains(&candidate) {
            let tail = format!("_{suffix}");
            candidate = format!(
                "{}{tail}",
                take_units(&base, MAX_NAME_LENGTH - unit_length(&tail))
            );
            suffix += 1;
        }
        if candidate != base {
            collisions.push(tool_name.clone());
        }
        taken.insert(candidate.clone());
        names.insert(tool_name.clone(), candidate);
    }

    NamespacedNames { names, collisions }
}

/// Whether a generated name is one a provider will accept. For tests.
pub fn is_advertisable_name(name: &str) -> bool {
    is_tool_name(name)
}
