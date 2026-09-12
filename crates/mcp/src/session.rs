//! What this crate needs from a live MCP server, and nothing else.
//!
//! Everything above the connector — the bridge, the connection's state
//! machine, the manager's reconciliation — is written against the trait here
//! rather than against the SDK's client. That buys two things worth the
//! indirection: a test drives a session that is a plain struct, so nothing in
//! CI spawns a subprocess or opens a socket to prove that a backoff timer
//! fires; and a breaking change in the SDK is a change to one file
//! ([`crate::sdk_connector`]) rather than to every file that touched a client.

use std::sync::Arc;

use futures::future::BoxFuture;
use ghostai_core::{GhostError, Result};
use ghostai_protocol::ToolAnnotations;
use ghostai_protocol::json::Object;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::oauth::OAuthFlow;
use crate::spec::McpConnectionSpec;

/// One tool as a server advertises it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDescriptor {
    /// The upstream name, before flattening.
    pub name: String,
    /// A human-readable title, when the server gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the tool does, in the server's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Raw JSON Schema. Normalised by [`crate::schema`] before it is advertised.
    pub input_schema: Value,
    /// Passed through unchanged: `ToolAnnotations` in the protocol crate was
    /// written to mirror MCP's vocabulary exactly. There is no mapping table
    /// here and there should never be one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// An embedded resource inside a tool result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceContents {
    /// Where the resource lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Its text, for a text resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Its media type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// One part of a tool result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpContentPart {
    /// `text`, `image`, `audio`, `resource`, `resource_link`, or something newer.
    #[serde(rename = "type")]
    pub kind: String,
    /// The text of a `text` part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Base64 payload of a binary part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// Media type of a binary part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Target of a `resource_link` part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Payload of a `resource` part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<McpResourceContents>,
}

impl McpContentPart {
    /// A `text` part.
    pub fn text(text: impl Into<String>) -> McpContentPart {
        McpContentPart {
            kind: "text".to_owned(),
            text: Some(text.into()),
            ..McpContentPart::default()
        }
    }
}

/// What a server answers a `tools/call` with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCallResult {
    /// The parts of the answer, in order.
    #[serde(default)]
    pub content: Vec<McpContentPart>,
    /// The server's own verdict on the call. Absent means it went fine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// The machine-readable twin of `content`, when the server produces one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}

impl McpCallResult {
    /// A result of one text part.
    pub fn text(text: impl Into<String>) -> McpCallResult {
        McpCallResult {
            content: vec![McpContentPart::text(text)],
            ..McpCallResult::default()
        }
    }
}

/// How one call is bounded.
#[derive(Debug, Clone)]
pub struct McpCallOptions {
    /// Cancelling it abandons the call.
    pub token: CancellationToken,
    /// `0` means no per-call cap; the registry's own timeout still applies.
    pub timeout_ms: u64,
}

/// Something a live session announces.
#[derive(Debug, Clone)]
pub enum McpSessionEvent {
    /// The server said its tool list moved. Fired at most once per change.
    ToolListChanged,
    /// The transport went away — a crash, a network drop. Never fired for a
    /// `close()` the session was asked for.
    Closed(Option<Arc<GhostError>>),
}

/// A live MCP server, as far as this crate is concerned.
///
/// Futures are boxed so the trait can be held as `dyn McpSession`; a
/// connection swaps sessions over its lifetime and a test hands in a fake.
pub trait McpSession: Send + Sync {
    /// What the server called itself in the handshake.
    fn server_name(&self) -> &str;
    /// The version it reported.
    fn server_version(&self) -> &str;
    /// Things worth a line on the status row that did not stop the session —
    /// a transport substituted for one this build lacks, say.
    fn warnings(&self) -> Vec<String> {
        Vec::new()
    }
    /// Every tool the server advertises, following pagination to the end.
    fn list_tools(&self, token: CancellationToken)
    -> BoxFuture<'_, Result<Vec<McpToolDescriptor>>>;
    /// One `tools/call`.
    fn call_tool(
        &self,
        name: &str,
        args: Object,
        options: McpCallOptions,
    ) -> BoxFuture<'_, Result<McpCallResult>>;
    /// A stream of what the session announces. Each call gets its own receiver.
    fn subscribe(&self) -> broadcast::Receiver<McpSessionEvent>;
    /// Tears the session down. Never fails: there is nothing a caller could do.
    fn close(&self) -> BoxFuture<'_, ()>;
}

impl std::fmt::Debug for dyn McpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSession")
            .field("server_name", &self.server_name())
            .field("server_version", &self.server_version())
            .finish_non_exhaustive()
    }
}

/// Where a stdio child's stderr lines go.
pub type ServerLogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Resolves with an authorization code once the operator has followed the
/// link. Built per attempt by the connection, consumed by the connector.
pub type AuthorizationCodeWaiter =
    Box<dyn FnOnce() -> BoxFuture<'static, Result<String>> + Send + Sync>;

/// What a connector is handed beside the spec.
pub struct McpConnectContext {
    /// Cancelling it abandons the attempt.
    pub token: CancellationToken,
    /// The OAuth flow for this server, when it has one.
    pub auth: Option<Arc<OAuthFlow>>,
    /// Absent means "do not wait" — a connection with no way to ask.
    ///
    /// The wait lives behind this rather than in the caller because finishing
    /// the flow means exchanging the code with the flow the *connector* holds,
    /// then dialling again on the same attempt; a connection would otherwise
    /// need two states for one outage.
    pub await_authorization_code: Option<AuthorizationCodeWaiter>,
    /// Where a stdio child's stderr goes. Defaults to the log at debug.
    pub on_server_log: Option<ServerLogSink>,
}

impl std::fmt::Debug for McpConnectContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpConnectContext")
            .field("auth", &self.auth.is_some())
            .field(
                "await_authorization_code",
                &self.await_authorization_code.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl McpConnectContext {
    /// A context that only carries a token: no OAuth, no stderr sink.
    pub fn bare(token: CancellationToken) -> McpConnectContext {
        McpConnectContext {
            token,
            auth: None,
            await_authorization_code: None,
            on_server_log: None,
        }
    }
}

/// Opens one connection. The seam a test replaces with a fake.
///
/// Fails rather than reporting: a connector's only job is to hand back a
/// session or say why it could not, and [`crate::connection::McpConnection`] is
/// what turns a failure into a state and a retry.
pub trait McpConnector: Send + Sync {
    /// Dials `spec` once.
    fn connect(
        &self,
        spec: McpConnectionSpec,
        context: McpConnectContext,
    ) -> BoxFuture<'_, Result<Arc<dyn McpSession>>>;
}
