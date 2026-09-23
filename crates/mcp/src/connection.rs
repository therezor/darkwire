//! One MCP server: its session, its tools, and what to do when it goes away.
//!
//! The contract that shapes everything here is that **nothing this type does
//! can fail a caller.** `start` returns nothing, `reconcile` on the manager is
//! synchronous, and the composition root calls into it from the region past
//! which nothing fails. A server that is unreachable is a state to report, not
//! an error to raise — the same stance an unconfigured provider already has,
//! and for the same reason: an operator editing one server's URL must not lose
//! the save because of it.
//!
//! Reconnection is full-jitter exponential backoff on tokio's timer, which a
//! test pauses, so it asserts the cadence instead of waiting for it. There is
//! no attempt cap: a laptop that was asleep for an hour has to come back
//! without an operator, and a cap is a quiet decision to stop trying. What is
//! capped is the *log*, at one warning per outage.
//!
//! The one state that does not retry is `needs_authorization`. Looping on a
//! redirect nobody is going to follow spends the authorization server's rate
//! limit to reach the same answer.

use std::sync::Arc;
use std::time::Duration;

use darkwire_core::{Clock, ErrorKind, Result, WireError};
use darkwire_protocol::{McpServerState, McpServerStatus};
use darkwire_security::RandomSource;
use darkwire_tools::AnyTool;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::bridge::{BridgeOptions, McpCallTarget, bridge_tool};
use crate::callback::AuthorizationHandle;
use crate::filter::select_tools;
use crate::names::{MCP_TOOL_PREFIX, flatten_tool_names};
use crate::oauth::OAuthFlow;
use crate::session::{
    McpCallOptions, McpCallResult, McpConnectContext, McpConnector, McpSession, McpSessionEvent,
    McpToolDescriptor,
};
use crate::spec::{McpConnectionSpec, exposure_fingerprint, transport_fingerprint};

/// How long `tools/list` may take before the attempt counts as failed. A
/// server that accepts the handshake and then never lists would otherwise hold
/// the connection in `connecting` for good.
pub const LIST_TOOLS_TIMEOUT: Duration = Duration::from_secs(30);

/// The jitter function: a sample in `[0, ceiling]` for a ceiling.
pub type Jitter = Arc<dyn Fn(f64) -> f64 + Send + Sync>;

/// How reconnection waits grow.
#[derive(Clone)]
pub struct BackoffOptions {
    /// The first ceiling, in milliseconds.
    pub initial_ms: u64,
    /// How much the ceiling grows per attempt.
    pub factor: f64,
    /// The ceiling's ceiling.
    pub max_ms: u64,
    /// Injected so a test gets a cadence rather than a distribution. `None`
    /// samples the injected CSPRNG.
    pub jitter: Option<Jitter>,
}

impl std::fmt::Debug for BackoffOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackoffOptions")
            .field("initial_ms", &self.initial_ms)
            .field("factor", &self.factor)
            .field("max_ms", &self.max_ms)
            .field("jitter", &self.jitter.is_some())
            .finish()
    }
}

impl Default for BackoffOptions {
    fn default() -> BackoffOptions {
        BackoffOptions {
            initial_ms: 1_000,
            factor: 2.0,
            max_ms: 60_000,
            jitter: None,
        }
    }
}

/// Where an authorization link is asked for, and where the code comes back.
pub struct AuthorizationAttempt {
    /// The flow the connector drives.
    pub auth: Arc<OAuthFlow>,
    /// The callback half: `state`, redirect URL, and the code.
    pub handle: Arc<AuthorizationHandle>,
}

/// Prepares one authorization attempt. Resolves before the connection is
/// dialled, so the flow it produces already knows its redirect URL and
/// `state`.
pub trait AuthorizationBroker: Send + Sync {
    /// One attempt for `server_id`.
    fn begin(&self, server_id: &str) -> BoxFuture<'_, Result<AuthorizationAttempt>>;
}

/// Called whenever this server's contribution to the registry changes.
pub type PublishFn = Arc<dyn Fn(&str, Vec<AnyTool>) + Send + Sync>;

/// Called whenever `status` would answer differently.
pub type StatusChangedFn = Arc<dyn Fn() + Send + Sync>;

/// Everything one connection needs.
pub struct McpConnectionOptions {
    /// The resolved entry.
    pub spec: McpConnectionSpec,
    /// Opens sessions.
    pub connect: Arc<dyn McpConnector>,
    /// Where tools go.
    pub publish: PublishFn,
    /// Status observer.
    pub on_status_changed: Option<StatusChangedFn>,
    /// Only consulted for an HTTP server with an `oauth` block.
    pub authorization: Option<Arc<dyn AuthorizationBroker>>,
    /// Stamps `last_connected_at_ms`.
    pub clock: Arc<dyn Clock>,
    /// Jitters the backoff when no `jitter` is injected.
    pub random: Arc<dyn RandomSource>,
    /// The backoff shape.
    pub backoff: BackoffOptions,
}

struct Inner {
    spec: McpConnectionSpec,
    state: McpServerState,
    session: Option<Arc<dyn McpSession>>,
    descriptors: Vec<McpToolDescriptor>,
    tools: Vec<AnyTool>,
    warnings: Vec<String>,
    filtered: Vec<String>,
    last_error: Option<String>,
    authorization_url: Option<String>,
    last_connected_at_ms: Option<i64>,
    attempts: u32,
    timer: Option<JoinHandle<()>>,
    watcher: Option<JoinHandle<()>>,
    attempt_token: Option<CancellationToken>,
    closed: bool,
    /// Bumped on every (re)connect so a slow attempt cannot publish over a new one.
    generation: u64,
    warned_this_outage: bool,
}

struct Shared {
    connect: Arc<dyn McpConnector>,
    publish: PublishFn,
    on_status_changed: Option<StatusChangedFn>,
    authorization: Option<Arc<dyn AuthorizationBroker>>,
    clock: Arc<dyn Clock>,
    random: Arc<dyn RandomSource>,
    backoff: BackoffOptions,
    inner: Mutex<Inner>,
}

/// A bridged tool's route to whichever session the connection holds now.
struct ConnectionTarget {
    shared: Arc<Shared>,
}

impl McpCallTarget for ConnectionTarget {
    fn call(
        &self,
        upstream_name: &str,
        args: darkwire_protocol::json::Object,
        options: McpCallOptions,
    ) -> BoxFuture<'_, Result<McpCallResult>> {
        let session = self.shared.inner.lock().session.clone();
        let server_id = self.shared.inner.lock().spec.server_id.clone();
        let upstream_name = upstream_name.to_owned();
        Box::pin(async move {
            let Some(session) = session else {
                return Err(WireError::new(
                    ErrorKind::Network,
                    format!("The {server_id} MCP server is not connected"),
                ));
            };
            session.call_tool(&upstream_name, args, options).await
        })
    }
}

/// One server's connection.
#[derive(Clone)]
pub struct McpConnection {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for McpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.shared.inner.lock();
        f.debug_struct("McpConnection")
            .field("server_id", &inner.spec.server_id)
            .field("state", &inner.state)
            .finish_non_exhaustive()
    }
}

impl McpConnection {
    /// A connection that has not dialled yet.
    pub fn new(options: McpConnectionOptions) -> McpConnection {
        McpConnection {
            shared: Arc::new(Shared {
                connect: options.connect,
                publish: options.publish,
                on_status_changed: options.on_status_changed,
                authorization: options.authorization,
                clock: options.clock,
                random: options.random,
                backoff: options.backoff,
                inner: Mutex::new(Inner {
                    spec: options.spec,
                    state: McpServerState::Connecting,
                    session: None,
                    descriptors: Vec::new(),
                    tools: Vec::new(),
                    warnings: Vec::new(),
                    filtered: Vec::new(),
                    last_error: None,
                    authorization_url: None,
                    last_connected_at_ms: None,
                    attempts: 0,
                    timer: None,
                    watcher: None,
                    attempt_token: None,
                    closed: false,
                    generation: 0,
                    warned_this_outage: false,
                }),
            }),
        }
    }

    /// The config key.
    pub fn server_id(&self) -> String {
        self.shared.inner.lock().spec.server_id.clone()
    }

    /// The spec this connection currently applies.
    pub fn spec(&self) -> McpConnectionSpec {
        self.shared.inner.lock().spec.clone()
    }

    /// Where it is.
    pub fn state(&self) -> McpServerState {
        self.shared.inner.lock().state
    }

    /// The tools currently published.
    pub fn tools(&self) -> Vec<AnyTool> {
        self.shared.inner.lock().tools.clone()
    }

    /// The status row.
    pub fn status(&self) -> McpServerStatus {
        let inner = self.shared.inner.lock();
        let mut tools: Vec<String> = inner
            .tools
            .iter()
            .map(|tool| tool.definition().name.clone())
            .collect();
        tools.sort();
        McpServerStatus {
            id: inner.spec.server_id.clone(),
            transport: Some(inner.spec.kind()),
            state: inner.state,
            enabled: true,
            tools,
            filtered_tools: inner.filtered.clone(),
            server_name: inner
                .session
                .as_ref()
                .map(|session| session.server_name().to_owned())
                .unwrap_or_default(),
            server_version: inner
                .session
                .as_ref()
                .map(|session| session.server_version().to_owned())
                .unwrap_or_default(),
            last_error: inner.last_error.clone(),
            last_connected_at_ms: inner
                .last_connected_at_ms
                .and_then(|ms| u64::try_from(ms).ok()),
            authorization_url: inner.authorization_url.clone(),
            warnings: inner.warnings.clone(),
        }
    }

    /// Begins connecting. Fire-and-forget by contract; never fails.
    ///
    /// Must be called from inside a Tokio runtime: the work it cannot fail on
    /// is done on a task it spawns.
    pub fn start(&self) {
        {
            let mut inner = self.shared.inner.lock();
            if inner.closed {
                return;
            }
            inner.attempts = 0;
            inner.warned_this_outage = false;
        }
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move { dial(shared).await });
    }

    /// Applies a spec that changed only in what it exposes.
    ///
    /// The whole reason [`crate::spec`] keeps two fingerprints: narrowing
    /// `enabledTools` is the edit an operator makes most, and killing a
    /// subprocess to re-filter a list already in memory would be a visible
    /// stall for no reason.
    pub fn rebridge(&self, spec: McpConnectionSpec) -> Result<()> {
        let ready = {
            let mut inner = self.shared.inner.lock();
            if transport_fingerprint(&spec) != transport_fingerprint(&inner.spec) {
                return Err(WireError::new(
                    ErrorKind::Internal,
                    "rebridge was given a spec that needs a new connection",
                ));
            }
            if exposure_fingerprint(&spec) == exposure_fingerprint(&inner.spec) {
                return Ok(());
            }
            inner.spec = spec;
            inner.state == McpServerState::Ready
        };
        if ready {
            republish(&self.shared);
        }
        Ok(())
    }

    /// Records the link an operator has to follow.
    pub fn report_authorization_url(&self, url: &str) {
        {
            let mut inner = self.shared.inner.lock();
            inner.authorization_url = Some(url.to_owned());
            inner.state = McpServerState::NeedsAuthorization;
        }
        changed(&self.shared);
    }

    /// Tears the connection down. The tools go at once; the session's own
    /// close may take longer and its failure is nobody's to act on.
    pub async fn close(&self) {
        let (server_id, session) = {
            let mut inner = self.shared.inner.lock();
            inner.closed = true;
            inner.generation += 1;
            if let Some(timer) = inner.timer.take() {
                timer.abort();
            }
            if let Some(watcher) = inner.watcher.take() {
                watcher.abort();
            }
            if let Some(token) = inner.attempt_token.take() {
                token.cancel();
            }
            inner.state = McpServerState::Disabled;
            inner.tools = Vec::new();
            (inner.spec.server_id.clone(), inner.session.take())
        };
        (self.shared.publish)(&server_id, Vec::new());
        if let Some(session) = session {
            session.close().await;
        }
    }
}

fn changed(shared: &Shared) {
    if let Some(listener) = &shared.on_status_changed {
        listener();
    }
}

fn stale(shared: &Shared, generation: u64) -> bool {
    let inner = shared.inner.lock();
    inner.generation != generation || inner.closed
}

async fn dial(shared: Arc<Shared>) {
    let (generation, token, spec) = {
        let mut inner = shared.inner.lock();
        if inner.closed {
            return;
        }
        inner.generation += 1;
        inner.state = McpServerState::Connecting;
        inner.authorization_url = None;
        let token = CancellationToken::new();
        inner.attempt_token = Some(token.clone());
        (inner.generation, token, inner.spec.clone())
    };
    changed(&shared);

    // Only for a server that both wants OAuth and has somewhere to ask.
    let broker = if spec.oauth().is_some() {
        shared.authorization.clone()
    } else {
        None
    };
    let pending = match broker {
        Some(broker) => match broker.begin(&spec.server_id).await {
            Ok(attempt) => Some(attempt),
            Err(error) => {
                if !stale(&shared, generation) {
                    fail(&shared, &error);
                }
                return;
            }
        },
        None => None,
    };

    let context = McpConnectContext {
        token,
        auth: pending.as_ref().map(|attempt| Arc::clone(&attempt.auth)),
        await_authorization_code: pending.as_ref().map(|attempt| {
            let handle = Arc::clone(&attempt.handle);
            Box::new(move || handle.code()) as crate::session::AuthorizationCodeWaiter
        }),
        on_server_log: None,
    };

    let outcome = shared.connect.connect(spec, context).await;
    // Either way this attempt is over, and an authorization nobody is going to
    // answer must not keep the loopback port bound until its timeout.
    if let Some(attempt) = pending {
        attempt.handle.cancel("The connection attempt ended").await;
    }

    match outcome {
        Ok(session) => {
            if stale(&shared, generation) {
                session.close().await;
                return;
            }
            adopt(&shared, session, generation).await;
        }
        Err(error) => {
            if stale(&shared, generation) {
                return;
            }
            fail(&shared, &error);
        }
    }
}

async fn adopt(shared: &Arc<Shared>, session: Arc<dyn McpSession>, generation: u64) {
    let mut events = session.subscribe();
    {
        let mut inner = shared.inner.lock();
        inner.session = Some(Arc::clone(&session));
        inner.last_error = None;
        inner.authorization_url = None;
        inner.last_connected_at_ms = Some(shared.clock.now_ms());
        let watcher = {
            let shared = Arc::clone(shared);
            tokio::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(McpSessionEvent::ToolListChanged) => {
                            if stale(&shared, generation) {
                                return;
                            }
                            refresh(&shared, generation).await;
                        }
                        Ok(McpSessionEvent::Closed(error)) => {
                            if stale(&shared, generation) {
                                return;
                            }
                            // A drop is not an authorization problem even when
                            // it follows one, and the message an operator gets
                            // should say which happened.
                            let error = error.map_or_else(
                                || {
                                    WireError::new(
                                        ErrorKind::Network,
                                        "The MCP server closed the connection",
                                    )
                                },
                                |shared_error| {
                                    WireError::new(shared_error.kind, shared_error.message.clone())
                                        .with_details(shared_error.details.clone())
                                },
                            );
                            fail(&shared, &error);
                            return;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    }
                }
            })
        };
        if let Some(previous) = inner.watcher.replace(watcher) {
            previous.abort();
        }
    }
    refresh(shared, generation).await;
}

/// Re-reads the server's tool list and republishes.
async fn refresh(shared: &Arc<Shared>, generation: u64) {
    let (session, token) = {
        let inner = shared.inner.lock();
        (inner.session.clone(), inner.attempt_token.clone())
    };
    let Some(session) = session else {
        return;
    };
    let token = token.unwrap_or_else(|| {
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        cancelled
    });
    let listed = tokio::time::timeout(LIST_TOOLS_TIMEOUT, session.list_tools(token))
        .await
        .unwrap_or_else(|_| {
            Err(WireError::new(
                ErrorKind::Timeout,
                format!(
                    "The MCP server did not list its tools within {} s",
                    LIST_TOOLS_TIMEOUT.as_secs()
                ),
            ))
        });
    match listed {
        Ok(descriptors) => {
            if stale(shared, generation) {
                return;
            }
            {
                let mut inner = shared.inner.lock();
                inner.descriptors = descriptors;
                inner.state = McpServerState::Ready;
                // Only a server that listed its tools has recovered. Resetting
                // at the handshake would redial one that refuses tools/list
                // every second, forever.
                inner.attempts = 0;
                inner.warned_this_outage = false;
            }
            republish(shared);
        }
        Err(error) => {
            if !stale(shared, generation) {
                fail(shared, &error);
            }
        }
    }
}

/// Filters, flattens and bridges the descriptors this connection holds.
fn republish(shared: &Arc<Shared>) {
    let (server_id, tools) = {
        let mut inner = shared.inner.lock();
        let spec = inner.spec.clone();
        let mut warnings = inner
            .session
            .as_ref()
            .map(|session| session.warnings())
            .unwrap_or_default();

        let selection = select_tools(&inner.descriptors, &spec.enabled_tools);
        inner.filtered = inner
            .descriptors
            .iter()
            .filter(|descriptor| !selection.selected.contains(descriptor))
            .map(|descriptor| descriptor.name.clone())
            .collect();
        for pattern in &selection.unmatched {
            warnings.push(format!(
                "\"{pattern}\" in enabledTools matches no tool this server offers"
            ));
        }

        let upstream: Vec<String> = selection
            .selected
            .iter()
            .map(|descriptor| descriptor.name.clone())
            .collect();
        let flattened = flatten_tool_names(MCP_TOOL_PREFIX, &spec.server_id, &upstream);
        for collision in &flattened.collisions {
            warnings.push(format!(
                "\"{collision}\" was renamed: another tool on this server flattens to the same name"
            ));
        }

        let mut tools: Vec<AnyTool> = Vec::new();
        for descriptor in &selection.selected {
            let Some(advertised) = flattened.names.get(&descriptor.name) else {
                continue;
            };
            let target: Arc<dyn McpCallTarget> = Arc::new(ConnectionTarget {
                shared: Arc::clone(shared),
            });
            let bridged = bridge_tool(
                descriptor,
                target,
                BridgeOptions::new(MCP_TOOL_PREFIX, &spec.server_id, descriptor)
                    .advertised_as(advertised.clone())
                    .timeout_ms(spec.tool_timeout_ms),
            );
            for issue in &bridged.issues {
                warnings.push(format!("{}: {}", issue.tool, issue.message));
            }
            if let Some(tool) = bridged.tool {
                tools.push(tool);
            }
        }

        inner.warnings = warnings;
        inner.tools.clone_from(&tools);
        (spec.server_id, tools)
    };
    (shared.publish)(&server_id, tools);
    changed(shared);
}

/// Records why this server is down, lets go of its session, and arms the next
/// attempt.
fn fail(shared: &Arc<Shared>, error: &WireError) {
    let needs_auth = error.details.get("needsAuthorization") == Some(&Value::Bool(true));
    let (server_id, session) = {
        let mut inner = shared.inner.lock();
        inner.descriptors = Vec::new();
        inner.tools = Vec::new();
        // The watcher belongs to the session being dropped. Left running, it
        // would read that session's teardown as a second failure.
        if let Some(watcher) = inner.watcher.take() {
            watcher.abort();
        }
        // The session's service runs on a child of this token, so cancelling
        // it is what ends a stdio child even if close() never gets to run.
        if let Some(token) = inner.attempt_token.take() {
            token.cancel();
        }
        let session = inner.session.take();
        inner.last_error = Some(error.message.clone());
        inner.state = if needs_auth {
            McpServerState::NeedsAuthorization
        } else {
            McpServerState::Failed
        };
        // One warning per outage. A server that has been unreachable since a
        // laptop closed would otherwise write a line a second forever, and the
        // second line says nothing the first did not.
        if inner.warned_this_outage {
            tracing::debug!(server = %inner.spec.server_id, error = %error.message, "mcp server still unavailable");
        } else {
            inner.warned_this_outage = true;
            tracing::warn!(server = %inner.spec.server_id, error = %error.message, "mcp server unavailable");
        }
        (inner.spec.server_id.clone(), session)
    };
    if let Some(session) = session {
        tokio::spawn(async move { session.close().await });
    }
    (shared.publish)(&server_id, Vec::new());
    changed(shared);
    if !needs_auth {
        arm(shared);
    }
}

/// Full jitter, from the injected CSPRNG unless a test supplied a cadence.
///
/// The thread-local generator is banned repo-wide because it makes a test's
/// outcome depend on the run, and the rule is right even where — as here — the
/// value is a politeness rather than a secret: a dozen servers behind one flaky
/// network must not retry in lockstep.
fn jittered(shared: &Shared, ceiling: f64) -> f64 {
    if let Some(jitter) = &shared.backoff.jitter {
        jitter(ceiling)
    } else {
        let mut bytes = [0u8; 4];
        shared.random.fill(&mut bytes);
        f64::from(u32::from_be_bytes(bytes)) / f64::from(u32::MAX) * ceiling
    }
}

fn arm(shared: &Arc<Shared>) {
    let delay = {
        let mut inner = shared.inner.lock();
        if inner.closed {
            return;
        }
        if let Some(timer) = inner.timer.take() {
            timer.abort();
        }
        #[allow(clippy::cast_precision_loss)] // milliseconds never approach 2^52
        let ceiling = (shared.backoff.initial_ms as f64
            * shared
                .backoff
                .factor
                .powi(i32::try_from(inner.attempts).unwrap_or(i32::MAX)))
        .min(shared.backoff.max_ms as f64);
        inner.attempts = inner.attempts.saturating_add(1);
        let sampled = jittered(shared, ceiling).round().max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped above
        let delay = Duration::from_millis(sampled as u64);
        let shared_for_timer = Arc::clone(shared);
        inner.timer = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            shared_for_timer.inner.lock().timer = None;
            dial(shared_for_timer).await;
        }));
        delay
    };
    tracing::debug!(delay_ms = delay.as_millis(), "mcp reconnect armed");
}
