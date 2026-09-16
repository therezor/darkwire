//! The handlers, joined to the manifest by route id.
//!
//! Nothing here registers itself. [`router`] walks [`ROUTE_MANIFEST`] and
//! `match`es each entry's [`RouteId`] to its handler, so the set of handlers
//! and the set of served routes are the same set *by construction*: a manifest
//! entry with no handler and a handler with no manifest entry are both compile
//! errors rather than a 404 found later.
//!
//! The method comes from the manifest too, not from the arm. Each arm names
//! only the function; the verb is applied around it by the macro below, so a
//! handler cannot end up mounted on a method the manifest does not claim.
//!
//! The handlers themselves live in `routes/`, one module per section of the
//! API, because sixty-three of them in one file is a file nobody reads twice.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::{MethodFilter, on};
use ghostai_core::{Clock, Database};
use ghostai_protocol::config::Config;
use ghostai_security::random::RandomSource;

use crate::auth_store::AuthStore;
use crate::automation_store::AutomationStore;
use crate::hub::SessionHub;
use crate::login_throttle::LoginThrottle;
use crate::manifest::{ROUTE_MANIFEST, Route, RouteAuth, RouteId, RouteMethod};
use crate::notifications::NotificationStore;
use crate::rate_limit::{Quota, RateLimitLayer, RateLimiter};
use crate::runtime::ServerRuntime;
use crate::scheduler::SchedulerPort;
use crate::ui::UiRoot;

pub mod agents;
pub mod auth;
pub mod automation;
pub mod commands;
pub mod environments;
pub mod extensions;
pub mod files;
pub mod mcp;
pub mod notifications;
pub mod providers;
pub mod sandboxes;
pub mod sessions;
pub mod settings;
pub mod system;
pub mod tools;
pub mod workspaces;
pub mod ws;

#[cfg(feature = "test-hooks")]
pub mod hooks;

pub use files::{MAX_TEXT_BODY_BYTES, MAX_UPLOAD_BYTES};
pub use ws::MAX_BUFFERED_BYTES;

/// How many login attempts one caller gets per minute.
///
/// Independent of `server.auth.rateLimitPerMinute` and not switched off with
/// it: an operator turning off the general limiter is not asking for unlimited
/// password attempts.
pub const LOGIN_ATTEMPTS_PER_MINUTE: u32 = 10;

/// What every route may reach.
pub struct RouteDeps {
    /// The settings the *listener* was built with.
    ///
    /// Deliberately distinct from `runtime.config()`, which is live. Host,
    /// port, rate limits and `auth.enabled` were read once at boot and are
    /// baked into the layers; reporting a patched value for them would describe
    /// a server that is not running. Everything a route reads for its own
    /// answer comes from `runtime.config()` instead.
    pub config: Config,
    /// Everything below the transport.
    pub runtime: Arc<dyn ServerRuntime>,
    /// The one hub in the process.
    ///
    /// Required, not optional: it is what the socket route serves, and a server
    /// built without one would register `GET /ws` — which the manifest says is
    /// served — over nothing.
    pub hub: Arc<SessionHub>,
    /// Passwords, sessions and the signing secret.
    pub auth: Arc<AuthStore>,
    /// The brute-force throttle the credential routes share.
    ///
    /// One instance, shared rather than one per route, because the account
    /// scope is only meaningful if every guess at the account lands in it — a
    /// login and a setup code are two credentials for the same single account,
    /// and two throttles would give an attacker two budgets.
    pub login_throttle: Arc<LoginThrottle>,
    /// The notification store the panel and the scheduler both write to.
    pub notifications: Arc<NotificationStore>,
    /// Automation jobs and their run history.
    pub automation: Arc<AutomationStore>,
    /// The engine, deferred because it is built over the notification store
    /// this bag already holds.
    ///
    /// `None` means this build has no engine — the CRUD routes still work, so
    /// an operator can author jobs, and only the run route refuses.
    pub scheduler: Option<Arc<dyn SchedulerPort>>,
    /// Pinged by the health check, which is the only honest liveness signal.
    pub database: Database,
    /// The served UI, for the SPA fallback.
    pub ui: UiRoot,
    /// Monotonic, from the injected clock, so uptime survives an NTP step.
    pub started_at: Duration,
    /// The injected clock.
    pub clock: Arc<dyn Clock>,
    /// The injected randomness.
    ///
    /// One route mints a session key when a client sends none, and a key is a
    /// UUIDv7 — half clock, half random bytes. Reaching for ambient randomness
    /// there is exactly what the workspace-wide clippy denial exists to catch,
    /// so the source arrives here like the clock does.
    pub random: Arc<dyn RandomSource>,
}

/// The bag every handler extracts with `State`.
pub type AppState = Arc<RouteDeps>;

/// axum's filter for one manifest method.
fn method_filter(method: RouteMethod) -> MethodFilter {
    match method {
        RouteMethod::GET => MethodFilter::GET,
        RouteMethod::POST => MethodFilter::POST,
        RouteMethod::PATCH => MethodFilter::PATCH,
        RouteMethod::PUT => MethodFilter::PUT,
        RouteMethod::DELETE => MethodFilter::DELETE,
    }
}

/// The per-route limits that are not the global one.
///
/// Kept beside the router rather than in the manifest because a limit is a
/// property of the handler's cost, not of the route's identity, and the
/// auth-matrix test that walks the manifest has no business knowing about it.
fn quota_for(id: RouteId) -> Option<Quota> {
    match id {
        // All three take a credential: the login and the claim mint a session,
        // and on a claimed install the password route *takes* the current
        // password before it rotates it. The per-account throttle delays a
        // guess; this is the per-address bucket that bounds how many can be
        // tried at all.
        RouteId::AuthLogin | RouteId::SetupClaim | RouteId::SetupPassword => {
            Some(Quota::per_minute(LOGIN_ATTEMPTS_PER_MINUTE))
        }
        _ => None,
    }
}

/// Builds the router from the manifest, and from nothing else.
pub fn router(state: AppState, global: Option<&Arc<RateLimiter>>) -> Router {
    let mut router = Router::new();
    for route in ROUTE_MANIFEST {
        router = router.route(&route.axum_path(), mount(route, &state, global));
    }
    router.with_state(state)
}

/// One manifest entry as a mounted method router, with its layers.
///
/// Long by construction: the `match` is one arm per served route, and that is
/// the point — exhaustiveness is what makes a missing handler a compile error.
/// Splitting it per section would put the join back into a place where a
/// section could be forgotten whole.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per route is what makes the manifest-to-handler join exhaustive"
)]
fn mount(
    route: &Route,
    state: &AppState,
    global: Option<&Arc<RateLimiter>>,
) -> axum::routing::MethodRouter<AppState> {
    let filter = method_filter(route.method);

    // Each arm names only the handler; `on` applies the manifest's verb, so a
    // handler cannot be mounted on a method the manifest does not claim.
    macro_rules! handler {
        ($function:path) => {
            on(filter, $function)
        };
    }

    let mounted: axum::routing::MethodRouter<AppState> = match route.id {
        RouteId::SystemHealth => handler!(system::health),
        RouteId::SystemStatus => handler!(system::status),
        RouteId::SystemOpenapi => handler!(system::openapi),

        RouteId::WsConnect => handler!(ws::connect),

        RouteId::AuthLogin => handler!(auth::login),
        RouteId::AuthLogout => handler!(auth::logout),
        RouteId::AuthMe => handler!(auth::me),
        RouteId::SetupStatus => handler!(auth::setup_status),
        RouteId::SetupClaim => handler!(auth::setup_claim),
        RouteId::SetupPassword => handler!(auth::setup_password),

        RouteId::SettingsGet => handler!(settings::get),
        RouteId::SettingsPatch => handler!(settings::patch),
        RouteId::SettingsCredential => handler!(settings::credential),
        RouteId::SettingsReload => handler!(settings::reload),

        RouteId::ProvidersList => handler!(providers::list),
        RouteId::ProvidersTest => handler!(providers::test),
        RouteId::ModelsList => handler!(providers::models),
        RouteId::ModelsRefresh => handler!(providers::refresh_models),

        RouteId::SessionsList => handler!(sessions::list),
        RouteId::SessionsCreate => handler!(sessions::create),
        RouteId::SessionsGet => handler!(sessions::get),
        RouteId::SessionsUpdate => handler!(sessions::update),
        RouteId::SessionsDelete => handler!(sessions::delete),
        RouteId::SessionsMessages => handler!(sessions::messages),
        RouteId::SessionsClear => handler!(sessions::clear),
        RouteId::SessionsContext => handler!(sessions::context),
        RouteId::SessionsBranch => handler!(sessions::branch),
        RouteId::SessionsTurns => handler!(sessions::turns),

        RouteId::AgentsList => handler!(agents::list),
        RouteId::ToolsList => handler!(tools::list),
        RouteId::EnvironmentsList => handler!(environments::list_environments),
        RouteId::SandboxesList => handler!(sandboxes::list_sandboxes),
        RouteId::SandboxesManage => handler!(sandboxes::manage),
        RouteId::McpList => handler!(mcp::list),

        RouteId::ExtensionsList => handler!(extensions::list),
        RouteId::ExtensionsApprove => handler!(extensions::approve),
        RouteId::ExtensionsRevoke => handler!(extensions::revoke),
        RouteId::CommandsList => handler!(commands::list),
        RouteId::CommandsRun => handler!(commands::run),

        RouteId::FilesList => handler!(files::list),
        RouteId::FilesDelete => handler!(files::delete),
        RouteId::FilesUpload => handler!(files::upload),
        RouteId::FilesRead => handler!(files::read),
        RouteId::FilesWrite => handler!(files::write),
        RouteId::FilesMkdir => handler!(files::mkdir),
        RouteId::FilesMove => handler!(files::move_entry),
        RouteId::FilesSign => handler!(files::sign),
        RouteId::MediaGet => handler!(files::media),

        RouteId::WorkspacesList => handler!(workspaces::list),
        RouteId::WorkspacesCreate => handler!(workspaces::create),
        RouteId::WorkspacesUpdate => handler!(workspaces::update),
        RouteId::WorkspacesDelete => handler!(workspaces::delete),
        RouteId::WorkspacesMoveSessions => handler!(workspaces::move_sessions),

        RouteId::NotificationsList => handler!(notifications::list),
        RouteId::NotificationsReadAll => handler!(notifications::read_all),
        RouteId::NotificationsRead => handler!(notifications::read),
        RouteId::NotificationsDelete => handler!(notifications::delete),
        RouteId::NotificationsDeleteAll => handler!(notifications::delete_all),

        RouteId::AutomationList => handler!(automation::list),
        RouteId::AutomationCreate => handler!(automation::create),
        RouteId::AutomationGet => handler!(automation::get),
        RouteId::AutomationUpdate => handler!(automation::update),
        RouteId::AutomationDelete => handler!(automation::delete),
        RouteId::AutomationRun => handler!(automation::run),
        RouteId::AutomationRuns => handler!(automation::runs),

        #[cfg(feature = "test-hooks")]
        RouteId::TestSessions => handler!(hooks::sessions),
        #[cfg(feature = "test-hooks")]
        RouteId::TestNotifications => handler!(hooks::notifications),
        #[cfg(feature = "test-hooks")]
        RouteId::TestAutomationRun => handler!(hooks::automation_run),
        #[cfg(feature = "test-hooks")]
        RouteId::TestAutomationRunFinish => handler!(hooks::automation_run_finish),
        #[cfg(feature = "test-hooks")]
        RouteId::TestSchedulerTick => handler!(hooks::scheduler_tick),
    };

    // The auth class comes off the manifest entry and is applied here, never
    // inside a handler: a check a handler has to remember to make is a check
    // that is eventually forgotten.
    let mounted = match route.auth {
        RouteAuth::Public => mounted,
        RouteAuth::Required => mounted.route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(state),
            crate::auth::require_session,
        )),
        RouteAuth::Signed => mounted.route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(state),
            crate::auth::require_signature,
        )),
    };

    // A per-route quota is its own limiter, so it survives the global switch.
    let mounted = match quota_for(route.id) {
        Some(quota) => mounted.route_layer(RateLimitLayer::new(RateLimiter::new(
            quota,
            Arc::clone(&state.clock),
        ))),
        None => mounted,
    };

    match global {
        Some(limiter) => mounted.route_layer(RateLimitLayer::new(Arc::clone(limiter))),
        None => mounted,
    }
}
