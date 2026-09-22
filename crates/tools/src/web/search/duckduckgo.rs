//! DuckDuckGo's no-JavaScript endpoint.
//!
//! First in the rotation, because it is what answered when the fixtures were
//! captured. It also serves an anomaly page often enough that it cannot be the
//! only one.
//!
//! Every result is wrapped in a `/l/?uddg=` counter, written **protocol
//! relative**, so the href has to be resolved against the page before it can be
//! unwrapped. Left wrapped, the model fetches the engine rather than the page,
//! and an allow-list sees the engine's host rather than the destination an
//! operator permitted.

use std::fmt::Write as _;

use dom_query::Document;

use crate::web::port::{SearchHit, SearchQuery};
use crate::web::search::{clean, resolve};

/// The host an allow-listed agent has to name.
pub const HOST: &str = "html.duckduckgo.com";

/// Where a browser would have come from.
pub const REFERER: &str = "https://html.duckduckgo.com/";

/// The request URL for one query.
pub fn url(query: &SearchQuery) -> String {
    let mut url = format!("https://{HOST}/html/?q={}", query.terms.replace(' ', "+"));
    if let Some(recent) = query.recent {
        let _ = write!(url, "&df={}", recent.letter());
    }
    if let Some(region) = &query.region {
        let _ = write!(url, "&kl={region}");
    }
    url
}

/// Whether the response is the anomaly page rather than results.
pub fn is_anomaly(html: &str) -> bool {
    html.contains("anomaly-modal") || html.contains("Unfortunately, bots use DuckDuckGo too")
}

/// Every result on one page.
pub fn parse(html: &str, base: &str) -> Vec<SearchHit> {
    if is_anomaly(html) {
        return Vec::new();
    }
    let document = Document::from(html);
    let mut hits = Vec::new();
    for result in &document.select("div.result") {
        let anchor = result.select("a.result__a");
        let href = anchor.attr("href").unwrap_or_default().to_string();
        if href.is_empty() {
            continue;
        }
        hits.push(SearchHit {
            title: clean(&anchor.text()),
            url: resolve(&href, base),
            snippet: clean(&result.select("a.result__snippet").text()),
            source: String::new(),
        });
    }
    hits
}
