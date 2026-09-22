//! Egress with the DNS rebinding window closed.
//!
//! The usual shape of an SSRF guard — resolve the hostname, check the addresses,
//! then hand the *URL* to an HTTP client — is advisory only. The client performs
//! its own lookup when it connects, and nothing obliges the second answer to
//! match the first. A DNS server that alternates between a public address and
//! 169.254.169.254 passes validation on every attempt and connects to the
//! metadata endpoint on roughly half of them. The check and the connection have
//! to share one resolution or the check is decoration.
//!
//! So validation resolves the host itself and the resulting addresses are pinned
//! into a client built for that one request: the client's resolver is told the
//! answer for that host and never consults DNS. There is no second resolution to
//! differ from the first. Proxies from the environment are ignored for the same
//! reason — a proxy would connect wherever it liked.
//!
//! The rest follows from taking redirects seriously. Every hop is validated
//! again, with a fresh pin, because a public URL that 302s to
//! `http://169.254.169.254/` is the same attack with one more step.
//! `Authorization` and `Cookie` are dropped when the origin changes, so a
//! redirect cannot turn a credential into an exfiltration channel. And the
//! response body is capped as it streams rather than after it arrives, because
//! "the model asked for a URL that serves an endless stream" must not be a way to
//! exhaust the host's memory.
//!
//! Cancellation and the deadline are one mechanism: the caller's token, and a
//! timer raced against every await, including each body chunk.
//!
//! What may be reached is an [`EgressAllow`], not a set of flags. Under an
//! allow-list a host nobody named is refused **before** it is resolved, so a
//! destination the policy rejects never becomes a DNS query whose name the
//! caller chose. A literal is the one exception to that order: there is nothing
//! to resolve, so it is classified first and the refusal names the range it
//! landed in, which is the more useful sentence for a redirect aimed at the
//! metadata endpoint.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{EnvironmentNetwork, NetworkMode};
use futures::stream::{Stream, StreamExt};
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode, Url};
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::allow::AllowList;
use crate::ip::{AddressCategory, AddressRange, ParsedIp, classify_address, parse_ip_literal};

/// Answers a hostname with addresses. Injected so tests never touch DNS.
pub trait DnsResolver: Send + Sync {
    /// Every address `host` resolves to, in connection order.
    fn resolve<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>>> + Send + 'a>>;
}

/// The system's resolver, through hickory, reading the host's own configuration.
#[derive(Debug)]
pub struct HickoryResolver {
    inner: hickory_resolver::TokioResolver,
}

impl HickoryResolver {
    /// A resolver over the system configuration.
    pub fn new() -> Result<HickoryResolver> {
        let cannot = |error: hickory_resolver::net::NetError| {
            WireError::new(ErrorKind::Network, "Cannot configure the DNS resolver")
                .with_source(error)
        };
        let inner = hickory_resolver::TokioResolver::builder_tokio()
            .map_err(cannot)?
            .build()
            .map_err(cannot)?;
        Ok(HickoryResolver { inner })
    }
}

impl DnsResolver for HickoryResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>>> + Send + 'a>> {
        Box::pin(async move {
            let lookup = self.inner.lookup_ip(host).await.map_err(|error| {
                WireError::new(ErrorKind::Network, format!("Cannot resolve host: {host}"))
                    .with_source(error)
            })?;
            Ok(lookup.iter().collect())
        })
    }
}

/// What egress may reach, and whether that is a ceiling or an allow-list.
///
/// The distinction matters more than it looks. An earlier shape had a list of
/// hosts that were *exempt* from address classification, which reads like an
/// allow-list and is not one: a host nobody listed still fell through to the
/// classifier, and a public address passed. Refusing what is not named has to
/// be its own state, so it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressAllow {
    /// Everything publicly routable. Loopback, private and the hard-blocked
    /// ranges stay refused, because nothing named them.
    Public,
    /// Only what this list names. Empty reaches nothing.
    ///
    /// A matched entry also lifts the loopback and private refusals for the
    /// destination it matched: naming `10.0.0.0/8` is the operator saying they
    /// know what is there. It never lifts link-local, multicast, unspecified or
    /// reserved, and [`crate::parse_allow_entry`] refuses an entry naming one.
    Only(AllowList),
}

/// What egress may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkPolicy {
    /// The destinations, and whether anything outside them is permitted.
    pub allow: EgressAllow,
    /// Whose policy this is, so a refusal can say which agent to edit. Empty
    /// for a fetch with no agent behind it.
    pub agent_id: String,
    /// How many hops to follow.
    pub max_redirects: usize,
    /// `0` disables the cap.
    pub max_bytes: u64,
    /// `0` disables the timeout.
    pub timeout_ms: u64,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        NetworkPolicy {
            allow: EgressAllow::Public,
            agent_id: String::new(),
            max_redirects: 3,
            max_bytes: 5 * 1024 * 1024,
            timeout_ms: 30_000,
        }
    }
}

impl NetworkPolicy {
    /// The policy one agent's egress config describes.
    ///
    /// `on_host` carries the placement, because `none` cannot be enforced
    /// there: `exec` on this machine already reaches the whole network and
    /// nothing in this process changes that, so a host agent's `none` means the
    /// same as `open`. Withholding web access on the host is the permission
    /// map's job, which is the switch that actually works.
    pub fn for_agent(
        network: &EnvironmentNetwork,
        agent_id: &str,
        on_host: bool,
    ) -> Result<NetworkPolicy> {
        let allow = match (network.mode, on_host) {
            (NetworkMode::Open, _) | (NetworkMode::None, true) => EgressAllow::Public,
            (NetworkMode::None, false) => EgressAllow::Only(AllowList::default()),
            (NetworkMode::Allowlist, _) => EgressAllow::Only(AllowList::parse(&network.allow)?),
        };
        Ok(NetworkPolicy {
            allow,
            agent_id: agent_id.to_owned(),
            ..NetworkPolicy::default()
        })
    }

    /// The list, when there is one.
    pub fn allow_list(&self) -> Option<&AllowList> {
        match &self.allow {
            EgressAllow::Only(list) => Some(list),
            EgressAllow::Public => None,
        }
    }
}

/// A validated URL and the addresses a request to it may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedTarget {
    /// The parsed URL.
    pub url: Url,
    /// The host as the URL spells it, brackets included for an IPv6 literal.
    pub host: String,
    /// Validated and in connection order. The client may use only these.
    pub addresses: Vec<IpAddr>,
    /// An allow-list entry named this host, which lifts the loopback and
    /// private refusals for it. Never the hard-blocked ranges.
    pub exempt: bool,
}

/// One request's worth of choices.
pub struct GuardedFetchOptions<'a> {
    /// The policy.
    pub policy: &'a NetworkPolicy,
    /// Resolves names. Production passes a [`HickoryResolver`].
    pub resolver: &'a dyn DnsResolver,
    /// The method. Default `GET`.
    pub method: Method,
    /// Request headers.
    pub headers: HeaderMap,
    /// The body, if any.
    pub body: Option<Vec<u8>>,
    /// Cancels the request and its body stream.
    pub token: CancellationToken,
}

impl<'a> GuardedFetchOptions<'a> {
    /// A `GET` with no headers and no body.
    pub fn new(
        policy: &'a NetworkPolicy,
        resolver: &'a dyn DnsResolver,
    ) -> GuardedFetchOptions<'a> {
        GuardedFetchOptions {
            policy,
            resolver,
            method: Method::GET,
            headers: HeaderMap::new(),
            body: None,
            token: CancellationToken::new(),
        }
    }
}

/// A body chunk stream, already capped and already racing the deadline.
pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send>>;

/// A response whose body is capped as it streams.
pub struct GuardedResponse {
    /// The status.
    pub status: StatusCode,
    /// The response headers, untouched.
    pub headers: HeaderMap,
    /// The body. Errors with `network` once the byte cap is exceeded.
    pub body: BodyStream,
}

impl std::fmt::Debug for GuardedResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardedResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

impl GuardedResponse {
    /// Collects the whole body, subject to the cap.
    pub async fn bytes(mut self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(chunk) = self.body.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(out)
    }

    /// Collects the whole body as text, subject to the cap.
    pub async fn text(self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.bytes().await?).into_owned())
    }
}

/// What a guarded fetch produced.
#[derive(Debug)]
pub struct GuardedFetchResult {
    /// Body already capped. Status and headers are untouched.
    pub response: GuardedResponse,
    /// The final URL. Differs from the request when redirects were followed.
    pub url: Url,
    /// Every hop followed, in order.
    pub redirects: Vec<String>,
    /// The address actually connected to, for the audit log.
    pub address: IpAddr,
}

const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

/// Explicitly not retryable: `network` defaults to retryable because DNS and TCP
/// failures are transient, but a blocked target will be blocked again and a
/// retry loop against it is just a slower refusal.
fn blocked(message: impl Into<String>) -> WireError {
    WireError::new(ErrorKind::Network, message).with_retryable(false)
}

fn detail_list(items: &[String]) -> Value {
    Value::Array(items.iter().map(|s| Value::from(s.as_str())).collect())
}

/// The refusal for a destination no entry names.
///
/// It names the agent and the setting, because the operator reading it is the
/// only one who can act on it, and the model reading it needs to say what to
/// ask for rather than try the next URL.
fn not_allowed(policy: &NetworkPolicy, host: &str, raw_url: &str) -> WireError {
    let who = if policy.agent_id.is_empty() {
        "This agent".to_owned()
    } else {
        format!("Agent \"{}\"", policy.agent_id)
    };
    let where_ = if policy.agent_id.is_empty() {
        "its allowed networks".to_owned()
    } else {
        format!("agents.list.{}.environment.network.allow", policy.agent_id)
    };
    blocked(format!(
        "{who} may only reach the destinations in its allow-list, and {host} is not\n  \
         one of them. Add \"{host}\" to {where_}."
    ))
    .with_detail("url", raw_url)
    .with_detail("host", host)
}

/// The refusal for an agent whose egress is switched off entirely.
fn no_egress(policy: &NetworkPolicy, raw_url: &str) -> WireError {
    let who = if policy.agent_id.is_empty() {
        "This agent".to_owned()
    } else {
        format!("Agent \"{}\"", policy.agent_id)
    };
    blocked(format!(
        "{who} has no network access, so it cannot reach anything.\n  \
         An operator grants it on the agent's environment."
    ))
    .with_detail("url", raw_url)
}

/// Whether a classified range may be reached, given what matched the host.
///
/// Link-local (the cloud metadata endpoint), multicast, the unspecified address
/// and the transition prefixes have no legitimate agent use, and **no entry
/// unlocks them**. That is a deliberate tightening over the shape this replaced,
/// where naming a host skipped classification wholesale and so an entry that
/// resolved to 169.254.169.254 was reachable.
///
/// Loopback and private are reachable when an entry matched, because that is an
/// operator naming a model server on this host or a service on the LAN.
fn is_permitted(range: &AddressRange, matched: bool) -> bool {
    !is_hard(range.category) && matched
}

/// An address as the resolver handed it back, in the form the table classifies.
/// A mapped IPv6 answer comes back as family 4, exactly as a literal would.
fn parsed_from(ip: IpAddr) -> Option<ParsedIp> {
    parse_ip_literal(&ip.to_string())
}

/// Whether a category is one no entry ever unlocks.
fn is_hard(category: AddressCategory) -> bool {
    !matches!(
        category,
        AddressCategory::Loopback | AddressCategory::Private
    )
}

/// Refuses an address the policy does not admit, naming why.
fn assert_address(
    parsed: &ParsedIp,
    matched: bool,
    host: &str,
    raw_url: &str,
    resolved: bool,
) -> Result<()> {
    let Some(range) = classify_address(parsed).filter(|range| !is_permitted(range, matched)) else {
        return Ok(());
    };
    let message = if resolved {
        format!(
            "{host} resolves to {}, which is in a blocked range ({})",
            parsed.canonical, range.label
        )
    } else {
        format!(
            "Address {} is in a blocked range ({})",
            parsed.canonical, range.label
        )
    };
    Err(blocked(message)
        .with_detail("url", raw_url)
        .with_detail("host", host)
        .with_detail("address", parsed.canonical.as_str())
        .with_detail("range", range.cidr))
}

/// Resolves and validates a URL, returning the addresses a request to it may use.
///
/// Public because the MCP HTTP transport, the egress proxy and the media proxy
/// validate a URL at configuration time, before any request exists.
///
/// Order is load-bearing. Under an allow-list, a host nobody named is refused
/// **before** it is resolved, so a destination the policy rejects never becomes
/// a DNS query an attacker chose the name of.
pub async fn validate_target(
    raw_url: &str,
    policy: &NetworkPolicy,
    resolver: &dyn DnsResolver,
) -> Result<PinnedTarget> {
    let url = Url::parse(raw_url).map_err(|error| {
        WireError::new(ErrorKind::InvalidInput, format!("Not a URL: {raw_url}")).with_source(error)
    })?;

    if url.scheme() != "http" && url.scheme() != "https" {
        // `file:`, `gopher:` and `data:` are the classic pivots out of an HTTP
        // client that accepts whatever scheme it is handed.
        return Err(blocked(format!(
            "Only http and https are allowed, got \"{}:\"",
            url.scheme()
        ))
        .with_detail("url", raw_url));
    }
    // `http` and `https` are special schemes, for which the URL parser rejects an
    // empty host outright, so the host is always present here.
    let host = url.host_str().unwrap_or_default().to_owned();

    // A literal is judged in the other order: there is nothing to resolve, so
    // classifying first costs nothing and says the more useful thing. "169.254
    // .169.254 is in a blocked range" tells a reader what the redirect was
    // reaching for; "not on your allow-list" invites them to add it.
    let literal = parse_ip_literal(&host);
    if let Some(parsed) = &literal
        && classify_address(parsed).is_some_and(|range| is_hard(range.category))
    {
        assert_address(parsed, true, &host, raw_url, false)?;
    }

    let matched = match &policy.allow {
        EgressAllow::Public => false,
        EgressAllow::Only(list) if list.is_empty() => {
            return Err(no_egress(policy, raw_url));
        }
        EgressAllow::Only(list) => {
            if list.match_host(&host).is_none() {
                return Err(not_allowed(policy, &host, raw_url));
            }
            true
        }
    };

    if let Some(parsed) = literal {
        // Re-checked with the match in hand, because loopback and private are
        // unlocked by an entry and the first pass could not know there was one.
        assert_address(&parsed, matched, &host, raw_url, false)?;
        return Ok(PinnedTarget {
            url,
            host,
            addresses: vec![parsed.to_ip_addr()],
            exempt: matched,
        });
    }

    let answers = resolver.resolve(&host).await.map_err(|error| {
        WireError::new(ErrorKind::Network, format!("Cannot resolve host: {host}"))
            .with_detail("url", raw_url)
            .with_detail("host", host.as_str())
            .with_source(error)
    })?;
    if answers.is_empty() {
        return Err(
            WireError::new(ErrorKind::Network, format!("Cannot resolve host: {host}"))
                .with_detail("url", raw_url)
                .with_detail("host", host.as_str()),
        );
    }

    for candidate in &answers {
        let Some(parsed) = parsed_from(*candidate) else {
            // The resolver returned something the table cannot classify.
            // Refusing is the only safe reading of a result we cannot judge.
            return Err(blocked(format!(
                "Resolver returned an unparseable address for {host}"
            ))
            .with_detail("url", raw_url)
            .with_detail("host", host.as_str())
            .with_detail("address", candidate.to_string()));
        };
        // Every address is checked, not just the first: the connection may use
        // any of them, so one blocked answer poisons the whole set.
        assert_address(&parsed, matched, &host, raw_url, true)?;
    }

    Ok(PinnedTarget {
        url,
        host,
        addresses: answers,
        exempt: matched,
    })
}

/// The deadline every await in one fetch races against.
#[derive(Clone)]
struct Deadline {
    token: CancellationToken,
    at: Option<Instant>,
    host: String,
    timeout_ms: u64,
}

impl Deadline {
    fn aborted() -> WireError {
        WireError::aborted("Fetch")
    }

    fn timed_out(&self) -> WireError {
        WireError::new(
            ErrorKind::Timeout,
            format!(
                "Request to {} timed out after {} ms",
                self.host, self.timeout_ms
            ),
        )
        .with_detail("host", self.host.as_str())
        .with_detail("timeoutMs", self.timeout_ms)
    }

    async fn expired(&self) {
        match self.at {
            Some(at) => tokio::time::sleep_until(at).await,
            None => std::future::pending().await,
        }
    }

    /// Runs `work` unless the token fires or the deadline passes first.
    async fn race<T>(&self, work: impl Future<Output = T>) -> Result<T> {
        if self.token.is_cancelled() {
            return Err(Self::aborted());
        }
        tokio::select! {
            () = self.token.cancelled() => Err(Self::aborted()),
            () = self.expired() => Err(self.timed_out()),
            value = work => Ok(value),
        }
    }
}

/// A client that will connect to `target`'s pinned addresses and nowhere else.
///
/// Redirects are never followed by the client: the next hop would be connected
/// through the resolver pinned for the *previous* host, which is both wrong and
/// unvalidated. Environment proxies are ignored for the same reason.
fn pinned_client(target: &PinnedTarget) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy();
    if let Some(domain) = target.url.domain() {
        // The port in each pin is ignored by the client; the URL's port is what
        // connects. A literal host needs no pin: there is nothing to resolve.
        let addresses: Vec<SocketAddr> = target
            .addresses
            .iter()
            .map(|ip| SocketAddr::new(*ip, 0))
            .collect();
        builder = builder.resolve_to_addrs(domain, &addresses);
    }
    builder.build().map_err(|error| {
        WireError::new(ErrorKind::Network, "Cannot build the HTTP client").with_source(error)
    })
}

/// Caps the body while it streams, racing every chunk against the deadline.
///
/// Checking `content-length` is not enough — it is optional, and a hostile server
/// simply omits it — so the bytes are counted as they arrive and the stream errors
/// the moment the budget is exceeded.
fn cap_body(response: reqwest::Response, max_bytes: u64, deadline: Deadline) -> BodyStream {
    let host = deadline.host.clone();
    let inner = response.bytes_stream();
    let state = (inner, 0u64, deadline, host);
    Box::pin(futures::stream::try_unfold(
        state,
        move |(mut inner, seen, deadline, host)| async move {
            let next = deadline.race(inner.next()).await?;
            match next {
                None => Ok(None),
                Some(Err(error)) => Err(WireError::new(
                    ErrorKind::Network,
                    format!("Response body from {host} failed"),
                )
                .with_source(error)),
                Some(Ok(chunk)) => {
                    let seen = seen + u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                    if max_bytes > 0 && seen > max_bytes {
                        return Err(blocked(format!("Response body exceeded {max_bytes} bytes"))
                            .with_detail("maxBytes", max_bytes));
                    }
                    Ok(Some((chunk.to_vec(), (inner, seen, deadline, host))))
                }
            }
        },
    ))
}

/// Headers that must not survive a change of origin.
fn strip_credentials(headers: &mut HeaderMap) {
    headers.remove(reqwest::header::AUTHORIZATION);
    headers.remove(reqwest::header::COOKIE);
}

/// The request state that carries from hop to hop.
struct Hop {
    url: String,
    method: Method,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    redirects: Vec<String>,
}

/// One hop: validate, pin, send.
async fn send_hop(
    hop: &Hop,
    options: &GuardedFetchOptions<'_>,
    deadline: &mut Deadline,
) -> Result<(PinnedTarget, reqwest::Response)> {
    let target = deadline
        .race(validate_target(&hop.url, options.policy, options.resolver))
        .await??;
    deadline.host.clone_from(&target.host);
    let client = pinned_client(&target)?;
    let mut request = client
        .request(hop.method.clone(), target.url.clone())
        .headers(hop.headers.clone());
    if let Some(body) = &hop.body {
        request = request.body(body.clone());
    }
    let response = deadline.race(request.send()).await?.map_err(|error| {
        WireError::new(
            ErrorKind::Network,
            format!("Request to {} failed", target.host),
        )
        .with_detail("url", target.url.as_str())
        .with_detail("host", target.host.as_str())
        .with_source(error)
    })?;
    Ok((target, response))
}

/// Where a redirect points, or the refusal.
fn next_hop(
    hop: &mut Hop,
    target: &PinnedTarget,
    status: StatusCode,
    location: &str,
    max_redirects: usize,
    original_url: &str,
) -> Result<()> {
    if hop.redirects.len() >= max_redirects {
        let mut all = hop.redirects.clone();
        all.push(location.to_owned());
        return Err(
            blocked(format!("Too many redirects (limit {max_redirects})"))
                .with_detail("url", original_url)
                .with_detail("redirects", detail_list(&all)),
        );
    }
    let next = target.url.join(location).map_err(|error| {
        blocked(format!("Redirect target is not a URL: {location}"))
            .with_detail("url", target.url.as_str())
            .with_detail("location", location)
            .with_detail("cause", error.to_string())
    })?;
    if next.origin() != target.url.origin() {
        strip_credentials(&mut hop.headers);
    }
    if status == StatusCode::SEE_OTHER && hop.method != Method::GET && hop.method != Method::HEAD {
        hop.method = Method::GET;
        hop.body = None;
    }
    hop.redirects.push(next.to_string());
    hop.url = next.to_string();
    Ok(())
}

fn location_of(response: &reqwest::Response) -> Option<String> {
    if !REDIRECT_STATUSES.contains(&response.status().as_u16()) {
        return None;
    }
    response
        .headers()
        .get(reqwest::header::LOCATION)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
}

/// Fetches a URL under the network policy, following redirects by hand.
pub async fn guarded_fetch(
    raw_url: &str,
    options: GuardedFetchOptions<'_>,
) -> Result<GuardedFetchResult> {
    let policy = options.policy;
    let mut deadline = Deadline {
        token: options.token.clone(),
        at: (policy.timeout_ms > 0)
            .then(|| Instant::now() + Duration::from_millis(policy.timeout_ms)),
        host: String::new(),
        timeout_ms: policy.timeout_ms,
    };
    let mut hop = Hop {
        url: raw_url.to_owned(),
        method: options.method.clone(),
        headers: options.headers.clone(),
        body: options.body.clone(),
        redirects: Vec::new(),
    };

    loop {
        let (target, response) = send_hop(&hop, &options, &mut deadline).await?;
        let Some(location) = location_of(&response) else {
            let address = target
                .addresses
                .first()
                .copied()
                .unwrap_or(IpAddr::from([0, 0, 0, 0]));
            let status = response.status();
            let response_headers = response.headers().clone();
            return Ok(GuardedFetchResult {
                response: GuardedResponse {
                    status,
                    headers: response_headers,
                    body: cap_body(response, policy.max_bytes, deadline.clone()),
                },
                url: target.url,
                redirects: hop.redirects,
                address,
            });
        };
        let status = response.status();
        drop(response);
        next_hop(
            &mut hop,
            &target,
            status,
            &location,
            policy.max_redirects,
            raw_url,
        )?;
    }
}
