//! The OAuth flow against a mock authorization server.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use ghostai_core::testkit::ManualClock;
use ghostai_core::{Clock, ErrorKind};
use ghostai_mcp::{
    ClientInformation, EndpointGuard, InvalidationScope, McpSecretSlot, McpSecretStore,
    MemorySecretStore, OAuthFlow, OAuthFlowOptions, StoredTokens,
};
use ghostai_protocol::McpOAuthConfig;
use ghostai_security::testkit::{FixedRandom, StaticResolver};
use ghostai_security::{DnsResolver, NetworkPolicy};
use parking_lot::Mutex;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NOW_MS: i64 = 1_700_000_000_000;

fn config(auth: &str, token: &str, client_id: &str, scopes: &[&str]) -> McpOAuthConfig {
    serde_json::from_value(json!({
        "authUrl": auth,
        "tokenUrl": token,
        "clientId": client_id,
        "scopes": scopes,
    }))
    .unwrap()
}

struct Built {
    flow: OAuthFlow,
    store: Arc<MemorySecretStore>,
    seen: Arc<Mutex<Vec<String>>>,
    clock: Arc<ManualClock>,
}

fn build(config: McpOAuthConfig, guard: Option<Arc<EndpointGuard>>) -> Built {
    let store = Arc::new(MemorySecretStore::new());
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(ManualClock::at(NOW_MS));
    let recorder = Arc::clone(&seen);
    let flow = OAuthFlow::new(OAuthFlowOptions {
        server_id: "github".to_owned(),
        config,
        store: Arc::clone(&store) as Arc<dyn McpSecretStore>,
        redirect_url: "http://127.0.0.1:33418/mcp/callback".to_owned(),
        state: "abc123".to_owned(),
        http: reqwest::Client::new(),
        random: Arc::new(FixedRandom::constant(7)),
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        guard,
        on_authorization_required: Arc::new(move |url: &str| recorder.lock().push(url.to_owned())),
    });
    Built {
        flow,
        store,
        seen,
        clock,
    }
}

fn standard() -> Built {
    build(
        config(
            "https://auth.test/authorize",
            "https://auth.test/token",
            "configured-id",
            &["read", "write"],
        ),
        None,
    )
}

fn tokens(access: &str, refresh: Option<&str>, expires_at_ms: Option<i64>) -> StoredTokens {
    StoredTokens {
        access_token: access.to_owned(),
        token_type: "bearer".to_owned(),
        refresh_token: refresh.map(str::to_owned),
        expires_at_ms,
        scope: None,
    }
}

#[test]
fn describes_ghostai_as_a_public_client_using_pkce() {
    // Nowhere to keep a client secret the operator cannot already read, which
    // is what `token_endpoint_auth_method: none` says out loud.
    let built = standard();
    let metadata = built.flow.client_metadata();
    assert_eq!(metadata.client_name, "GhostAI");
    assert_eq!(
        metadata.redirect_uris,
        ["http://127.0.0.1:33418/mcp/callback"]
    );
    assert_eq!(metadata.token_endpoint_auth_method, "none");
    assert_eq!(
        metadata.grant_types,
        ["authorization_code", "refresh_token"]
    );
    assert_eq!(metadata.scope.as_deref(), Some("read write"));
    let wire = serde_json::to_value(&metadata).unwrap();
    assert!(wire.get("scope").is_some());
}

#[test]
fn omits_the_scope_entirely_when_none_is_configured() {
    let bare = build(
        config("https://auth.test/a", "https://auth.test/t", "x", &[]),
        None,
    );
    let wire = serde_json::to_value(bare.flow.client_metadata()).unwrap();
    assert!(wire.get("scope").is_none());
}

#[test]
fn routes_a_redirect_back_with_the_state_it_was_given() {
    let built = standard();
    assert_eq!(built.flow.state(), "abc123");
    assert_eq!(
        built.flow.redirect_url(),
        "http://127.0.0.1:33418/mcp/callback"
    );
    assert_eq!(built.flow.server_id(), "github");
}

#[test]
fn falls_back_to_the_configured_client_id_until_one_is_registered() {
    let built = standard();
    assert_eq!(
        built.flow.client_information(),
        Some(ClientInformation {
            client_id: "configured-id".to_owned(),
            client_secret: None
        })
    );

    built
        .flow
        .save_client_information(&ClientInformation {
            client_id: "issued-id".to_owned(),
            client_secret: None,
        })
        .unwrap();
    // Dynamic registration wins: that is the identity the server knows us by.
    assert_eq!(
        built.flow.client_information().unwrap().client_id,
        "issued-id"
    );
    assert!(
        built
            .store
            .read("github", McpSecretSlot::Client)
            .unwrap()
            .contains("issued-id")
    );
}

#[test]
fn has_no_client_information_when_nothing_is_configured_or_registered() {
    let bare = build(
        config("https://auth.test/a", "https://auth.test/t", "  ", &[]),
        None,
    );
    assert_eq!(bare.flow.client_information(), None);
}

#[test]
fn round_trips_tokens_through_the_store() {
    let built = standard();
    assert!(built.flow.tokens().is_none());

    built.flow.save_tokens(&tokens("at", None, None)).unwrap();
    assert_eq!(built.flow.tokens().unwrap().access_token, "at");
    // In the vault, never in `config.yaml`: this is a credential this process
    // obtained rather than a setting somebody typed.
    assert!(
        built
            .store
            .read("github", McpSecretSlot::Tokens)
            .unwrap()
            .contains("at")
    );
}

#[test]
fn treats_an_unreadable_stored_value_as_absent_so_the_flow_can_restart() {
    let built = standard();
    built
        .store
        .write("github", McpSecretSlot::Tokens, "not json")
        .unwrap();
    assert!(built.flow.tokens().is_none());
}

#[test]
fn keeps_the_pkce_verifier_in_memory_only() {
    let built = standard();
    let error = built.flow.code_verifier().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(error.message.contains("No PKCE verifier"));

    built.flow.save_code_verifier("verifier".to_owned());
    assert_eq!(built.flow.code_verifier().unwrap(), "verifier");
    // Valid for one exchange, over in seconds. Persisting it would leave a
    // credential on disk with no remaining purpose.
    assert!(built.store.read("github", McpSecretSlot::Tokens).is_none());
    assert!(built.store.read("github", McpSecretSlot::Client).is_none());
}

#[test]
fn hands_the_authorization_url_over_instead_of_opening_anything() {
    let built = standard();
    built
        .flow
        .report_authorization_url(&"https://auth.test/authorize?x=1".parse().unwrap());
    // A headless server that shells out to `open` fails where nobody can see.
    assert_eq!(*built.seen.lock(), ["https://auth.test/authorize?x=1"]);
}

#[test]
fn clears_exactly_the_credentials_it_is_told_are_no_longer_good() {
    let built = standard();
    built.flow.save_tokens(&tokens("at", None, None)).unwrap();
    built
        .flow
        .save_client_information(&ClientInformation {
            client_id: "issued".to_owned(),
            client_secret: None,
        })
        .unwrap();
    built.flow.save_code_verifier("verifier".to_owned());

    built
        .flow
        .invalidate_credentials(InvalidationScope::Tokens)
        .unwrap();
    assert!(built.store.read("github", McpSecretSlot::Tokens).is_none());
    assert!(built.store.read("github", McpSecretSlot::Client).is_some());

    built
        .flow
        .invalidate_credentials(InvalidationScope::Verifier)
        .unwrap();
    assert!(built.flow.code_verifier().is_err());

    built.flow.save_code_verifier("again".to_owned());
    built
        .flow
        .invalidate_credentials(InvalidationScope::Discovery)
        .unwrap();
    assert!(built.flow.code_verifier().is_err());

    built
        .flow
        .invalidate_credentials(InvalidationScope::Client)
        .unwrap();
    assert!(built.store.read("github", McpSecretSlot::Client).is_none());

    built.flow.save_tokens(&tokens("at", None, None)).unwrap();
    built
        .flow
        .invalidate_credentials(InvalidationScope::All)
        .unwrap();
    assert!(built.store.read("github", McpSecretSlot::Tokens).is_none());
}

#[tokio::test]
async fn hands_back_a_fresh_access_token_without_touching_the_network() {
    let built = standard();
    built
        .flow
        .save_tokens(&tokens("fresh", Some("r"), Some(NOW_MS + 3_600_000)))
        .unwrap();
    let token = built
        .flow
        .access_token("http://127.0.0.1:9/mcp")
        .await
        .unwrap();
    assert_eq!(token.as_deref(), Some("fresh"));

    let stateless = standard();
    stateless
        .flow
        .save_tokens(&tokens("forever", None, None))
        .unwrap();
    assert_eq!(
        stateless
            .flow
            .access_token("http://127.0.0.1:9/mcp")
            .await
            .unwrap()
            .as_deref(),
        Some("forever")
    );
}

#[tokio::test]
async fn has_no_token_for_a_server_that_never_authorized() {
    let built = standard();
    assert_eq!(
        built
            .flow
            .access_token("http://127.0.0.1:9/mcp")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn drops_an_expired_token_that_cannot_be_refreshed() {
    let built = standard();
    built
        .flow
        .save_tokens(&tokens("stale", None, Some(NOW_MS - 1)))
        .unwrap();
    assert_eq!(
        built
            .flow
            .access_token("http://127.0.0.1:9/mcp")
            .await
            .unwrap(),
        None
    );
    assert!(built.flow.tokens().is_none());
}

#[tokio::test]
async fn refreshes_an_expiring_token_at_the_configured_endpoint_when_discovery_finds_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=old-refresh"))
        .and(body_string_contains("client_id=configured-id"))
        .and(body_string_contains("resource="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access",
            "token_type": "Bearer",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    let built = build(
        config(
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            "configured-id",
            &["read"],
        ),
        None,
    );
    // Inside the skew window counts as expired: a call that starts just under
    // the line must not fail just over it.
    built
        .flow
        .save_tokens(&tokens(
            "old-access",
            Some("old-refresh"),
            Some(NOW_MS + 30_000),
        ))
        .unwrap();

    let token = built
        .flow
        .access_token(&format!("{}/mcp", server.uri()))
        .await
        .unwrap();
    assert_eq!(token.as_deref(), Some("new-access"));
    let stored = built.flow.tokens().unwrap();
    // A server that rotates nothing leaves the refresh token that worked.
    assert_eq!(stored.refresh_token.as_deref(), Some("old-refresh"));
    assert_eq!(stored.expires_at_ms, Some(NOW_MS + 3_600_000));
    assert_eq!(stored.token_type, "bearer");
}

#[tokio::test]
async fn a_refused_refresh_clears_the_tokens_so_authorization_can_restart() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })))
        .mount(&server)
        .await;
    let built = build(
        config(
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            "configured-id",
            &[],
        ),
        None,
    );
    built
        .flow
        .save_tokens(&tokens("old", Some("dead"), Some(NOW_MS - 1)))
        .unwrap();

    assert_eq!(
        built
            .flow
            .access_token(&format!("{}/mcp", server.uri()))
            .await
            .unwrap(),
        None
    );
    assert!(built.flow.tokens().is_none());
}

#[tokio::test]
async fn a_refresh_that_never_reaches_the_server_keeps_the_tokens() {
    // Port 9 is discard; nothing answers. A transient failure must not cost the
    // operator a refresh token that may still be good.
    let built = build(
        config(
            "http://127.0.0.1:9/authorize",
            "http://127.0.0.1:9/token",
            "configured-id",
            &[],
        ),
        None,
    );
    built
        .flow
        .save_tokens(&tokens("old", Some("maybe-good"), Some(NOW_MS - 1)))
        .unwrap();
    let error = built
        .flow
        .access_token("http://127.0.0.1:9/mcp")
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(built.flow.tokens().is_some());
}

#[tokio::test]
async fn discovers_registers_and_builds_the_authorization_link() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": server.uri(),
            "authorization_endpoint": format!("{}/discovered/authorize", server.uri()),
            "token_endpoint": format!("{}/discovered/token", server.uri()),
            "registration_endpoint": format!("{}/register", server.uri())
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/register"))
        .and(body_string_contains("\"client_name\":\"GhostAI\""))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "client_id": "issued-id" })))
        .mount(&server)
        .await;

    // No configured client id: the server has to issue one.
    let built = build(
        config(
            "https://typed.test/authorize",
            "https://typed.test/token",
            " ",
            &["read"],
        ),
        None,
    );
    let resource = format!("{}/mcp", server.uri());
    let url = built.flow.begin_authorization(&resource).await.unwrap();

    assert!(
        url.as_str()
            .starts_with(&format!("{}/discovered/authorize?", server.uri()))
    );
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "issued-id");
    assert_eq!(query["state"], "abc123");
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["redirect_uri"], "http://127.0.0.1:33418/mcp/callback");
    assert_eq!(query["scope"], "read");
    assert_eq!(query["resource"], resource);
    assert!(!query["code_challenge"].is_empty());
    // The link went to the operator, not to a browser.
    assert_eq!(*built.seen.lock(), [url.to_string()]);
    // The verifier is outstanding, in memory; the client id is in the store.
    assert!(built.flow.code_verifier().is_ok());
    assert_eq!(
        built.flow.client_information().unwrap().client_id,
        "issued-id"
    );
}

#[tokio::test]
async fn refuses_a_server_that_needs_oauth_but_offers_neither_a_client_id_nor_registration() {
    let server = MockServer::start().await;
    let built = build(
        config(
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            "",
            &[],
        ),
        None,
    );
    let error = built
        .flow
        .begin_authorization(&format!("{}/mcp", server.uri()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("no registration"));
}

#[tokio::test]
async fn exchanges_the_code_with_the_verifier_and_stores_what_came_back() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains("code_verifier="))
        .and(body_string_contains("redirect_uri="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at",
            "token_type": "Bearer",
            "refresh_token": "rt",
            "expires_in": 3600,
            "scope": "read write"
        })))
        .mount(&server)
        .await;

    let built = build(
        config(
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            "configured-id",
            &["read", "write"],
        ),
        None,
    );
    let resource = format!("{}/mcp", server.uri());
    built.flow.begin_authorization(&resource).await.unwrap();
    built
        .flow
        .finish_authorization(&resource, "the-code")
        .await
        .unwrap();

    let stored = built.flow.tokens().unwrap();
    assert_eq!(stored.access_token, "at");
    assert_eq!(stored.refresh_token.as_deref(), Some("rt"));
    assert_eq!(stored.scope.as_deref(), Some("read write"));
    assert_eq!(stored.expires_at_ms, Some(NOW_MS + 3_600_000));
    // The verifier was for one exchange.
    assert!(built.flow.code_verifier().is_err());
    // And the token is now what the connector gets, with no second exchange.
    built.clock.advance(std::time::Duration::from_secs(1));
    assert_eq!(
        built.flow.access_token(&resource).await.unwrap().as_deref(),
        Some("at")
    );
}

#[tokio::test]
async fn a_refused_exchange_is_permission_denied() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })))
        .mount(&server)
        .await;
    let built = build(
        config(
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            "configured-id",
            &[],
        ),
        None,
    );
    let resource = format!("{}/mcp", server.uri());
    built.flow.begin_authorization(&resource).await.unwrap();
    let error = built
        .flow
        .finish_authorization(&resource, "bad")
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::PermissionDenied);
}

#[tokio::test]
async fn finishing_without_a_verifier_is_a_conflict() {
    let built = standard();
    let error = built
        .flow
        .finish_authorization("http://127.0.0.1:9/mcp", "code")
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
}

#[tokio::test]
async fn follows_protected_resource_metadata_to_another_issuer_path_aware_first() {
    let server = MockServer::start().await;
    // RFC 9728 on the resource's own path names the authorization server.
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_servers": [format!("{}/issuer", server.uri())]
        })))
        .mount(&server)
        .await;
    // RFC 8414 at the issuer, path-aware.
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server/issuer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_endpoint": format!("{}/issuer/authorize", server.uri()),
            "token_endpoint": format!("{}/issuer/token", server.uri())
        })))
        .mount(&server)
        .await;

    let built = build(
        config(
            "https://typed.test/a",
            "https://typed.test/t",
            "configured-id",
            &[],
        ),
        None,
    );
    let url = built
        .flow
        .begin_authorization(&format!("{}/mcp", server.uri()))
        .await
        .unwrap();
    assert!(
        url.as_str()
            .starts_with(&format!("{}/issuer/authorize?", server.uri()))
    );
}

#[tokio::test]
async fn an_unreadable_metadata_document_falls_back_to_the_configured_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let built = build(
        config(
            &format!("{}/typed/authorize", server.uri()),
            &format!("{}/typed/token", server.uri()),
            "configured-id",
            &[],
        ),
        None,
    );
    let url = built
        .flow
        .begin_authorization(&format!("{}/mcp", server.uri()))
        .await
        .unwrap();
    assert!(
        url.as_str()
            .starts_with(&format!("{}/typed/authorize?", server.uri()))
    );
}

#[tokio::test]
async fn a_configured_endpoint_that_is_not_a_url_is_a_config_error() {
    let built = build(
        config("nope", "https://auth.test/t", "configured-id", &[]),
        None,
    );
    let error = built
        .flow
        .begin_authorization("http://127.0.0.1:9/mcp")
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("authUrl"));
}

#[tokio::test]
async fn guards_a_discovered_endpoint_but_not_the_operators_own() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_endpoint": "http://auth.internal/authorize",
            "token_endpoint": "http://auth.internal/token"
        })))
        .mount(&server)
        .await;
    let resolver: Arc<dyn DnsResolver> = Arc::new(
        StaticResolver::new().with("auth.internal", &[IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))]),
    );
    let guard = Arc::new(EndpointGuard {
        policy: NetworkPolicy {
            allow_private: false,
            ..NetworkPolicy::default()
        },
        resolver,
    });

    // Discovery names a private host: refused, and the refusal says which.
    let guarded = build(
        config(
            "http://127.0.0.1:9/a",
            "http://127.0.0.1:9/t",
            "configured-id",
            &[],
        ),
        Some(Arc::clone(&guard)),
    );
    let error = guarded
        .flow
        .begin_authorization(&format!("{}/mcp", server.uri()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert_eq!(
        error.details.get("endpoint"),
        Some(&Value::String("http://auth.internal/authorize".to_owned()))
    );

    // The same host typed by the operator is trusted the way the MCP url is.
    let typed = build(
        config(
            "http://auth.internal/authorize",
            "http://auth.internal/token",
            "configured-id",
            &[],
        ),
        Some(guard),
    );
    let url = typed
        .flow
        .begin_authorization("http://127.0.0.1:9/mcp")
        .await
        .unwrap();
    assert!(url.as_str().starts_with("http://auth.internal/authorize?"));
}

#[test]
fn the_seen_urls_recorder_starts_empty() {
    assert!(standard().seen.lock().is_empty());
}
