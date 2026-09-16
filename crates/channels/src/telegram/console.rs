//! What the chat commands need that no hub frame expresses.
//!
//! The commands split cleanly in two, and the split is the reason there are two
//! seams rather than one. Anything that *starts, stops or rewrites a turn* —
//! `/stop`, `/edit`, `/regenerate`, an approval — goes through
//! [`crate::ChannelContext::control`] and out the same door a browser uses, so
//! it inherits the busy check, the FIFO queue and the `session.truncated`
//! broadcast that tells an open tab its transcript was rewritten. Everything
//! else is a read or a write against stores the hub has no frame for: listing
//! conversations, renaming one, measuring a context window, naming an agent.
//!
//! This is that second half, and it is a **factory option rather than a
//! `ChannelContext` member** on purpose. `channel.rs` states that a channel
//! never sees a session store; putting one on the context would hand it to
//! every channel any extension registers. The composition root compiles this
//! channel in and hands it a store deliberately, which is a different act.
//!
//! Typed entirely in `darkwire-core` and `darkwire-protocol` vocabulary, because
//! that is the whole of what this crate may import — and because it happens to
//! be the same vocabulary the REST API answers in, so the composition root
//! satisfies most of it with what it already exposes.

use darkwire_core::{Result, SessionStore, WorkspaceStore};
use darkwire_protocol::{AgentSummary, ContextResponse, ModelsResponse};

use crate::channel::BoxFuture;

/// Everything `/memory` prints. It changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryState {
    /// Whether the agent holds the `memory` tool. Absent counts as denied.
    pub granted: bool,
    /// How many memories the workspace holds.
    pub count: usize,
    /// Estimated tokens their index costs in every prompt. `0` when there are
    /// none.
    pub tokens: u64,
}

/// One row of the catalogue. The body stays on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    /// The folder name, which is what `/skills` prints.
    pub name: String,
    /// The one line the catalogue advertises it with.
    pub description: String,
    /// Whether this chat's agent is one the sheet is for.
    ///
    /// A boolean rather than the list of agent ids, so the renderer has one
    /// branch instead of a set membership test — deciding who a sheet is for is
    /// the console's job, and saying so is this layer's. A sheet written before
    /// scope existed is everyone's, which is what `true` means here.
    pub mine: bool,
}

/// Everything `/skills` prints. It changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillsState {
    /// Whether the agent holds the `skill` tool. Absent counts as denied.
    pub granted: bool,
    /// Name and one line each, in the order the catalogue advertises them.
    pub skills: Vec<SkillSummary>,
}

/// The half of a chat command's world that is not a hub frame.
pub trait TelegramConsole: Send + Sync {
    /// Concrete, and narrow about *behaviour* — what a chat is allowed to reach
    /// — rather than about types.
    fn store(&self) -> &SessionStore;

    /// The workspaces on this install.
    fn workspaces(&self) -> &WorkspaceStore;

    /// Agents a conversation may be bound to.
    fn agents(&self) -> Vec<AgentSummary>;

    /// What the configured endpoints answered with when last asked.
    fn models(&self) -> BoxFuture<'_, Result<ModelsResponse>>;

    /// Moves this process onto another model.
    ///
    /// Process-wide and not persisted, which is exactly what the terminal's
    /// `/model` does — and the reason the command is admin-gated: it moves the
    /// browser and every other chat too.
    fn set_model(&self, id: &str);

    /// `None` when the session has nothing to measure yet.
    fn context<'a>(
        &'a self,
        session_key: &'a str,
    ) -> BoxFuture<'a, Result<Option<ContextResponse>>>;

    /// What this chat's agent remembers, and whether it may.
    ///
    /// A read, so it belongs on this side of the split rather than in a frame.
    fn memory<'a>(&'a self, session_key: &'a str) -> BoxFuture<'a, Result<MemoryState>>;

    /// The sheets this chat's workspace holds, and whether the agent may use
    /// them.
    ///
    /// Beside `memory` and for the same reason: a read against a store this
    /// crate may not open for itself. What it feeds is discovery rather than
    /// the prompt — the catalogue is already in the prompt, and this is what
    /// puts it on a person's screen.
    fn skills<'a>(&'a self, session_key: &'a str) -> BoxFuture<'a, Result<SkillsState>>;
}
