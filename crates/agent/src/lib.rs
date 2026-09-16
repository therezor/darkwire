//! The agent: the turn loop, its events, and the prompt it runs on.
//!
//! `AgentLoop::run` returns a stream of events plus a completion; dropping the
//! stream unwinds the turn through the same cancellation an explicit stop uses.
//! History is append-only, an error response is never appended, a denied tool
//! call still gets a `tool` message, and one cancellation token threads from the
//! transport to the child process. Delegation lives here rather than in a tool
//! because `darkwire-tools` sits below this crate.
#![forbid(unsafe_code)]

pub mod approval;
pub mod attachments;
pub mod context;
pub mod dispatch;
pub mod events;
pub mod memory_contributor;
pub mod prompt;
pub mod skills;
pub mod skills_contributor;
pub mod steering;
pub mod subagent;
pub mod text_tool_call;

// `loop` is a keyword, so the module that holds the loop is spelled out.
#[path = "agent_loop.rs"]
pub mod agent_loop;

#[cfg(feature = "testkit")]
pub mod testkit;

pub use agent_loop::{
    AgentLoop, AgentLoopOptions, LoopAgent, LoopResolver, PromptPreview, PromptPreviewInput, Turn,
    TurnGuard, TurnInput, TurnResult,
};
pub use approval::{
    ApprovalDecision, ApprovalGate, ApprovalRequest, DenialReason, denied_notice,
    denied_tool_result,
};
pub use attachments::{
    AttachmentCache, MAX_INLINE_TEXT_BYTES, MaterialiseOptions, materialise_attachments,
    materialise_file_part,
};
pub use context::{
    ContextBreakdown, ContextReport, MeasureContext, describe_context, measure_context,
};
pub use dispatch::{
    CANCELLED_TOOL_RESULT, MAX_PARALLEL_TOOL_CALLS, NoDelegation, SubagentDelegate,
    TOOL_HEARTBEAT_MS, ToolCallOutcome, ToolDispatcher, ToolDispatcherOptions, TurnScope,
    parse_tool_args,
};
pub use events::{AgentEvent, EVENT_CHANNEL_CAPACITY, EventSink, Stamped};
pub use memory_contributor::{MemoryContributor, render_memory_section};
pub use prompt::{
    BuildRawPrompt, BuildRuntimeBlock, BuildStaticPrompt, ContextContributor, Host, Platform,
    PromptAgent, PromptTools, RuntimePromptContext, StaticPromptContext, build_raw_prompt,
    build_runtime_block, build_static_prompt, contributor_sections, runtime_reminder, template_or,
};
pub use skills::{
    MAX_DESCRIPTION_CHARS, MAX_SKILLS, SKILL_FILENAME, SKILL_MAX_BYTES, SKILLS_DIRNAME, Skill,
    parse_skill_agents, read_skills, skills_for_agent,
};
pub use skills_contributor::{SkillsContributor, render_skills};
pub use steering::{
    MAX_PENDING_STEER, STEERING_PREFIX, SteeringMessage, SteeringQueue, steering_text,
};
pub use subagent::{
    DelegationRefusal, MAX_SUBAGENT_DEPTH, SubagentBinding, parse_task, refuse_delegation,
    refused_execution, subagent_definition, subagent_map, subagent_result,
};
pub use text_tool_call::{text_tool_call_correction, text_tool_call_name};

/// The separator between top-level sections of the assembled prompt.
///
/// Re-exported rather than declared: it is defined beside the prompt template
/// in `darkwire-protocol`, so the template and the separator that joins its
/// sections cannot drift.
pub use darkwire_protocol::SECTION_SEPARATOR;
