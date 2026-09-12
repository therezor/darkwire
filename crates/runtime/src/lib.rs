//! The composition root.
//!
//! The one place a config file becomes a provider, a jail, a store, a registry
//! and one loop per agent, so the CLI, the server, the scheduler and every
//! channel share one wiring. No HTTP here and nothing above `ghostai-agent`: a
//! transport builds a runtime; a runtime never knows what drives it. An
//! unconfigured install is a state, not an error.
#![forbid(unsafe_code)]

pub mod agents;
pub mod credentials;
pub mod jail_cache;
pub mod loop_cache;
pub mod merge;
pub mod provider_cache;
pub mod runtime;
pub mod tool_sink;
pub mod toolbox_pool;

pub use agents::{
    AgentConfigWarning, AgentMissReason, AgentResolution, AgentWarningCode, EffectiveAgent,
    PrunedSubagent, assert_writable_agent_ids, granted, has_agent, list_agents,
    prune_dangling_subagents, resolve_agent, resolve_agent_or_default, resolve_agents,
    tool_prompt_warnings,
};
pub use credentials::{PROVIDER_CREDENTIAL_NAMESPACE, VaultChoice, find_credential, open_vault};
pub use jail_cache::{JailCache, JailFactory, MAX_CACHED_JAILS};
pub use loop_cache::{LoopCache, LoopFactory, MAX_CACHED_LOOPS};
pub use merge::{DELETE_BY_NULL, REPLACE_WHOLESALE, merge_config_patch};
pub use provider_cache::{
    MAX_CACHED_PROVIDERS, ProviderCache, ProviderFactory, ProviderRequest, provider_cache_key,
};
pub use runtime::{ExtensionChoice, GhostRuntime, McpChoice, RuntimeOptions, create_runtime};
pub use tool_sink::{RegistrySink, registry_tool_sink};
pub use toolbox_pool::{
    ContainerEngine, DockerEngine, DockerEngineOptions, HostPathFn, IdFactory, LivenessFn,
    MAX_LIVE_TOOLBOXES, OWNER_LABEL, RunnerFactory, TOOLBOX_IDLE_MS, ToolboxPool,
    ToolboxPoolOptions, docker_engine, owner_process_looks_alive, owner_tag,
};
