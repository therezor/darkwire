//! An MCP server that is a plain struct, behind the `testkit` feature.
//!
//! Every test above the connector uses this rather than the SDK, which is the
//! whole point of [`McpSession`] existing: proving that a backoff timer fires
//! on the right cadence should not require a subprocess, and a suite that
//! spawns one is a suite that is slow on a laptop and flaky on a shared runner.
//! `ghostai-runtime`'s tests drive it too, so nothing here depends on a test
//! framework.

use std::sync::Arc;

use futures::future::BoxFuture;
use ghostai_core::{GhostError, Result};
use ghostai_protocol::ToolAnnotations;
use ghostai_protocol::json::Object;
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::session::{
    McpCallOptions, McpCallResult, McpConnectContext, McpConnector, McpSession, McpSessionEvent,
    McpToolDescriptor,
};
use crate::spec::McpConnectionSpec;

/// One `tools/call` the fake received.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeCall {
    /// The upstream tool name.
    pub name: String,
    /// The arguments, after the bridge validated them.
    pub args: Object,
}

/// What `call_tool` answers.
pub type CallHandler = Arc<dyn Fn(&FakeCall) -> Result<McpCallResult> + Send + Sync>;

/// A well-formed descriptor: one string, one required, one integer with a
/// minimum, and a read-only claim.
pub fn echo_tool() -> McpToolDescriptor {
    McpToolDescriptor {
        name: "echo".to_owned(),
        title: None,
        description: Some("Repeats what it is given.".to_owned()),
        input_schema: json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "What to repeat." },
                "times": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "How many times to repeat it."
                }
            },
            "required": ["text"],
            "additionalProperties": false
        }),
        annotations: Some(ToolAnnotations {
            read_only_hint: Some(true),
            ..ToolAnnotations::default()
        }),
    }
}

struct Shared {
    tools: Vec<McpToolDescriptor>,
    calls: Vec<FakeCall>,
    attempts: usize,
    last_context: Option<McpConnectContext>,
    closed: bool,
    one_shot_failure: Option<GhostError>,
    standing_failure: Option<GhostError>,
    handler: CallHandler,
    events: Option<broadcast::Sender<McpSessionEvent>>,
}

/// The fake, shared between the test and the sessions it hands out.
#[derive(Clone)]
pub struct FakeServer {
    shared: Arc<Mutex<Shared>>,
}

impl std::fmt::Debug for FakeServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shared = self.shared.lock();
        f.debug_struct("FakeServer")
            .field("attempts", &shared.attempts)
            .field("closed", &shared.closed)
            .finish_non_exhaustive()
    }
}

/// A `GhostError` cannot be cloned; a failure is replayed by rebuilding it.
fn replay(error: &GhostError) -> GhostError {
    GhostError::new(error.kind, error.message.clone())
        .with_retryable(error.retryable)
        .with_details(error.details.clone())
}

impl Default for FakeServer {
    fn default() -> FakeServer {
        FakeServer::new(vec![echo_tool()])
    }
}

impl FakeServer {
    /// A fake advertising `tools`, echoing every call's arguments as JSON.
    pub fn new(tools: Vec<McpToolDescriptor>) -> FakeServer {
        FakeServer {
            shared: Arc::new(Mutex::new(Shared {
                tools,
                calls: Vec::new(),
                attempts: 0,
                last_context: None,
                closed: false,
                one_shot_failure: None,
                standing_failure: None,
                handler: Arc::new(|call: &FakeCall| {
                    Ok(McpCallResult::text(
                        serde_json::to_string(&call.args).unwrap_or_default(),
                    ))
                }),
                events: None,
            })),
        }
    }

    /// Hand this to a connection or a manager.
    pub fn connector(&self) -> Arc<dyn McpConnector> {
        Arc::new(self.clone())
    }

    /// Every call so far.
    pub fn calls(&self) -> Vec<FakeCall> {
        self.shared.lock().calls.clone()
    }

    /// How many times the connector has been asked for a session.
    pub fn attempts(&self) -> usize {
        self.shared.lock().attempts
    }

    /// Whether the most recent attempt carried an OAuth flow.
    pub fn last_attempt_had_auth(&self) -> bool {
        self.shared
            .lock()
            .last_context
            .as_ref()
            .is_some_and(|context| context.auth.is_some())
    }

    /// Whether the session this server last handed out has been closed.
    pub fn closed(&self) -> bool {
        self.shared.lock().closed
    }

    /// Replaces the advertised list and fires `tools/list_changed`.
    pub fn set_tools(&self, tools: Vec<McpToolDescriptor>) {
        let events = {
            let mut shared = self.shared.lock();
            shared.tools = tools;
            shared.events.clone()
        };
        if let Some(events) = events {
            let _ = events.send(McpSessionEvent::ToolListChanged);
        }
    }

    /// The next `connect` fails with this, then normal service resumes.
    pub fn fail_next_connect(&self, error: GhostError) {
        self.shared.lock().one_shot_failure = Some(error);
    }

    /// Every `connect` fails until [`FakeServer::recover`].
    pub fn fail_connects(&self, error: GhostError) {
        self.shared.lock().standing_failure = Some(error);
    }

    /// Ends a standing or one-shot failure.
    pub fn recover(&self) {
        let mut shared = self.shared.lock();
        shared.standing_failure = None;
        shared.one_shot_failure = None;
    }

    /// Replaces what `call_tool` answers.
    pub fn on_call(&self, handler: CallHandler) {
        self.shared.lock().handler = handler;
    }

    /// Simulates the server going away mid-session.
    pub fn drop_session(&self, error: Option<GhostError>) {
        let events = self.shared.lock().events.clone();
        if let Some(events) = events {
            let _ = events.send(McpSessionEvent::Closed(error.map(Arc::new)));
        }
    }
}

impl McpConnector for FakeServer {
    fn connect(
        &self,
        spec: McpConnectionSpec,
        context: McpConnectContext,
    ) -> BoxFuture<'_, Result<Arc<dyn McpSession>>> {
        Box::pin(async move {
            let (events, _) = broadcast::channel(16);
            let mut shared = self.shared.lock();
            shared.attempts += 1;
            shared.last_context = Some(context);
            let failure = shared
                .one_shot_failure
                .take()
                .or_else(|| shared.standing_failure.as_ref().map(replay));
            if let Some(failure) = failure {
                return Err(failure);
            }
            shared.closed = false;
            shared.events = Some(events.clone());
            let session: Arc<dyn McpSession> = Arc::new(FakeSession {
                server_name: format!("fake-{}", spec.server_id),
                shared: Arc::clone(&self.shared),
                events,
            });
            Ok(session)
        })
    }
}

struct FakeSession {
    server_name: String,
    shared: Arc<Mutex<Shared>>,
    events: broadcast::Sender<McpSessionEvent>,
}

impl McpSession for FakeSession {
    fn server_name(&self) -> &str {
        &self.server_name
    }

    fn server_version(&self) -> &'static str {
        "1.0.0"
    }

    fn list_tools(&self, _: CancellationToken) -> BoxFuture<'_, Result<Vec<McpToolDescriptor>>> {
        Box::pin(async move { Ok(self.shared.lock().tools.clone()) })
    }

    fn call_tool(
        &self,
        name: &str,
        args: Object,
        _: McpCallOptions,
    ) -> BoxFuture<'_, Result<McpCallResult>> {
        let call = FakeCall {
            name: name.to_owned(),
            args,
        };
        Box::pin(async move {
            let handler = {
                let mut shared = self.shared.lock();
                shared.calls.push(call.clone());
                shared.handler.clone()
            };
            handler(&call)
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<McpSessionEvent> {
        self.events.subscribe()
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut shared = self.shared.lock();
            shared.closed = true;
            shared.events = None;
        })
    }
}
