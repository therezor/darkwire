//! Which agent runs a turn.
//!
//! A rule rather than a lookup, and a pure function of three inputs — so it can
//! be read, and tested, without a socket, a store or a session.

/// Picks the agent a turn runs on.
///
/// The **stored session wins**, exactly as its workspace does. A history built
/// under one agent's prompt, tools and permissions must not silently continue
/// under another's, so a frame naming an agent can only ever decide the binding
/// of a session that does not exist yet. Moving an existing one is an explicit
/// `PATCH /api/sessions/:key`.
///
/// The loop applies the same rule to the prompt one layer down. This is the
/// same decision made earlier, because *which loop* runs the turn has to agree
/// with what that loop then puts in the prompt.
///
/// One exception, and only one: a stored id that **no longer resolves** loses to
/// a frame that names an agent which does. The rule above protects a
/// conversation from being continued under settings it was not built with, and
/// an agent that has been deleted offers no such settings to protect — so the
/// only thing outranking the operator's explicit pick would achieve is dropping
/// them onto the default while they watched themselves choose something else.
///
/// When neither resolves, the stored id is returned so the notice that follows
/// names what the conversation actually claims rather than whatever the last
/// frame happened to carry.
///
/// An empty stored id is the same as none: a row that was written with a blank
/// binding is unbound, not bound to an agent called "".
pub fn agent_for_turn(
    stored: Option<&str>,
    requested: Option<&str>,
    resolves: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let stored = stored.filter(|id| !id.is_empty());
    let Some(stored) = stored else {
        return requested.map(str::to_owned);
    };
    if resolves(stored) {
        return Some(stored.to_owned());
    }
    if let Some(requested) = requested
        && resolves(requested)
    {
        return Some(requested.to_owned());
    }
    Some(stored.to_owned())
}
