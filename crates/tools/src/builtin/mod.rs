//! The built-in tool set.
//!
//! Nine tools, and the reason there are only nine is that every one of them
//! is a capability the agent cannot obtain any other way. Anything expressible
//! as a command — `grep`, `find`, `git` — is `exec`'s job, and adding a tool
//! per command would spend context on definitions the model already knows how
//! to write in argv form.
//!
//! `memory` and `skill` are the two that do not quite fit that test, and they
//! are here for a second reason: a tool carries a per-agent permission, so
//! being a tool is what makes each feature switchable without a config flag
//! beside it that could disagree.
//!
//! `automation` is registered conditionally, against `scheduler.enabled`: an
//! install with the scheduler switched off should not advertise a way to
//! schedule, because a tool that can only answer "there is no scheduler" costs
//! a turn to learn what its absence would have said for free.
//!
//! `automation` is also the one built-in absent from `DEFAULT_AGENT_TOOLS`, so
//! being registered is not the same as being reachable: no agent has it until
//! an operator grants it.
//!
//! Everything else is registered once, for every agent, and narrowed per agent
//! by the loop: whether an agent has `exec` is its permission map's answer,
//! and `tool_search` is advertised only to an agent whose `lazy_discovery` is
//! on. The registry is shared by every agent, so a per-agent decision cannot be
//! made here.

pub mod automation;
pub mod edit_file;
pub mod exec;
pub mod list_dir;
pub mod memory;
pub mod read_file;
pub mod shared;
pub mod skill;
pub mod tool_search;
pub mod write_file;

use std::sync::Arc;

use darkwire_core::Result;
use darkwire_protocol::ToolSource;

pub use automation::automation_tool;
pub use edit_file::edit_file_tool;
pub use exec::{exec_tool, render_run};
pub use list_dir::list_dir_tool;
pub use memory::memory_tool;
pub use read_file::read_file_tool;
pub use shared::format_bytes;
pub use skill::skill_tool;
pub use tool_search::{
    Hit, MAX_SEARCH_RESULTS, SearchResults, TOOL_SEARCH_NAME, render_activation, render_search,
    search, tool_search_tool,
};
pub use write_file::write_file_tool;

use crate::registry::ToolRegistry;
use crate::tool::{AnyTool, ToolHandler, TypedTool};

/// A built-in as an [`AnyTool`].
///
/// Every built-in's spec is a literal checked by the crate's own tests, so a
/// construction failure is a defect in this crate rather than a condition to
/// report; the `unreachable!` says so at the one place it could surface.
pub(crate) fn built<H: ToolHandler>(tool: Result<TypedTool<H>>) -> AnyTool {
    Arc::new(tool.unwrap_or_else(|error| unreachable!("a built-in's spec is a literal: {error}")))
}

/// Every built-in, including `exec` and `automation`, in registration order.
pub fn all_builtin_tools() -> Vec<AnyTool> {
    vec![
        read_file_tool(),
        write_file_tool(),
        edit_file_tool(),
        list_dir_tool(),
        exec_tool(),
        automation_tool(),
        memory_tool(),
        skill_tool(),
        tool_search_tool(),
    ]
}

/// Which of the conditionally-registered built-ins this install wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinOptions {
    /// `false` drops `automation`. Defaults to keeping it.
    pub scheduler: bool,
}

impl Default for BuiltinOptions {
    fn default() -> BuiltinOptions {
        BuiltinOptions { scheduler: true }
    }
}

/// The built-ins this install registers: everything, less `automation` when
/// there is no scheduler to write to.
pub fn builtin_tools(options: BuiltinOptions) -> Vec<AnyTool> {
    all_builtin_tools()
        .into_iter()
        .filter(|tool| tool.definition().name != "automation" || options.scheduler)
        .collect()
}

/// Registers the built-ins under the `builtin` source.
pub fn register_builtins(registry: &ToolRegistry, options: BuiltinOptions) -> Result<()> {
    registry.register_all(builtin_tools(options), ToolSource::Builtin)
}
