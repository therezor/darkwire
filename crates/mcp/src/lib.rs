//! The MCP client.
//!
//! Third-party tools, from servers an operator configured. The crate sits above
//! `ghostai-tools` and below `ghostai-runtime`, and it is deliberately ignorant
//! of everything on either side: it has never heard of a config file, an HTTP
//! route or a WebSocket. The composition root hands [`McpManager`] a map of
//! servers and a sink, and the rest — a connecting server's tools appearing in
//! the agent editor, a `tools.changed` frame reaching an open tab — falls out
//! of the shared registry mutating.
//!
//! Two structural rules hold the crate together:
//!
//! - **[`sdk_connector`] is the only module that imports `rmcp`.** Everything
//!   else is written against [`McpSession`], so no test spawns a subprocess or
//!   opens a socket to prove that a backoff timer fires, and an SDK breaking
//!   change is one file.
//! - **Nothing here can fail its caller.** `reconcile` is synchronous and
//!   infallible; a server that is unreachable is a status row, not an error.
//!
//! The security posture — why a stdio `command` does not go through the exec
//! guard and an MCP `url` does not go through the guarded fetch — is argued in
//! [`spec`]. The short version is that both guards exist to constrain what a
//! *model* chose, and these are operator configuration in the same trust class
//! as a provider's `apiBase`.
//!
//! The bridge is shared with the extension host: one bridge, two name prefixes.
#![forbid(unsafe_code)]

pub mod bridge;
pub mod callback;
pub mod connection;
pub mod filter;
pub mod manager;
pub mod names;
pub mod oauth;
pub mod schema;
pub mod sdk_connector;
pub mod session;
pub mod spec;
pub mod store;

#[cfg(feature = "testkit")]
pub mod testkit;

pub use bridge::{BridgeOptions, BridgedTool, McpCallTarget, bridge_tool, flatten_content};
pub use callback::{
    AuthorizationHandle, CALLBACK_PATH, CallbackListener, CallbackListenerOptions,
    DEFAULT_CALLBACK_PORT,
};
pub use connection::{
    AuthorizationAttempt, AuthorizationBroker, BackoffOptions, McpConnection, McpConnectionOptions,
};
pub use filter::{ToolSelection, select_tools};
pub use manager::{McpManager, McpManagerOptions, McpToolSink};
pub use names::{
    FlattenedNames, MCP_TOOL_PREFIX, flatten_mcp_tool_name, flatten_tool_name, flatten_tool_names,
    is_advertisable_name,
};
pub use oauth::{
    ClientInformation, ClientMetadata, EndpointGuard, Endpoints, InvalidationScope,
    OAUTH_CLIENT_NAME, OAuthFlow, OAuthFlowOptions, StoredTokens,
};
pub use schema::{
    ArgFailure, ArgValidator, NormalisedSchema, SchemaIssue, compile_validator, normalise_schema,
};
pub use sdk_connector::{
    CLIENT_NAME, DEFAULT_INHERITED_ENV_VARS, PipeFactory, STDERR_BUDGET_BYTES, SdkConnector,
    SdkConnectorOptions, default_environment,
};
pub use session::{
    AuthorizationCodeWaiter, McpCallOptions, McpCallResult, McpConnectContext, McpConnector,
    McpContentPart, McpResourceContents, McpSession, McpSessionEvent, McpToolDescriptor,
    ServerLogSink,
};
pub use spec::{
    McpConnectionSpec, McpTransportSpec, exposure_fingerprint, resolve_spec, transport_fingerprint,
};
pub use store::{
    MCP_CREDENTIAL_NAMESPACE, McpSecretSlot, McpSecretStore, MemorySecretStore, VaultSecretStore,
};
