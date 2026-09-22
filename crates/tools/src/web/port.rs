//! What a turn is handed when it may reach the web.
//!
//! The same shape as [`crate::tasks::TaskPort`] and
//! [`crate::automation::AutomationPort`]: the interface is declared down here
//! and the composition root supplies the implementation, already scoped to the
//! turn that got it. A model cannot name another agent's policy because it never
//! names a policy at all.
//!
//! Two levels of absence, and they need different sentences. `ctx.web` is `None`
//! only when the install has no web layer at all. A port whose [`WebPort::policy`]
//! returns `None` is an agent whose egress is switched off, which is an operator
//! decision about that agent. Telling a model the first when it is the second
//! wastes a turn.

use std::sync::Arc;

use darkwire_core::Result;
use darkwire_security::NetworkPolicy;
use tokio_util::sync::CancellationToken;

use crate::tool::BoxFuture;
use crate::web::extract::Page;

/// How recent a result has to be. Best effort: several backends ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recency {
    /// The last day.
    Day,
    /// The last week.
    Week,
    /// The last month.
    Month,
    /// The last year.
    Year,
}

impl Recency {
    /// The single letter every scraped front door spells this with.
    pub fn letter(self) -> &'static str {
        match self {
            Recency::Day => "d",
            Recency::Week => "w",
            Recency::Month => "m",
            Recency::Year => "y",
        }
    }
}

/// One query, as the backends receive it.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// The terms, already stripped of quoting the model may have added.
    pub terms: String,
    /// How many results to return.
    pub count: usize,
    /// Only results from this window.
    pub recent: Option<Recency>,
    /// Bias to a region, such as `uk-en`.
    pub region: Option<String>,
}

impl SearchQuery {
    /// The cache key for this query. Two queries differing in any field are two
    /// queries.
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.count,
            self.recent.map_or("", Recency::letter),
            self.region.as_deref().unwrap_or(""),
            self.terms
        )
    }
}

/// One result, before anything has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The link text.
    pub title: String,
    /// Where it points.
    pub url: String,
    /// The backend's one-line summary, which is often all there is.
    pub snippet: String,
    /// Named only when it is not a general web result, so the model can judge
    /// fit for itself.
    pub source: String,
}

/// What one search produced, including what it could not.
#[derive(Debug, Clone, Default)]
pub struct SearchOutcome {
    /// The results, best first.
    pub hits: Vec<SearchHit>,
    /// One line per backend that failed, for the refusal when none succeeded.
    /// The advice differs by reason, so they are kept apart rather than counted.
    pub problems: Vec<String>,
}

/// One turn's access to the web, already scoped to its agent's egress.
pub trait WebPort: Send + Sync {
    /// The policy every fetch runs under. `None` is an agent with no network.
    fn policy(&self) -> Option<&NetworkPolicy>;

    /// One page, fetched and extracted. Cache aware.
    fn fetch<'a>(&'a self, url: &'a str, token: CancellationToken) -> BoxFuture<'a, Result<Page>>;

    /// One query against the configured backends. Cache aware.
    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
        token: CancellationToken,
    ) -> BoxFuture<'a, Result<SearchOutcome>>;

    /// The hosts an allow-listed agent would have to name for search to work.
    /// The refusal prints them, because the operator reading it is the only one
    /// who can act on it.
    fn backend_hosts(&self) -> Vec<String>;

    /// How long one page read inside a search may take.
    fn read_timeout_ms(&self) -> u64;
}

/// Supplies the port a turn's web tools use.
pub trait WebResolver: Send + Sync {
    /// The port for one agent, or `None` when the install has no web layer.
    fn for_agent(&self, agent_id: &str) -> Option<Arc<dyn WebPort>>;
}
