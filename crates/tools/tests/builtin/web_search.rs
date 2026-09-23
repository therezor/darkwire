//! `web_search`: reading by default, staying inside the budget, and naming what
//! it could not read.

#![allow(clippy::unwrap_used, reason = "test assertions")]

use std::sync::Arc;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_tools::testkit::{FakeWeb, TestWorkspace};
use darkwire_tools::web::{Page, PageKind, SearchHit, SearchOutcome, SearchQuery, WebPort};
use darkwire_tools::{AnyTool, BoxFuture, ToolContext, web_search_tool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{run, text};

fn tool() -> AnyTool {
    web_search_tool()
}

fn hits(count: usize) -> Vec<SearchHit> {
    (0..count)
        .map(|index| SearchHit {
            title: format!("Result {index}"),
            url: format!("https://example.test/{index}"),
            snippet: "What it is about.".to_owned(),
            source: String::new(),
        })
        .collect()
}

/// A port whose pages each answer, or refuse, as the test dictates.
#[derive(Debug)]
struct Scripted {
    hits: Vec<SearchHit>,
    problems: Vec<String>,
    /// URLs that come back unreadable rather than as text.
    unreadable: Vec<String>,
}

impl WebPort for Scripted {
    fn policy(&self) -> Option<&darkwire_security::NetworkPolicy> {
        static OPEN: std::sync::OnceLock<darkwire_security::NetworkPolicy> =
            std::sync::OnceLock::new();
        Some(OPEN.get_or_init(darkwire_security::NetworkPolicy::default))
    }
    fn read_timeout_ms(&self) -> u64 {
        1_000
    }
    fn backend_hosts(&self) -> Vec<String> {
        vec!["search.test".to_owned()]
    }
    fn fetch<'a>(&'a self, url: &'a str, _t: CancellationToken) -> BoxFuture<'a, Result<Page>> {
        Box::pin(async move {
            if self.unreadable.iter().any(|dead| dead == url) {
                return Err(WireError::new(
                    ErrorKind::Network,
                    "HTTP 403 from example.test",
                ));
            }
            Ok(Page {
                url: url.to_owned(),
                title: format!("Page at {url}"),
                text: "Something worth reading, at length. ".repeat(30),
                kind: PageKind::Article,
                content_type: "text/html".to_owned(),
                note: String::new(),
            })
        })
    }
    fn search<'a>(
        &'a self,
        _q: &'a SearchQuery,
        _t: CancellationToken,
    ) -> BoxFuture<'a, Result<SearchOutcome>> {
        Box::pin(async move {
            Ok(SearchOutcome {
                hits: self.hits.clone(),
                problems: self.problems.clone(),
            })
        })
    }
}

fn with(port: Scripted, budget: u64) -> (TestWorkspace, ToolContext) {
    let ws = TestWorkspace::new();
    let mut ctx = ws.context().clone();
    ctx.web = Some(Arc::new(port));
    let mut config = (*ctx.config).clone();
    config.max_output_chars = budget;
    (ws, ctx.with_config(config))
}

/// The decision this tool turns on. A model handed six one-line snippets answers
/// from the snippets, so the pages come back without being asked for.
#[tokio::test]
async fn reads_the_top_results_without_being_asked() {
    let (_ws, ctx) = with(
        Scripted {
            hits: hits(6),
            problems: Vec::new(),
            unreadable: Vec::new(),
        },
        16_000,
    );
    let out = text(&tool(), json!({"query": "sqlite wal"}), &ctx).await;

    assert!(out.contains("1. Result 0"), "the list is there: {out}");
    // Three, because one source is an opinion and two that agree is a coincidence.
    assert!(out.contains("===== [1] "), "{out}");
    assert!(out.contains("===== [3] "), "{out}");
    assert!(!out.contains("===== [4] "), "{out}");
}

#[tokio::test]
async fn read_zero_returns_the_list_alone() {
    let (_ws, ctx) = with(
        Scripted {
            hits: hits(4),
            problems: Vec::new(),
            unreadable: Vec::new(),
        },
        16_000,
    );
    let out = text(&tool(), json!({"query": "x", "read": 0}), &ctx).await;
    assert!(out.contains("1. Result 0"));
    assert!(!out.contains("====="), "{out}");
}

/// "Read the top three" means three that produced text, not three attempts, and
/// the ones that did not are named rather than dropped.
#[tokio::test]
async fn walks_past_unreadable_results_and_names_them() {
    let (_ws, ctx) = with(
        Scripted {
            hits: hits(8),
            problems: Vec::new(),
            unreadable: vec![
                "https://example.test/0".to_owned(),
                "https://example.test/1".to_owned(),
            ],
        },
        16_000,
    );
    let out = text(&tool(), json!({"query": "x"}), &ctx).await;

    assert!(out.contains("===== [3] "), "still read three: {out}");
    assert!(out.contains("not readable, skipped"), "{out}");
    assert!(out.contains("https://example.test/0"), "{out}");
}

/// Overflowing hands the middle of the result to the registry's head-and-tail
/// cut, so fewer extracts is the answer rather than a smaller share.
#[tokio::test]
async fn a_small_budget_reduces_the_reads_and_says_so() {
    let (_ws, ctx) = with(
        Scripted {
            hits: hits(3),
            problems: Vec::new(),
            unreadable: Vec::new(),
        },
        3_000,
    );
    let out = text(&tool(), json!({"query": "x"}), &ctx).await;
    assert!(out.contains("budget for"), "{out}");
    assert!(out.encode_utf16().count() <= 3_000, "over budget");
}

/// Every backend failing is a different situation from finding nothing, and the
/// advice differs, so the reasons are printed rather than counted.
#[tokio::test]
async fn names_every_backend_that_failed_and_says_not_to_retry() {
    let (_ws, ctx) = with(
        Scripted {
            hits: Vec::new(),
            problems: vec![
                "search.test: HTTP 429".to_owned(),
                "hn.algolia.com: connection reset".to_owned(),
            ],
            unreadable: Vec::new(),
        },
        16_000,
    );
    let out = run(&tool(), json!({"query": "x"}), &ctx).await;
    assert!(out.is_error);
    assert!(out.content.contains("HTTP 429"), "{}", out.content);
    assert!(out.content.contains("connection reset"), "{}", out.content);
    assert!(out.content.contains("Do not re-ask it"), "{}", out.content);
    // The operator reading this is the only one who can fix an allow-list.
    assert!(out.content.contains("search.test"), "{}", out.content);
}

#[tokio::test]
async fn an_agent_with_no_egress_says_so_rather_than_searching() {
    let ws = TestWorkspace::new();
    let mut ctx = ws.context().clone();
    ctx.web = Some(Arc::new(FakeWeb {
        reachable: false,
        ..FakeWeb::default()
    }));
    let out = run(&tool(), json!({"query": "x"}), &ctx).await;
    assert!(out.is_error);
    assert!(out.content.contains("no network access"), "{}", out.content);
}

/// A phrase search for a quoted string matches almost nothing, and the
/// instruction not to quote is not always followed.
#[tokio::test]
async fn a_quoted_query_is_accepted_rather_than_matched_literally() {
    let (_ws, ctx) = with(
        Scripted {
            hits: hits(2),
            problems: Vec::new(),
            unreadable: Vec::new(),
        },
        16_000,
    );
    let out = text(&tool(), json!({"query": "\"sqlite wal\"", "read": 0}), &ctx).await;
    assert!(out.contains("1. Result 0"), "{out}");
}

#[tokio::test]
async fn an_empty_query_is_refused() {
    let (_ws, ctx) = with(
        Scripted {
            hits: Vec::new(),
            problems: Vec::new(),
            unreadable: Vec::new(),
        },
        16_000,
    );
    let out = run(&tool(), json!({"query": "  "}), &ctx).await;
    assert!(out.is_error);
    assert!(out.content.contains("empty"), "{}", out.content);
}
