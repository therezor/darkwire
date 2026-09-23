//! The login, its inverse, the question the UI asks before deciding whether to
//! show the login overlay at all — and the three routes that claim an install
//! which has no password yet.
//!
//! Setup lives here rather than in a module of its own because it *is*
//! authentication: `setup.claim` mints exactly what `auth.login` mints, from a
//! different credential, and keeping the two beside each other is what makes it
//! obvious that the one-time code is a login and has to be throttled like one.
//!
//! Two properties hold across every handler here:
//!
//!  - **The token never appears in a response body.** A body a browser can read
//!    is a body an injected script can read, and this application's whole job
//!    is rendering markdown a language model wrote. The credential leaves in an
//!    `HttpOnly` cookie and nowhere else.
//!  - **Hashing happens off the runtime.** argon2id is deliberately around 50 ms
//!    and 19 MiB per call, and the hasher below the transport is synchronous;
//!    every call into it goes through [`tokio::task::spawn_blocking`], or one
//!    login would stall every other request sharing the worker.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use darkwire_core::WireError;
use darkwire_protocol::json::True;
use darkwire_protocol::rest::{
    AuthSessionResponse, LoginRequest, LoginResponse, SetupClaimRequest, SetupPasswordRequest,
    SetupStatusResponse,
};

use crate::auth::{
    clear_session_cookie, cookie_secure, scheme_and_host, session_cookie, session_of,
};
use crate::auth_store::IssuedToken;
use crate::errors::HttpError;
use crate::login_throttle::{Admission, ThrottleBlock};
use crate::routes::AppState;
use crate::schema::parse_body;

/// The body cap on the routes an anonymous caller can reach.
///
/// The general limit is a megabyte, which is right for an upload and absurd for
/// a username and a password. Every byte above this is one an unauthenticated
/// caller can make the server buffer and parse before anything has decided
/// whether to talk to them at all.
const CREDENTIAL_BODY_LIMIT: usize = 4096;

/// What a login mints for a browser.
const WEB_LABEL: &str = "web";

/// What a claimed setup code mints, so the session list says where it came from.
const SETUP_LABEL: &str = "setup";

// Shared decisions

/// Who is being throttled.
///
/// The peer address, which is the only identity available before a credential
/// has been checked. A request with no recorded peer — an in-process test
/// service — shares one bucket, which is what a test wants and is unreachable
/// over a socket.
fn caller(parts: &Parts) -> String {
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| "local".to_owned(), |info| info.0.ip().to_string())
}

/// The response a throttled caller gets.
///
/// Built rather than returned as an error, because it carries a header the
/// error envelope has no slot for — and `Retry-After` is the whole of what a
/// well-behaved client needs from this answer. The body is still the one
/// envelope every other refusal uses.
///
/// Whole seconds, rounded **up**: the header is defined in seconds, and
/// rounding down would tell a client to come back at a moment the throttle
/// still refuses, which reads to them as the limit being broken.
fn throttled(block: &ThrottleBlock) -> Response {
    let seconds = block.retry_after_ms.div_euclid(1000)
        + i64::from(block.retry_after_ms.rem_euclid(1000) > 0);
    // The scope is deliberately not in the message. "Your address is locked
    // out" and "the account is locked out" tell an attacker whether their
    // address has been singled out, which is exactly what they would use to
    // decide whether rotating through a botnet is working.
    let error =
        HttpError::too_many_requests(format!("Too many attempts. Try again in {seconds}s."));
    (
        error.status,
        [(header::RETRY_AFTER, seconds.to_string())],
        Json(error.body()),
    )
        .into_response()
}

/// Reads a credential body, bounded before it is buffered.
///
/// A malformed document is a 400 — the caller has to fix the request itself —
/// and a well-formed one that fails the schema is the 422 every other route
/// answers with, keyed by the field that failed.
async fn credential_body<T>(body: Body) -> Result<T, HttpError>
where
    T: serde::de::DeserializeOwned + garde::Validate<Context = ()>,
{
    let bytes = axum::body::to_bytes(body, CREDENTIAL_BODY_LIMIT)
        .await
        .map_err(|_| {
            HttpError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                darkwire_protocol::ws::ErrorCode::BadRequest,
                darkwire_core::ErrorKind::InvalidInput,
                "The request body is too large.",
            )
        })?;
    let raw = serde_json::from_slice(&bytes)
        .map_err(|error| HttpError::bad_request(format!("Invalid JSON body: {error}")))?;
    parse_body("body", raw)
}

/// Runs one blocking store call off the runtime's worker threads.
///
/// argon2id is the reason: it is meant to be expensive, and a handler that
/// awaited it inline would hold a worker for the whole of that expense.
async fn blocking<T, F>(work: F) -> Result<T, HttpError>
where
    F: FnOnce() -> darkwire_core::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result.map_err(HttpError::from),
        // The task panicked or was cancelled. Neither is something a caller
        // can act on, and neither is a credential failure, so it is the one
        // generic 500 rather than a 401 that would read as a wrong password.
        Err(error) => Err(HttpError::from(WireError::new(
            darkwire_core::ErrorKind::Internal,
            format!("The credential check did not complete: {error}"),
        ))),
    }
}

/// The success both logins share: a cookie, and a body with nothing secret in
/// it.
fn session_response(parts: &Parts, issued: &IssuedToken, now_ms: i64) -> Response {
    let (scheme, host) = scheme_and_host(parts);
    let cookie = session_cookie(
        &issued.token,
        issued.expires_at_ms,
        now_ms,
        cookie_secure(&scheme, &host),
    );
    (
        [(header::SET_COOKIE, cookie)],
        Json(LoginResponse {
            ok: True,
            expires_at_ms: u64::try_from(issued.expires_at_ms).unwrap_or(0),
        }),
    )
        .into_response()
}

/// Refuses every credential route when there is no credential to check.
///
/// Not a 401: the credential is not wrong, there is nothing to log in to. A UI
/// that reached here has misread the setup status.
fn require_auth_enabled(state: &AppState) -> Result<(), HttpError> {
    if state.config.server.auth.enabled {
        Ok(())
    } else {
        Err(HttpError::bad_request(
            "Authentication is disabled on this server",
        ))
    }
}

// The routes

/// Exchange the username and password for a session.
pub async fn login(
    State(state): State<AppState>,
    parts: Parts,
    body: Body,
) -> Result<Response, HttpError> {
    require_auth_enabled(&state)?;

    // Before the key derivation, not after. A caller who is already locked out
    // must not be able to spend 50 ms and 19 MiB of the server's budget per
    // request — a throttle that still does the expensive work is an amplifier.
    let address = caller(&parts);
    if let Some(block) = state.login_throttle.check(&address)? {
        return Ok(throttled(&block));
    }

    let request: LoginRequest = credential_body(body).await?;

    // Counted as a failure before the hash, and in the same step as a second
    // look at the lock. Guesses sent in parallel all passed the check above
    // while each other's hashes ran; this is where they meet the count.
    let created = match state.login_throttle.admit(&address)? {
        Admission::Refused(block) => return Ok(throttled(&block)),
        Admission::Admitted(created) => created,
    };

    let auth = Arc::clone(&state.auth);
    let username = request.username.as_str().to_owned();
    let password = request.password.0.clone();
    let matched = blocking(move || auth.verify_login(&username, &password)).await?;

    if !matched {
        // The block is reported on the attempt that created it rather than on
        // the next one. A 401 tells the attacker the guess was wrong and leaves
        // them free to send another immediately; the delay is only real once
        // the response says so.
        if let Some(block) = created {
            return Ok(throttled(&block));
        }
        // One message for a wrong username and a wrong password. Naming which
        // half failed hands over the other half.
        return Err(HttpError::unauthorized("Incorrect username or password"));
    }
    state.login_throttle.succeed(&address)?;

    let issued = state.auth.issue(WEB_LABEL)?;
    Ok(session_response(&parts, &issued, state.clock.now_ms()))
}

/// Revoke the presented session.
pub async fn logout(State(state): State<AppState>, parts: Parts) -> Result<Response, HttpError> {
    if let Some(session) = session_of(&parts.extensions) {
        state.auth.revoke_by_id(&session.id)?;
    }
    let (scheme, host) = scheme_and_host(&parts);
    // 204 rather than a body: there is nothing to say, and inventing an
    // `{"ok": true}` shape for it would put a schema in the protocol that
    // exists only to be ignored.
    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            clear_session_cookie(cookie_secure(&scheme, &host)),
        )],
    )
        .into_response())
}

/// Whether the caller is authenticated, and until when.
pub async fn me(
    State(state): State<AppState>,
    parts: Parts,
) -> Result<Json<AuthSessionResponse>, HttpError> {
    let auth_enabled = state.config.server.auth.enabled;
    let session = session_of(&parts.extensions);
    Ok(Json(AuthSessionResponse {
        // Reaching this handler means the layer let the request through, which
        // is true either because a session checked out or because
        // authentication is off.
        authenticated: true,
        auth_enabled,
        expires_at_ms: session.map(|session| u64::try_from(session.expires_at_ms).unwrap_or(0)),
        // Only when authentication is on. With it off there is no account, and
        // reporting the name of one would describe a login that does not exist.
        username: if auth_enabled {
            Some(state.auth.username()?)
        } else {
            None
        },
    }))
}

/// Whether this install still has to be claimed.
pub async fn setup_status(
    State(state): State<AppState>,
) -> Result<Json<SetupStatusResponse>, HttpError> {
    // With authentication off there is nothing to claim: the server is
    // reachable without a credential by design, and offering a setup screen
    // would be asking for a password that would never be checked.
    let required = state.config.server.auth.enabled && !state.auth.has_password()?;
    Ok(Json(SetupStatusResponse { required }))
}

/// Spend the one-time code printed at startup for a session.
pub async fn setup_claim(
    State(state): State<AppState>,
    parts: Parts,
    body: Body,
) -> Result<Response, HttpError> {
    require_auth_enabled(&state)?;
    if state.auth.has_password()? {
        // Not a 401 either: the code is not wrong, the install is already
        // claimed and the caller should be logging in with the password.
        return Err(HttpError::bad_request(
            "This server already has a password; sign in instead",
        ));
    }

    // The same throttle the login uses, and the same buckets: a code is a
    // credential for exactly the same account, so guesses at it have to count
    // against the same aggregate. Two independent counters would let an
    // attacker have both budgets.
    let address = caller(&parts);
    if let Some(block) = state.login_throttle.check(&address)? {
        return Ok(throttled(&block));
    }

    let request: SetupClaimRequest = credential_body(body).await?;
    let created = match state.login_throttle.admit(&address)? {
        Admission::Refused(block) => return Ok(throttled(&block)),
        Admission::Admitted(created) => created,
    };
    if !state.auth.consume_setup_code(&request.code)? {
        if let Some(block) = created {
            return Ok(throttled(&block));
        }
        // One message for a wrong code and a spent one, for the same reason the
        // login gives one for a bad password and an unknown session.
        return Err(HttpError::unauthorized(
            "Incorrect or already-used setup code",
        ));
    }
    state.login_throttle.succeed(&address)?;

    let issued = state.auth.issue(SETUP_LABEL)?;
    Ok(session_response(&parts, &issued, state.clock.now_ms()))
}

/// Set the login password and name, finishing the claim or rotating both.
pub async fn setup_password(
    State(state): State<AppState>,
    parts: Parts,
    body: Body,
) -> Result<Response, HttpError> {
    require_auth_enabled(&state)?;
    let request: SetupPasswordRequest = credential_body(body).await?;
    let address = caller(&parts);

    // The route is `Required`, so the caller already holds a session — either
    // the one the claim minted, or a normal login rotating their password.
    //
    // A session is sufficient for the first and not for the second. During a
    // claim there is no password to prove and demanding one would make the
    // wizard unfinishable; afterwards, the session is a credential an injected
    // script in a page full of model-authored markdown can spend, and the old
    // password is the thing it cannot produce.
    if state.auth.has_password()? {
        if let Some(block) = state.login_throttle.check(&address)? {
            return Ok(throttled(&block));
        }
        let Some(current) = request.current_password.clone() else {
            return Err(HttpError::bad_request(
                "The current password is required to change it",
            ));
        };
        let created = match state.login_throttle.admit(&address)? {
            Admission::Refused(block) => return Ok(throttled(&block)),
            Admission::Admitted(created) => created,
        };

        let auth = Arc::clone(&state.auth);
        let matched = blocking(move || auth.verify_password(&current.0)).await?;
        if !matched {
            if let Some(block) = created {
                return Ok(throttled(&block));
            }
            return Err(HttpError::unauthorized("Incorrect current password"));
        }
        state.login_throttle.succeed(&address)?;
    }

    let auth = Arc::clone(&state.auth);
    let password = request.password.0.clone();
    let username = request
        .username
        .as_ref()
        .map(|name| name.as_str().to_owned());
    blocking(move || auth.set_password(&password, username.as_deref())).await?;

    // Setting a password revokes every session, including the caller's own.
    // Re-issuing is not a convenience: without it the browser is signed out in
    // the middle of the wizard, with the code it would need to get back in
    // already spent.
    let issued = state.auth.issue(WEB_LABEL)?;
    Ok(session_response(&parts, &issued, state.clock.now_ms()))
}
