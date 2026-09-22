//! Finding pages, and being honest about how well that can work.
//!
//! **Scraping consumer search front doors degrades, and no amount of care in
//! this module changes that.** There is no maintained Rust equivalent of the
//! multi-backend Python libraries, so these are hand-written parsers against
//! HTML written for humans, served to a single server address that gets
//! rate-limited faster than a residential one. The fixtures under
//! `tests/fixtures/serp` freeze a layout, not a guarantee.
//!
//! Three things make that survivable, and all three are required:
//!
//!  - **More front doors.** One blocked backend is a backend that is skipped,
//!    not a failed search, and the rotation start is random so an install does
//!    not hammer the same one on every cold query.
//!  - **A parser never panics.** It returns an empty list on anything it does
//!    not recognise, so a markup change degrades to the next backend instead of
//!    taking the turn down.
//!  - **Hacker News is the floor.** Algolia's public API is keyless, is a real
//!    API and does not rate-limit. It searches discussion rather than the web,
//!    so it never displaces a working web result, but it answers when scraping
//!    cannot.
//!
//! An operator who needs reliability points this at a SearXNG instance they
//! run. That is documented rather than implied, and it is the only alternative
//! offered: no API key, because a search tool should not require an account
//! with a search company.

pub mod brave;
pub mod duckduckgo;
pub mod hn;
pub mod mojeek;
pub mod searxng;

use crate::web::port::SearchHit;

/// How long a backend is left alone after it fails.
///
/// Not a per-request politeness delay: it exists so a model that retries a
/// failing query does not send another four requests each time it does.
pub const COOLDOWN_MS: i64 = 300_000;

/// Sponsored placements and click trackers, which some front doors mix in.
///
/// Worth dropping rather than ranking, on two counts: one is never the answer,
/// and left in the list it spends one of the reads on a page with nothing to
/// read.
pub fn is_advert(url: &str) -> bool {
    const MARKERS: [&str; 8] = [
        "/aclick?",
        "/aclk?",
        "/pagead/",
        "doubleclick.net",
        "googleadservices.com",
        "googlesyndication.com",
        "duckduckgo.com/y.js",
        "/y.js?",
    ];
    let lowered = url.to_lowercase();
    MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// Collapses runs of whitespace, which SERP markup is full of.
pub fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a result is worth returning at all.
pub fn usable(hit: &SearchHit) -> bool {
    !hit.title.is_empty()
        && (hit.url.starts_with("https://") || hit.url.starts_with("http://"))
        && !is_advert(&hit.url)
}

/// Drops adverts, empties and repeats, and cuts the list to `count`.
///
/// Deduplicated by URL because two front doors agreeing is common and a model
/// handed the same page twice reads it as two sources.
pub fn tidy_hits(hits: Vec<SearchHit>, count: usize) -> Vec<SearchHit> {
    let mut seen: Vec<String> = Vec::new();
    let mut kept: Vec<SearchHit> = Vec::new();
    for hit in hits {
        if !usable(&hit) {
            continue;
        }
        let key = hit.url.trim_end_matches('/').to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        kept.push(hit);
        if kept.len() >= count {
            break;
        }
    }
    kept
}

/// A DuckDuckGo-style redirect wrapper unwrapped back to its target.
///
/// Several front doors route every result through their own counter. Left
/// wrapped, the model fetches the counter, and the allow-list sees the engine's
/// host rather than the destination the operator actually permitted.
pub fn unwrap_redirect(url: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return url.to_owned();
    };
    for key in ["uddg", "url", "u", "q"] {
        if let Some((_, value)) = parsed.query_pairs().find(|(name, _)| name == key)
            && (value.starts_with("http://") || value.starts_with("https://"))
        {
            return value.into_owned();
        }
    }
    url.to_owned()
}

/// A result URL that may be relative, absolute, or wrapped in a redirect.
///
/// **Resolved before it is unwrapped, and that order is the whole function.**
/// DuckDuckGo writes its counter protocol-relative (`//duckduckgo.com/l/?uddg=`),
/// which no URL parser will take on its own, so unwrapping first leaves the
/// counter in place: the model then fetches the engine rather than the page, and
/// an allow-list sees the engine's host rather than the destination an operator
/// actually permitted.
pub fn resolve(href: &str, base: &str) -> String {
    let absolute = if href.starts_with("http://") || href.starts_with("https://") {
        href.to_owned()
    } else {
        reqwest::Url::parse(base)
            .and_then(|base| base.join(href))
            .map_or_else(|_| href.to_owned(), |joined| joined.to_string())
    };
    unwrap_redirect(&absolute)
}
