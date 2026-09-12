//! Reading a credential off a request, and the two guards the manifest names.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use ghostai_core::testkit::ManualClock;
use ghostai_core::{Clock, Database, Result};
use ghostai_protocol::config::Config;
use ghostai_security::random::RandomSource;
use ghostai_server::auth::{
    Credential, SESSION_COOKIE, authenticate, clear_session_cookie, cookie_secure, media_claim_of,
    read_credential, scheme_and_host, session_cookie, session_of, verify_signed,
};
use ghostai_server::auth_store::{AuthStore, AuthStoreOptions, PasswordHasher};
use ghostai_server::signing::{MEDIA_SECRET_NAME, MediaClaim, sign_media_token};

const NOW: i64 = 1_700_000_000_000;

/// Cheap and reversible, so a test can assert *what* was hashed.
struct FakeHasher;

impl PasswordHasher for FakeHasher {
    fn hash(&self, password: &str) -> Result<String> {
        Ok(format!("fake:{password}"))
    }

    fn verify(&self, hash: &str, password: &str) -> bool {
        hash == format!("fake:{password}")
    }
}

/// Distinct per call, so two tokens never collide, and reproducible.
struct CountingRandom(std::sync::atomic::AtomicU8);

impl RandomSource for CountingRandom {
    fn fill(&self, buf: &mut [u8]) {
        let previous = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        buf.fill(previous.wrapping_add(1));
    }
}

fn store(clock: &Arc<ManualClock>) -> AuthStore {
    AuthStore::new(AuthStoreOptions {
        db: Database::in_memory().unwrap(),
        session_ttl_ms: 60_000,
        clock: Arc::clone(clock) as Arc<dyn Clock>,
        random: Arc::new(CountingRandom(std::sync::atomic::AtomicU8::new(0))),
        hasher: Arc::new(FakeHasher),
    })
    .unwrap()
}

fn headers(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(name.clone(), HeaderValue::from_str(value).unwrap());
    }
    map
}

fn cookie_header(value: &str) -> HeaderMap {
    headers(&[(header::COOKIE, &format!("{SESSION_COOKIE}={value}"))])
}

// read_credential

#[test]
fn nothing_is_found_when_nothing_was_sent() {
    assert_eq!(read_credential(&HeaderMap::new()), None);
}

#[test]
fn a_bearer_token_is_read_in_any_case_of_the_scheme() {
    for scheme in ["Bearer", "bearer", "BEARER"] {
        let map = headers(&[(header::AUTHORIZATION, &format!("{scheme} abc.def"))]);
        assert_eq!(
            read_credential(&map),
            Some(Credential::Bearer("abc.def".to_owned())),
            "{scheme}"
        );
    }
}

#[test]
fn another_scheme_is_ignored_entirely() {
    let map = headers(&[(header::AUTHORIZATION, "Basic dXNlcjpwdw==")]);
    assert_eq!(read_credential(&map), None);
}

#[test]
fn an_empty_bearer_value_is_ignored() {
    for value in ["Bearer   ", "Bearer ", "Bearer"] {
        let map = headers(&[(header::AUTHORIZATION, value)]);
        assert_eq!(read_credential(&map), None, "{value:?}");
    }
}

#[test]
fn the_session_cookie_is_read() {
    assert_eq!(
        read_credential(&cookie_header("abc.def")),
        Some(Credential::Cookie("abc.def".to_owned()))
    );
}

#[test]
fn an_empty_cookie_is_ignored() {
    assert_eq!(read_credential(&cookie_header("")), None);
    assert_eq!(
        read_credential(&headers(&[(header::COOKIE, "other=value")])),
        None
    );
}

/// An expired cookie left in a jar must not shadow a token the caller
/// deliberately attached.
#[test]
fn the_header_wins_over_the_cookie() {
    let mut map = cookie_header("from-cookie");
    map.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer from-header"),
    );
    let credential = read_credential(&map).unwrap();
    assert_eq!(credential, Credential::Bearer("from-header".to_owned()));
    assert_eq!(credential.token(), "from-header");
}

#[test]
fn a_cookie_credential_reports_its_own_token() {
    assert_eq!(
        read_credential(&cookie_header("abc.def")).unwrap().token(),
        "abc.def"
    );
}

// cookie_secure

#[test]
fn the_secure_flag_is_set_over_https_regardless_of_host() {
    assert!(cookie_secure("https", "127.0.0.1"));
    assert!(cookie_secure("HTTPS", "localhost"));
}

/// Safari refuses to store a `Secure` cookie over `http://`, localhost
/// included, so an unconditional flag would break login on the default bind.
#[test]
fn the_secure_flag_is_not_set_over_plain_http_to_loopback() {
    for host in ["127.0.0.1", "localhost", "::1", "[::1]"] {
        assert!(!cookie_secure("http", host), "{host}");
    }
}

/// The consequence — a plain-HTTP network bind cannot hold a session — is the
/// point, not a bug: that is a session cookie crossing a network in the clear.
#[test]
fn the_secure_flag_is_set_over_plain_http_to_anything_else() {
    for host in ["192.168.1.10", "ghost.local", "0.0.0.0"] {
        assert!(cookie_secure("http", host), "{host}");
    }
}

#[test]
fn the_scheme_and_host_come_off_the_request_with_the_port_stripped() {
    let parts = |uri: &str, host: Option<&str>| {
        let mut builder = Request::builder().uri(uri);
        if let Some(host) = host {
            builder = builder.header(header::HOST, host);
        }
        builder.body(()).unwrap().into_parts().0
    };

    assert_eq!(
        scheme_and_host(&parts("/api/health", Some("127.0.0.1:7071"))),
        ("http".to_owned(), "127.0.0.1".to_owned())
    );
    assert_eq!(
        scheme_and_host(&parts("/api/health", Some("[::1]:7071"))),
        ("http".to_owned(), "[::1]".to_owned())
    );
    assert_eq!(
        scheme_and_host(&parts("/api/health", Some("ghost.local"))),
        ("http".to_owned(), "ghost.local".to_owned())
    );
    // An absolute-form request line carries both, which is what a proxy sends.
    assert_eq!(
        scheme_and_host(&parts("https://ghost.local/api/health", None)),
        ("https".to_owned(), "ghost.local".to_owned())
    );
    assert_eq!(
        scheme_and_host(&parts("/api/health", None)),
        ("http".to_owned(), String::new())
    );
}

// the cookie itself

#[test]
fn the_session_cookie_carries_every_attribute_that_protects_it() {
    let value = session_cookie("abc.def", NOW + 60_000, NOW, true);
    assert!(value.starts_with("ghost_session=abc.def"), "{value}");
    assert!(value.contains("HttpOnly"), "{value}");
    assert!(value.contains("SameSite=Strict"), "{value}");
    assert!(value.contains("Secure"), "{value}");
    assert!(value.contains("Path=/"), "{value}");
    assert!(value.contains("Max-Age=60"), "{value}");
}

#[test]
fn the_secure_flag_is_left_off_when_the_caller_says_so() {
    let value = session_cookie("abc.def", NOW + 60_000, NOW, false);
    assert!(!value.contains("Secure"), "{value}");
}

/// A cookie whose lifetime rounds below zero is a cookie the browser deletes on
/// arrival, which is not what a login means to do.
#[test]
fn a_lifetime_that_has_already_passed_becomes_zero_not_a_negative_number() {
    let value = session_cookie("abc.def", NOW - 60_000, NOW, false);
    assert!(value.contains("Max-Age=0"), "{value}");
}

/// The attributes have to match the ones it was set with or the browser keeps
/// the original cookie and clears nothing.
#[test]
fn clearing_repeats_the_attributes_it_was_set_with() {
    let value = clear_session_cookie(true);
    assert!(value.starts_with("ghost_session="), "{value}");
    assert!(value.contains("HttpOnly"), "{value}");
    assert!(value.contains("SameSite=Strict"), "{value}");
    assert!(value.contains("Secure"), "{value}");
    assert!(value.contains("Path=/"), "{value}");
    assert!(value.contains("Max-Age=0"), "{value}");
}

// authenticate

#[test]
fn authentication_passes_through_when_it_is_off_for_the_whole_server() {
    let clock = Arc::new(ManualClock::at(NOW));
    let auth = store(&clock);
    let mut config = Config::default();
    config.server.auth.enabled = false;

    assert_eq!(
        authenticate(&config, &auth, &HeaderMap::new()).unwrap(),
        None
    );
}

#[test]
fn a_request_with_no_credential_is_refused_before_its_body_is_read() {
    let clock = Arc::new(ManualClock::at(NOW));
    let auth = store(&clock);
    let error = authenticate(&Config::default(), &auth, &HeaderMap::new()).unwrap_err();

    assert_eq!(error.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error.message, "Authentication required");
}

/// One message for a malformed token, an unknown one and an expired one.
/// Distinguishing them tells a caller which half of a guess was right.
#[test]
fn every_bad_credential_gets_the_same_answer() {
    let clock = Arc::new(ManualClock::at(NOW));
    let auth = store(&clock);
    let expired = auth.issue("web").unwrap();
    clock.advance(std::time::Duration::from_millis(60_001));

    for token in ["not-a-token", "nosuchid.secret", expired.token.as_str()] {
        let map = headers(&[(header::AUTHORIZATION, &format!("Bearer {token}"))]);
        let error = authenticate(&Config::default(), &auth, &map).unwrap_err();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED, "{token}");
        assert_eq!(
            error.message, "You have been signed out. Sign in again to continue.",
            "{token}"
        );
    }
}

#[test]
fn a_live_token_resolves_to_its_session_through_either_carrier() {
    let clock = Arc::new(ManualClock::at(NOW));
    let auth = store(&clock);
    let issued = auth.issue("web").unwrap();

    let by_header = headers(&[(header::AUTHORIZATION, &format!("Bearer {}", issued.token))]);
    let session = authenticate(&Config::default(), &auth, &by_header)
        .unwrap()
        .unwrap();
    assert_eq!(session.id, issued.id);
    assert_eq!(session.label, "web");

    let by_cookie = cookie_header(&issued.token);
    assert_eq!(
        authenticate(&Config::default(), &auth, &by_cookie)
            .unwrap()
            .unwrap()
            .id,
        issued.id
    );
}

// verify_signed

#[test]
fn a_signed_route_with_no_token_is_refused() {
    let clock = Arc::new(ManualClock::at(NOW));
    let auth = store(&clock);

    for token in [None, Some("")] {
        let error = verify_signed(&auth, token, clock.as_ref()).unwrap_err();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED);
        assert_eq!(error.message, "A signed URL is required");
    }
}

#[test]
fn a_signature_this_server_minted_resolves_to_its_claim() {
    let clock = Arc::new(ManualClock::at(NOW));
    let auth = store(&clock);
    let secret = auth.ensure_secret(MEDIA_SECRET_NAME).unwrap();
    let token = sign_media_token(
        &secret,
        &MediaClaim {
            path: "notes/photo.png".to_owned(),
            workspace_id: "default".to_owned(),
            expires_at_ms: NOW + 60_000,
        },
    );

    let claim = verify_signed(&auth, Some(&token), clock.as_ref()).unwrap();
    assert_eq!(claim.path, "notes/photo.png");
    assert_eq!(claim.workspace_id, "default");
}

/// A session is deliberately not accepted here: accepting a cookie as well
/// would make the signature optional, which is the same as not having one.
#[test]
fn a_forged_or_expired_signature_gets_one_answer() {
    let clock = Arc::new(ManualClock::at(NOW));
    let auth = store(&clock);
    let secret = auth.ensure_secret(MEDIA_SECRET_NAME).unwrap();
    let expired = sign_media_token(
        &secret,
        &MediaClaim {
            path: "notes/photo.png".to_owned(),
            workspace_id: "default".to_owned(),
            expires_at_ms: NOW,
        },
    );
    let forged = sign_media_token(
        "some other key",
        &MediaClaim {
            path: "notes/photo.png".to_owned(),
            workspace_id: "default".to_owned(),
            expires_at_ms: NOW + 60_000,
        },
    );

    for token in [expired.as_str(), forged.as_str(), "rubbish"] {
        let error = verify_signed(&auth, Some(token), clock.as_ref()).unwrap_err();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED, "{token}");
        assert_eq!(error.message, "Invalid or expired signed URL", "{token}");
    }
}

// the two extension slots

/// Separate storage and separate accessors: a route reading a session must not
/// be satisfied by a signature, and one reading a claim must not be satisfied
/// by a session.
#[test]
fn a_session_and_a_media_claim_do_not_satisfy_each_others_accessor() {
    let clock = Arc::new(ManualClock::at(NOW));
    let auth = store(&clock);
    let issued = auth.issue("web").unwrap();
    let session = auth.verify(&issued.token).unwrap().unwrap();
    let claim = MediaClaim {
        path: "notes/photo.png".to_owned(),
        workspace_id: "default".to_owned(),
        expires_at_ms: NOW + 60_000,
    };

    let mut extensions = axum::http::Extensions::new();
    assert!(session_of(&extensions).is_none());
    assert!(media_claim_of(&extensions).is_none());

    extensions.insert(claim.clone());
    assert!(session_of(&extensions).is_none());
    assert_eq!(media_claim_of(&extensions), Some(&claim));

    extensions.insert(session.clone());
    assert_eq!(session_of(&extensions), Some(&session));
    assert_eq!(media_claim_of(&extensions), Some(&claim));
}
