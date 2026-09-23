//! The extension host.
//!
//! An extension is a directory the operator approved by content digest, run as a
//! child process speaking JSON-RPC over stdio on MCP's wire. A plain MCP server
//! is a valid tools-only extension; `darkwire/`-namespaced methods add context
//! sections, commands and channels. The host holds what `initialize` and the
//! list methods returned, so unload is exact and a partial activation installs
//! nothing. The boundary is a process: the host's registries are unreachable.
#![forbid(unsafe_code)]

#[cfg(feature = "testkit")]
pub mod testkit;

pub mod bag;
pub mod host;
pub mod manifest;
pub mod methods;
pub mod process;
pub mod registration;
pub mod rpc;

pub use bag::{Registration, RegistrationBag, kind_name};
pub use host::{ExtensionHost, ExtensionHostOptions, SecretLookup, Timings};
pub use manifest::{V1_UNSUPPORTED, discover, refuses_version, schema_on_disk, settings_for};
pub use methods::{
    ChannelPublish, CommandEntry, CommandOutcome, ContextSection, ContextSections,
    ExtensionContributor, HostMethods, SecretFn, channel_factory, run_command,
};
pub use process::{
    ENV_EXTENSION_DATA_DIR, ENV_EXTENSION_ID, ExtensionProcess, KILL_GRACE_MS, SpawnOptions,
    Spawned, data_dir_for, spawn,
};
pub use registration::{EXTENSION_TOOL_PREFIX, add_bridged_tool, add_provider};

pub use rpc::{
    DarkwireInit, InitializeResult, JSONRPC_VERSION, METHOD_NOT_FOUND, NoHostMethods,
    OUTBOUND_QUEUE, PROTOCOL_VERSION, REQUEST_TIMEOUT, RpcClient, RpcError, RpcFailure, RpcHandler,
};
