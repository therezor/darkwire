//! A SearXNG instance the operator runs.
//!
//! The only free configuration that is actually reliable, because the instance
//! is theirs: it is not rate-limiting them, its markup is not going to change
//! under them, and it returns JSON rather than a page written for a human.

use std::fmt::Write as _;

use serde::Deserialize;

use crate::web::port::{SearchHit, SearchQuery};
use crate::web::search::clean;

/// The request URL against a configured instance.
pub fn url(base: &str, query: &SearchQuery) -> String {
    let root = base.trim_end_matches('/');
    let mut url = format!(
        "{root}/search?q={}&format=json",
        query.terms.replace(' ', "+")
    );
    if let Some(recent) = query.recent {
        let _ = write!(url, "&time_range={}", window(recent));
    }
    url
}

fn window(recent: crate::web::port::Recency) -> &'static str {
    use crate::web::port::Recency;
    match recent {
        Recency::Day => "day",
        Recency::Week => "week",
        Recency::Month => "month",
        Recency::Year => "year",
    }
}

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    results: Vec<Row>,
}

#[derive(Debug, Deserialize)]
struct Row {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

/// Every result in one response.
pub fn parse(body: &str) -> Vec<SearchHit> {
    let Ok(response) = serde_json::from_str::<Response>(body) else {
        return Vec::new();
    };
    response
        .results
        .into_iter()
        .map(|row| SearchHit {
            title: clean(&row.title),
            url: row.url,
            snippet: clean(&row.content),
            source: String::new(),
        })
        .collect()
}
