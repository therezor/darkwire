//! Newline-delimited JSON-RPC 2.0 over a pair of streams — MCP's stdio
//! transport, framing and lifecycle verbatim.
//!
//! "Verbatim" is the whole design, and it buys one specific thing: **a plain
//! MCP server is a valid tools-only extension.** A server that has never heard
//! of GhostAI completes the handshake below, answers `tools/list` and
//! `tools/call`, replies `-32601` to every `ghostai/` method, and the host
//! registers its tools and moves on. Nothing had to be written for us.
//!
//! Three decisions that a reader will otherwise have to reverse-engineer:
//!
//!  - **The GhostAI block travels in `params._meta`,** the field MCP reserves
//!    for implementation data. A server that ignores it is not broken, it is
//!    the tools-only case; a server that reads it gets its id, its settings
//!    block, its data directory and the host version without a second round
//!    trip. There is no `ghostai/hello` handshake because there does not need
//!    to be one.
//!  - **Requests go both ways over one connection.** The extension may ask the
//!    host for exactly one thing — its own secret — and may announce channel
//!    traffic. So this is a peer, not a client: the reader dispatches a frame
//!    carrying `method` to the handler and a frame carrying `result`/`error`
//!    to whoever is waiting on that id.
//!  - **A failure is a value, not a panic.** [`RpcFailure`] separates "the peer
//!    answered with an error" from "the peer is gone", because the host acts on
//!    the difference: a `-32601` on a declared kind is a warning on a row, and a
//!    dead transport is a `failed` row and a respawn.
//!
//! Writing is serialised through one task fed by a channel, so several
//! in-flight calls cannot interleave halves of a line on the pipe.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use futures::future::BoxFuture;
use ghostai_core::{ErrorKind, GhostError, Result};
use ghostai_protocol::json::Object;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// The only JSON-RPC version this speaks.
pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC's "method not found".
///
/// The one error code the host reads rather than merely reports: it is how a
/// server says "I do not implement that", which for a *declared* contribution
/// kind is a warning on the extension's row and for an undeclared one is the
/// expected answer.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// The MCP revision the host announces.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// What the host tells an extension about itself, under `params._meta.ghostai`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhostaiInit {
    /// The id, which is also the directory name and every contributed prefix.
    pub extension_id: String,
    /// This extension's block of `config.extensions.settings`, unparsed.
    ///
    /// Unparsed for the reason a channel's block is: the schema cannot know its
    /// shape, so the extension parses it and reports a bad block by refusing to
    /// finish `initialize` — which lands on its row as `failed` with the
    /// message beside it.
    pub settings: Object,
    /// A directory this extension may write to.
    ///
    /// A sibling of its install directory, never a child: the approval covers
    /// every byte under the install, so state written in there would revoke the
    /// extension's own approval on the first write.
    pub data_dir: String,
    /// The host's version, for an extension that wants to refuse an old one.
    pub host_version: String,
}

/// What a server answered `initialize` with.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// The revision the server agreed to. Recorded, never enforced.
    #[serde(default)]
    pub protocol_version: String,
    /// What it says it can do. Advisory; the host probes anyway.
    #[serde(default)]
    pub capabilities: Value,
    /// Its name and version, for the log line and the status row.
    #[serde(default)]
    pub server_info: Value,
}

/// A JSON-RPC error object, as the peer sent it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RpcError {
    /// The JSON-RPC code. [`METHOD_NOT_FOUND`] is the one with meaning here.
    pub code: i64,
    /// The peer's sentence.
    pub message: String,
    /// Whatever else it attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// An error with a code and a sentence.
    pub fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// The answer to a method this side does not implement.
    pub fn method_not_found(method: &str) -> RpcError {
        RpcError::new(METHOD_NOT_FOUND, format!("Method not found: {method}"))
    }
}

/// Why a call did not produce a result.
///
/// Two variants rather than one error type, because the host acts on the
/// difference: a peer that answered is alive and merely declined, and a peer
/// that did not answer is one the row has to report as gone.
#[derive(Debug)]
pub enum RpcFailure {
    /// The peer answered with a JSON-RPC error object.
    Peer(RpcError),
    /// The connection is gone, the call timed out, or it was cancelled.
    Transport(GhostError),
}

impl RpcFailure {
    /// Whether the peer said it does not implement the method.
    pub fn is_method_not_found(&self) -> bool {
        matches!(self, RpcFailure::Peer(error) if error.code == METHOD_NOT_FOUND)
    }

    /// The sentence to put on a row.
    pub fn message(&self) -> String {
        match self {
            RpcFailure::Peer(error) => error.message.clone(),
            RpcFailure::Transport(error) => error.message.clone(),
        }
    }
}

impl From<RpcFailure> for GhostError {
    fn from(failure: RpcFailure) -> GhostError {
        match failure {
            RpcFailure::Peer(error) => {
                GhostError::new(ErrorKind::Extension, error.message).with_detail("code", error.code)
            }
            RpcFailure::Transport(error) => error,
        }
    }
}

/// What an extension may ask the *host* for.
///
/// Deliberately tiny, and the shape is the argument: an extension can request
/// exactly one thing (its own secret) and announce exactly two (a channel
/// message, a channel control frame). Everything else the host offers is
/// reached by being *asked*, not by asking. A wider trait here would be a
/// second capability surface with no approval screen in front of it.
pub trait RpcHandler: Send + Sync {
    /// A request the extension expects an answer to.
    fn request(
        &self,
        method: String,
        params: Value,
    ) -> BoxFuture<'_, std::result::Result<Value, RpcError>>;

    /// A notification. Nothing is sent back, including on failure.
    fn notify(&self, method: String, params: Value);
}

/// A handler that answers nothing, for a connection with no host side.
///
/// Used by the framing tests and by any caller that only drives an extension
/// one way. Every request gets `-32601`, which is what the wire says when a
/// method is absent — never silence, which would hang the peer.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHostMethods;

impl RpcHandler for NoHostMethods {
    fn request(
        &self,
        method: String,
        _params: Value,
    ) -> BoxFuture<'_, std::result::Result<Value, RpcError>> {
        Box::pin(async move { Err(RpcError::method_not_found(&method)) })
    }

    fn notify(&self, _method: String, _params: Value) {}
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<std::result::Result<Value, RpcError>>>>>;

/// One live connection to an extension process.
///
/// Cloneable and shared: the bridged tools, the context contributor and the
/// command runner all hold one, and every call multiplexes over the same pipe.
pub struct RpcClient {
    outbound: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: AtomicI64,
    token: CancellationToken,
}

impl std::fmt::Debug for RpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcClient")
            .field("in_flight", &self.pending.lock().len())
            .field("closed", &self.token.is_cancelled())
            .finish_non_exhaustive()
    }
}

fn transport_gone() -> RpcFailure {
    RpcFailure::Transport(GhostError::new(
        ErrorKind::Extension,
        "The extension process is not answering; its connection is closed.",
    ))
}

impl RpcClient {
    /// Starts the reader and writer tasks over one pair of streams.
    ///
    /// Generic over the streams rather than taking a child process, so the
    /// framing is testable over `tokio::io::duplex` with nothing spawned. Both
    /// tasks end when `token` fires or either stream closes, and cancelling the
    /// token is what [`close`](Self::close) does.
    pub fn start<R, W>(
        reader: R,
        writer: W,
        handler: Arc<dyn RpcHandler>,
        token: &CancellationToken,
    ) -> Arc<RpcClient>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (outbound, mut outbox) = mpsc::unbounded_channel::<String>();
        let client = Arc::new(RpcClient {
            outbound,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicI64::new(1),
            token: token.clone(),
        });

        let writing = token.clone();
        tokio::spawn(async move {
            let mut writer = writer;
            loop {
                let line = tokio::select! {
                    () = writing.cancelled() => break,
                    line = outbox.recv() => match line {
                        Some(line) => line,
                        None => break,
                    },
                };
                if writer.write_all(line.as_bytes()).await.is_err() || writer.flush().await.is_err()
                {
                    break;
                }
            }
            // Closing stdin is how the host asks a well-behaved child to stop,
            // so the writer half is dropped here rather than held open.
            let _ = writer.shutdown().await;
        });

        let reading = Arc::clone(&client);
        let read_token = token.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            loop {
                let line = tokio::select! {
                    () = read_token.cancelled() => break,
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => line,
                        Ok(None) | Err(_) => break,
                    },
                };
                reading.dispatch(&line, &handler);
            }
            // The peer is gone. Cancelling first is what makes a *later* call
            // fail at once rather than join a queue nothing will ever answer:
            // `fail_everything` only reaches the calls already waiting.
            read_token.cancel();
            reading.fail_everything();
        });

        client
    }

    /// Routes one inbound line: a response, a request, or a notification.
    ///
    /// A line that is not JSON is dropped. That is deliberate rather than
    /// lenient: a child that printed a banner to stdout has broken the wire,
    /// and taking the connection down for it would turn a cosmetic bug into an
    /// unloadable extension. What it cannot do is be mistaken for a frame.
    fn dispatch(self: &Arc<Self>, line: &str, handler: &Arc<dyn RpcHandler>) {
        let Ok(Value::Object(frame)) = serde_json::from_str::<Value>(line) else {
            tracing::debug!(target: "extension", line, "ignoring a line that is not a JSON-RPC frame");
            return;
        };

        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = frame.get("id").and_then(Value::as_i64);
        let params = frame.get("params").cloned().unwrap_or(Value::Null);

        match (method, id) {
            (Some(method), Some(id)) => {
                let handler = Arc::clone(handler);
                let client = Arc::clone(self);
                tokio::spawn(async move {
                    let answer = handler.request(method, params).await;
                    client.respond(id, answer);
                });
            }
            (Some(method), None) => handler.notify(method, params),
            (None, Some(id)) => {
                let waiting = self.pending.lock().remove(&id);
                if let Some(waiting) = waiting {
                    let answer = match frame.get("error") {
                        Some(error) => Err(serde_json::from_value(error.clone())
                            .unwrap_or_else(|_| RpcError::new(0, error.to_string()))),
                        None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = waiting.send(answer);
                }
            }
            (None, None) => {}
        }
    }

    fn respond(&self, id: i64, answer: std::result::Result<Value, RpcError>) {
        let frame = match answer {
            Ok(result) => json!({"jsonrpc": JSONRPC_VERSION, "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": JSONRPC_VERSION, "id": id, "error": error}),
        };
        self.send(&frame);
    }

    fn fail_everything(&self) {
        let waiting: Vec<_> = self.pending.lock().drain().map(|(_, tx)| tx).collect();
        for tx in waiting {
            let _ = tx.send(Err(RpcError::new(
                0,
                "The extension process is not answering; its connection is closed.",
            )));
        }
    }

    fn send(&self, frame: &Value) -> bool {
        match serde_json::to_string(frame) {
            Ok(text) => self.outbound.send(format!("{text}\n")).is_ok(),
            Err(_) => false,
        }
    }

    /// Sends a notification. Nothing comes back, including a failure.
    pub fn notify(&self, method: &str, params: Value) {
        // Built by hand rather than through `json!` so that `params` is moved
        // into the frame rather than serialised again on the way past.
        let mut frame = serde_json::Map::new();
        frame.insert("jsonrpc".to_owned(), Value::from(JSONRPC_VERSION));
        frame.insert("method".to_owned(), Value::from(method));
        frame.insert("params".to_owned(), params);
        self.send(&Value::Object(frame));
    }

    /// One request, answered or failed. Waits as long as the peer takes.
    ///
    /// Every caller in this crate wraps it in a cap — the handshake's ten
    /// seconds, a runtime context section's one — so a bound does not belong
    /// here, where it would be a second one to reason about.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, RpcFailure> {
        if self.is_closed() {
            return Err(transport_gone());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);

        let frame =
            json!({"jsonrpc": JSONRPC_VERSION, "id": id, "method": method, "params": params});
        if !self.send(&frame) {
            self.pending.lock().remove(&id);
            return Err(transport_gone());
        }

        match rx.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(RpcFailure::Peer(error)),
            Err(_) => Err(transport_gone()),
        }
    }

    /// A request that ends when `token` fires, with MCP's own cancellation.
    ///
    /// The notification is best-effort by design — MCP says a cancelled request
    /// may still be answered, and a peer that ignores it is not misbehaving. So
    /// the host stops waiting either way; the notification is what lets a
    /// well-behaved extension stop *working*, which is the part that costs
    /// money on a slow command.
    pub async fn request_cancellable(
        &self,
        method: &str,
        params: Value,
        token: &CancellationToken,
    ) -> std::result::Result<Value, RpcFailure> {
        if self.is_closed() {
            return Err(transport_gone());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);

        let frame =
            json!({"jsonrpc": JSONRPC_VERSION, "id": id, "method": method, "params": params});
        if !self.send(&frame) {
            self.pending.lock().remove(&id);
            return Err(transport_gone());
        }

        tokio::select! {
            answer = rx => match answer {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(error)) => Err(RpcFailure::Peer(error)),
                Err(_) => Err(transport_gone()),
            },
            () = token.cancelled() => {
                self.pending.lock().remove(&id);
                self.notify(
                    "notifications/cancelled",
                    json!({"requestId": id, "reason": "the host cancelled the request"}),
                );
                Err(RpcFailure::Transport(GhostError::new(
                    ErrorKind::Aborted,
                    "The call was cancelled.",
                )))
            }
        }
    }

    /// The MCP handshake, plus the GhostAI block under `params._meta`.
    ///
    /// `notifications/initialized` follows the reply, as MCP requires, and the
    /// two together are the whole of the lifecycle: there is no `ghostai/hello`
    /// and nothing else to complete before the host may call a method.
    pub async fn initialize(&self, init: &GhostaiInit) -> Result<InitializeResult> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": ghostai_mcp::CLIENT_NAME, "version": init.host_version},
            "_meta": {"ghostai": init},
        });
        let result = self
            .request("initialize", params)
            .await
            .map_err(|failure| {
                GhostError::from(failure).with_detail("extension", init.extension_id.as_str())
            })?;
        let parsed: InitializeResult = serde_json::from_value(result).unwrap_or_default();
        self.notify("notifications/initialized", json!({}));
        Ok(parsed)
    }

    /// Stops both tasks and fails everything still waiting.
    pub fn close(&self) {
        self.token.cancel();
        self.fail_everything();
    }

    /// Whether the connection has been closed from either end.
    pub fn is_closed(&self) -> bool {
        self.token.is_cancelled() || self.outbound.is_closed()
    }
}
