//! The result parsers, against pages the real front doors actually served.
//!
//! **A green test here says the parser handles the HTML that backend served on
//! the day the fixture was captured, and nothing about what it serves today.**
//! That is the honest claim, and it is why the rotation skips a backend that
//! returns nothing rather than treating it as an error: these will drift.
//!
//! One of the three captures is a captcha, because that is what Mojeek served
//! from this address on the first attempt. It is the most valuable fixture in
//! the directory: a parser that panics on it takes the turn down, and one that
//! returns nothing falls through to the next backend, which is the whole point.

use darkwire_tools::web::search::{brave, duckduckgo, mojeek, tidy_hits};

const DDG: &str = include_str!("../fixtures/serp/duckduckgo.html");
const BRAVE: &str = include_str!("../fixtures/serp/brave.html");
const CAPTCHA: &str = include_str!("../fixtures/serp/mojeek-captcha.html");

#[test]
fn duckduckgo_results_parse() {
    let hits = duckduckgo::parse(DDG, "https://html.duckduckgo.com/html/");
    assert!(hits.len() >= 3, "{} results", hits.len());
    assert!(hits.iter().all(|hit| !hit.title.is_empty()));
    assert!(hits.iter().all(|hit| hit.url.starts_with("https://")));
}

/// Every result is wrapped in a counter, written protocol-relative. Left
/// wrapped, the model fetches the engine instead of the page, and an allow-list
/// sees the engine's host rather than the destination it permitted.
#[test]
fn duckduckgo_redirects_are_unwrapped_to_the_destination() {
    let hits = duckduckgo::parse(DDG, "https://html.duckduckgo.com/html/");
    assert!(
        hits.iter()
            .all(|hit| !hit.url.contains("duckduckgo.com/l/")),
        "{:?}",
        hits.iter().map(|hit| &hit.url).collect::<Vec<_>>()
    );
    assert!(
        hits.iter().any(|hit| hit.url.contains("sqlite.org")),
        "{:?}",
        hits.iter().map(|hit| &hit.url).collect::<Vec<_>>()
    );
}

#[test]
fn brave_results_parse() {
    let hits = brave::parse(BRAVE, "https://search.brave.com/search");
    assert!(!hits.is_empty(), "no results");
    assert!(hits.iter().all(|hit| !hit.title.is_empty()), "{hits:?}");
    assert!(hits.iter().all(|hit| hit.url.starts_with("https://")));
    assert!(hits.iter().any(|hit| hit.url.contains("sqlite.org")));
}

/// The fixture that matters most. A captcha is the common failure, and the
/// parser's contract is to come back empty so the rotation moves on.
#[test]
fn a_captcha_page_yields_nothing_rather_than_panicking() {
    assert!(mojeek::parse(CAPTCHA, "https://www.mojeek.com/search").is_empty());
    assert!(duckduckgo::parse(CAPTCHA, "https://html.duckduckgo.com/html/").is_empty());
    assert!(brave::parse(CAPTCHA, "https://search.brave.com/search").is_empty());
}

/// A parser that panics takes the turn down with it, so every shape of rubbish
/// has to come back as an empty list.
#[test]
fn no_parser_panics_on_anything_it_does_not_recognise() {
    for body in [
        "",
        "<!doctype html><html><body></body></html>",
        "<div class=\"result\">",
        "not html at all",
        "<html><body><div class=\"result\"><a class=\"result__a\"></a></div></body></html>",
    ] {
        assert!(
            mojeek::parse(body, "https://www.mojeek.com/").is_empty(),
            "{body}"
        );
        assert!(
            duckduckgo::parse(body, "https://html.duckduckgo.com/").is_empty(),
            "{body}"
        );
        assert!(
            brave::parse(body, "https://search.brave.com/").is_empty(),
            "{body}"
        );
    }
}

/// Two front doors agreeing is common, and a model handed the same page twice
/// reads it as two sources.
#[test]
fn duplicates_and_adverts_are_dropped_and_the_list_is_cut() {
    let mut hits = duckduckgo::parse(DDG, "https://html.duckduckgo.com/html/");
    let first = hits[0].clone();
    hits.push(first.clone());
    hits.push(darkwire_tools::web::SearchHit {
        title: "An advert".to_owned(),
        url: "https://www.bing.com/aclick?ld=abc".to_owned(),
        snippet: String::new(),
        source: String::new(),
    });
    let tidied = tidy_hits(hits, 3);
    assert_eq!(tidied.len(), 3);
    assert_eq!(tidied.iter().filter(|hit| hit.url == first.url).count(), 1);
    assert!(!tidied.iter().any(|hit| hit.url.contains("aclick")));
}
