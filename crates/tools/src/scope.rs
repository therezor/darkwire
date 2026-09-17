//! Which of a registry's tools one agent may call, and what happens when it
//! does.
//!
//! The registry is shared and must stay that way: an MCP server is one
//! connection and one set of registrations no matter how many agents are
//! configured, and giving each agent its own registry would open that
//! connection once per agent and tear it down N times on unload. So an agent
//! gets a *view* rather than a copy, and this module is the rule that view
//! applies.
//!
//! The rule is a map from tool name to permission, and it does both jobs:
//!
//!  - **Absent means disabled.** Not "unrestricted" — the opposite. A tool the
//!    map does not mention never reaches the definitions the model is sent, so
//!    an agent holds exactly what somebody enabled on it and nothing an
//!    extension registered later quietly joins. This replaces an `{allow,
//!    deny}` pair where an empty `allow` meant "everything", which made a
//!    freshly created agent the most powerful one in the install.
//!  - **`deny` is a spelling of absent.** Both are disabled; `deny` exists so a
//!    UI has somewhere to put the off switch. Deleting the key would make the
//!    row vanish from the editor, which is not what switching something off
//!    looks like.
//!
//! Enablement and permission being one field is the point. Two mechanisms
//! could disagree — a tool admitted by the list and refused by the policy is a
//! turn spent discovering that — and there is no arrangement of one map that
//! can.

use darkwire_protocol::{ToolPermission, ToolPermissions};

use crate::builtin::TOOL_SEARCH_NAME;

/// What `perms` says about `name`.
///
/// `None` for the whole map means `allow`, which is the bare registry rather
/// than an agent's view of one: the CLI's one-shot paths and most tests hold a
/// registry directly and were never handed a permission map. Every path that
/// resolves an *agent* builds one, so this fallback is not reachable from a
/// turn.
pub fn permission_for(perms: Option<&ToolPermissions>, name: &str) -> ToolPermission {
    // The one name the map does not decide. `tool_search` reveals nothing the
    // agent could not already call and runs nothing itself, so there is no
    // decision for a permission to record; and an agent whose short list had
    // no door would be sent tools it could never reach. Whether the door is
    // advertised at all is the agent's `lazy_discovery` switch, in the loop.
    if name == TOOL_SEARCH_NAME {
        return ToolPermission::Allow;
    }
    match perms {
        None => ToolPermission::Allow,
        Some(perms) => perms.get(name).copied().unwrap_or(ToolPermission::Deny),
    }
}

/// Whether the model is offered `name` at all. See the module docs.
pub fn is_enabled(perms: Option<&ToolPermissions>, name: &str) -> bool {
    permission_for(perms, name) != ToolPermission::Deny
}
