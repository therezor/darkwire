//! Hacker News, through Algolia's public API.
//!
//! **Not a peer of the web backends, and never ranked against them.** It
//! searches discussion rather than the web, so it cannot answer "what happened
//! today" and a thread is rarely what a model asked for. It is here because it
//! is a real API with no key and no rate limit, which makes it the one thing
//! that still answers when every scraped front door is blocked at once. For the
//! questions a coding agent actually asks, a thread arguing about SQLite over
//! NFS is often better than the third blog post about it.

use serde::Deserialize;

use crate::web::port::{SearchHit, SearchQuery};
use crate::web::search::clean;

/// The host an allow-listed agent has to name.
pub const HOST: &str = "hn.algolia.com";

/// Where a browser would have come from.
pub const REFERER: &str = "https://hn.algolia.com/";

/// The request URL for one query.
pub fn url(query: &SearchQuery) -> String {
    format!(
        "https://{HOST}/api/v1/search?query={}&hitsPerPage={}",
        query.terms.replace(' ', "%20"),
        query.count
    )
}

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    hits: Vec<Hit>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hit {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    story_title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default, rename = "objectID")]
    object_id: Option<String>,
    #[serde(default)]
    points: Option<i64>,
    #[serde(default)]
    num_comments: Option<i64>,
}

/// Every result in one response.
pub fn parse(body: &str) -> Vec<SearchHit> {
    let Ok(response) = serde_json::from_str::<Response>(body) else {
        return Vec::new();
    };
    response
        .hits
        .into_iter()
        .filter_map(|hit| {
            let title = clean(hit.title.or(hit.story_title).unwrap_or_default().as_str());
            if title.is_empty() {
                return None;
            }
            let thread = format!(
                "https://news.ycombinator.com/item?id={}",
                hit.object_id.unwrap_or_default()
            );
            // An `Ask HN` post has no URL of its own: the thread is the content.
            let url = match hit.url {
                Some(url) if url.starts_with("http") => url,
                _ => thread.clone(),
            };
            Some(SearchHit {
                title,
                url,
                snippet: format!(
                    "{} points, {} comments, {thread}",
                    hit.points.unwrap_or_default(),
                    hit.num_comments.unwrap_or_default()
                ),
                source: "hn".to_owned(),
            })
        })
        .collect()
}
