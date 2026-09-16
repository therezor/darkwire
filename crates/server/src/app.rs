//! One router on one port, for the API, the WebSocket and the UI.
//!
//! Single-process by default, and single-port with it. Nothing DarkWire does is
//! heavy enough to justify splitting the API from the socket, and a split would
//! cost every client a second origin to configure, a second certificate to
//! trust, and a reconnect story that has to survive one half being up.
//!
//! Construction order matters and is not arbitrary:
//!
//!  1. The auth store opens its tables and any provided password is written, so
//!     that
//!  2. [`assert_boot_policy`] can ask whether a login could ever succeed — and
//!     refuse *before* a listener exists, when there is nothing to unwind.
//!  3. Layers apply outermost-first: rate limiting before authentication, so a
//!     flood of bad passwords is refused before it reaches argon2id.
//!  4. Routes come from the manifest, which is the only path to a served route.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use darkwire_core::session_store::IdSource;
use darkwire_core::{Clock, Database, Result, SystemClock, WireError};
use darkwire_protocol::config::Config;
use darkwire_security::random::{OsRandom, RandomSource};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::auth_store::{Argon2Hasher, AuthStore, AuthStoreOptions, PasswordHasher};
use crate::automation_store::AutomationStore;
use crate::boot::assert_boot_policy;
use crate::errors::{HttpError, error_body};
use crate::hub::SessionHub;
use crate::login_throttle::LoginThrottle;
use crate::notifications::NotificationStore;
use crate::rate_limit::{Quota, RateLimiter};
use crate::routes::{AppState, RouteDeps, router};
use crate::runtime::ServerRuntime;
use crate::scheduler::SchedulerPort;
use crate::ui::{UiRoot, may_fall_back};

pub use crate::version::SERVER_VERSION;

/// How to build the server.
pub struct ServerOptions {
    /// The boot settings.
    ///
    /// Read once, here, for everything baked into the listener: the bind, the
    /// rate limits, whether the auth layer checks anything. `runtime.config()`
    /// is the live tree a settings save moves, and the routes read that one —
    /// but a live toggle of `server.auth.enabled` is a request to
    /// unauthenticate an already-authenticated session, so this one wins for
    /// the questions the layers ask.
    pub config: Config,
    /// Everything below the transport, as a trait.
    pub runtime: Arc<dyn ServerRuntime>,
    /// The hub the socket route serves.
    ///
    /// Built by the caller, not here: the hub needs an approval gate, and the
    /// gate has to exist before the runtime is constructed, because that is
    /// what threads it into the loop. Untying that knot from inside this
    /// function is not possible, and a hub built here would be a second one.
    pub hub: Arc<SessionHub>,
    /// The single-page app to serve.
    pub ui: UiRoot,
    /// The connection the session store and the scheduler share.
    ///
    /// Required rather than opened here: one write-ahead log is the point, and
    /// a server that quietly opened its own would put auth writes in a second
    /// connection to the same file and reintroduce the lock contention the
    /// sharing avoids.
    pub database: Database,
    /// The engine.
    ///
    /// `None` means no engine: the CRUD routes still work and only the run
    /// route refuses, which is what a route test that never wanted a timer
    /// needs.
    pub scheduler: Option<Arc<dyn SchedulerPort>>,
    /// The injected clock.
    pub clock: Arc<dyn Clock>,
    /// The injected randomness.
    pub random: Arc<dyn RandomSource>,
    /// Sets or rotates the password at boot, then is not retained.
    ///
    /// This is how `--password` and `DARKWIRE_PASSWORD` reach the store. Reading
    /// the environment is deliberately the caller's job — a server that read it
    /// itself would be untestable without mutating the process environment.
    pub password: Option<String>,
    /// Sets the login name, and only in the same breath as `password`.
    ///
    /// Alone it is a configuration error rather than a no-op: rotating a name
    /// without a password would leave sessions minted under the old credential
    /// alive, and silently ignoring the flag would leave an operator convinced
    /// they had changed something.
    pub username: Option<String>,
    /// Injected by tests; argon2id is around 50 ms per call by design.
    pub hasher: Option<Arc<dyn PasswordHasher>>,
}

impl ServerOptions {
    /// The options with the real clock, the real randomness and no UI.
    pub fn new(
        config: Config,
        runtime: Arc<dyn ServerRuntime>,
        hub: Arc<SessionHub>,
        database: Database,
    ) -> ServerOptions {
        ServerOptions {
            config,
            runtime,
            hub,
            ui: UiRoot::None,
            database,
            scheduler: None,
            clock: Arc::new(SystemClock),
            random: Arc::new(OsRandom),
            password: None,
            username: None,
            hasher: None,
        }
    }
}

/// A built server, before it is listening.
pub struct WireServer {
    /// The router, ready to serve or to be driven directly by a test.
    pub router: Router,
    /// Passwords, sessions and the signing secret.
    pub auth: Arc<AuthStore>,
    /// Raised by the scheduler and the hub; read over `/api/notifications`.
    pub notifications: Arc<NotificationStore>,
    /// The jobs and runs the scheduler drives.
    ///
    /// Returned so the caller can build a scheduler over it — the engine needs
    /// a hub and a runtime that this function does not have, so it is
    /// constructed outside and handed back in through
    /// [`ServerOptions::scheduler`].
    pub automation: Arc<AutomationStore>,
    /// The boot settings, as read.
    pub config: Config,
}

impl WireServer {
    /// Binds and serves until `token` is cancelled, answering with the address
    /// it actually bound.
    ///
    /// The address comes back rather than being assumed, because `port: 0` asks
    /// the operating system for a free one and the caller — a test, or the
    /// ready file the end-to-end harness polls — has no other way to learn it.
    pub async fn serve(
        self,
        address: SocketAddr,
        token: CancellationToken,
    ) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|error| storage_error("bind the listener", &error))?;
        let bound = listener
            .local_addr()
            .map_err(|error| storage_error("read the bound address", &error))?;
        let router = self.router;
        let handle = tokio::spawn(async move {
            let served = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move { token.cancelled().await });
            if let Err(error) = served.await {
                tracing::error!(%error, "the listener stopped");
            }
        });
        Ok((bound, handle))
    }
}

/// Row ids, as a UUIDv7 over the injected clock and randomness.
///
/// Built here rather than taken as an option: a store's ids are not a seam
/// anything outside a test needs to move, and the two seams that *are* — the
/// clock and the randomness — are already injected and are what a v7 id is
/// made of.
fn id_source(clock: Arc<dyn Clock>, random: Arc<dyn RandomSource>) -> IdSource {
    Box::new(move || {
        let mut bytes = [0u8; 10];
        random.fill(&mut bytes);
        darkwire_protocol::new_uuid(u64::try_from(clock.now_ms()).unwrap_or(0), &bytes)
    })
}

fn storage_error(what: &str, error: &std::io::Error) -> WireError {
    WireError::new(
        darkwire_core::ErrorKind::Storage,
        format!("Could not {what}: {error}"),
    )
}

/// Builds the server.
///
/// Returns `Err` rather than starting on a configuration that must not be
/// served — see [`assert_boot_policy`], which runs here, before a listener
/// exists.
pub fn create_server(options: ServerOptions) -> Result<WireServer> {
    // (1) The store opens its tables and any provided password is written…
    let auth = Arc::new(AuthStore::new(AuthStoreOptions {
        db: options.database.clone(),
        session_ttl_ms: i64::try_from(options.config.server.auth.session_ttl_ms)
            .unwrap_or(i64::MAX),
        clock: Arc::clone(&options.clock),
        random: Arc::clone(&options.random),
        hasher: options
            .hasher
            .clone()
            .unwrap_or_else(|| Arc::new(Argon2Hasher)),
    })?);
    if options.username.is_some() && options.password.is_none() {
        // Rotating a name without a password would leave sessions minted under
        // the old credential alive, and ignoring the flag would leave an
        // operator convinced they had changed something.
        return Err(WireError::new(
            darkwire_core::ErrorKind::Config,
            "A username can only be set together with a password.",
        ));
    }
    if let Some(password) = options.password.as_deref() {
        auth.set_password(password, options.username.as_deref())?;
    }

    // (2) …so the boot policy can ask whether a login could ever succeed, and
    // refuse while there is nothing to unwind.
    assert_boot_policy(&options.config)?;

    let notifications = Arc::new(NotificationStore::new(
        options.database.clone(),
        Arc::clone(&options.clock),
        id_source(Arc::clone(&options.clock), Arc::clone(&options.random)),
    )?);
    let automation = Arc::new(AutomationStore::new(
        options.database.clone(),
        Arc::clone(&options.clock),
        id_source(Arc::clone(&options.clock), Arc::clone(&options.random)),
    )?);
    // On the same connection as the sessions it guards, so a restart does not
    // hand an attacker a fresh counter.
    let login_throttle = Arc::new(LoginThrottle::new(
        options.database.clone(),
        Arc::clone(&options.clock),
    )?);

    let ui = options.ui.clone();
    let state: AppState = Arc::new(RouteDeps {
        config: options.config.clone(),
        runtime: Arc::clone(&options.runtime),
        hub: Arc::clone(&options.hub),
        auth: Arc::clone(&auth),
        login_throttle,
        notifications: Arc::clone(&notifications),
        automation: Arc::clone(&automation),
        scheduler: options.scheduler.clone(),
        database: options.database.clone(),
        ui: ui.clone(),
        started_at: options.clock.monotonic(),
        clock: Arc::clone(&options.clock),
        random: Arc::clone(&options.random),
    });

    // `0` means no limit, the same convention every other `*PerMinute` field in
    // the config uses. Per-route limits still apply — the login's does not come
    // from this setting and is not switched off with it.
    let per_minute = options.config.server.auth.rate_limit_per_minute;
    let global = if per_minute > 0 {
        Some(RateLimiter::new(
            Quota::per_minute(u32::try_from(per_minute).unwrap_or(u32::MAX)),
            Arc::clone(&options.clock),
        ))
    } else {
        None
    };

    // (4) Routes from the manifest, then the fallback under them: a single-page
    // app owns URLs the server has never heard of, and the router matching none
    // of them is what "the client routed it" looks like from here.
    let router =
        router(state, global.as_ref()).fallback(axum::routing::any(not_found).with_state(ui));

    Ok(WireServer {
        router,
        auth,
        notifications,
        automation,
        config: options.config,
    })
}

/// Everything the router did not match.
///
/// Three answers, in order. A `GET` that names a file in the bundle is that
/// file. A `GET` outside `/api` and `/ws` that names nothing is the shell,
/// because a single-page app owns URLs the server has never heard of and the
/// router matching none of them is what "the client routed it" looks like from
/// here. Everything else is the one error envelope: an unknown API path
/// answered with HTML would surface as a JSON parse error somewhere entirely
/// unrelated, which is a much longer bug than a 404.
///
/// The shell is served without a credential, deliberately. The UI is a static
/// asset, every byte of data behind it is authenticated, and a login screen
/// that needed a session to load could never be reached.
async fn not_found(State(ui): State<UiRoot>, request: Request) -> Response {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();

    if may_fall_back(&method, &path) {
        if let Some(asset) = ui.asset(&path) {
            return file_response(asset);
        }
        if let Some(shell) = ui.shell() {
            return file_response(shell);
        }
    }

    let error = HttpError::not_found(format!("No route for {method} {path}"));
    (
        error.status,
        axum::Json(error_body(error.code, &error.message, error.details)),
    )
        .into_response()
}

fn file_response(file: crate::ui::UiFile) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, file.content_type)],
        Body::from(file.body),
    )
        .into_response()
}
