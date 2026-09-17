//! Where `tool_search` looks, and what it may reveal.
//!
//! The list of tools an agent may call is the loop's: it is the registry
//! narrowed by that agent's permissions, with the delegation tools composed on
//! top and the operator's wording applied. None of that is visible from this
//! crate, so the tool takes the same shape `automation` does: **the interface
//! is declared down here, and the loop supplies the implementation**, bound to
//! the turn that asked. The tool runs on arguments a model wrote, so every
//! decision about what a session may see stays on the other side of the port.
//!
//! Activation is a fact about a session, not a turn. A tool the model pulled in
//! stays in the list for the rest of the conversation, because dropping it at
//! the next user message would make the model search for it again and would
//! change the request prefix a provider caches, twice per turn.

use darkwire_protocol::ToolDefinition;

/// What one activation call did with each name it was given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activation {
    /// Newly in the list from the next request on.
    pub activated: Vec<String>,
    /// Pinned or activated earlier, so nothing changed.
    pub already_visible: Vec<String>,
    /// Not a tool this agent may call. Nothing is said about whether the name
    /// exists somewhere else.
    pub unknown: Vec<String>,
}

/// One turn's view of the tools lazy discovery hides, already scoped to the
/// agent and session that asked.
pub trait ToolDiscovery: Send + Sync {
    /// Every tool this agent may call, with the operator's wording applied and
    /// `tool_search` itself left out. The search runs over this and nothing
    /// else, so a denied tool cannot be found.
    fn corpus(&self) -> Vec<ToolDefinition>;
    /// The names already in the model's list: the pinned tools it may call and
    /// this session's activations.
    fn visible(&self) -> Vec<String>;
    /// Adds the named tools to this session's list.
    fn activate(&self, names: &[String]) -> Activation;
}
