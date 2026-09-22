//! `web_fetch`: what the model is handed, and what each failure tells it to do.

#![allow(clippy::unwrap_used, reason = "test assertions")]

use std::sync::Arc;

use darkwire_core::ErrorKind;
use darkwire_tools::testkit::{FakeWeb, TestWorkspace};
use darkwire_tools::web::{Page, PageKind, SearchOutcome, SearchQuery, WebPort};
use darkwire_tools::{AnyTool, BoxFuture, ToolContext, web_fetch_tool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::common::{run, text};

fn with_web(port: Arc<dyn WebPort>) -> (TestWorkspace, ToolContext) {
    let ws = TestWorkspace::new();
    let mut ctx = ws.context().clone();
    ctx.web = Some(port);
    (ws, ctx)
}

/// A port that always answers with one page of a given kind.
#[derive(Debug)]
struct OnePage(Page);

impl WebPort for OnePage {
    fn policy(&self) -> Option<&darkwire_security::NetworkPolicy> {
        static OPEN: std::sync::OnceLock<darkwire_security::NetworkPolicy> =
            std::sync::OnceLock::new();
        Some(OPEN.get_or_init(darkwire_security::NetworkPolicy::default))
    }
    fn read_timeout_ms(&self) -> u64 {
        1_000
    }
    fn backend_hosts(&self) -> Vec<String> {
        Vec::new()
    }
    fn fetch<'a>(
        &'a self,
        _url: &'a str,
        _token: CancellationToken,
    ) -> BoxFuture<'a, darkwire_core::Result<Page>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
    fn search<'a>(
        &'a self,
        _query: &'a SearchQuery,
        _token: CancellationToken,
    ) -> BoxFuture<'a, darkwire_core::Result<SearchOutcome>> {
        Box::pin(async move { Ok(SearchOutcome::default()) })
    }
}

fn page(kind: PageKind, text: &str) -> Arc<dyn WebPort> {
    Arc::new(OnePage(Page {
        url: "https://example.test/a".to_owned(),
        title: "A page".to_owned(),
        text: text.to_owned(),
        kind,
        content_type: "text/html".to_owned(),
        note: String::new(),
    })) as Arc<dyn WebPort>
}

fn tool() -> AnyTool {
    web_fetch_tool()
}

#[tokio::test]
async fn returns_the_page_under_its_title_and_url() {
    let (_ws, ctx) = with_web(Arc::new(FakeWeb::default()));
    let out = text(&tool(), json!({"url": "https://example.test/a"}), &ctx).await;
    assert!(out.contains("# A page"), "{out}");
    assert!(out.contains("<https://example.test/a>"), "{out}");
    assert!(out.contains("worth reading"), "{out}");
}

/// The two absences are different facts and need different sentences: one is
/// about the install, the other about this agent.
#[tokio::test]
async fn says_which_kind_of_absence_it_met() {
    let ws = TestWorkspace::new();
    let bare = ws.context().clone();
    let out = run(&tool(), json!({"url": "https://example.test/"}), &bare).await;
    assert!(out.is_error);
    assert!(
        out.content.contains("no web access configured"),
        "{}",
        out.content
    );

    let (_ws, ctx) = with_web(Arc::new(FakeWeb {
        reachable: false,
        ..FakeWeb::default()
    }));
    let off = run(&tool(), json!({"url": "https://example.test/"}), &ctx).await;
    assert!(off.is_error);
    assert!(off.content.contains("no network access"), "{}", off.content);
}

/// Each of these arrived with a body that did not answer, so each names the next
/// move rather than reporting nothing.
#[tokio::test]
async fn every_unreadable_body_says_what_to_try_instead() {
    for (kind, needle) in [
        (PageKind::Empty, "rendered\nclient-side"),
        (PageKind::Pdf, "No PDF text extraction"),
        (PageKind::Binary, "nothing to read"),
    ] {
        let (_ws, ctx) = with_web(page(kind, ""));
        let out = run(&tool(), json!({"url": "https://example.test/a"}), &ctx).await;
        assert!(out.is_error, "{kind:?}");
        assert!(
            out.content.to_lowercase().contains(&needle.to_lowercase()),
            "{kind:?}: {}",
            out.content
        );
    }
}

/// The registry keeps the head and the tail, so a page that overflows would come
/// back with its middle removed. Cutting here is what keeps it recoverable.
#[tokio::test]
async fn a_long_page_is_cut_at_a_line_and_says_how_to_get_the_rest() {
    let (_ws, mut ctx) = with_web(page(
        PageKind::Article,
        &"a line of text that is long enough to matter\n".repeat(400),
    ));
    let mut config = (*ctx.config).clone();
    config.max_output_chars = 2_000;
    ctx = ctx.with_config(config);

    let out = text(&tool(), json!({"url": "https://example.test/a"}), &ctx).await;
    assert!(out.contains("characters not shown"), "{out}");
    assert!(out.contains("web_fetch https://example.test/a"), "{out}");
    assert!(out.encode_utf16().count() <= 2_000, "over budget");
    // Cut at a newline, never mid-sentence: a broken line reads as corruption.
    let body = out.split("\n\n[").next().unwrap();
    assert!(body.trim_end().ends_with("matter"), "{body}");
}

/// An explicit larger cap is clamped rather than honoured, because honouring it
/// guarantees the head-and-tail cut the budget exists to avoid.
#[tokio::test]
async fn an_oversized_max_chars_is_clamped_to_the_budget() {
    let (_ws, mut ctx) = with_web(page(PageKind::Article, &"x".repeat(50_000)));
    let mut config = (*ctx.config).clone();
    config.max_output_chars = 1_500;
    ctx = ctx.with_config(config);

    let out = text(
        &tool(),
        json!({"url": "https://example.test/a", "maxChars": 40_000}),
        &ctx,
    )
    .await;
    assert!(out.encode_utf16().count() <= 1_500, "over budget");
}

#[tokio::test]
async fn a_cancelled_turn_is_noticed_before_anything_is_fetched() {
    let (_ws, ctx) = with_web(Arc::new(FakeWeb::default()));
    ctx.token.cancel();
    let out = run(&tool(), json!({"url": "https://example.test/a"}), &ctx).await;
    assert_eq!(out.kind, Some(ErrorKind::Aborted));
}
