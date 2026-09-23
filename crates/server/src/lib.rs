//! The server.
//!
//! One axum router on one port, serving the REST API, the WebSocket and the
//! built UI. It depends on `protocol`, `core`, `security`, `providers`, `tools`
//! and `agent`, and on nothing above them: the agent loop must never be able to
//! reach back into the transport, so the dependency only points one way and the
//! event stream is the only path out. The composition root arrives as the
//! [`ServerRuntime`] trait, so this crate never depends on `darkwire-runtime`.
//!
//! Two invariants this crate exists to hold:
//!
//!  - **No route is served except from [`ROUTE_MANIFEST`].** The router is
//!    built from it and the auth-matrix test iterates it, so "is this route
//!    authenticated" is answered by a table rather than by remembering.
//!  - **A configuration that would expose an unauthenticated, shell-capable
//!    agent does not start.** [`assert_boot_policy`] is a refusal, not a
//!    warning, and it runs before a listener exists.
#![forbid(unsafe_code)]

pub mod agent_binding;
pub mod app;
pub mod approvals;
pub mod auth;
pub mod auth_store;
pub mod automation_port;
pub mod automation_store;
pub mod boot;
pub mod context;
pub mod cursor;
pub mod errors;
pub mod exec_rules;
pub mod heartbeat;
pub mod hub;
pub mod login_throttle;
pub mod manifest;
pub mod notifications;
pub mod openapi;
pub mod queries;
pub mod rate_limit;
pub mod replay;
pub mod routes;
pub mod runtime;
pub mod scheduler;
pub mod schema;
pub mod signing;
pub mod turn_log;
pub mod ui;
pub mod version;
pub mod workspace;

#[cfg(feature = "testkit")]
pub mod testkit;

pub use agent_binding::agent_for_turn;
pub use app::{ServerOptions, WireServer, create_server};
pub use approvals::{HubApprovalGate, HubApprovalGateOptions, UnattendedApproval};
pub use auth::{
    Credential, SESSION_COOKIE, authenticate, clear_session_cookie, cookie_secure, media_claim_of,
    read_credential, session_cookie, session_of, verify_signed,
};
pub use auth_store::{Argon2Hasher, AuthSession, AuthStore, AuthStoreOptions, PasswordHasher};
pub use automation_port::{MAX_AGENT_JOBS, ServerAutomationResolver};
pub use automation_store::{AutomationStore, CreateJobInput, UpdateJobInput};
pub use boot::assert_boot_policy;
pub use context::build_context_response;
pub use cursor::{
    AutomationRunCursor, MessageCursor, NotificationCursor, SessionListCursor,
    assert_one_paging_mode, decode_automation_run_cursor, decode_message_cursor,
    decode_notification_cursor, decode_session_cursor, encode_automation_run_cursor,
    encode_message_cursor, encode_notification_cursor, encode_session_cursor,
};
pub use errors::{HttpError, error_body, resolve_error, status_and_code};
pub use heartbeat::{HEARTBEAT_RESULT_TOOL, HEARTBEAT_TOOL, MAX_TASK_FILE_BYTES};
pub use hub::{ConnectOptions, HubClient, SessionHub, SessionHubOptions, TurnHandle, TurnRunner};
pub use login_throttle::{
    ACCOUNT_SCOPE, DECAY_MS, FREE_ATTEMPTS, LoginThrottle, MAX_ACCOUNT_DELAY_MS,
    MAX_ADDRESS_DELAY_MS, ThrottleBlock, delay_for,
};
pub use manifest::{ROUTE_MANIFEST, Route, RouteAuth, RouteId, RouteMethod};
pub use notifications::{CreateNotificationInput, NotificationStore};
pub use openapi::openapi_document;
pub use rate_limit::{Quota, RateLimitLayer, RateLimiter};
pub use replay::ReplayBuffer;
pub use routes::{LOGIN_ATTEMPTS_PER_MINUTE, MAX_BUFFERED_BYTES, MAX_UPLOAD_BYTES, RouteDeps};
pub use runtime::{AgentSummary, AgentView, ExtensionCounts, ServerRuntime};
pub use scheduler::{
    MAX_ARM_MS, NotificationBroadcast, Scheduler, SchedulerConnectOptions, SchedulerOptions,
    SchedulerPort, first_run_at, next_run_after,
};
pub use schema::{PROTOCOL_COMPONENTS, component_ref};
pub use signing::{
    MEDIA_SECRET_NAME, MediaClaim, assert_signing_key, media_url, sign_media_token,
    verify_media_token,
};
pub use turn_log::TurnLog;
pub use ui::UiRoot;
pub use version::SERVER_VERSION;
pub use workspace::{DEFAULT_MIME_TYPE, inline_safe, list_directory, mime_type_for};
