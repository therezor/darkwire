//! A token bucket per caller, on the injected clock.
//!
//! Hand-rolled rather than taken off the shelf, for one reason that decides it:
//! every rate limiter available reads the wall clock directly, and a limit
//! tested by sleeping through its window is a test that is either slow or
//! flaky. This one reads [`Clock`], so a test moves a minute in one call and
//! asserts on the boundary rather than around it.
//!
//! Two independent limits, deliberately. The global one comes from
//! `server.auth.rateLimitPerMinute`, where `0` means off — the same convention
//! every other `*PerMinute` field in the config uses. A per-route limit is not
//! part of that setting and is **not** switched off with it: the login's ten a
//! minute is a credential-guessing defence, and an operator turning off the
//! general limiter is not asking for unlimited password attempts.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use ghostai_core::Clock;
use parking_lot::Mutex;
use tower::{Layer, Service};

use crate::errors::HttpError;

/// One window, in milliseconds.
pub const WINDOW_MS: i64 = 60_000;

/// How many requests one caller may make in one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    /// Requests allowed per window.
    pub max: u32,
    /// The window, in milliseconds.
    pub window_ms: i64,
}

impl Quota {
    /// A per-minute allowance.
    pub fn per_minute(max: u32) -> Quota {
        Quota {
            max,
            window_ms: WINDOW_MS,
        }
    }
}

/// One caller's consumption of one quota.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    /// Requests counted so far in this window.
    used: u32,
    /// When the window this count belongs to began.
    started_ms: i64,
}

/// The shared counter behind one quota.
///
/// A fixed window rather than a sliding one, which is what an operator setting
/// "sixty a minute" expects to read in a log: a burst at the boundary is the
/// known cost, and the limit exists to stop a script, not a coordinated
/// attack.
pub struct RateLimiter {
    quota: Quota,
    clock: Arc<dyn Clock>,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    /// A limiter over one quota.
    pub fn new(quota: Quota, clock: Arc<dyn Clock>) -> Arc<RateLimiter> {
        Arc::new(RateLimiter {
            quota,
            clock,
            buckets: Mutex::new(HashMap::new()),
        })
    }

    /// Counts one request, answering with the milliseconds to wait when the
    /// caller is over.
    ///
    /// A refusal does **not** consume: otherwise a client polling through a
    /// closed window would hold it closed forever, which turns a rate limit
    /// into a lockout.
    pub fn check(&self, key: &str) -> Option<i64> {
        let now = self.clock.now_ms();
        let mut buckets = self.buckets.lock();

        // The map is only ever read by key, so expired entries cost memory and
        // nothing else. Dropping them here keeps a long-lived process from
        // holding one entry per address it has ever seen.
        buckets.retain(|_, bucket| now.saturating_sub(bucket.started_ms) < self.quota.window_ms);

        let bucket = buckets.entry(key.to_owned()).or_insert(Bucket {
            used: 0,
            started_ms: now,
        });
        if now.saturating_sub(bucket.started_ms) >= self.quota.window_ms {
            bucket.used = 0;
            bucket.started_ms = now;
        }
        if bucket.used >= self.quota.max {
            return Some(self.quota.window_ms - (now - bucket.started_ms));
        }
        bucket.used += 1;
        None
    }

    /// Forgets every count. The composition root calls this when the settings
    /// that configure the limit are saved.
    pub fn reset(&self) {
        self.buckets.lock().clear();
    }
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("quota", &self.quota)
            .field("tracked", &self.buckets.lock().len())
            .finish_non_exhaustive()
    }
}

/// The tower layer that applies one limiter to whatever it wraps.
#[derive(Debug, Clone)]
pub struct RateLimitLayer {
    limiter: Arc<RateLimiter>,
}

impl RateLimitLayer {
    /// Wraps one limiter.
    pub fn new(limiter: Arc<RateLimiter>) -> RateLimitLayer {
        RateLimitLayer { limiter }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;

    fn layer(&self, inner: S) -> RateLimitService<S> {
        RateLimitService {
            inner,
            limiter: Arc::clone(&self.limiter),
        }
    }
}

/// The service [`RateLimitLayer`] produces.
#[derive(Debug, Clone)]
pub struct RateLimitService<S> {
    inner: S,
    limiter: Arc<RateLimiter>,
}

impl<S> Service<Request> for RateLimitService<S>
where
    S: Service<Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = futures::future::BoxFuture<'static, Result<Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let key = caller_key(&request);
        if let Some(retry_ms) = self.limiter.check(&key) {
            let seconds = retry_ms.div_euclid(1000) + i64::from(retry_ms.rem_euclid(1000) > 0);
            let response = HttpError::too_many_requests(format!(
                "Rate limit exceeded. Retry in {seconds} second{}.",
                if seconds == 1 { "" } else { "s" }
            ))
            .into_response();
            return Box::pin(async move { Ok(response) });
        }
        // `Service::call` may only be invoked on the instance `poll_ready`
        // returned for, so the clone that is spawned is the *ready* one.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move { inner.call(request).await })
    }
}

/// Who is being limited.
///
/// The peer address, which is the only identity available before
/// authentication and the one a login limit has to work on. A request with no
/// recorded peer — an in-process test service — shares one bucket, which is
/// what a test wants and is unreachable over a socket.
fn caller_key(request: &Request) -> String {
    request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip())
        .map_or_else(|| "local".to_owned(), |ip: IpAddr| ip.to_string())
}
