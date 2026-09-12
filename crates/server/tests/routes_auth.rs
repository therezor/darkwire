//! The login, the logout, and the three routes that claim an unclaimed
//! install.
//!
//! Two properties are asserted on almost every test here rather than once,
//! because they are the ones that would be silently lost by an otherwise
//! reasonable refactor: **the token is never in a response body**, and **a
//! refusal writes nothing**. The rest is statuses — and the statuses matter,
//! because the difference between a 400 and a 401 on these routes is the
//! difference between "there is nothing to log in to" and "your guess was
//! wrong", which is exactly what an attacker is trying to learn.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a harness that cannot stand up is a failing test either way"
)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use ghostai_core::testkit::ManualClock;
use ghostai_core::{Clock, Database};
use ghostai_protocol::config::Config;
use ghostai_server::app::{ServerOptions, create_server};
use ghostai_server::approvals::{HubApprovalGate, HubApprovalGateOptions};
use ghostai_server::auth::SESSION_COOKIE;
use ghostai_server::auth_store::AuthStore;
use ghostai_server::hub::{AgentResolution, SessionHub, SessionHubOptions};
use ghostai_server::runtime::ServerRuntime;
use ghostai_server::testkit::{
    CountingRandom, FakeHasher, FakeRuntime, FakeRuntimeOptions, NOW, TestServer,
    TestServerOptions, start_test_server,
};
use ghostai_server::ui::UiRoot;
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// The password `start_test_server` installs.
const PASSWORD: &str = "correct horse battery staple";

/// The login name an install starts with.
const USERNAME: &str = "ghost";

// Harnesses

fn claimed() -> TestServer {
    start_test_server(TestServerOptions::default()).expect("a test server")
}

fn claimed_with(config: Config) -> TestServer {
    start_test_server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    })
    .expect("a test server")
}

/// A server that has never been claimed.
///
/// Built here rather than through the testkit because `start_test_server`
/// always installs a password — which is right for every other route's tests
/// and is precisely the state the setup routes exist to leave.
struct Unclaimed {
    router: Router,
    auth: Arc<AuthStore>,
    /// Kept alive: the workspace tree and the shared connection outlive the
    /// router that reads them.
    _home: tempfile::TempDir,
    _database: Database,
}

fn unclaimed(config: Config) -> Unclaimed {
    let home = tempfile::tempdir().expect("a temporary home");
    let database = Database::in_memory().expect("an in-memory database");
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::at(NOW));

    let counter = std::sync::Mutex::new(0u64);
    let runtime = FakeRuntime::new(
        &database,
        home.path(),
        &clock,
        Box::new(move || {
            let mut next = counter
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *next += 1;
            format!("id-{next}")
        }),
        &FakeRuntimeOptions {
            config: Some(config.clone()),
            ..FakeRuntimeOptions::default()
        },
    )
    .expect("a fake runtime");

    let hub = SessionHub::new(SessionHubOptions {
        config: config.clone(),
        // Nothing is configured, which is the honest state of an install nobody
        // has finished setting up. Every frame but a turn still works.
        loop_for: Arc::new(|_| Ok(None)),
        resolve_agent_id: Arc::new(|agent_id| AgentResolution {
            agent_id: agent_id.unwrap_or("default").to_owned(),
            miss: None,
        }),
        store: runtime.store(),
        approvals: Arc::new(HubApprovalGate::new(HubApprovalGateOptions {
            clock: Some(Arc::clone(&clock)),
            ..HubApprovalGateOptions::default()
        })),
        clock: Some(Arc::clone(&clock)),
        new_id: None,
        max_queue_depth: None,
        max_sessions: None,
    });

    let built = create_server(ServerOptions {
        config,
        runtime: Arc::clone(&runtime) as Arc<dyn ServerRuntime>,
        hub,
        ui: UiRoot::None,
        database: database.clone(),
        scheduler: None,
        clock,
        random: Arc::new(CountingRandom::default()),
        // The whole point: no password is installed at boot.
        password: None,
        username: None,
        hasher: Some(Arc::new(FakeHasher)),
    })
    .expect("a server with no password");

    Unclaimed {
        router: built.router,
        auth: built.auth,
        _home: home,
        _database: database,
    }
}

fn auth_off() -> Config {
    let mut config = Config::default();
    config.server.auth.enabled = false;
    config
}

// Request helpers

struct Answer {
    status: StatusCode,
    body: String,
    cookies: Vec<String>,
    retry_after: Option<String>,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    /// The session cookie's value, or `None` when the response set none.
    fn session(&self) -> Option<String> {
        let prefix = format!("{SESSION_COOKIE}=");
        let raw = self
            .cookies
            .iter()
            .find(|cookie| cookie.starts_with(&prefix))?;
        let value = raw
            .split(';')
            .next()?
            .strip_prefix(&prefix)?
            .trim()
            .to_owned();
        if value.is_empty() { None } else { Some(value) }
    }

    /// The raw `Set-Cookie` line for the session, attributes included.
    fn session_cookie_line(&self) -> Option<&String> {
        let prefix = format!("{SESSION_COOKIE}=");
        self.cookies
            .iter()
            .find(|cookie| cookie.starts_with(&prefix))
    }
}

async fn send(router: &Router, request: Request<Body>) -> Answer {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    let status = response.status();
    let cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_owned))
        .collect();
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("a body");
    Answer {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
        cookies,
        retry_after,
    }
}

fn post(path: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("a well-formed request")
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("a well-formed request")
}

fn with_cookie(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::COOKIE,
        format!("{SESSION_COOKIE}={token}")
            .parse()
            .expect("a header value"),
    );
    request
}

fn with_bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().expect("a header value"),
    );
    request
}

// Login

#[tokio::test]
async fn a_login_exchanges_the_password_for_a_session_cookie() {
    let test = claimed();
    let answer = send(
        &test.router,
        post(
            "/api/auth/login",
            &json!({ "username": USERNAME, "password": PASSWORD }),
        ),
    )
    .await;

    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["ok"], true);
    assert!(answer.json()["expiresAtMs"].is_number());

    let line = answer.session_cookie_line().expect("a session cookie");
    assert!(line.contains("HttpOnly"), "{line}");
    // `SameSite=Strict` is what stands in for a CSRF token: the cookie is
    // simply not attached to a cross-site request.
    assert!(line.contains("SameSite=Strict"), "{line}");
    assert!(line.contains("Path=/"), "{line}");
}

#[tokio::test]
async fn a_login_never_puts_the_token_or_the_password_in_the_body() {
    let test = claimed();
    let answer = send(
        &test.router,
        post(
            "/api/auth/login",
            &json!({ "username": USERNAME, "password": PASSWORD }),
        ),
    )
    .await;

    let token = answer.session().expect("a session cookie");
    // A body a browser can read is a body an injected script can read, and this
    // application's whole job is rendering markdown a language model wrote.
    assert!(!answer.body.contains(&token), "{}", answer.body);
    assert!(!answer.body.contains(PASSWORD), "{}", answer.body);
}

#[tokio::test]
async fn the_cookie_a_login_issues_then_authenticates() {
    let test = claimed();
    let login = send(
        &test.router,
        post(
            "/api/auth/login",
            &json!({ "username": USERNAME, "password": PASSWORD }),
        ),
    )
    .await;
    let token = login.session().expect("a session cookie");

    let me = send(&test.router, with_cookie(get("/api/auth/me"), &token)).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json()["authenticated"], true);
    assert_eq!(me.json()["authEnabled"], true);
    assert_eq!(me.json()["username"], USERNAME);
    assert!(me.json()["expiresAtMs"].is_number());
}

#[tokio::test]
async fn a_bearer_token_wins_over_a_cookie() {
    let test = claimed();
    // An expired or revoked cookie left in a jar must not shadow a token the
    // caller deliberately attached.
    let request = with_bearer(
        with_cookie(get("/api/auth/me"), "not-a-session"),
        &test.token,
    );
    assert_eq!(send(&test.router, request).await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_wrong_password_is_a_refusal_with_no_cookie() {
    let test = claimed();
    let answer = send(
        &test.router,
        post(
            "/api/auth/login",
            &json!({ "username": USERNAME, "password": "wrong" }),
        ),
    )
    .await;

    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    assert_eq!(answer.json()["error"]["code"], "unauthorized");
    assert!(answer.cookies.is_empty());
}

#[tokio::test]
async fn an_unknown_username_is_refused_in_the_same_words_as_a_wrong_password() {
    let test = claimed();
    let wrong_name = send(
        &test.router,
        post(
            "/api/auth/login",
            &json!({ "username": "nobody", "password": PASSWORD }),
        ),
    )
    .await;
    let wrong_password = send(
        &test.router,
        post(
            "/api/auth/login",
            &json!({ "username": USERNAME, "password": "wrong" }),
        ),
    )
    .await;

    // Naming which half failed hands over the other half.
    assert_eq!(wrong_name.status, wrong_password.status);
    assert_eq!(
        wrong_name.json()["error"]["message"],
        wrong_password.json()["error"]["message"]
    );
}

#[tokio::test]
async fn a_body_that_fails_the_schema_points_at_the_field() {
    let test = claimed();
    let answer = send(
        &test.router,
        post(
            "/api/auth/login",
            &json!({ "username": USERNAME, "password": 42 }),
        ),
    )
    .await;

    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(answer.json()["error"]["code"], "bad_request");
    let details = answer.json()["error"]["details"].clone();
    let keys: Vec<String> = details
        .as_object()
        .expect("details")
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, ["/password"]);
}

#[tokio::test]
async fn a_malformed_document_is_a_bad_request_rather_than_a_schema_failure() {
    let test = claimed();
    let request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .expect("a well-formed request");

    // The caller has to fix the request itself, which is a different repair
    // from "this field is wrong".
    assert_eq!(
        send(&test.router, request).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn a_credential_body_over_the_cap_is_refused_before_it_is_parsed() {
    let test = claimed();
    let request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "username": USERNAME,
                "password": "x".repeat(8192),
            })
            .to_string(),
        ))
        .expect("a well-formed request");

    // Every byte above the cap is one an unauthenticated caller can make the
    // server buffer before anything has decided whether to talk to them.
    assert_eq!(
        send(&test.router, request).await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn a_login_is_refused_when_authentication_is_disabled() {
    let test = claimed_with(auth_off());
    let answer = send(
        &test.router,
        post(
            "/api/auth/login",
            &json!({ "username": USERNAME, "password": PASSWORD }),
        ),
    )
    .await;

    // Not a 401: the credential is not wrong, there is nothing to log in to.
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert!(
        answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("disabled"),
        "{}",
        answer.body
    );
}

// Logout, and who the caller is

#[tokio::test]
async fn a_logout_revokes_the_session_it_was_given() {
    let test = claimed();
    let answer = send(
        &test.router,
        with_bearer(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .body(Body::empty())
                .expect("a well-formed request"),
            &test.token,
        ),
    )
    .await;

    // 204 rather than a body: there is nothing to say, and inventing a shape
    // for it would put a schema in the protocol that exists only to be ignored.
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    assert!(answer.body.is_empty());
    // The clearing cookie has to match the attributes it was set with, or the
    // browser keeps the original and clears nothing.
    let line = answer.session_cookie_line().expect("a clearing cookie");
    assert!(line.contains("Max-Age=0"), "{line}");

    let me = send(&test.router, with_bearer(get("/api/auth/me"), &test.token)).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_logout_without_a_session_is_still_answered_when_authentication_is_off() {
    let test = claimed_with(auth_off());
    let answer = send(
        &test.router,
        Request::builder()
            .method("POST")
            .uri("/api/auth/logout")
            .body(Body::empty())
            .expect("a well-formed request"),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn who_am_i_names_no_account_when_authentication_is_off() {
    let test = claimed_with(auth_off());
    let answer = send(&test.router, get("/api/auth/me")).await;

    // With authentication off there is no account, and reporting the name of
    // one would describe a login that does not exist.
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        answer.json(),
        json!({ "authenticated": true, "authEnabled": false })
    );
}

// Setup status

#[tokio::test]
async fn an_unclaimed_install_reports_that_it_needs_claiming() {
    let server = unclaimed(Config::default());
    let answer = send(&server.router, get("/api/setup")).await;

    assert_eq!(answer.status, StatusCode::OK);
    // One bit and nothing else. An unauthenticated caller learns this anyway by
    // watching every login fail; saying more would be telling whoever asked
    // first what they had found.
    assert_eq!(answer.json(), json!({ "required": true }));
}

#[tokio::test]
async fn a_claimed_install_reports_that_it_does_not() {
    let test = claimed();
    assert_eq!(
        send(&test.router, get("/api/setup")).await.json(),
        json!({ "required": false })
    );
}

#[tokio::test]
async fn an_install_with_authentication_off_has_nothing_to_claim() {
    let server = unclaimed(auth_off());
    // The server is reachable without a credential by design, and asking for a
    // password that would never be checked is a login form and nothing more.
    assert_eq!(
        send(&server.router, get("/api/setup")).await.json(),
        json!({ "required": false })
    );
}

// Claiming

#[tokio::test]
async fn the_one_time_code_buys_a_session_that_works() {
    let server = unclaimed(Config::default());
    let code = server.auth.issue_setup_code().expect("a setup code");

    let claim = send(
        &server.router,
        post("/api/setup/claim", &json!({ "code": code })),
    )
    .await;
    assert_eq!(claim.status, StatusCode::OK);

    let token = claim.session().expect("a session cookie");
    // The code is a login, so it gets the same cookie treatment: a token in the
    // body is a token an injected script can read.
    assert!(!claim.body.contains(&token), "{}", claim.body);
    assert!(!claim.body.contains(&code), "{}", claim.body);

    let me = send(&server.router, with_cookie(get("/api/auth/me"), &token)).await;
    assert_eq!(me.status, StatusCode::OK);
}

#[tokio::test]
async fn a_wrong_code_is_refused_and_sets_no_cookie() {
    let server = unclaimed(Config::default());
    server.auth.issue_setup_code().expect("a setup code");

    let answer = send(
        &server.router,
        post("/api/setup/claim", &json!({ "code": "ZZZZ-ZZZZ-ZZZZ" })),
    )
    .await;

    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    assert!(answer.cookies.is_empty());
}

#[tokio::test]
async fn a_spent_code_is_refused_in_the_same_words_as_a_wrong_one() {
    let server = unclaimed(Config::default());
    let code = server.auth.issue_setup_code().expect("a setup code");

    let first = send(
        &server.router,
        post("/api/setup/claim", &json!({ "code": code.clone() })),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK);

    let again = send(
        &server.router,
        post("/api/setup/claim", &json!({ "code": code })),
    )
    .await;
    assert_eq!(again.status, StatusCode::UNAUTHORIZED);
    assert!(again.cookies.is_empty());
}

#[tokio::test]
async fn a_claimed_install_refuses_a_claim_rather_than_calling_it_wrong() {
    let test = claimed();
    let answer = send(
        &test.router,
        post("/api/setup/claim", &json!({ "code": "AAAA-BBBB-CCCC" })),
    )
    .await;

    // Not a 401: the code is not wrong, the install is claimed and the caller
    // should be signing in with the password.
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert!(
        answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("already has a password"),
        "{}",
        answer.body
    );
}

#[tokio::test]
async fn a_claim_is_refused_when_authentication_is_disabled() {
    let server = unclaimed(auth_off());
    let answer = send(
        &server.router,
        post("/api/setup/claim", &json!({ "code": "AAAA-BBBB-CCCC" })),
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

// Setting and rotating the password

#[tokio::test]
async fn finishing_the_wizard_keeps_the_caller_signed_in() {
    let server = unclaimed(Config::default());
    let code = server.auth.issue_setup_code().expect("a setup code");
    let claimed_token = send(
        &server.router,
        post("/api/setup/claim", &json!({ "code": code })),
    )
    .await
    .session()
    .expect("a session cookie");

    let set = send(
        &server.router,
        with_cookie(
            post(
                "/api/setup/password",
                &json!({ "password": "chosen-in-the-wizard" }),
            ),
            &claimed_token,
        ),
    )
    .await;
    assert_eq!(set.status, StatusCode::OK);

    // Setting a password revokes every session including the caller's own, so
    // without a re-issue the browser is signed out mid-wizard with the code it
    // would need to get back in already spent.
    let reissued = set.session().expect("a re-issued session cookie");
    assert_ne!(reissued, claimed_token);

    assert_eq!(
        send(&server.router, with_cookie(get("/api/auth/me"), &reissued))
            .await
            .status,
        StatusCode::OK
    );
    // And the old one is genuinely dead.
    assert_eq!(
        send(
            &server.router,
            with_cookie(get("/api/auth/me"), &claimed_token)
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn setup_closes_once_the_password_is_set() {
    let server = unclaimed(Config::default());
    let code = server.auth.issue_setup_code().expect("a setup code");
    let token = send(
        &server.router,
        post("/api/setup/claim", &json!({ "code": code })),
    )
    .await
    .session()
    .expect("a session cookie");

    send(
        &server.router,
        with_cookie(
            post(
                "/api/setup/password",
                &json!({ "password": "chosen-in-the-wizard" }),
            ),
            &token,
        ),
    )
    .await;

    assert_eq!(
        send(&server.router, get("/api/setup")).await.json(),
        json!({ "required": false })
    );
}

#[tokio::test]
async fn a_session_alone_cannot_rotate_a_password_that_already_exists() {
    let test = claimed();

    let missing = send(
        &test.router,
        with_bearer(
            post(
                "/api/setup/password",
                &json!({ "password": "chosen-in-the-panel" }),
            ),
            &test.token,
        ),
    )
    .await;
    // The cookie is `HttpOnly`, but this application renders markdown a model
    // wrote, and the failure being closed here is an injection that changes the
    // password and locks the operator out of their own agent.
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert!(
        missing.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("current password"),
        "{}",
        missing.body
    );

    let wrong = send(
        &test.router,
        with_bearer(
            post(
                "/api/setup/password",
                &json!({
                    "password": "chosen-in-the-panel",
                    "currentPassword": "not-the-old-one",
                }),
            ),
            &test.token,
        ),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);

    // The old password still works, because neither refusal wrote anything.
    let login = send(
        &test.router,
        post(
            "/api/auth/login",
            &json!({ "username": USERNAME, "password": PASSWORD }),
        ),
    )
    .await;
    assert_eq!(login.status, StatusCode::OK);
}

#[tokio::test]
async fn a_rotation_moves_the_password_and_the_name_together() {
    let test = claimed();
    let answer = send(
        &test.router,
        with_bearer(
            post(
                "/api/setup/password",
                &json!({
                    "username": "Operator",
                    "password": "chosen-in-the-panel",
                    "currentPassword": PASSWORD,
                }),
            ),
            &test.token,
        ),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);

    // The new credential works, under the normalised name…
    assert_eq!(
        send(
            &test.router,
            post(
                "/api/auth/login",
                &json!({ "username": "operator", "password": "chosen-in-the-panel" }),
            ),
        )
        .await
        .status,
        StatusCode::OK
    );
    // …and the old name does not, even with the new password.
    assert_eq!(
        send(
            &test.router,
            post(
                "/api/auth/login",
                &json!({ "username": USERNAME, "password": "chosen-in-the-panel" }),
            ),
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_password_below_the_minimum_never_reaches_the_store() {
    let test = claimed();
    let answer = send(
        &test.router,
        with_bearer(
            post(
                "/api/setup/password",
                &json!({ "password": "short", "currentPassword": PASSWORD }),
            ),
            &test.token,
        ),
    )
    .await;

    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
    let keys: Vec<String> = answer.json()["error"]["details"]
        .as_object()
        .expect("details")
        .keys()
        .cloned()
        .collect();
    // `/password/0`, not `/password`: the password is a transparent newtype, so
    // the rule that refused it is declared on the tuple's single field and the
    // pointer names it. A *shape* failure on the same field — a number where a
    // string belongs — is keyed `/password`, because serde walks the wire
    // document rather than the Rust type. Both are true statements about where
    // the failure is; they are simply about different trees.
    assert_eq!(keys, ["/password/0"]);
}

#[tokio::test]
async fn setting_a_password_is_refused_when_authentication_is_disabled() {
    let test = claimed_with(auth_off());
    let answer = send(
        &test.router,
        post(
            "/api/setup/password",
            &json!({ "password": "chosen-in-the-panel" }),
        ),
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
}

// The throttle

#[tokio::test]
async fn repeated_wrong_passwords_earn_a_delay_that_says_how_long() {
    let test = claimed();
    let mut statuses = Vec::new();
    for _ in 0..6 {
        let answer = send(
            &test.router,
            post(
                "/api/auth/login",
                &json!({ "username": USERNAME, "password": "wrong" }),
            ),
        )
        .await;
        statuses.push((answer.status, answer.retry_after.clone()));
    }

    let throttled = statuses
        .iter()
        .find(|(status, _)| *status == StatusCode::TOO_MANY_REQUESTS)
        .expect("the throttle eventually refuses");
    // `Retry-After` is the whole of what a well-behaved client needs from this
    // answer, and it is in whole seconds, rounded up — rounding down would send
    // them back at a moment the throttle still refuses.
    let seconds: i64 = throttled
        .1
        .as_deref()
        .expect("a Retry-After header")
        .parse()
        .expect("whole seconds");
    assert!(seconds >= 1, "{seconds}");

    // The first few attempts are the ones a person mistypes, and they are not
    // delayed at all.
    assert_eq!(statuses[0].0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_throttled_caller_is_not_told_which_bucket_refused() {
    let test = claimed();
    let mut message = String::new();
    for _ in 0..6 {
        let answer = send(
            &test.router,
            post(
                "/api/auth/login",
                &json!({ "username": USERNAME, "password": "wrong" }),
            ),
        )
        .await;
        if answer.status == StatusCode::TOO_MANY_REQUESTS {
            message = answer.json()["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            break;
        }
    }

    assert!(message.contains("Too many attempts"), "{message}");
    // "Your address is locked out" and "the account is locked out" tell an
    // attacker whether rotating through a botnet is working.
    assert!(!message.contains("account"), "{message}");
    assert!(!message.contains("ip:"), "{message}");
}

#[tokio::test]
async fn the_right_password_clears_the_count() {
    let test = claimed();
    for _ in 0..3 {
        send(
            &test.router,
            post(
                "/api/auth/login",
                &json!({ "username": USERNAME, "password": "wrong" }),
            ),
        )
        .await;
    }
    assert_eq!(
        send(
            &test.router,
            post(
                "/api/auth/login",
                &json!({ "username": USERNAME, "password": PASSWORD }),
            ),
        )
        .await
        .status,
        StatusCode::OK
    );

    // A successful login forgets the failures, so the next mistype starts from
    // nothing rather than from a budget the operator already spent.
    assert_eq!(
        send(
            &test.router,
            post(
                "/api/auth/login",
                &json!({ "username": USERNAME, "password": "wrong" }),
            ),
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_claim_and_a_login_share_one_budget() {
    let server = unclaimed(Config::default());
    server.auth.issue_setup_code().expect("a setup code");

    // A code is a credential for exactly the same account, so guesses at it
    // count against the same aggregate — two counters would give an attacker
    // two budgets.
    let mut throttled = false;
    for _ in 0..6 {
        let answer = send(
            &server.router,
            post("/api/setup/claim", &json!({ "code": "ZZZZ-ZZZZ-ZZZZ" })),
        )
        .await;
        if answer.status == StatusCode::TOO_MANY_REQUESTS {
            throttled = true;
            break;
        }
    }
    assert!(throttled, "the claim route is throttled like a login");
}
