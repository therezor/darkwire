//! Tools — how the agent acts on the world.
//!
//! Everything a model can *do* enters through this crate: a tool is defined
//! here, registered here, and called here, and there is no second path. That
//! is what makes the security boundary reviewable — `darkwire-security` decides
//! whether a path, a binary or a host is acceptable, and this crate is the only
//! caller that asks.
//!
//! The pieces:
//!
//!  - [`Tool`] is the trait; [`TypedTool`] collapses a tool's argument type,
//!    its advertised JSON Schema, its validation and its handler into one
//!    declaration.
//!  - [`ToolRegistry`] is the source-tagged, per-agent collection: memoised
//!    definitions for the prompt, exact teardown by source for extension
//!    unload, and an `execute` that validates, bounds, fences and reports
//!    without ever failing.
//!  - The fourteen built-ins, every one of which routes its filesystem access
//!    through the workspace jail and, for `exec`, an argv guard rather than a
//!    shell. `grep` and `find` are ripgrep compiled in, not spawned, so they
//!    hold inside the jail and need no search binary on the host.
//!  - Two command runners behind one seam: a local process and a container.
//!    Neither ever builds a shell string. A failed sandbox start is a refusal,
//!    never a downgrade to the host.
#![forbid(unsafe_code)]

pub mod argv;
pub mod automation;
pub mod builtin;
pub mod container_runner;
pub mod discovery;
pub mod environment;
pub mod names;
pub mod registry;
pub mod runner;
pub mod scope;
pub mod sink;
pub mod tasks;
pub mod tool;
pub mod web;

#[cfg(feature = "testkit")]
pub mod testkit;

pub use argv::coerce_argv;
pub use automation::{AutomationOutcome, AutomationPort, AutomationRefusal, AutomationResolver};
pub use builtin::{
    BuiltinOptions, FindRequest, GrepMode, GrepRequest, Hit, MAX_SEARCH_RESULTS, SearchResults,
    TOOL_SEARCH_NAME, automation_tool, builtin_tools, edit_tool, exec_tool, find_blocking,
    find_tool, format_bytes, grep_blocking, grep_tool, ls_tool, memory_tool, read_tool,
    register_builtins, render_activation, render_search, search, skill_tool, todo_tool,
    tool_search_tool, web_fetch_tool, web_search_tool, write_tool,
};
pub use container_runner::{
    ContainerCreateOptions, ContainerExecOptions, ContainerRunner, ContainerRunnerOptions,
    KillSignal, RUNS_MOUNT_DIR, Transcript, WorkspaceMount, container_create_argv,
    container_exec_argv, container_is_gone, container_kill_argv, container_run_dir,
};
pub use discovery::{Activation, ToolDiscovery};
pub use environment::{Environment, EnvironmentResolver, HostEnvironment, Placed};
pub use names::{is_advertisable_name, namespaced_tool_name, namespaced_tool_names};
pub use registry::{ListenerId, ToolInvocation, ToolRegistry, ToolRegistryOptions, ToolScope};
pub use runner::{
    CommandRunner, KILL_GRACE_MS, LocalRunner, OutputStream, OutputTee, PlacementRequest,
    RunOutcome, RunRequest,
};
pub use scope::{is_enabled, permission_for};
pub use sink::ToolSink;
pub use tasks::TaskPort;
pub use tool::{
    AnyTool, ArgIssue, BoxFuture, CallPolicy, Preprocess, TOOL_NAME_PATTERN, Tool, ToolContext,
    ToolExecution, ToolHandler, ToolOutput, ToolSpec, TypedTool, assert_not_aborted,
    default_tools_config, is_tool_name, parameters_for,
};
pub use web::{LiveWebResolver, WebPort, WebResolver, WebSettings};
