//! Mojeek: an independent index, served as static HTML.
//!
//! Server-rendered and pleasant to parse when it answers. It is not first in
//! the rotation, because it served a captcha on the first request made while
//! writing this, which is the fixture kept under `tests/fixtures/serp`. The
//! selector below is therefore the one part of this module that has never been
//! checked against a page with results on it.

use std::fmt::Write as _;

use dom_query::Document;

use crate::web::port::{SearchHit, SearchQuery};
use crate::web::search::{clean, resolve};

/// The host an allow-listed agent has to name.
pub const HOST: &str = "www.mojeek.com";

/// Where a browser would have come from.
pub const REFERER: &str = "https://www.mojeek.com/";

/// The request URL for one query.
pub fn url(query: &SearchQuery) -> String {
    let mut url = format!("https://{HOST}/search?q={}", urlencoding(&query.terms));
    if let Some(recent) = query.recent {
        let _ = write!(url, "&since={}", recent.letter());
    }
    url
}

fn urlencoding(text: &str) -> String {
    reqwest::Url::parse("https://x.test/")
        .and_then(|base| base.join(&format!("?q={text}")))
        .map_or_else(
            |_| text.replace(' ', "+"),
            |url| {
                url.query()
                    .unwrap_or_default()
                    .trim_start_matches("q=")
                    .to_owned()
            },
        )
}

/// Every result on one page.
///
/// Returns an empty list rather than failing on markup it does not recognise,
/// because the rotation's whole point is that one broken parser is skipped.
pub fn parse(html: &str, base: &str) -> Vec<SearchHit> {
    let document = Document::from(html);
    let mut hits = Vec::new();
    for result in &document.select("ul.results-standard li") {
        let anchor = result.select("a.title");
        let href = anchor.attr("href").unwrap_or_default().to_string();
        if href.is_empty() {
            continue;
        }
        hits.push(SearchHit {
            title: clean(&anchor.text()),
            url: resolve(&href, base),
            snippet: clean(&result.select("p.s").text()),
            source: String::new(),
        });
    }
    hits
}
