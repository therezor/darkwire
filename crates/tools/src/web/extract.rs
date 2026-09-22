//! A response body into the least markdown that still answers a question.
//!
//! Three decisions shape this module.
//!
//! **Markdown, not flattened text.** A model reads structure. Headings say which
//! section answers the question, a fenced block stays copyable, a table stays a
//! table, and a link keeps its target so a follow-up fetch is possible. Joining
//! every text node with newlines throws all of that away and produces prose that
//! cannot be navigated.
//!
//! **Main-content extraction, not tag stripping.** Removing `nav` and `footer`
//! by name catches the sites that use those elements and nothing else; the rest
//! of the web is `<div class="sidebar">`. `dom_smoothie` scores the DOM for
//! content density instead, which is the difference between 900 tokens of
//! documentation and 6,000 tokens of documentation plus navigation.
//!
//! **Every failure says what to do next.** A client-rendered page, a PDF, a body
//! that is not text at all: each returns a sentence naming the next move rather
//! than empty output, because an agent that cannot tell "nothing there" from
//! "wrong tool" spends the rest of its turn guessing.
//!
//! `dom_smoothie` selects and `htmd` converts, rather than `dom_smoothie`'s own
//! markdown mode, which was measured against it: that mode backslash-escapes
//! every sentence-ending period, numbers every ordered item `1.`, and leaves a
//! blank line inside each code fence. One markdown writer also means the article
//! path and the whole-page fallback cannot disagree about how a table renders.

use std::sync::LazyLock;

use dom_smoothie::Readability;
use regex::Regex;

/// What a body turned out to be. The caller's next sentence depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// An article was found and converted.
    Article,
    /// No article, so this is the page's whole visible text.
    WholePage,
    /// Not HTML, but text a model can read: JSON, CSV, plain.
    Text,
    /// A PDF. No text extraction is built in.
    Pdf,
    /// Bytes that are not text at all.
    Binary,
    /// Markup with no text in it, which means it is rendered client-side.
    Empty,
}

/// One body, extracted.
#[derive(Debug, Clone)]
pub struct Page {
    /// Where it ended up, after redirects. What a relative link resolves against.
    pub url: String,
    /// The `<title>`, or empty.
    pub title: String,
    /// Markdown, or the text itself for a source with no structure to keep.
    pub text: String,
    /// What the body was.
    pub kind: PageKind,
    /// The content type as the server spelled it.
    pub content_type: String,
    /// What went wrong, or what to try instead. Empty on a clean extraction.
    pub note: String,
}

impl Page {
    /// Whether there is text worth showing.
    pub fn ok(&self) -> bool {
        !matches!(
            self.kind,
            PageKind::Empty | PageKind::Pdf | PageKind::Binary
        )
    }
}

/// Below this, markup that produced "text" produced navigation chrome.
const MIN_TEXT_CHARS: usize = 200;

/// How far into a body to look for a `<meta charset>`.
const CHARSET_SNIFF_BYTES: usize = 1024;

/// `[¶](url)` and `[#](url)`: the permalink anchor beside every heading on a
/// Sphinx or Docusaurus page. Kept by extraction because it is a real link, and
/// worthless, because its text is a symbol.
static PERMALINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[[¶#§]\]\([^)]*\)").unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// A markdown link that points inside the page it came from: either a bare
/// `#id2`, or the same anchor resolved against the page URL, which yields the
/// *site root* and so is a real URL pointing at the wrong document. Neither is
/// fetchable, and a model that follows the second one gets a front page. Keep
/// the text, drop the link.
static IN_PAGE_ANCHOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[([^\]]*)\]\((?:https?://[^/)\s]+/?)?#[^)\s]*\)")
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// The separators a site puts between a page title and its own name, so
/// `Coroutines and tasks | Python` can be recognised as the `<h1>` repeated.
static TITLE_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s+[|·:—–-]\s+").unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

static META_CHARSET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)charset\s*=\s*["']?([A-Za-z0-9_.:-]+)"#)
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// Decodes a body, honouring the charset the server or the document declares.
///
/// `String::from_utf8_lossy` alone turns a Shift-JIS or GBK page into mojibake,
/// and a model handed mojibake reports the site as broken rather than the tool.
pub fn decode(body: &[u8], content_type: &str) -> String {
    let label = META_CHARSET
        .captures(content_type)
        .map(|found| found[1].to_owned())
        .or_else(|| {
            // Lossy on purpose: the head of a Shift-JIS document is not valid
            // UTF-8, and that is precisely the document whose declaration we
            // need to read.
            let head = &body[..body.len().min(CHARSET_SNIFF_BYTES)];
            let text = String::from_utf8_lossy(head);
            META_CHARSET
                .captures(&text)
                .map(|found| found[1].to_owned())
        });
    let encoding = label
        .as_deref()
        .and_then(|name| encoding_rs::Encoding::for_label(name.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    encoding.decode(body).0.into_owned()
}

/// Three kinds of noise that survive extraction, removed.
///
/// Small individually, and all three cost tokens on every fetch. Two of them
/// also actively mislead, because a permalink and an in-page anchor both extract
/// as links to a document other than the one they came from.
fn tidy(text: &str, title: &str) -> String {
    let text = PERMALINK.replace_all(text, "");
    let text = IN_PAGE_ANCHOR.replace_all(&text, "$1");

    let mut lines: Vec<&str> = text.trim_start().lines().collect();
    if !title.is_empty()
        && let Some(first) = lines.first()
    {
        // The caller prints the title as the heading, so an `h1` repeating it is
        // said twice. Compared against the title's first segment as well as the
        // whole of it, because a `<title>` usually carries a site-name suffix
        // the `<h1>` does not.
        let heading = first.trim_start_matches('#').trim().to_lowercase();
        let whole = title.trim().to_lowercase();
        let stem = TITLE_SUFFIX
            .split(title.trim())
            .next()
            .unwrap_or_default()
            .to_lowercase();
        if !heading.is_empty() && (heading == whole || heading == stem) {
            lines.remove(0);
            while lines.first().is_some_and(|line| line.trim().is_empty()) {
                lines.remove(0);
            }
        }
    }

    lines
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn to_markdown(html: &str) -> String {
    htmd::HtmlToMarkdown::builder()
        .skip_tags(vec![
            "script", "style", "noscript", "svg", "template", "head", "title",
        ])
        .build()
        .convert(html)
        .unwrap_or_default()
}

/// Whether a content type names something a model can read as text.
fn is_textual(content_type: &str) -> bool {
    let lowered = content_type.to_lowercase();
    lowered.starts_with("text/")
        || lowered.contains("json")
        || lowered.contains("xml")
        || lowered.contains("javascript")
        || lowered.contains("csv")
}

/// One body into a [`Page`].
///
/// Synchronous and free of any runtime type on purpose: `dom_smoothie`'s parser
/// is `!Send`, so this has to run inside `spawn_blocking`, and html5ever over a
/// multi-megabyte document is CPU-bound enough that it should anyway.
pub fn extract(body: &[u8], url: &str, content_type: &str, raw: bool) -> Page {
    let page = |kind: PageKind, title: String, text: String, note: &str| Page {
        url: url.to_owned(),
        title,
        text,
        kind,
        content_type: content_type.to_owned(),
        note: note.to_owned(),
    };

    if content_type.to_lowercase().contains("pdf") || body.starts_with(b"%PDF-") {
        return page(PageKind::Pdf, String::new(), String::new(), "");
    }

    let looks_html = content_type.to_lowercase().contains("html")
        || body
            .iter()
            .find(|byte| !byte.is_ascii_whitespace())
            .is_some_and(|byte| *byte == b'<');

    if !looks_html {
        if !is_textual(content_type) && !body.is_empty() {
            return page(PageKind::Binary, String::new(), String::new(), "");
        }
        let text = decode(body, content_type).trim().to_owned();
        let kind = if text.is_empty() {
            PageKind::Empty
        } else {
            PageKind::Text
        };
        return page(kind, String::new(), text, "");
    }

    let document = decode(body, content_type);
    if raw {
        return page(PageKind::Text, String::new(), document, "");
    }

    let Ok(mut reader) = Readability::new(document.as_str(), Some(url), None) else {
        // A document the parser will not take at all is still a document whose
        // visible text is worth printing.
        let whole = tidy(&to_markdown(&document), "");
        let kind = if whole.len() < MIN_TEXT_CHARS {
            PageKind::Empty
        } else {
            PageKind::WholePage
        };
        return page(kind, String::new(), whole, "");
    };

    let (title, article) = match reader.parse() {
        Ok(article) => (
            article.title.clone(),
            tidy(&to_markdown(&article.content), &article.title),
        ),
        Err(_) => (String::new(), String::new()),
    };
    if article.len() >= MIN_TEXT_CHARS {
        return page(PageKind::Article, title, article, "");
    }

    // A directory listing, a wiki index or a status page has nothing
    // article-shaped to score. Printing its visible text beats telling the agent
    // the page was empty when it plainly is not.
    let whole = tidy(&to_markdown(&document), &title);
    if whole.len() >= MIN_TEXT_CHARS {
        return page(
            PageKind::WholePage,
            title,
            whole,
            "no article found on this page, so this is its whole visible text",
        );
    }
    // Both paths came back under the floor. That is a page rendered
    // client-side, and the caller needs to say so rather than print a heading
    // and call it a result.
    page(PageKind::Empty, title, String::new(), "")
}
