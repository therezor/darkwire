//! The only module in this crate that imports `rmcp`.
//!
//! Everything else speaks [`McpSession`], so a test drives a plain struct and
//! CI spawns no subprocess and opens no socket. This file is the adapter, and
//! the whole of the SDK's surface area we depend on: the client service, two
//! transports, and the initialise error's authorization signal.
//!
//! ## The transports
//!
//! - **stdio** is a child process. Its `command` and `args` are operator
//!   configuration and do not go through the exec guard — [`crate::spec`]
//!   argues why. What *is* enforced: the environment is a minimal inherited
//!   set plus what the entry names, never this process's whole environment, so
//!   a provider API key in `darkwire serve`'s environment does not silently
//!   land inside third-party code; stderr is piped to the log under a budget
//!   rather than interleaved into DarkWire's own output; and the child is
//!   killed when the session closes.
//! - **Streamable HTTP** is the default for a `url`.
//! - **SSE**, the legacy HTTP transport, has no client in this SDK version.
//!   An entry that names it by hand is dialled as Streamable HTTP with a
//!   warning on its status row: a server that speaks both works, and one that
//!   does not fails visibly with the reason beside it, rather than the entry
//!   being refused outright.
//!
//! ## Authorization
//!
//! The flow is this crate's ([`crate::oauth`]); the SDK only says *that* a
//! server wants it. Before an HTTP dial the flow is asked for an access token
//! (refreshing an expired one). A 401 during the handshake becomes a typed
//! `needs_authorization` failure; when the caller supplied somewhere to wait,
//! the connector reports the link, waits for the code, exchanges it and dials
//! again on the same attempt, so the connection sees one outcome.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::json::Object;
use darkwire_protocol::{McpTransport, ToolAnnotations};
use futures::future::BoxFuture;
use rmcp::ClientHandler;
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResult, CancelledNotificationParam,
    ClientCapabilities, ClientInfo, ClientRequest, ContentBlock, Implementation, ProtocolVersion,
    ResourceContents, ServerResult,
};
use rmcp::service::{
    ClientInitializeError, Peer, PeerRequestOptions, QuitReason, RoleClient,
    RunningServiceCancellationToken, RxJsonRpcMessage, ServiceError, TxJsonRpcMessage,
    serve_client_with_ct,
};
use rmcp::transport::Transport;
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig, StreamableHttpError,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::process::ChildStderr;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::session::{
    McpCallOptions, McpCallResult, McpConnectContext, McpConnector, McpContentPart,
    McpResourceContents, McpSession, McpSessionEvent, McpToolDescriptor, ServerLogSink,
};
use crate::spec::{McpConnectionSpec, McpTransportSpec};

/// What DarkWire calls itself in the MCP initialise handshake.
pub const CLIENT_NAME: &str = "darkwire";

/// Beyond this many stderr bytes per server, logging stops. The stream is
/// still drained so the child never blocks on a full pipe.
pub const STDERR_BUDGET_BYTES: usize = 64 * 1024;

/// The variables a stdio child inherits when the entry does not set them.
///
/// The SDK's own safe-to-inherit set: enough to find binaries and behave like
/// a program run from a terminal, and nothing that could be a credential.
pub const DEFAULT_INHERITED_ENV_VARS: &[&str] =
    &["HOME", "LOGNAME", "PATH", "SHELL", "TERM", "USER"];

/// One end of an in-memory pipe, for the test that proves this adapter speaks
/// the protocol without spawning or dialling anything.
pub type BoxRead = Box<dyn AsyncRead + Send + Unpin>;
/// The other end.
pub type BoxWrite = Box<dyn AsyncWrite + Send + Unpin>;
/// Opens a fresh pipe per attempt.
pub type PipeFactory = Arc<dyn Fn() -> std::io::Result<(BoxRead, BoxWrite)> + Send + Sync>;

/// How the connector is built.
pub struct SdkConnectorOptions {
    /// Overrides [`CLIENT_NAME`].
    pub client_name: Option<String>,
    /// Overrides the crate version.
    pub client_version: Option<String>,
    /// Speaks Streamable HTTP. Not the guarded fetch; see [`crate::spec`].
    pub http: reqwest::Client,
    /// Replaces every transport with an in-memory pipe. Tests only.
    pub pipe: Option<PipeFactory>,
}

impl Default for SdkConnectorOptions {
    fn default() -> SdkConnectorOptions {
        SdkConnectorOptions {
            client_name: None,
            client_version: None,
            http: reqwest::Client::new(),
            pipe: None,
        }
    }
}

/// The real connector. Injected everywhere so a test can supply another.
pub struct SdkConnector {
    name: String,
    version: String,
    http: reqwest::Client,
    pipe: Option<PipeFactory>,
}

impl std::fmt::Debug for SdkConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkConnector")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("pipe", &self.pipe.is_some())
            .finish_non_exhaustive()
    }
}

/// The environment a stdio child starts from.
pub fn default_environment() -> HashMap<String, String> {
    DEFAULT_INHERITED_ENV_VARS
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).to_owned(), value))
        })
        // A value starting with `()` is a shell function export, not a value.
        .filter(|(_, value)| !value.starts_with("()"))
        .collect()
}

/// The three transports as one type, since the SDK's trait is generic.
enum SessionTransport {
    Child(TokioChildProcess),
    Http(StreamableHttpClientTransport<reqwest::Client>),
    Pipe(AsyncRwTransport<RoleClient, BoxRead, BoxWrite>),
}

/// One error type over the three, keeping the inner error as `source` so the
/// SDK's authorization check can still find the 401 underneath.
#[derive(Debug)]
struct TransportError(Box<dyn std::error::Error + Send + Sync + 'static>);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

impl From<std::io::Error> for TransportError {
    fn from(error: std::io::Error) -> TransportError {
        TransportError(Box::new(error))
    }
}

impl From<StreamableHttpError<reqwest::Error>> for TransportError {
    fn from(error: StreamableHttpError<reqwest::Error>) -> TransportError {
        TransportError(Box::new(error))
    }
}

impl Transport<RoleClient> for SessionTransport {
    type Error = TransportError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = std::result::Result<(), TransportError>> + Send + 'static {
        let sending: BoxFuture<'static, std::result::Result<(), TransportError>> = match self {
            SessionTransport::Child(inner) => {
                let fut = inner.send(item);
                Box::pin(async move { fut.await.map_err(TransportError::from) })
            }
            SessionTransport::Http(inner) => {
                let fut = inner.send(item);
                Box::pin(async move { fut.await.map_err(TransportError::from) })
            }
            SessionTransport::Pipe(inner) => {
                let fut = inner.send(item);
                Box::pin(async move { fut.await.map_err(TransportError::from) })
            }
        };
        sending
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        match self {
            SessionTransport::Child(inner) => inner.receive().await,
            SessionTransport::Http(inner) => inner.receive().await,
            SessionTransport::Pipe(inner) => inner.receive().await,
        }
    }

    async fn close(&mut self) -> std::result::Result<(), TransportError> {
        match self {
            SessionTransport::Child(inner) => inner.close().await.map_err(TransportError::from),
            SessionTransport::Http(inner) => inner.close().await.map_err(TransportError::from),
            SessionTransport::Pipe(inner) => inner.close().await.map_err(TransportError::from),
        }
    }
}

/// The client half of the handshake, and the one notification we act on.
#[derive(Clone)]
struct Handler {
    info: ClientInfo,
    events: broadcast::Sender<McpSessionEvent>,
}

impl ClientHandler for Handler {
    fn get_info(&self) -> ClientInfo {
        self.info.clone()
    }

    fn on_tool_list_changed(
        &self,
        _: rmcp::service::NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let _ = self.events.send(McpSessionEvent::ToolListChanged);
        std::future::ready(())
    }
}

/// Why one dial did not produce a session.
enum AttemptError {
    /// The server wants OAuth; the challenge, when it sent one.
    NeedsAuthorization(Option<String>),
    /// Anything else.
    Failed(WireError),
}

impl AttemptError {
    fn into_wire(self) -> WireError {
        match self {
            AttemptError::Failed(error) => error,
            AttemptError::NeedsAuthorization(challenge) => needs_authorization(challenge),
        }
    }
}

/// The SDK's signal that the operator now has to go somewhere — a state, not a
/// failure to retry — tagged so the connection can tell.
fn needs_authorization(challenge: Option<String>) -> WireError {
    let error = WireError::new(
        ErrorKind::PermissionDenied,
        "The MCP server requires authorization",
    )
    .with_detail("needsAuthorization", true);
    match challenge {
        Some(challenge) => error.with_detail("wwwAuthenticate", challenge),
        None => error,
    }
}

/// The SDK's request-level failures as the taxonomy.
fn service_error(error: ServiceError) -> WireError {
    match error {
        ServiceError::Timeout { timeout } => WireError::new(
            ErrorKind::Timeout,
            format!(
                "The MCP server did not answer within {} ms",
                timeout.as_millis()
            ),
        ),
        ServiceError::Cancelled { .. } => WireError::aborted("MCP call"),
        ServiceError::McpError(data) => WireError::new(
            ErrorKind::Tool,
            format!("The MCP server refused the call: {}", data.message),
        )
        .with_detail("code", data.code.0),
        ServiceError::TransportClosed => {
            WireError::new(ErrorKind::Network, "The MCP server closed the connection")
        }
        other => WireError::new(ErrorKind::Network, other.to_string()),
    }
}

/// Drains a stdio child's stderr into the log, under a budget.
///
/// A server that writes a line per request would otherwise fill the log with
/// somebody else's diagnostics; a server that crashes writes its reason there
/// and it is the only place that reason exists.
async fn pump_stderr(stderr: ChildStderr, sink: ServerLogSink) {
    let mut lines = BufReader::new(stderr).lines();
    let mut spent = 0usize;
    while let Ok(Some(line)) = lines.next_line().await {
        if spent >= STDERR_BUDGET_BYTES {
            continue;
        }
        spent += line.len();
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            sink(trimmed);
        }
    }
}

fn content_part(block: ContentBlock) -> McpContentPart {
    match block {
        ContentBlock::Text(text) => McpContentPart::text(text.text),
        ContentBlock::Image(image) => McpContentPart {
            kind: "image".to_owned(),
            data: Some(image.data),
            mime_type: Some(image.mime_type),
            ..McpContentPart::default()
        },
        ContentBlock::Audio(audio) => McpContentPart {
            kind: "audio".to_owned(),
            data: Some(audio.data),
            mime_type: Some(audio.mime_type),
            ..McpContentPart::default()
        },
        ContentBlock::Resource(embedded) => {
            let resource = match embedded.resource {
                ResourceContents::TextResourceContents {
                    uri,
                    mime_type,
                    text,
                    ..
                } => McpResourceContents {
                    uri: Some(uri),
                    text: Some(text),
                    mime_type,
                },
                ResourceContents::BlobResourceContents { uri, mime_type, .. } => {
                    McpResourceContents {
                        uri: Some(uri),
                        text: None,
                        mime_type,
                    }
                }
                _ => McpResourceContents::default(),
            };
            McpContentPart {
                kind: "resource".to_owned(),
                resource: Some(resource),
                ..McpContentPart::default()
            }
        }
        ContentBlock::ResourceLink(link) => McpContentPart {
            kind: "resource_link".to_owned(),
            uri: Some(link.uri),
            mime_type: link.mime_type,
            ..McpContentPart::default()
        },
        _ => McpContentPart {
            kind: "unknown".to_owned(),
            ..McpContentPart::default()
        },
    }
}

fn call_result(result: CallToolResult) -> McpCallResult {
    McpCallResult {
        content: result.content.into_iter().map(content_part).collect(),
        is_error: result.is_error,
        structured_content: result.structured_content,
    }
}

fn descriptor(tool: rmcp::model::Tool) -> McpToolDescriptor {
    McpToolDescriptor {
        name: tool.name.into_owned(),
        title: tool.title,
        description: tool.description.map(std::borrow::Cow::into_owned),
        input_schema: Value::Object(tool.input_schema.as_ref().clone()),
        annotations: tool.annotations.map(|hints| ToolAnnotations {
            title: hints.title,
            read_only_hint: hints.read_only_hint,
            destructive_hint: hints.destructive_hint,
            idempotent_hint: hints.idempotent_hint,
            open_world_hint: hints.open_world_hint,
        }),
    }
}

impl SdkConnector {
    /// A connector over the real transports.
    pub fn new(options: SdkConnectorOptions) -> SdkConnector {
        SdkConnector {
            name: options
                .client_name
                .unwrap_or_else(|| CLIENT_NAME.to_owned()),
            version: options
                .client_version
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned()),
            http: options.http,
            pipe: options.pipe,
        }
    }

    fn client_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ClientCapabilities::default();
        info.client_info = Implementation::new(self.name.clone(), self.version.clone());
        info
    }

    /// Builds the transport for one attempt. Fails as `network` for anything
    /// that cannot start, and as `config` for headers that are not headers.
    fn transport(
        &self,
        spec: &McpConnectionSpec,
        context: &McpConnectContext,
        token: Option<&str>,
        warnings: &mut Vec<String>,
    ) -> Result<SessionTransport> {
        if let Some(pipe) = &self.pipe {
            let (read, write) = pipe().map_err(|error| {
                WireError::new(
                    ErrorKind::Network,
                    format!("Could not open the MCP transport: {error}"),
                )
                .with_detail("server", spec.server_id.as_str())
            })?;
            return Ok(SessionTransport::Pipe(AsyncRwTransport::new(read, write)));
        }
        match &spec.transport {
            McpTransportSpec::Stdio { command, args, env } => {
                let mut process = tokio::process::Command::new(command);
                process
                    .args(args)
                    .env_clear()
                    .envs(default_environment())
                    .envs(env.iter());
                let (child, stderr) = TokioChildProcess::builder(process)
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|error| {
                        WireError::new(
                            ErrorKind::Network,
                            format!("Could not start \"{command}\": {error}"),
                        )
                        .with_detail("server", spec.server_id.as_str())
                        .with_source(error)
                    })?;
                if let Some(stderr) = stderr {
                    let sink: ServerLogSink = if let Some(sink) = &context.on_server_log {
                        Arc::clone(sink)
                    } else {
                        let server = spec.server_id.clone();
                        Arc::new(move |line: &str| {
                            tracing::debug!(server = %server, line, "mcp stderr");
                        })
                    };
                    tokio::spawn(pump_stderr(stderr, sink));
                }
                Ok(SessionTransport::Child(child))
            }
            McpTransportSpec::Http {
                kind, url, headers, ..
            } => {
                if *kind == McpTransport::Sse {
                    warnings.push(
                        "the legacy SSE transport is not available in this build; \
                         the server was dialled as Streamable HTTP"
                            .to_owned(),
                    );
                }
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
                if let Some(token) = token {
                    config = config.auth_header(token);
                }
                let mut custom = HashMap::new();
                for (name, value) in headers {
                    let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                        .map_err(|_| bad_header(&spec.server_id, name))?;
                    let value = reqwest::header::HeaderValue::from_str(value)
                        .map_err(|_| bad_header(&spec.server_id, name.as_str()))?;
                    custom.insert(name, value);
                }
                config = config.custom_headers(custom);
                Ok(SessionTransport::Http(
                    StreamableHttpClientTransport::with_client(self.http.clone(), config),
                ))
            }
        }
    }

    /// One client over one transport, connected. Both are discarded on
    /// failure: dropping the transport kills a child or cancels a worker, so
    /// a retry loop leaks neither a subprocess nor a socket per attempt.
    async fn attempt(
        &self,
        spec: &McpConnectionSpec,
        context: &McpConnectContext,
        token: Option<&str>,
    ) -> std::result::Result<Arc<dyn McpSession>, AttemptError> {
        let mut warnings = Vec::new();
        let transport = self
            .transport(spec, context, token, &mut warnings)
            .map_err(AttemptError::Failed)?;
        let (events, _) = broadcast::channel(16);
        let handler = Handler {
            info: self.client_info(),
            events: events.clone(),
        };
        let cancel = context.token.child_token();
        let running = serve_client_with_ct(handler, transport, cancel)
            .await
            .map_err(|error| {
                if error.is_authorization_required() {
                    return AttemptError::NeedsAuthorization(
                        error.auth_challenge().map(str::to_owned),
                    );
                }
                AttemptError::Failed(match error {
                    ClientInitializeError::Cancelled => WireError::aborted("MCP connect"),
                    other => WireError::new(ErrorKind::Network, other.to_string())
                        .with_detail("server", spec.server_id.as_str()),
                })
            })?;

        let peer = running.peer().clone();
        let stopper = running.cancellation_token();
        let info = peer.peer_info().and_then(|info| info.server_info.clone());
        let closing = Arc::new(AtomicBool::new(false));
        {
            let events = events.clone();
            let closing = Arc::clone(&closing);
            tokio::spawn(async move {
                let quit = running.waiting().await;
                if closing.load(Ordering::SeqCst) {
                    return;
                }
                let error = match quit {
                    Ok(QuitReason::Closed | QuitReason::Cancelled) => None,
                    Ok(other) => Some(Arc::new(WireError::new(
                        ErrorKind::Network,
                        format!("The MCP session ended: {other:?}"),
                    ))),
                    Err(join) => Some(Arc::new(WireError::new(
                        ErrorKind::Network,
                        format!("The MCP session task failed: {join}"),
                    ))),
                };
                let _ = events.send(McpSessionEvent::Closed(error));
            });
        }

        Ok(Arc::new(SdkSession {
            server_name: info
                .as_ref()
                .map_or_else(|| spec.server_id.clone(), |info| info.name.clone()),
            server_version: info.map(|info| info.version).unwrap_or_default(),
            peer,
            events,
            closing,
            stopper: parking_lot::Mutex::new(Some(stopper)),
            warnings,
        }))
    }

    async fn dial(
        &self,
        spec: McpConnectionSpec,
        mut context: McpConnectContext,
    ) -> Result<Arc<dyn McpSession>> {
        let url = spec.url().map(str::to_owned);
        let token = match (&context.auth, &url) {
            (Some(auth), Some(url)) => auth.access_token(url).await?,
            _ => None,
        };

        let outcome = match self.attempt(&spec, &context, token.as_deref()).await {
            Ok(session) => Ok(session),
            Err(AttemptError::NeedsAuthorization(challenge)) => {
                let waiter = context.await_authorization_code.take();
                let (Some(auth), Some(wait), Some(url)) = (&context.auth, waiter, &url) else {
                    return Err(needs_authorization(challenge));
                };
                // The flow has reported the link; this resolves when the
                // operator follows it.
                auth.begin_authorization(url).await?;
                let code = wait().await?;
                auth.finish_authorization(url, &code).await?;
                let token = auth.access_token(url).await?;
                self.attempt(&spec, &context, token.as_deref())
                    .await
                    .map_err(AttemptError::into_wire)
            }
            Err(failed) => Err(failed.into_wire()),
        };
        outcome.map_err(|error| note_sse(&spec, error))
    }
}

/// Says why an SSE-only server could not be reached.
///
/// A server configured as `"type": "sse"` is dialled as Streamable HTTP,
/// because this build has no SSE client transport — and a server that only
/// speaks SSE is precisely the one that then fails. Without this the operator
/// reads a bare network error and has no way to connect the two facts; the
/// successful case carries the same sentence as a warning on its status row.
fn note_sse(spec: &McpConnectionSpec, mut error: WireError) -> WireError {
    if spec.kind() == McpTransport::Sse && !error.is_aborted() {
        error.message.push_str(
            " (configured as SSE, which this build cannot speak; \
             the server was dialled as Streamable HTTP)",
        );
    }
    error
}

fn bad_header(server_id: &str, name: &str) -> WireError {
    WireError::new(
        ErrorKind::Config,
        format!("MCP server \"{server_id}\": header \"{name}\" is not a valid HTTP header"),
    )
    .with_detail("server", server_id)
}

impl McpConnector for SdkConnector {
    fn connect(
        &self,
        spec: McpConnectionSpec,
        context: McpConnectContext,
    ) -> BoxFuture<'_, Result<Arc<dyn McpSession>>> {
        Box::pin(self.dial(spec, context))
    }
}

struct SdkSession {
    server_name: String,
    server_version: String,
    peer: Peer<RoleClient>,
    events: broadcast::Sender<McpSessionEvent>,
    closing: Arc<AtomicBool>,
    stopper: parking_lot::Mutex<Option<RunningServiceCancellationToken>>,
    warnings: Vec<String>,
}

impl McpSession for SdkSession {
    fn server_name(&self) -> &str {
        &self.server_name
    }

    fn server_version(&self) -> &str {
        &self.server_version
    }

    fn warnings(&self) -> Vec<String> {
        self.warnings.clone()
    }

    fn list_tools(
        &self,
        token: CancellationToken,
    ) -> BoxFuture<'_, Result<Vec<McpToolDescriptor>>> {
        Box::pin(async move {
            tokio::select! {
                () = token.cancelled() => Err(WireError::aborted("MCP tools/list")),
                listed = self.peer.list_all_tools() => listed
                    .map(|tools| tools.into_iter().map(descriptor).collect())
                    .map_err(service_error),
            }
        })
    }

    fn call_tool(
        &self,
        name: &str,
        args: Object,
        options: McpCallOptions,
    ) -> BoxFuture<'_, Result<McpCallResult>> {
        let mut params = CallToolRequestParams::new(name.to_owned());
        params.arguments = Some(args.into_iter().collect());
        Box::pin(async move {
            if options.timeout_ms == 0 {
                return tokio::select! {
                    () = options.token.cancelled() => Err(WireError::aborted("MCP tools/call")),
                    result = self.peer.call_tool(params) => result.map(call_result).map_err(service_error),
                };
            }
            // A tool reporting progress is a tool that is working. The timeout
            // is for a server that has stopped answering, not for one doing
            // something slow and saying so.
            let request_options =
                PeerRequestOptions::with_timeout(Duration::from_millis(options.timeout_ms))
                    .reset_timeout_on_progress();
            let handle = self
                .peer
                .send_request_with_option(
                    ClientRequest::CallToolRequest(CallToolRequest::new(params)),
                    request_options,
                )
                .await
                .map_err(service_error)?;
            let id = handle.id.clone();
            tokio::select! {
                () = options.token.cancelled() => {
                    let _ = self
                        .peer
                        .notify_cancelled(CancelledNotificationParam::new(
                            Some(id),
                            Some("The turn was cancelled".to_owned()),
                        ))
                        .await;
                    Err(WireError::aborted("MCP tools/call"))
                }
                response = handle.await_response() => match response.map_err(service_error)? {
                    ServerResult::CallToolResult(result) => Ok(call_result(result)),
                    _ => Err(WireError::new(
                        ErrorKind::Network,
                        "The MCP server answered tools/call with something else",
                    )),
                },
            }
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<McpSessionEvent> {
        self.events.subscribe()
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // The flag first: the watcher fires on the SDK's teardown, and a
            // session told to shut down must not read its own close as a drop
            // and arm a reconnect.
            self.closing.store(true, Ordering::SeqCst);
            if let Some(stopper) = self.stopper.lock().take() {
                stopper.cancel();
            }
        })
    }
}
