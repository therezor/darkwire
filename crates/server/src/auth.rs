//! Turning a request into a session, and a session into a cookie.
//!
//! Two credential carriers for two callers. A browser gets an `HttpOnly` cookie
//! because the alternative — a token in `localStorage` — is readable from
//! JavaScript, and this application's entire job is rendering markdown a
//! language model wrote. One successful injection would exfiltrate the session.
//! A command-line client or a CI job gets a `Bearer` header, because it has no
//! cookie jar and no cross-site scripting surface to protect it from.
//!
//! `SameSite=Strict` is what stands in for a CSRF token. The cookie is simply
//! not attached to a cross-site request, so a form on another origin cannot
//! spend it, and there is no second secret to mint, store and rotate.

use axum::extract::{FromRequestParts as _, RawPathParams, Request, State};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, header};
use axum::middleware::Next;
use axum::response::Response;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use ghostai_core::Clock;
use ghostai_protocol::config::{Config, is_loopback_host};

use crate::auth_store::{AuthSession, AuthStore};
use crate::errors::HttpError;
use crate::routes::AppState;
use crate::signing::{MEDIA_SECRET_NAME, MediaClaim, verify_media_token};

/// The cookie a browser session travels in.
pub const SESSION_COOKIE: &str = "ghost_session";

/// The scheme, matched case-insensitively as the HTTP specification requires.
const BEARER_PREFIX: &str = "bearer ";

/// What a request presented as its credential.
///
/// The carrier is kept rather than collapsed to the token, because which one
/// arrived is the difference between a browser and a script — and the rule that
/// a header wins over a cookie is only stateable if both are nameable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// An `Authorization: Bearer` token. Wins over a cookie.
    Bearer(String),
    /// The `ghost_session` cookie.
    Cookie(String),
}

impl Credential {
    /// The token, whichever carrier brought it.
    pub fn token(&self) -> &str {
        match self {
            Credential::Bearer(token) | Credential::Cookie(token) => token,
        }
    }
}

/// The credential presented, if any. `Authorization` wins over the cookie.
///
/// A caller that sends both is a command-line client driving a browser session,
/// or a test; taking the explicit header first means an expired cookie left in
/// a jar cannot shadow a token the caller deliberately attached.
pub fn read_credential(headers: &HeaderMap) -> Option<Credential> {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        && value.len() >= BEARER_PREFIX.len()
        && value
            .get(..BEARER_PREFIX.len())
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case(BEARER_PREFIX))
    {
        let token = value.get(BEARER_PREFIX.len()..).unwrap_or_default().trim();
        if !token.is_empty() {
            return Some(Credential::Bearer(token.to_owned()));
        }
    }

    let jar = CookieJar::from_headers(headers);
    let cookie = jar.get(SESSION_COOKIE)?;
    let value = cookie.value();
    if value.is_empty() {
        None
    } else {
        Some(Credential::Cookie(value.to_owned()))
    }
}

/// `Secure` unless this is plain HTTP to a loopback host.
///
/// The rule has to bend exactly that far and no further. Safari refuses to
/// store a `Secure` cookie over `http://`, including on localhost, so an
/// unconditional flag would make a default bind unusable in one browser.
/// Everywhere else the flag stays on, and the consequence — a plain-HTTP
/// network bind cannot hold a session — is the correct outcome rather than a
/// bug: a session cookie crossing a network in the clear is the thing being
/// prevented.
pub fn cookie_secure(scheme: &str, hostname: &str) -> bool {
    if scheme.eq_ignore_ascii_case("https") {
        return true;
    }
    !is_loopback_host(hostname)
}

/// The scheme and host a request arrived on, as [`cookie_secure`] wants them.
///
/// A forwarding header is deliberately not consulted: it is attacker-supplied
/// on any deployment that does not strip it, and trusting it would let a client
/// turn the `Secure` flag off by asking.
pub fn scheme_and_host(parts: &Parts) -> (String, String) {
    let scheme = parts.uri.scheme_str().unwrap_or("http").to_owned();
    let raw = parts
        .headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| parts.uri.host().map(str::to_owned))
        .unwrap_or_default();
    (scheme, strip_port(&raw))
}

/// The host without its port, keeping a bracketed IPv6 literal intact.
fn strip_port(raw: &str) -> String {
    if let Some(end) = raw.find(']') {
        return raw.get(..=end).unwrap_or(raw).to_owned();
    }
    match raw.split_once(':') {
        Some((host, _)) => host.to_owned(),
        None => raw.to_owned(),
    }
}

/// The `Set-Cookie` value that installs a session.
///
/// A header value rather than a jar entry: every attribute here is fixed by
/// this module rather than chosen per request, and the one that varies —
/// `Max-Age` — is a count of seconds the cookie crate cannot express without
/// pulling a date-time library in behind it. The name and value still go
/// through the cookie crate's encoder, which is the part that has to be right.
pub fn session_cookie(token: &str, expires_at_ms: i64, now_ms: i64, secure: bool) -> String {
    // Seconds, and never negative: a cookie whose lifetime rounds below zero is
    // a cookie the browser deletes on arrival.
    let max_age = ((expires_at_ms - now_ms) / 1000).max(0);
    format!("{}; Max-Age={max_age}", base_cookie(token, secure))
}

/// The `Set-Cookie` value that removes a session.
///
/// The attributes have to match the ones it was set with, or the browser keeps
/// the original cookie and clears nothing.
pub fn clear_session_cookie(secure: bool) -> String {
    format!("{}; Max-Age=0", base_cookie("", secure))
}

fn base_cookie(value: &str, secure: bool) -> String {
    let cookie = Cookie::build((SESSION_COOKIE, value.to_owned()))
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(secure)
        .path("/")
        .build();
    cookie.encoded().to_string()
}

/// The verified session for a request, if one was recorded.
///
/// A separate accessor from [`media_claim_of`], and separate storage, because
/// the two credentials authorise different things — one is "this user", the
/// other is "this file" — and conflating them is how a signature ends up
/// granting more than the file it names.
pub fn session_of(extensions: &Extensions) -> Option<&AuthSession> {
    extensions.get::<AuthSession>()
}

/// The verified media claim for a request, if one was recorded.
pub fn media_claim_of(extensions: &Extensions) -> Option<&MediaClaim> {
    extensions.get::<MediaClaim>()
}

/// The decision every `required` route in the manifest is guarded by.
///
/// Made before a body is read, so an unauthenticated caller cannot make the
/// server buffer a megabyte of JSON. `Ok(None)` means authentication is off for
/// the whole server and there is no session behind the request.
pub fn authenticate(
    config: &Config,
    auth: &AuthStore,
    headers: &HeaderMap,
) -> Result<Option<AuthSession>, HttpError> {
    // Disabling authentication is a boot-time decision, not a request-time one:
    // `assert_boot_policy` has already refused the combination that makes this
    // dangerous, so a loopback-only server can be reached without a login.
    if !config.server.auth.enabled {
        return Ok(None);
    }

    let Some(credential) = read_credential(headers) else {
        return Err(HttpError::unauthorized("Authentication required"));
    };

    match auth.verify(credential.token()) {
        Err(error) => Err(HttpError::from(error)),
        // One message for a malformed token, an unknown one and an expired one.
        // Distinguishing them tells a caller which half of a guess was right.
        Ok(None) => Err(HttpError::unauthorized(
            "You have been signed out. Sign in again to continue.",
        )),
        Ok(Some(session)) => Ok(Some(session)),
    }
}

/// The decision the one `signed` route in the manifest is guarded by.
///
/// A session is deliberately not accepted here. The point of the signed URL is
/// that it works where a credential cannot travel — an `<img src>` — and
/// accepting a cookie as well would make the signature optional, which is the
/// same as not having one.
///
/// Note what this does *not* do: it does not ask whether authentication is
/// enabled. A signature is checked whether or not the server has a password,
/// because it is not standing in for a login — it is naming a file.
pub fn verify_signed(
    auth: &AuthStore,
    token: Option<&str>,
    clock: &dyn Clock,
) -> Result<MediaClaim, HttpError> {
    let token = token.unwrap_or_default();
    if token.is_empty() {
        return Err(HttpError::unauthorized("A signed URL is required"));
    }

    let secret = auth
        .ensure_secret(MEDIA_SECRET_NAME)
        .map_err(HttpError::from)?;
    // One message for forged, malformed and expired, for the same reason the
    // session check gives one for all three of its failures.
    verify_media_token(&secret, token, clock.now_ms())
        .ok_or_else(|| HttpError::unauthorized("Invalid or expired signed URL"))
}

/// Refuses a request with no valid session cookie or bearer token.
///
/// Registered from the manifest against every `Required` route, never chosen by
/// a handler: "remembered to authenticate this one" is not a property a
/// codebase holds onto, and the table is.
pub async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, HttpError> {
    let session = authenticate(&state.config, &state.auth, request.headers())?;
    // Recorded even when it is `None`, which is what authentication being off
    // for the whole server looks like: the handler asks for a session and
    // correctly finds none, rather than seeing a stale one from elsewhere.
    if let Some(session) = session {
        request.extensions_mut().insert(session);
    }
    Ok(next.run(request).await)
}

/// Refuses a request whose URL does not carry a valid, unexpired media token.
///
/// The claim it verifies is left in the request extensions rather than
/// re-derived by the handler, so there is exactly one place a signature is
/// checked and no route can accidentally serve a path the signature did not
/// name.
pub async fn require_signature(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, HttpError> {
    let (mut parts, body) = request.into_parts();
    // The parameters are in the extensions by the time a per-route layer runs,
    // so this reads the same `:token` the manifest declares rather than
    // re-parsing the path.
    let token = RawPathParams::from_request_parts(&mut parts, &())
        .await
        .ok()
        .and_then(|params| {
            params
                .iter()
                .find(|(name, _)| *name == "token")
                .map(|(_, value)| value.to_owned())
        });

    let claim = verify_signed(&state.auth, token.as_deref(), state.clock.as_ref())?;
    let mut request = Request::from_parts(parts, body);
    request.extensions_mut().insert(claim);
    Ok(next.run(request).await)
}
