//! The pinned-address fetch, against a real local server.
//!
//! Every hostname here resolves through a static table, so nothing reaches DNS
//! and the pin is what carries a request to the server: `example.test` has no
//! address anywhere except the one the resolver hands back.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_security::testkit::StaticResolver;
use darkwire_security::{
    DnsResolver, GuardedFetchOptions, GuardedFetchResult, HickoryResolver, NetworkPolicy,
    guarded_fetch, validate_target,
};
use proptest::prelude::*;
use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderValue};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_string, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LOOPBACK: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
const PUBLIC: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34));

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn resolves_to(host: &str, addresses: &[IpAddr]) -> StaticResolver {
    StaticResolver::new().with(host, addresses)
}

fn public_resolver() -> StaticResolver {
    resolves_to("example.com", &[PUBLIC])
}

fn kind_of<T>(result: &Result<T>) -> ErrorKind {
    result.as_ref().err().map(|e| e.kind).expect("an error")
}

fn expect_blocked<T>(result: Result<T>, needle: &str) -> WireError {
    let error = result.err().expect("a refusal");
    assert_eq!(error.kind, ErrorKind::Network, "{}", error.message);
    // A blocked target stays blocked; retrying it only burns time.
    assert!(!error.retryable, "{}", error.message);
    assert!(error.message.contains(needle), "{}", error.message);
    error
}

/// A resolver that counts its calls and answers loopback for every name, so the
/// hop loop's re-resolution can be observed.
struct Counting {
    calls: AtomicUsize,
}

impl DnsResolver for Counting {
    fn resolve<'a>(
        &'a self,
        _host: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>>> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(vec![LOOPBACK]) })
    }
}

/// A server whose hostname is pinned to loopback and exempt from classification.
struct Local {
    server: MockServer,
    host: String,
    resolver: StaticResolver,
    policy: NetworkPolicy,
}

impl Local {
    async fn start(host: &str) -> Local {
        let server = MockServer::start().await;
        Local {
            server,
            host: host.to_owned(),
            resolver: resolves_to(host, &[LOOPBACK]),
            policy: NetworkPolicy {
                allowed_hosts: vec![host.to_owned()],
                ..NetworkPolicy::default()
            },
        }
    }

    fn url(&self, path: &str) -> String {
        format!(
            "http://{}:{}{path}",
            self.host,
            self.server.address().port()
        )
    }

    fn options(&self) -> GuardedFetchOptions<'_> {
        GuardedFetchOptions::new(&self.policy, &self.resolver)
    }

    async fn fetch(&self, path: &str) -> Result<GuardedFetchResult> {
        guarded_fetch(&self.url(path), self.options()).await
    }

    async fn requests(&self) -> Vec<wiremock::Request> {
        self.server.received_requests().await.unwrap_or_default()
    }
}

// validate_target

#[tokio::test]
async fn refuses_schemes_other_than_http_and_https() {
    let policy = NetworkPolicy::default();
    let resolver = StaticResolver::new();
    for url in [
        "file:///etc/passwd",
        "ftp://example.com/x",
        "gopher://example.com",
        "data:text/plain,x",
    ] {
        expect_blocked(
            validate_target(url, &policy, &resolver).await,
            "Only http and https",
        );
    }
}

#[tokio::test]
async fn refuses_a_string_that_is_not_a_url() {
    let policy = NetworkPolicy::default();
    let resolver = StaticResolver::new();
    for url in ["not a url", "http://", "https://", "http://@", "https:// /"] {
        let error = validate_target(url, &policy, &resolver).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{url}");
        assert!(error.message.contains("Not a URL"), "{url}");
    }
}

#[tokio::test]
async fn accepts_a_public_host_and_pins_what_the_resolver_returned() {
    let target = validate_target(
        "https://example.com/a?b=c",
        &NetworkPolicy::default(),
        &public_resolver(),
    )
    .await
    .unwrap();
    assert_eq!(target.host, "example.com");
    assert_eq!(target.url.as_str(), "https://example.com/a?b=c");
    assert_eq!(target.addresses, [PUBLIC]);
    assert!(!target.exempt);
    assert_eq!(target, target.clone());
}

#[tokio::test]
async fn refuses_a_denied_host_before_anything_else() {
    let policy = NetworkPolicy {
        denied_hosts: vec!["example.com".to_owned()],
        allowed_hosts: vec!["example.com".to_owned()],
        ..NetworkPolicy::default()
    };
    expect_blocked(
        validate_target("https://example.com/", &policy, &public_resolver()).await,
        "denied by configuration",
    );
    let subdomains = NetworkPolicy {
        denied_hosts: vec![".internal.example".to_owned()],
        ..NetworkPolicy::default()
    };
    for url in ["https://api.internal.example/", "https://INTERNAL.example/"] {
        expect_blocked(
            validate_target(url, &subdomains, &StaticResolver::new()).await,
            "denied by configuration",
        );
    }
    let empties = NetworkPolicy {
        denied_hosts: vec![String::new(), "   ".to_owned()],
        ..NetworkPolicy::default()
    };
    let target = validate_target("https://example.com/", &empties, &public_resolver())
        .await
        .unwrap();
    assert!(!target.exempt);
}

#[tokio::test]
async fn refuses_blocked_literals_in_every_encoding() {
    let policy = NetworkPolicy::default();
    let resolver = StaticResolver::new();
    for url in [
        "http://127.0.0.1/",
        "http://2130706433/",
        "http://0177.0.0.1/",
        "http://0x7f000001/",
        "http://127.1/",
        "http://[::1]/",
        "http://[::ffff:127.0.0.1]/",
        "http://169.254.169.254/latest/meta-data/",
        "http://[fe80::1]/",
        "http://10.0.0.1/",
        "http://192.168.1.1/",
        "http://[fc00::1]/",
        "http://0.0.0.0/",
        "http://[64:ff9b::7f00:1]/",
    ] {
        let error = expect_blocked(
            validate_target(url, &policy, &resolver).await,
            "blocked range",
        );
        assert!(error.details["range"].is_string(), "{url}");
    }
}

#[tokio::test]
async fn policy_flags_unlock_loopback_and_private_but_never_link_local() {
    let resolver = StaticResolver::new();
    let strict = NetworkPolicy::default();
    let loopback = NetworkPolicy {
        allow_loopback: true,
        ..NetworkPolicy::default()
    };
    let private = NetworkPolicy {
        allow_private: true,
        ..NetworkPolicy::default()
    };
    let everything = NetworkPolicy {
        allow_loopback: true,
        allow_private: true,
        ..NetworkPolicy::default()
    };
    expect_blocked(
        validate_target("http://127.0.0.1:11434/api/chat", &strict, &resolver).await,
        "blocked range",
    );
    let target = validate_target("http://127.0.0.1:11434/api/chat", &loopback, &resolver)
        .await
        .unwrap();
    assert_eq!(target.addresses, [LOOPBACK]);
    let six = validate_target("http://[::1]:11434/", &loopback, &resolver)
        .await
        .unwrap();
    assert_eq!(six.addresses, [ip("::1")]);
    assert_eq!(six.host, "[::1]");
    expect_blocked(
        validate_target("http://10.1.2.3/", &strict, &resolver).await,
        "blocked range",
    );
    assert!(
        validate_target("http://10.1.2.3/", &private, &resolver)
            .await
            .is_ok()
    );
    expect_blocked(
        validate_target("http://169.254.169.254/", &everything, &resolver).await,
        "blocked range",
    );
    let exempt = NetworkPolicy {
        allowed_hosts: vec!["127.0.0.1".to_owned()],
        ..NetworkPolicy::default()
    };
    assert!(
        validate_target("http://127.0.0.1:11434/", &exempt, &resolver)
            .await
            .unwrap()
            .exempt
    );
    let routable = validate_target("https://8.8.8.8/", &strict, &resolver)
        .await
        .unwrap();
    assert_eq!(routable.addresses, [ip("8.8.8.8")]);
}

#[tokio::test]
async fn refuses_names_that_resolve_into_blocked_ranges() {
    let policy = NetworkPolicy::default();
    let error = expect_blocked(
        validate_target(
            "http://rebind.example/",
            &policy,
            &resolves_to("rebind.example", &[ip("169.254.169.254")]),
        )
        .await,
        "resolves to 169.254.169.254",
    );
    assert_eq!(error.details["host"], "rebind.example");
    // The connection may use any answer, so one poisoned answer blocks the set.
    expect_blocked(
        validate_target(
            "http://rebind.example/",
            &policy,
            &resolves_to("rebind.example", &[PUBLIC, LOOPBACK]),
        )
        .await,
        "blocked range",
    );
    expect_blocked(
        validate_target(
            "http://rebind.example/",
            &policy,
            &resolves_to("rebind.example", &[ip("fd00::1")]),
        )
        .await,
        "blocked range",
    );
    // A mapped IPv6 answer meets the IPv4 table.
    expect_blocked(
        validate_target(
            "http://rebind.example/",
            &policy,
            &resolves_to("rebind.example", &[ip("::ffff:127.0.0.1")]),
        )
        .await,
        "blocked range",
    );
}

#[tokio::test]
async fn reports_resolution_failures_as_network_errors() {
    let policy = NetworkPolicy::default();
    let empty = validate_target(
        "http://void.example/",
        &policy,
        &resolves_to("void.example", &[]),
    )
    .await
    .unwrap_err();
    assert_eq!(empty.kind, ErrorKind::Network);
    assert!(empty.message.contains("Cannot resolve host"));
    let failing = validate_target("http://void.example/", &policy, &StaticResolver::new())
        .await
        .unwrap_err();
    assert_eq!(failing.kind, ErrorKind::Network);
    assert!(failing.message.contains("Cannot resolve host"));
}

#[tokio::test]
async fn an_allow_listed_name_skips_classification_but_is_still_pinned() {
    let policy = NetworkPolicy {
        allowed_hosts: vec![".internal".to_owned()],
        ..NetworkPolicy::default()
    };
    let target = validate_target(
        "http://ollama.internal/",
        &policy,
        &resolves_to("ollama.internal", &[ip("10.0.0.5")]),
    )
    .await
    .unwrap();
    assert!(target.exempt);
    assert_eq!(target.addresses, [ip("10.0.0.5")]);
}

#[tokio::test]
async fn the_system_resolver_answers_for_localhost() {
    // The one name guaranteed to resolve without a network, and the way it
    // gets blocked: the name is innocuous, the answer is not.
    let resolver = HickoryResolver::new().unwrap();
    let addresses = resolver.resolve("localhost").await.unwrap();
    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(IpAddr::is_loopback));
    expect_blocked(
        validate_target(
            "http://localhost:11434/",
            &NetworkPolicy::default(),
            &resolver,
        )
        .await,
        "blocked range",
    );
    let loopback = NetworkPolicy {
        allow_loopback: true,
        ..NetworkPolicy::default()
    };
    let target = validate_target("http://localhost:11434/", &loopback, &resolver)
        .await
        .unwrap();
    assert_eq!(target.host, "localhost");
    // A name the resolver refuses before any query leaves the host: a label
    // over 63 characters is not a DNS name at all.
    let too_long = format!("{}.test", "x".repeat(64));
    assert!(resolver.resolve(&too_long).await.is_err());
    assert!(format!("{resolver:?}").contains("HickoryResolver"));
}

// guarded_fetch

#[tokio::test]
async fn fetches_through_the_pin_and_reports_the_address() {
    let local = Local::start("example.test").await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .and(header(
            "host",
            format!("example.test:{}", local.server.address().port()),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
        .mount(&local.server)
        .await;
    let result = local.fetch("/x").await.unwrap();
    assert_eq!(result.url.as_str(), local.url("/x"));
    assert_eq!(result.address, LOOPBACK);
    assert!(result.redirects.is_empty());
    assert_eq!(result.response.status, 200);
    assert!(format!("{result:?}").contains("GuardedFetchResult"));
    assert_eq!(result.response.text().await.unwrap(), "hello");
}

#[tokio::test]
async fn sends_the_method_headers_and_body_it_was_given() {
    let local = Local::start("example.test").await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("content-type", "application/json"))
        .and(header("authorization", "Bearer secret"))
        .and(body_string("{\"a\":1}"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&local.server)
        .await;
    let mut options = local.options();
    options.method = Method::POST;
    options
        .headers
        .insert("Content-Type", HeaderValue::from_static("application/json"));
    options
        .headers
        .insert("Authorization", HeaderValue::from_static("Bearer secret"));
    options.body = Some(b"{\"a\":1}".to_vec());
    let result = guarded_fetch(&local.url("/"), options).await.unwrap();
    assert_eq!(result.response.bytes().await.unwrap(), b"ok");
}

#[tokio::test]
async fn refuses_before_connecting_when_the_target_is_blocked() {
    // A resolver that answers loopback for a name is exactly the rebinding
    // attack; the server must never see a socket.
    let server = MockServer::start().await;
    let resolver = resolves_to("evil.example", &[LOOPBACK]);
    let policy = NetworkPolicy::default();
    let url = format!("http://evil.example:{}/steal", server.address().port());
    expect_blocked(
        guarded_fetch(&url, GuardedFetchOptions::new(&policy, &resolver)).await,
        "blocked range",
    );
    expect_blocked(
        guarded_fetch(
            &format!("http://127.0.0.1:{}/steal", server.address().port()),
            GuardedFetchOptions::new(&policy, &resolver),
        )
        .await,
        "blocked range",
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn follows_a_redirect_and_re_validates_the_next_hop() {
    let local = Local::start("example.test").await;
    Mock::given(path("/start"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", local.url("/final")))
        .mount(&local.server)
        .await;
    Mock::given(path("/final"))
        .respond_with(ResponseTemplate::new(200).set_body_string("landed"))
        .mount(&local.server)
        .await;
    let result = local.fetch("/start").await.unwrap();
    assert_eq!(result.url.as_str(), local.url("/final"));
    assert_eq!(result.redirects, [local.url("/final")]);
    assert_eq!(result.response.text().await.unwrap(), "landed");
}

#[tokio::test]
async fn refuses_a_redirect_into_a_blocked_range() {
    let local = Local::start("example.test").await;
    Mock::given(path("/start"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "http://169.254.169.254/latest/meta-data/"),
        )
        .mount(&local.server)
        .await;
    expect_blocked(local.fetch("/start").await, "blocked range");
    assert_eq!(local.requests().await.len(), 1);
}

#[tokio::test]
async fn re_resolves_each_hop_rather_than_reusing_the_first_pin() {
    let server = MockServer::start().await;
    let port = server.address().port();
    Mock::given(path("/"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("http://elsewhere.test:{port}/x")),
        )
        .mount(&server)
        .await;
    Mock::given(path("/x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    let resolver = Counting {
        calls: AtomicUsize::new(0),
    };
    let policy = NetworkPolicy {
        allowed_hosts: vec!["first.test".to_owned(), "elsewhere.test".to_owned()],
        ..NetworkPolicy::default()
    };
    let result = guarded_fetch(
        &format!("http://first.test:{port}/"),
        GuardedFetchOptions::new(&policy, &resolver),
    )
    .await
    .unwrap();
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
    assert_eq!(result.address, LOOPBACK);
}

#[tokio::test]
async fn drops_credentials_when_the_origin_changes_and_keeps_them_otherwise() {
    let local = Local::start("example.test").await;
    let other = MockServer::start().await;
    let other_url = format!("http://attacker.test:{}/collect", other.address().port());
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", other_url.clone()))
        .mount(&local.server)
        .await;
    Mock::given(path("/collect"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&other)
        .await;
    Mock::given(path("/same"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", local.url("/next")))
        .mount(&local.server)
        .await;
    Mock::given(path("/next"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&local.server)
        .await;

    let resolver = StaticResolver::new()
        .with("example.test", &[LOOPBACK])
        .with("attacker.test", &[LOOPBACK]);
    let policy = NetworkPolicy {
        allowed_hosts: vec!["example.test".to_owned(), "attacker.test".to_owned()],
        ..NetworkPolicy::default()
    };
    let mut headers = HeaderMap::new();
    headers.insert("Authorization", HeaderValue::from_static("Bearer secret"));
    headers.insert("Cookie", HeaderValue::from_static("session=1"));
    headers.insert("Accept", HeaderValue::from_static("text/html"));

    let mut options = GuardedFetchOptions::new(&policy, &resolver);
    options.headers = headers.clone();
    guarded_fetch(&local.url("/"), options).await.unwrap();
    let collected = other.received_requests().await.unwrap();
    assert_eq!(collected.len(), 1);
    assert!(collected[0].headers.get("authorization").is_none());
    assert!(collected[0].headers.get("cookie").is_none());
    assert_eq!(collected[0].headers.get("accept").unwrap(), "text/html");

    let mut options = GuardedFetchOptions::new(&policy, &resolver);
    options.headers = headers;
    guarded_fetch(&local.url("/same"), options).await.unwrap();
    let same = local.requests().await;
    let next = same.iter().find(|r| r.url.path() == "/next").unwrap();
    assert_eq!(next.headers.get("authorization").unwrap(), "Bearer secret");
}

#[tokio::test]
async fn a_303_becomes_a_get_and_a_307_keeps_the_method() {
    let local = Local::start("example.test").await;
    Mock::given(path("/submit303"))
        .respond_with(ResponseTemplate::new(303).insert_header("location", local.url("/result")))
        .mount(&local.server)
        .await;
    Mock::given(path("/submit307"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", local.url("/result")))
        .mount(&local.server)
        .await;
    Mock::given(path("/result"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&local.server)
        .await;
    for route in ["/submit303", "/submit307"] {
        let mut options = local.options();
        options.method = Method::POST;
        options.body = Some(b"a=1".to_vec());
        guarded_fetch(&local.url(route), options).await.unwrap();
    }
    let results: Vec<wiremock::Request> = local
        .requests()
        .await
        .into_iter()
        .filter(|r| r.url.path() == "/result")
        .collect();
    assert_eq!(results[0].method, "GET");
    assert!(results[0].body.is_empty());
    assert_eq!(results[1].method, "POST");
    assert_eq!(results[1].body, b"a=1");
}

#[tokio::test]
async fn stops_after_the_redirect_limit() {
    let local = Local::start("example.test").await;
    for hop in 0..4 {
        Mock::given(path(format!("/{hop}")))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", local.url(&format!("/{}", hop + 1))),
            )
            .mount(&local.server)
            .await;
    }
    let policy = NetworkPolicy {
        max_redirects: 2,
        ..local.policy.clone()
    };
    let error = expect_blocked(
        guarded_fetch(
            &local.url("/0"),
            GuardedFetchOptions::new(&policy, &local.resolver),
        )
        .await,
        "Too many redirects (limit 2)",
    );
    assert_eq!(error.details["redirects"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn redirect_edge_cases() {
    let local = Local::start("example.test").await;
    Mock::given(path("/no-location"))
        .respond_with(ResponseTemplate::new(302).set_body_string("body"))
        .mount(&local.server)
        .await;
    Mock::given(path("/bad-location"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "http://["))
        .mount(&local.server)
        .await;
    Mock::given(path("/deep/path"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/next"))
        .mount(&local.server)
        .await;
    Mock::given(path("/next"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&local.server)
        .await;

    let final_response = local.fetch("/no-location").await.unwrap();
    assert_eq!(final_response.response.status, 302);
    expect_blocked(local.fetch("/bad-location").await, "not a URL");
    let relative = local.fetch("/deep/path").await.unwrap();
    assert_eq!(relative.url.as_str(), local.url("/next"));
}

#[tokio::test]
async fn wraps_a_transport_failure() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let resolver = resolves_to("closed.test", &[LOOPBACK]);
    let policy = NetworkPolicy {
        allowed_hosts: vec!["closed.test".to_owned()],
        ..NetworkPolicy::default()
    };
    let error = guarded_fetch(
        &format!("http://closed.test:{port}/"),
        GuardedFetchOptions::new(&policy, &resolver),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("Request to closed.test failed"));
}

#[tokio::test]
async fn honours_a_cancelled_token_and_a_deadline() {
    let local = Local::start("example.test").await;
    Mock::given(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(5)))
        .mount(&local.server)
        .await;
    Mock::given(path("/fast"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&local.server)
        .await;

    let mut options = local.options();
    options.token = CancellationToken::new();
    options.token.cancel();
    let cancelled = guarded_fetch(&local.url("/fast"), options)
        .await
        .unwrap_err();
    assert_eq!(cancelled.kind, ErrorKind::Aborted);
    assert!(local.requests().await.is_empty());

    let short = NetworkPolicy {
        timeout_ms: 100,
        ..local.policy.clone()
    };
    let timed_out = guarded_fetch(
        &local.url("/slow"),
        GuardedFetchOptions::new(&short, &local.resolver),
    )
    .await
    .unwrap_err();
    assert_eq!(timed_out.kind, ErrorKind::Timeout);
    assert!(timed_out.message.contains("timed out after 100 ms"));

    let none = NetworkPolicy {
        timeout_ms: 0,
        ..local.policy.clone()
    };
    let result = guarded_fetch(
        &local.url("/fast"),
        GuardedFetchOptions::new(&none, &local.resolver),
    )
    .await
    .unwrap();
    assert_eq!(result.response.status, 200);
}

#[tokio::test]
async fn a_cancelled_token_stops_the_body_stream() {
    let local = Local::start("example.test").await;
    Mock::given(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(4096)))
        .mount(&local.server)
        .await;
    let token = CancellationToken::new();
    let mut options = local.options();
    options.token = token.clone();
    let result = guarded_fetch(&local.url("/big"), options).await.unwrap();
    token.cancel();
    let error = result.response.bytes().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Aborted);
}

#[tokio::test]
async fn caps_the_body_while_it_streams() {
    let local = Local::start("example.test").await;
    Mock::given(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(4096)))
        .mount(&local.server)
        .await;
    Mock::given(path("/small"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("x-trace", "abc")
                .set_body_string("body"),
        )
        .mount(&local.server)
        .await;
    Mock::given(path("/empty"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&local.server)
        .await;

    let capped = NetworkPolicy {
        max_bytes: 1024,
        ..local.policy.clone()
    };
    let result = guarded_fetch(
        &local.url("/big"),
        GuardedFetchOptions::new(&capped, &local.resolver),
    )
    .await
    .unwrap();
    let error = result.response.text().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(!error.retryable);
    assert!(error.message.contains("exceeded 1024 bytes"));

    let uncapped = NetworkPolicy {
        max_bytes: 0,
        ..local.policy.clone()
    };
    let result = guarded_fetch(
        &local.url("/big"),
        GuardedFetchOptions::new(&uncapped, &local.resolver),
    )
    .await
    .unwrap();
    assert_eq!(result.response.text().await.unwrap().len(), 4096);

    let fits = NetworkPolicy {
        max_bytes: 64,
        ..local.policy.clone()
    };
    let small = guarded_fetch(
        &local.url("/small"),
        GuardedFetchOptions::new(&fits, &local.resolver),
    )
    .await
    .unwrap();
    assert_eq!(small.response.status, 201);
    assert_eq!(small.response.headers.get("x-trace").unwrap(), "abc");
    assert!(format!("{:?}", small.response).contains("GuardedResponse"));
    assert_eq!(small.response.text().await.unwrap(), "body");

    let empty = local.fetch("/empty").await.unwrap();
    assert_eq!(empty.response.status, 204);
    assert!(empty.response.bytes().await.unwrap().is_empty());
}

#[test]
fn the_default_policy_matches_the_documented_numbers() {
    let policy = NetworkPolicy::default();
    assert_eq!(policy.max_redirects, 3);
    assert_eq!(policy.max_bytes, 5 * 1024 * 1024);
    assert_eq!(policy.timeout_ms, 30_000);
    assert!(!policy.allow_loopback && !policy.allow_private);
}

// Properties: no blocked address is reachable

fn blocked_hosts() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "127.0.0.1",
        "2130706433",
        "0177.0.0.1",
        "0x7f000001",
        "127.1",
        "[::1]",
        "[::ffff:127.0.0.1]",
        "169.254.169.254",
        "0xa9fea9fe",
        "10.0.0.1",
        "192.168.0.1",
        "172.20.1.1",
        "[fc00::1]",
        "[fe80::1]",
        "0.0.0.0",
        "[::]",
        "224.0.0.1",
    ])
}

static RUNTIME: Mutex<Option<tokio::runtime::Runtime>> = Mutex::new(None);

fn block_on<T>(future: impl Future<Output = T>) -> T {
    let mut guard = RUNTIME.lock().unwrap();
    let runtime = guard.get_or_insert_with(|| tokio::runtime::Runtime::new().unwrap());
    runtime.block_on(future)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn blocked_literals_are_refused_whatever_the_scheme(host in blocked_hosts(), scheme in prop::sample::select(vec!["http", "https"])) {
        let policy = NetworkPolicy::default();
        let resolver = StaticResolver::new();
        let outcome = block_on(guarded_fetch(
            &format!("{scheme}://{host}/x"),
            GuardedFetchOptions::new(&policy, &resolver),
        ));
        prop_assert_eq!(kind_of(&outcome), ErrorKind::Network);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn names_resolving_to_blocked_addresses_are_refused(address in prop::sample::select(vec![
        "127.0.0.1", "169.254.169.254", "10.0.0.1", "fd00::1", "::1",
    ])) {
        let policy = NetworkPolicy::default();
        let resolver = resolves_to("rebind.example", &[ip(address)]);
        let outcome = block_on(guarded_fetch(
            "https://rebind.example/",
            GuardedFetchOptions::new(&policy, &resolver),
        ));
        prop_assert_eq!(kind_of(&outcome), ErrorKind::Network);
    }
}
