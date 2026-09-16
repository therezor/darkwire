//! The loopback listener an OAuth redirect lands on.
//!
//! **Why a listener of our own rather than a route on the DarkWire server.**
//! Three reasons, and any one of them is sufficient. This crate sits below
//! `darkwire-server` in the layer graph and may not reach it. `darkwire chat` in
//! a terminal has no HTTP server at all and still has to be able to authorize
//! a server. And the server's public route list is three entries long on
//! purpose — an OAuth redirect cannot carry the session cookie, because it
//! arrives as a cross-site navigation and the cookie is `SameSite=Strict`, so a
//! route would have to be unauthenticated. A loopback bind is what every
//! desktop OAuth client does, and it is reachable only from this machine.
//!
//! The `state` parameter is the authentication. It is minted per attempt from
//! the same CSPRNG the tool-output nonce uses, checked against what was stored,
//! and consumed once — which is exactly the job `state` exists for in the OAuth
//! spec, rather than a mechanism invented here.
//!
//! One listener serves every server, and it stays up only while at least one
//! authorization is outstanding.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_security::RandomSource;
use futures::future::BoxFuture;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Tried first so the redirect URI is stable across attempts.
///
/// An authorization server that requires an exact pre-registered redirect URI —
/// which is most of them, for a confidential client — cannot work with an
/// ephemeral port. Falling back to one is still better than failing outright:
/// dynamic registration, which is the common case for MCP, registers whatever
/// this reports.
pub const DEFAULT_CALLBACK_PORT: u16 = 33_418;

/// The path the redirect lands on.
pub const CALLBACK_PATH: &str = "/mcp/callback";

/// Twelve bytes of CSPRNG, hex. Long enough that guessing is not a strategy.
const STATE_BYTES: usize = 12;

/// `0` in the schema means "no limit", and an authorization that can never
/// expire holds a listener open forever; this is the bound it gets instead.
const DEFAULT_AUTHORIZATION_TIMEOUT: Duration = Duration::from_mins(5);

/// How long a stopping listener waits for the redirect's response to finish
/// before the port is closed underneath it.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

fn page(message: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>DarkWire</title>\
         <body style=\"font:16px system-ui;padding:3rem;max-width:32rem;margin:auto\">\
         <p>{message}</p></body>"
    )
}

/// One outstanding authorization.
struct Pending {
    server_id: String,
    settle: oneshot::Sender<Result<String>>,
    timer: JoinHandle<()>,
}

/// The bound socket and the task serving it.
struct Running {
    redirect_base: String,
    shutdown: CancellationToken,
    serving: JoinHandle<()>,
}

#[derive(Default)]
struct ListenerState {
    running: Option<Running>,
    pending: HashMap<String, Pending>,
}

struct Inner {
    random: Arc<dyn RandomSource>,
    port: u16,
    state: Mutex<ListenerState>,
}

/// How the listener is built.
pub struct CallbackListenerOptions {
    /// Where `state` comes from.
    pub random: Arc<dyn RandomSource>,
    /// `Some(0)` asks the OS for any free port. Defaults to
    /// [`DEFAULT_CALLBACK_PORT`].
    pub port: Option<u16>,
}

/// One authorization's half of the exchange.
pub struct AuthorizationHandle {
    /// The `state` the provider must send and the redirect must carry back.
    pub state: String,
    /// Where a provider should tell the authorization server to send the user.
    pub redirect_url: String,
    code: std::sync::Mutex<Option<oneshot::Receiver<Result<String>>>>,
    listener: Arc<Inner>,
}

impl std::fmt::Debug for AuthorizationHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizationHandle")
            .field("state", &self.state)
            .field("redirect_url", &self.redirect_url)
            .finish_non_exhaustive()
    }
}

impl AuthorizationHandle {
    /// Resolves with the authorization code, or fails on timeout, refusal or
    /// cancel. Takes the one receiver; a second call fails at once.
    pub fn code(&self) -> BoxFuture<'static, Result<String>> {
        let receiver = self
            .code
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        Box::pin(async move {
            match receiver {
                Some(receiver) => receiver.await.unwrap_or_else(|_| {
                    Err(WireError::new(
                        ErrorKind::Aborted,
                        "The authorization was dropped before it settled",
                    ))
                }),
                None => Err(WireError::new(
                    ErrorKind::Conflict,
                    "The authorization code was already taken",
                )),
            }
        })
    }

    /// Gives up on this authorization; `code()` fails with `aborted`.
    pub async fn cancel(&self, reason: &str) {
        self.listener
            .settle_now(&self.state, Err(WireError::new(ErrorKind::Aborted, reason)))
            .await;
    }
}

/// One listener, shared by every server that needs to authorize.
///
/// `begin` is what a connection calls before it hands a flow to a transport;
/// the listener is started on the first call and stopped when the last
/// authorization settles.
#[derive(Clone)]
pub struct CallbackListener {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for CallbackListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackListener")
            .field("port", &self.inner.port)
            .finish_non_exhaustive()
    }
}

impl CallbackListener {
    /// A listener that binds nothing until asked.
    pub fn new(options: CallbackListenerOptions) -> CallbackListener {
        CallbackListener {
            inner: Arc::new(Inner {
                random: options.random,
                port: options.port.unwrap_or(DEFAULT_CALLBACK_PORT),
                state: Mutex::new(ListenerState::default()),
            }),
        }
    }

    /// Where a redirect should currently go, or empty while nothing is bound.
    pub async fn redirect_url(&self) -> String {
        self.inner
            .state
            .lock()
            .await
            .running
            .as_ref()
            .map(|running| running.redirect_base.clone())
            .unwrap_or_default()
    }

    /// Prepares to receive one authorization for `server_id`.
    ///
    /// Starts the listener when it is the first. Fails only if no loopback
    /// port could be bound at all.
    pub async fn begin(&self, server_id: &str, timeout_ms: u64) -> Result<AuthorizationHandle> {
        let mut state = self.inner.state.lock().await;
        // Two servers authorizing at once must not each bind a port; the lock
        // is held across the bind so the second sees the first's socket.
        let redirect_url = if let Some(running) = &state.running {
            running.redirect_base.clone()
        } else {
            let running = self.inner.listen().await?;
            let base = running.redirect_base.clone();
            state.running = Some(running);
            base
        };

        let mut bytes = [0u8; STATE_BYTES];
        self.inner.random.fill(&mut bytes);
        let token = bytes.iter().fold(String::new(), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        });

        let (settle, receiver) = oneshot::channel();
        let timeout = if timeout_ms > 0 {
            Duration::from_millis(timeout_ms)
        } else {
            DEFAULT_AUTHORIZATION_TIMEOUT
        };
        let timer = {
            let inner = Arc::clone(&self.inner);
            let token = token.clone();
            let server_id = server_id.to_owned();
            tokio::spawn(async move {
                tokio::time::sleep(timeout).await;
                inner
                    .settle_now(
                        &token,
                        Err(WireError::new(
                            ErrorKind::Timeout,
                            format!(
                                "Authorization for MCP server \"{server_id}\" was not completed in time"
                            ),
                        )),
                    )
                    .await;
            })
        };

        state.pending.insert(
            token.clone(),
            Pending {
                server_id: server_id.to_owned(),
                settle,
                timer,
            },
        );

        Ok(AuthorizationHandle {
            state: token,
            redirect_url,
            code: std::sync::Mutex::new(Some(receiver)),
            listener: Arc::clone(&self.inner),
        })
    }

    /// How many authorizations are outstanding.
    pub async fn pending(&self) -> usize {
        self.inner.state.lock().await.pending.len()
    }

    /// Stops listening and refuses everything outstanding.
    pub async fn close(&self) {
        let states: Vec<String> = self
            .inner
            .state
            .lock()
            .await
            .pending
            .keys()
            .cloned()
            .collect();
        for token in states {
            self.inner
                .settle_now(
                    &token,
                    Err(WireError::new(
                        ErrorKind::Aborted,
                        "The MCP client is shutting down",
                    )),
                )
                .await;
        }
        let running = self.inner.state.lock().await.running.take();
        Inner::stop(running).await;
    }
}

impl Inner {
    /// Answers one outstanding authorization, handing back the listener when
    /// this was the last one: an open port nobody is using is a surface with
    /// no purpose.
    ///
    /// The stop is the caller's to make rather than this function's, because
    /// one caller is a request *this listener is serving*. A graceful shutdown
    /// waits for that connection to finish; that connection would be waiting
    /// for the shutdown, and the browser would hold an empty page until the
    /// grace expired.
    async fn settle(&self, token: &str, outcome: Result<String>) -> Option<Running> {
        let mut state = self.state.lock().await;
        let entry = state.pending.remove(token)?;
        entry.timer.abort();
        // The receiver may already be gone; nothing is owed to it then.
        let _ = entry.settle.send(outcome);
        if state.pending.is_empty() {
            state.running.take()
        } else {
            None
        }
    }

    /// Settles and stops, for the callers the listener is not serving: a
    /// cancel, a timeout, a shutdown.
    async fn settle_now(&self, token: &str, outcome: Result<String>) {
        let running = self.settle(token, outcome).await;
        Inner::stop(running).await;
    }

    async fn stop(running: Option<Running>) {
        let Some(running) = running else {
            return;
        };
        running.shutdown.cancel();
        // Graceful, so the redirect that settled the last authorization still
        // gets its page; bounded, so an idle browser connection cannot hold the
        // port open indefinitely — past the grace the task is aborted, which
        // drops the socket.
        let mut serving = running.serving;
        if tokio::time::timeout(SHUTDOWN_GRACE, &mut serving)
            .await
            .is_err()
        {
            serving.abort();
        }
    }

    async fn listen(self: &Arc<Inner>) -> Result<Running> {
        // Loopback only, always. This port accepts an authorization code; it
        // has no business being reachable from the network.
        let listener = match TcpListener::bind(("127.0.0.1", self.port)).await {
            Ok(listener) => listener,
            Err(error) if self.port != 0 => {
                // Something else already holds the fixed port — commonly a
                // second DarkWire on the same machine. An ephemeral port still
                // works wherever the authorization server accepts a dynamically
                // registered redirect URI.
                tracing::debug!(
                    port = self.port,
                    error = %error,
                    "mcp callback port unavailable, falling back to an ephemeral one"
                );
                TcpListener::bind(("127.0.0.1", 0)).await?
            }
            Err(error) => return Err(error.into()),
        };
        let port = listener.local_addr()?.port();
        let shutdown = CancellationToken::new();
        let router = Router::new()
            .route(CALLBACK_PATH, get(handle))
            .fallback(not_found)
            .with_state(Arc::clone(self));
        let serving = {
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                let served = axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown.cancelled_owned())
                    .await;
                if let Err(error) = served {
                    tracing::debug!(error = %error, "mcp callback listener stopped with an error");
                }
            })
        };
        Ok(Running {
            redirect_base: format!("http://127.0.0.1:{port}{CALLBACK_PATH}"),
            shutdown,
            serving,
        })
    }
}

fn answer(status: StatusCode, message: &str) -> Response {
    (status, Html(page(message))).into_response()
}

async fn not_found() -> Response {
    answer(StatusCode::NOT_FOUND, "Not found.")
}

async fn handle(
    State(inner): State<Arc<Inner>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let token = query.get("state").cloned().unwrap_or_default();
    let server_id = {
        let state = inner.state.lock().await;
        state
            .pending
            .get(&token)
            .map(|entry| entry.server_id.clone())
    };
    let Some(server_id) = server_id else {
        // One answer for "no such state" and "already used", so a caller cannot
        // probe for the difference.
        return answer(
            StatusCode::BAD_REQUEST,
            "This authorization link is not one DarkWire is waiting for.",
        );
    };

    if let Some(failure) = query.get("error") {
        let description = query
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| failure.clone());
        let running = inner
            .settle(
                &token,
                Err(WireError::new(
                    ErrorKind::PermissionDenied,
                    format!(
                        "Authorization for MCP server \"{server_id}\" was refused: {description}"
                    ),
                )),
            )
            .await;
        stop_later(running);
        return answer(
            StatusCode::OK,
            &format!("Authorization was refused: {}", escape(&description)),
        );
    }

    match query.get("code").filter(|code| !code.is_empty()) {
        None => {
            let running = inner
                .settle(
                    &token,
                    Err(WireError::new(
                        ErrorKind::InvalidInput,
                        format!("The authorization redirect for \"{server_id}\" carried no code"),
                    )),
                )
                .await;
            stop_later(running);
            answer(
                StatusCode::BAD_REQUEST,
                "That redirect carried no authorization code.",
            )
        }
        Some(code) => {
            let running = inner.settle(&token, Ok(code.clone())).await;
            stop_later(running);
            answer(
                StatusCode::OK,
                &format!(
                    "DarkWire is now connected to <b>{}</b>. You can close this tab.",
                    escape(&server_id)
                ),
            )
        }
    }
}

/// Stops a finished listener on a task of its own.
///
/// The caller is a request the listener is serving, so the answer has to reach
/// the browser before the socket closes rather than after.
fn stop_later(running: Option<Running>) {
    if running.is_some() {
        tokio::spawn(Inner::stop(running));
    }
}

/// The page echoes strings the authorization server chose; they must not be
/// able to write markup into it.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
