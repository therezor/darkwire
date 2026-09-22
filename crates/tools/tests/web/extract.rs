//! A body into markdown, against committed pages rather than the live web.

#![allow(
    clippy::unwrap_used,
    reason = "a fixture that cannot load fails anyway"
)]

use darkwire_tools::web::{PageKind, decode, extract};

const DOCS: &[u8] = include_bytes!("../fixtures/pages/docs.html");
const SPA: &[u8] = include_bytes!("../fixtures/pages/spa-shell.html");
const INDEX: &[u8] = include_bytes!("../fixtures/pages/index.html");
const STATUS: &[u8] = include_bytes!("../fixtures/pages/status.html");
const SHIFT_JIS: &[u8] = include_bytes!("../fixtures/pages/shift-jis.html");

const URL: &str = "https://www.sqlite.org/wal.html";

#[test]
fn an_article_keeps_its_structure_and_loses_the_chrome() {
    let page = extract(DOCS, URL, "text/html; charset=utf-8", false);
    assert_eq!(page.kind, PageKind::Article);
    assert_eq!(page.title, "Write-Ahead Logging | SQLite");

    // Structure is the point: a model reads headings to find the section that
    // answers the question, and a flattened page cannot be navigated.
    assert!(page.text.contains("## Advantages"), "{}", page.text);
    assert!(page.text.contains("## Comparison"), "{}", page.text);
    assert!(page.text.contains("1.  WAL is significantly faster"));
    assert!(page.text.contains("2.  WAL provides more concurrency"));
    assert!(page.text.contains("```"), "the code fence survives");
    assert!(page.text.contains("PRAGMA journal_mode=WAL;"));
    assert!(page.text.contains("| Mode"), "the table stays a table");
    assert!(page.text.contains("| rollback"));

    // A link keeps its target, so a follow-up fetch is possible at all.
    assert!(page.text.contains("(https://www.sqlite.org/rollback.html)"));

    // Navigation, footer and script are what extraction exists to drop.
    for chrome in ["Download", "Copyright 2024", "tracking", "Privacy"] {
        assert!(!page.text.contains(chrome), "{chrome}: {}", page.text);
    }
}

#[test]
fn the_three_kinds_of_noise_are_tidied_away() {
    let page = extract(DOCS, URL, "text/html", false);

    // A permalink anchor beside a heading: a real link whose text is a symbol.
    assert!(!page.text.contains('¶'), "{}", page.text);
    // An in-page `#fragment` resolves to the site root, which is a real URL
    // pointing at the wrong document. The text stays, the link goes.
    assert!(page.text.contains("the note below"));
    assert!(!page.text.contains("#synchronous)"), "{}", page.text);
    // The `h1` repeats the `<title>`, which the caller already prints.
    assert!(
        !page.text.starts_with("# Write-Ahead Logging"),
        "{}",
        page.text
    );
}

/// Markup with no text is a page rendered client-side, and there is no
/// JavaScript engine here. That needs a different sentence from "nothing found".
#[test]
fn a_client_rendered_shell_is_empty_rather_than_wrong() {
    let page = extract(SPA, "https://app.example/", "text/html", false);
    assert_eq!(page.kind, PageKind::Empty);
    assert!(!page.ok());
}

/// A status page has no article in the sense a newspaper does, and the reader
/// still wants every line of it. Extraction is scored rather than structural,
/// so this comes back as an article; what matters is that nothing was dropped.
#[test]
fn a_page_that_is_not_prose_still_comes_back_whole() {
    let page = extract(STATUS, "https://status.example/", "text/html", false);
    assert!(page.ok());
    for line in ["api: ok", "db: degraded", "webhooks: ok", "11:40 UTC"] {
        assert!(page.text.contains(line), "{line}: {}", page.text);
    }
}

/// A directory listing scores as an article, and either way every file name
/// survives, which is the thing a reader came for.
#[test]
fn a_listing_keeps_its_entries_and_their_targets() {
    let page = extract(INDEX, "https://mirror.example/pub/", "text/html", false);
    assert!(page.ok());
    assert!(page.text.contains("v2.0.tar.gz"), "{}", page.text);
    assert!(page.text.contains("SHA256SUMS"));
    assert!(page.text.contains("https://mirror.example/pub/v1.0.tar.gz"));
}

/// Lossy UTF-8 would turn this into mojibake, and a model handed mojibake
/// reports the site as broken rather than the tool.
#[test]
fn a_declared_charset_is_honoured() {
    let text = decode(SHIFT_JIS, "text/html");
    assert!(text.contains("日本語"), "{text}");
    assert!(!text.contains('\u{fffd}'), "no replacement characters");
}

#[test]
fn a_charset_on_the_content_type_wins_over_sniffing() {
    let text = decode("héllo".as_bytes(), "text/plain; charset=utf-8");
    assert_eq!(text, "héllo");
}

#[test]
fn a_pdf_is_recognised_by_either_the_type_or_the_magic_bytes() {
    for (body, content_type) in [
        (b"%PDF-1.7\n..." as &[u8], "application/octet-stream"),
        (b"anything", "application/pdf"),
    ] {
        let page = extract(body, "https://x.example/p", content_type, false);
        assert_eq!(page.kind, PageKind::Pdf);
        assert!(!page.ok());
    }
}

#[test]
fn a_textual_body_comes_back_as_itself() {
    let page = extract(
        br#"{"ok":true}"#,
        "https://api.example/v1",
        "application/json",
        false,
    );
    assert_eq!(page.kind, PageKind::Text);
    assert_eq!(page.text, r#"{"ok":true}"#);
    assert!(page.ok());
}

#[test]
fn bytes_that_are_not_text_are_refused_rather_than_mangled() {
    let page = extract(
        &[0x89, 0x50, 0x4e, 0x47],
        "https://x.example/i.png",
        "image/png",
        false,
    );
    assert_eq!(page.kind, PageKind::Binary);
    assert!(!page.ok());
}

#[test]
fn an_empty_body_is_empty() {
    let page = extract(b"", "https://x.example/", "text/plain", false);
    assert_eq!(page.kind, PageKind::Empty);
}

/// `raw` is the escape hatch for an API that returns a document the extractor
/// would helpfully ruin.
#[test]
fn raw_returns_the_document_untouched() {
    let page = extract(DOCS, URL, "text/html", true);
    assert_eq!(page.kind, PageKind::Text);
    assert!(page.text.contains("<nav>"), "markup survives");
    assert!(page.text.contains("Copyright 2024"));
}

/// A body with no content type at all is judged by its first byte, because a
/// server that says nothing is common and a model should not have to care.
#[test]
fn html_is_recognised_without_a_content_type() {
    let page = extract(DOCS, URL, "", false);
    assert_eq!(page.kind, PageKind::Article);
}
