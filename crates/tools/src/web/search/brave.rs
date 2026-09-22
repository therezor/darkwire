//! Brave's web front door.
//!
//! JavaScript-heavy, and it still server-renders the results: the capture under
//! `tests/fixtures/serp` has them. Its class names are Svelte-hashed and change
//! with every deploy, so the selectors match on the stable half of each class
//! list (`snippet`, `title`, `description`) and never on the hash.

use dom_query::Document;

use crate::web::port::{SearchHit, SearchQuery};
use crate::web::search::{clean, resolve};

/// The host an allow-listed agent has to name.
pub const HOST: &str = "search.brave.com";

/// Where a browser would have come from.
pub const REFERER: &str = "https://search.brave.com/";

/// The request URL for one query.
pub fn url(query: &SearchQuery) -> String {
    format!(
        "https://{HOST}/search?q={}&source=web",
        query.terms.replace(' ', "+")
    )
}

/// Every result on one page.
pub fn parse(html: &str, base: &str) -> Vec<SearchHit> {
    let document = Document::from(html);
    let mut hits = Vec::new();
    for result in &document.select("div.snippet[data-type='web']") {
        let anchor = result.select("a").first();
        let href = anchor.attr("href").unwrap_or_default().to_string();
        if href.is_empty() {
            continue;
        }
        hits.push(SearchHit {
            title: clean(&result.select(".title").first().text()),
            url: resolve(&href, base),
            snippet: clean(&result.select(".description").first().text()),
            source: String::new(),
        });
    }
    hits
}
