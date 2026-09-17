//! What may name a directory DarkWire creates from user input.
//!
//! Two things are named this way — a workspace and an agent — and both arrive
//! over HTTP. A workspace id becomes a path; an agent id no longer does, but it
//! keeps the identical rules, because the reasons that made them identical have
//! not changed and two sets that agree today are two sets that drift apart in
//! exactly the case nobody tested.
//!
//! The rules, and why each is a rule rather than a preference:
//!
//! - **One segment, `[a-z0-9-]`, no leading or trailing hyphen, 1–40 chars.**
//!   `..`, `/`, `\`, `:`, NUL and a leading `~` are all unrepresentable, so a
//!   crafted id cannot become a path outside the tree it belongs to. The jail
//!   would catch it anyway; this catches it a layer earlier, where the error
//!   can say something useful.
//! - **Lowercase only, and that is a security rule.** APFS and NTFS fold case,
//!   so `Work` and `work` would be two rows sharing one directory — two things
//!   that believe they are isolated and are not.
//! - **The Windows device names are reserved**, because `mkdir con` fails on
//!   exactly one platform, and something that cannot be created on Windows is
//!   a bug report from a user who did nothing wrong.
//!
//! What is *not* here is which ids a particular kind reserves beyond those, or
//! what a name with nothing usable in it falls back to. Those differ between
//! workspaces and agents, so callers pass them in. It lives in the protocol
//! crate because both sides need it: the server turns an id into a path, and
//! the browser *mints* one when an operator creates an agent.

use std::sync::LazyLock;

use regex::Regex;

/// 1–40 chars, lowercase alphanumerics and hyphens, no leading or trailing hyphen.
pub static SLUG_ID_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(SLUG_ID_PATTERN_SOURCE).unwrap_or_else(|_| unreachable!()));

/// The pattern's source, for a `#[garde(pattern(...))]` or a JSON Schema.
pub const SLUG_ID_PATTERN_SOURCE: &str = "^[a-z0-9](?:[a-z0-9-]{0,38}[a-z0-9])?$";

/// The longest legal id.
pub const MAX_SLUG_ID_LENGTH: usize = 40;

/// Reserved on Windows whatever the id names.
pub const RESERVED_DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Whether a string may be resolved to a directory name.
pub fn is_slug_id(value: &str) -> bool {
    SLUG_ID_PATTERN.is_match(value)
}

/// A display name reduced to a legal id.
///
/// Lossy on purpose — the name is stored separately and is what the UI shows,
/// so this only has to produce something legal, stable and recognisable. A name
/// with nothing usable in it falls back rather than failing: the caller then
/// disambiguates against the rows that already exist.
pub fn slugify(name: &str, reserved: &[&str], fallback: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut pending_hyphen = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if pending_hyphen && !slug.is_empty() {
                slug.push('-');
            }
            pending_hyphen = false;
            slug.push(c);
        } else {
            pending_hyphen = true;
        }
    }
    // Every char left is ASCII, so a byte offset is a character offset.
    slug.truncate(MAX_SLUG_ID_LENGTH);
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() || reserved.contains(&slug) {
        fallback.to_owned()
    } else {
        slug.to_owned()
    }
}

// Workspaces

/// The workspace every install has, and the one that cannot be deleted.
///
/// Reserved as a *name to create* because the store bootstraps it; it is still
/// a legal id to resolve, and its folder is a sibling of every other
/// workspace's.
pub const DEFAULT_WORKSPACE_ID: &str = "default";

/// Reserved as *names to create*. `default` is still a legal id to resolve.
pub const RESERVED_WORKSPACE_IDS: &[&str] = &[
    DEFAULT_WORKSPACE_ID,
    "con",
    "prn",
    "aux",
    "nul",
    "com1",
    "com2",
    "com3",
    "com4",
    "com5",
    "com6",
    "com7",
    "com8",
    "com9",
    "lpt1",
    "lpt2",
    "lpt3",
    "lpt4",
    "lpt5",
    "lpt6",
    "lpt7",
    "lpt8",
    "lpt9",
];

/// Whether a string may be resolved to a workspace directory.
pub fn is_workspace_id(value: &str) -> bool {
    is_slug_id(value)
}

/// A display name reduced to a legal workspace id. See [`slugify`].
pub fn derive_workspace_id(name: &str) -> String {
    slugify(name, RESERVED_WORKSPACE_IDS, "workspace")
}

// Agents

/// The agent every install has: `agents.list.default`, always present.
///
/// Reserved as a *name to create* because it names the agent an install runs
/// as before anyone has defined one. `agents.list.default` may be written to
/// customise it; what the UI does not do is mint a second agent under it.
pub const DEFAULT_AGENT_ID: &str = "default";

/// Reserved as *names to create*. `default` is still a legal id to resolve.
pub const RESERVED_AGENT_IDS: &[&str] = RESERVED_WORKSPACE_IDS;

/// Whether a string may name an agent.
pub fn is_agent_id(value: &str) -> bool {
    is_slug_id(value)
}

/// A display label reduced to a legal agent id. See [`slugify`].
pub fn derive_agent_id(label: &str) -> String {
    slugify(label, RESERVED_AGENT_IDS, "agent")
}

/// The prefix that keeps a subagent's tool name out of every other namespace.
const SUBAGENT_TOOL_PREFIX: &str = "ask_";

/// The tool name a model calls to hand work to a subagent.
///
/// Derived rather than configured, so an operator cannot name two subagents
/// the same thing and cannot shadow a built-in: an agent id is 1–40 lowercase
/// alphanumerics and hyphens, so the prefixed form is 5–45 characters of
/// `[a-z0-9_]` — inside the tool-name limit of 64, and never equal to a
/// built-in name, none of which start with the prefix. The hyphens become
/// underscores because a leading `ask_` already reads as a prefix and
/// `ask_code-review` reads as two.
pub fn subagent_tool_name(agent_id: &str) -> String {
    format!("{SUBAGENT_TOOL_PREFIX}{}", agent_id.replace('-', "_"))
}

// Extensions

/// Whether a string may name an extension.
///
/// The same slug rules, for a stronger version of the same reason: an
/// extension id names a directory under `<root>/extensions`, and it is also the
/// prefix every id that extension contributes must carry — a channel, a
/// provider, a command and a tool. Nothing is reserved: there is no built-in
/// extension for a name to collide with, and shadowing between two installed
/// extensions is a conflict the host reports on the offending row.
pub fn is_extension_id(value: &str) -> bool {
    is_slug_id(value)
}
