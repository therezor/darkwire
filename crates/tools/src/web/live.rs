//! The working web port: the guard, the backends and the cache, wired together.
//!
//! Everything reaches the network through `guarded_fetch`, which resolves a host
//! itself, pins the answers into a client that never consults DNS again,
//! revalidates every redirect hop and caps the body as it streams. Nothing here
//! decides what may be reached; the [`NetworkPolicy`] the agent arrives with
//! does, and this module only ever hands it over.
//!
//! The long-lived state is on the resolver, not the port: one page cache, one
//! search cache and one cooldown map serve the whole install, while a port is
//! one agent's policy and a reference to that state.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::{Clock, ErrorKind, Result, WireError};
use darkwire_protocol::config::{Config, WebSearchProvider};
use darkwire_security::{
    DnsResolver, GuardedFetchOptions, NetworkPolicy, RandomSource, guarded_fetch,
};
use parking_lot::Mutex;
use reqwest::Method;
use reqwest::header::HeaderMap;
use tokio_util::sync::CancellationToken;

use crate::tool::BoxFuture;
use crate::web::cache::WebCache;
use crate::web::extract::{Page, extract};
use crate::web::headers::{backend_headers, browser_headers};
use crate::web::port::{SearchHit, SearchOutcome, SearchQuery, WebPort, WebResolver};
use crate::web::search::{self, brave, duckduckgo, hn, mojeek, searxng};

/// How this install reaches the web, from `tools.web`.
#[derive(Debug, Clone)]
pub struct WebSettings {
    /// Which backend answers a search.
    pub search_provider: WebSearchProvider,
    /// The SearXNG instance, for `searxng`.
    pub search_url: String,
    /// Sent verbatim, with no client hints. Empty is the browser profile.
    pub user_agent: String,
    /// Per fetch.
    pub timeout_ms: u64,
    /// Per read inside a search, so one slow page cannot eat the batch.
    pub read_timeout_ms: u64,
    /// The streaming body cap.
    pub max_bytes: u64,
    /// Pages and result sets held in memory. `0` disables the cache.
    pub cache_entries: usize,
    /// How long an entry stays usable.
    pub cache_ttl_ms: u64,
}

impl Default for WebSettings {
    fn default() -> WebSettings {
        WebSettings {
            search_provider: WebSearchProvider::Auto,
            search_url: String::new(),
            user_agent: String::new(),
            timeout_ms: 20_000,
            read_timeout_ms: 15_000,
            max_bytes: 5 * 1024 * 1024,
            cache_entries: 64,
            cache_ttl_ms: 900_000,
        }
    }
}

/// State shared by every agent on the install.
struct Shared {
    settings: WebSettings,
    resolver: Arc<dyn DnsResolver>,
    clock: Arc<dyn Clock>,
    random: Arc<dyn RandomSource>,
    pages: WebCache<Page>,
    searches: WebCache<Vec<SearchHit>>,
    /// Backend name to the epoch at which it may be tried again.
    cooling: Mutex<HashMap<&'static str, i64>>,
}

impl Shared {
    fn cooling_off(&self, backend: &'static str) -> bool {
        self.cooling
            .lock()
            .get(backend)
            .is_some_and(|until| self.clock.now_ms() < *until)
    }

    fn cool_down(&self, backend: &'static str) {
        self.cooling.lock().insert(
            backend,
            self.clock.now_ms().saturating_add(search::COOLDOWN_MS),
        );
    }
}

/// One agent's access to the web.
pub struct LiveWeb {
    shared: Arc<Shared>,
    agent_id: String,
    /// `None` is an agent whose egress is switched off.
    policy: Option<NetworkPolicy>,
}

/// One request's worth of choices, so the call sites stay readable.
struct Request<'a> {
    url: &'a str,
    headers: HeaderMap,
    timeout_ms: u64,
    body: Option<Vec<u8>>,
}

impl LiveWeb {
    fn policy_for(&self, timeout_ms: u64) -> Result<NetworkPolicy> {
        let mut policy = self.policy.clone().ok_or_else(|| {
            WireError::new(
                ErrorKind::PermissionDenied,
                format!(
                    "Agent \"{}\" has no network access, so it cannot reach anything.\n  \
                     An operator grants it on the agent's environment.",
                    self.agent_id
                ),
            )
            .with_retryable(false)
        })?;
        policy.timeout_ms = timeout_ms;
        policy.max_bytes = self.shared.settings.max_bytes;
        Ok(policy)
    }

    /// One request through the guard, returning the body and where it ended up.
    async fn send(
        &self,
        request: Request<'_>,
        token: &CancellationToken,
    ) -> Result<(Vec<u8>, String, String, u16)> {
        let policy = self.policy_for(request.timeout_ms)?;
        let mut options = GuardedFetchOptions::new(&policy, self.shared.resolver.as_ref());
        options.headers = request.headers;
        options.token = token.clone();
        if let Some(body) = request.body {
            options.method = Method::POST;
            options.body = Some(body);
        }
        let result = guarded_fetch(request.url, options).await?;
        let status = result.response.status.as_u16();
        let content_type = result
            .response
            .headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let retry_after = result
            .response
            .headers
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let url = result.url.to_string();
        let body = result.response.bytes().await?;
        if (status == 429 || status == 503) && retry_after.is_some() {
            return Err(WireError::new(
                ErrorKind::RateLimited,
                format!(
                    "HTTP {status} with Retry-After {}",
                    retry_after.unwrap_or(0)
                ),
            )
            .with_detail("retryAfterSeconds", retry_after.unwrap_or(0)));
        }
        Ok((body, url, content_type, status))
    }
}

impl WebPort for LiveWeb {
    fn policy(&self) -> Option<&NetworkPolicy> {
        self.policy.as_ref()
    }

    fn read_timeout_ms(&self) -> u64 {
        self.shared.settings.read_timeout_ms
    }

    fn backend_hosts(&self) -> Vec<String> {
        match self.shared.settings.search_provider {
            WebSearchProvider::Auto => vec![
                duckduckgo::HOST.to_owned(),
                brave::HOST.to_owned(),
                mojeek::HOST.to_owned(),
                hn::HOST.to_owned(),
            ],
            WebSearchProvider::Searxng => reqwest::Url::parse(&self.shared.settings.search_url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .into_iter()
                .collect(),
        }
    }

    fn fetch<'a>(&'a self, url: &'a str, token: CancellationToken) -> BoxFuture<'a, Result<Page>> {
        Box::pin(async move {
            // A bare domain is what a model copies out of a search result, and a
            // parser error there reads as a broken tool rather than a fixable
            // argument.
            let target = if url.contains("://") {
                url.to_owned()
            } else {
                format!("https://{url}")
            };

            if let Some(hit) = self.shared.pages.get(&self.agent_id, &target) {
                return Ok(hit);
            }

            let (body, final_url, content_type, status) = self
                .send(
                    Request {
                        url: &target,
                        headers: browser_headers(&self.shared.settings.user_agent),
                        timeout_ms: self.shared.settings.timeout_ms,
                        body: None,
                    },
                    &token,
                )
                .await?;

            if status >= 400 {
                return Err(http_failure(status, &final_url));
            }

            let page = tokio::task::spawn_blocking(move || {
                extract(&body, &final_url, &content_type, false)
            })
            .await
            .map_err(|error| {
                WireError::new(ErrorKind::Internal, "Extraction did not finish").with_source(error)
            })?;

            if page.ok() {
                self.shared.pages.put(&self.agent_id, &target, page.clone());
            }
            Ok(page)
        })
    }

    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
        token: CancellationToken,
    ) -> BoxFuture<'a, Result<SearchOutcome>> {
        Box::pin(async move {
            let key = query.key();
            if let Some(hits) = self.shared.searches.get(&self.agent_id, &key) {
                return Ok(SearchOutcome {
                    hits,
                    problems: Vec::new(),
                });
            }

            let mut problems: Vec<String> = Vec::new();
            let hits = match self.shared.settings.search_provider {
                WebSearchProvider::Auto => self.rotate(query, &token, &mut problems).await,
                WebSearchProvider::Searxng => self.configured(query, &token, &mut problems).await,
            };

            if !hits.is_empty() {
                self.shared.searches.put(&self.agent_id, &key, hits.clone());
            }
            Ok(SearchOutcome { hits, problems })
        })
    }
}

impl LiveWeb {
    /// The keyless rotation, in order, skipping whatever is cooling off.
    ///
    /// The starting point is random so an install does not send every cold query
    /// to the same front door, which is how one address gets itself blocked.
    async fn rotate(
        &self,
        query: &SearchQuery,
        token: &CancellationToken,
        problems: &mut Vec<String>,
    ) -> Vec<SearchHit> {
        type Backend = (&'static str, &'static str, fn(&SearchQuery) -> String);
        // Ordered by what they actually did when the fixtures were captured:
        // DuckDuckGo and Brave answered, Mojeek served a captcha on the first
        // request. That will change, which is why the order is a starting point
        // rather than a ranking and the start index is random.
        let web: [Backend; 3] = [
            (duckduckgo::HOST, duckduckgo::REFERER, duckduckgo::url),
            (brave::HOST, brave::REFERER, brave::url),
            (mojeek::HOST, mojeek::REFERER, mojeek::url),
        ];

        let mut seed = [0u8; 1];
        self.shared.random.fill(&mut seed);
        let start = usize::from(seed[0]) % web.len();

        for offset in 0..web.len() {
            let (host, referer, build) = web[(start + offset) % web.len()];
            if self.shared.cooling_off(host) {
                problems.push(format!("{host}: cooling off after a recent failure"));
                continue;
            }
            let url = build(query);
            match self
                .send(
                    Request {
                        url: &url,
                        headers: backend_headers(&self.shared.settings.user_agent, referer),
                        timeout_ms: self.shared.settings.timeout_ms,
                        body: None,
                    },
                    token,
                )
                .await
            {
                Ok((body, final_url, _, status)) if status < 400 => {
                    let html = String::from_utf8_lossy(&body);
                    let parsed = if host == mojeek::HOST {
                        mojeek::parse(&html, &final_url)
                    } else if host == duckduckgo::HOST {
                        duckduckgo::parse(&html, &final_url)
                    } else {
                        brave::parse(&html, &final_url)
                    };
                    let hits = search::tidy_hits(parsed, query.count);
                    if !hits.is_empty() {
                        return hits;
                    }
                    self.shared.cool_down(host);
                    problems.push(format!("{host}: answered with no results"));
                }
                Ok((_, _, _, status)) => {
                    self.shared.cool_down(host);
                    problems.push(format!("{host}: HTTP {status}"));
                }
                Err(error) if error.kind == ErrorKind::Aborted => return Vec::new(),
                Err(error) => {
                    self.shared.cool_down(host);
                    problems.push(format!("{host}: {}", one_line(&error.message)));
                }
            }
        }

        // The floor. A real API, no key, no rate limit, and a different index.
        if self.shared.cooling_off(hn::HOST) {
            problems.push(format!("{}: cooling off after a recent failure", hn::HOST));
            return Vec::new();
        }
        match self
            .send(
                Request {
                    url: &hn::url(query),
                    headers: backend_headers(&self.shared.settings.user_agent, hn::REFERER),
                    timeout_ms: self.shared.settings.timeout_ms,
                    body: None,
                },
                token,
            )
            .await
        {
            Ok((body, _, _, status)) if status < 400 => {
                search::tidy_hits(hn::parse(&String::from_utf8_lossy(&body)), query.count)
            }
            Ok((_, _, _, status)) => {
                self.shared.cool_down(hn::HOST);
                problems.push(format!("{}: HTTP {status}", hn::HOST));
                Vec::new()
            }
            Err(error) => {
                self.shared.cool_down(hn::HOST);
                problems.push(format!("{}: {}", hn::HOST, one_line(&error.message)));
                Vec::new()
            }
        }
    }

    /// The configured provider. One backend, so a failure is the answer rather
    /// than a reason to try something the operator did not choose.
    async fn configured(
        &self,
        query: &SearchQuery,
        token: &CancellationToken,
        problems: &mut Vec<String>,
    ) -> Vec<SearchHit> {
        let base = self.shared.settings.search_url.trim();
        if base.is_empty() {
            problems.push("searxng: no instance URL. Set tools.web.searchUrl.".to_owned());
            return Vec::new();
        }
        let url = searxng::url(base, query);
        match self
            .send(
                Request {
                    url: &url,
                    headers: backend_headers(&self.shared.settings.user_agent, base),
                    timeout_ms: self.shared.settings.timeout_ms,
                    body: None,
                },
                token,
            )
            .await
        {
            Ok((body, _, _, status)) if status < 400 => {
                let parsed = searxng::parse(&String::from_utf8_lossy(&body));
                search::tidy_hits(parsed, query.count)
            }
            Ok((_, _, _, status)) => {
                // A SearXNG instance answers 403 to a JSON request until its
                // `formats` list names `json`, which is the usual cause and not
                // something a retry fixes.
                problems.push(format!(
                    "searxng: HTTP {status}. Check that the instance enables the JSON format."
                ));
                Vec::new()
            }
            Err(error) => {
                problems.push(format!("searxng: {}", one_line(&error.message)));
                Vec::new()
            }
        }
    }
}

fn one_line(message: &str) -> String {
    let joined = message.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(120).collect()
}

/// What a status code means for the model's next move.
fn http_failure(status: u16, url: &str) -> WireError {
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned());
    let message = if matches!(status, 401 | 403 | 429) {
        format!(
            "HTTP {status} from {host}. The site is refusing automated requests.\n  \
             Try a different source, or its API if it has one. TLS fingerprinting is not\n  \
             defeated here, so retrying this URL will not help."
        )
    } else {
        format!("HTTP {status} from {host}. Try again, or a different source.")
    };
    // A bot wall stays a bot wall; a retry only spends the turn's clock.
    WireError::new(ErrorKind::Network, message).with_retryable(!matches!(status, 401 | 403 | 429))
}

/// Reads the live settings tree.
///
/// A closure rather than a snapshot, so the caches and the cooldown map survive
/// a reconfigure. Taking a copy here would empty both on every settings save,
/// which is exactly when an operator is most likely to be trying something
/// twice.
pub type ConfigSource = Arc<dyn Fn() -> Arc<Config> + Send + Sync>;

/// Builds a port per agent, over state the whole install shares.
pub struct LiveWebResolver {
    shared: Arc<Shared>,
    config: ConfigSource,
}

impl LiveWebResolver {
    /// The resolver for one running install.
    pub fn new(
        config: ConfigSource,
        settings: WebSettings,
        resolver: Arc<dyn DnsResolver>,
        clock: Arc<dyn Clock>,
        random: Arc<dyn RandomSource>,
    ) -> LiveWebResolver {
        let pages = WebCache::new(settings.cache_entries, settings.cache_ttl_ms, clock.clone());
        let searches = WebCache::new(settings.cache_entries, settings.cache_ttl_ms, clock.clone());
        LiveWebResolver {
            shared: Arc::new(Shared {
                settings,
                resolver,
                clock,
                random,
                pages,
                searches,
                cooling: Mutex::new(HashMap::new()),
            }),
            config,
        }
    }
}

impl WebResolver for LiveWebResolver {
    fn for_agent(&self, agent_id: &str) -> Option<Arc<dyn WebPort>> {
        let config = (self.config)();
        let entry = config.agents.list.get(agent_id);
        let environment = entry.map(|entry| &entry.environment);
        let on_host = environment.is_none_or(|environment| environment.name.is_empty());
        let network = environment.map(|environment| &environment.network).cloned();
        // A policy the grammar refuses is an agent that cannot reach anything,
        // which the tool then says in words. The config check refuses it at
        // startup too, so this is the belt rather than the braces.
        let policy = network
            .and_then(|network| NetworkPolicy::for_agent(&network, agent_id, on_host).ok())
            .or_else(|| {
                on_host.then(|| NetworkPolicy {
                    agent_id: agent_id.to_owned(),
                    ..NetworkPolicy::default()
                })
            });
        Some(Arc::new(LiveWeb {
            shared: Arc::clone(&self.shared),
            agent_id: agent_id.to_owned(),
            policy,
        }) as Arc<dyn WebPort>)
    }
}

/// A policy with a shorter deadline, for the concurrent reads inside a search.
///
/// `guarded_fetch` builds its own deadline from `policy.timeout_ms` at entry, so
/// a shorter per-read timeout is expressed by handing it a different policy.
/// There is no other way in: the deadline type is private.
pub fn with_timeout(policy: &NetworkPolicy, timeout_ms: u64) -> NetworkPolicy {
    NetworkPolicy {
        timeout_ms,
        ..policy.clone()
    }
}

/// The deadline a batch of reads shares, for the caller that fans them out.
pub fn read_deadline(settings: &WebSettings) -> Duration {
    Duration::from_millis(settings.read_timeout_ms)
}
