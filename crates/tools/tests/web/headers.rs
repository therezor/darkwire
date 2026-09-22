//! The request as a bot wall sees it.

use darkwire_tools::web::{CHROME_MAJOR, backend_headers, browser_headers, chrome_user_agent};

fn value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn the_default_request_looks_like_the_browser_it_claims_to_be() {
    let headers = browser_headers("");
    let agent = value(&headers, "user-agent");
    assert!(agent.contains("Chrome/"), "{agent}");
    assert_eq!(agent, chrome_user_agent());

    for name in [
        "accept",
        "accept-language",
        "sec-ch-ua",
        "sec-ch-ua-mobile",
        "sec-ch-ua-platform",
        "sec-fetch-dest",
        "sec-fetch-mode",
        "sec-fetch-site",
        "sec-fetch-user",
        "upgrade-insecure-requests",
    ] {
        assert!(!value(&headers, name).is_empty(), "{name}");
    }
}

/// A hint set naming a different Chrome than the user agent is a contradiction
/// a fingerprinter reads in one line, so the two share one constant.
#[test]
fn the_client_hints_agree_with_the_user_agent() {
    let headers = browser_headers("");
    let major = CHROME_MAJOR.to_string();
    assert!(value(&headers, "user-agent").contains(&format!("Chrome/{major}.")));
    let hints = value(&headers, "sec-ch-ua");
    assert!(
        hints.contains(&format!("\"Google Chrome\";v=\"{major}\"")),
        "{hints}"
    );
    assert!(
        hints.contains(&format!("\"Chromium\";v=\"{major}\"")),
        "{hints}"
    );
}

/// reqwest adds it from the compression features and decodes the response.
/// Setting it by hand turns that off and hands the extractor bytes it cannot
/// read.
#[test]
fn accept_encoding_is_left_to_the_client() {
    assert!(browser_headers("").get("accept-encoding").is_none());
    assert!(
        backend_headers("", "https://x.example/")
            .get("accept-encoding")
            .is_none()
    );
}

/// An operator who chose to be honest should not be given away by a header they
/// did not write.
#[test]
fn a_configured_agent_is_sent_alone_and_verbatim() {
    let headers = browser_headers("  DarkWire/0.5 (+https://example.test)  ");
    assert_eq!(
        value(&headers, "user-agent"),
        "DarkWire/0.5 (+https://example.test)"
    );
    for hint in [
        "sec-ch-ua",
        "sec-ch-ua-mobile",
        "sec-ch-ua-platform",
        "sec-fetch-dest",
        "sec-fetch-site",
    ] {
        assert!(headers.get(hint).is_none(), "{hint}");
    }
    // The two a plain client still sends survive, because they are content
    // negotiation rather than a fingerprint.
    assert!(!value(&headers, "accept").is_empty());
    assert!(!value(&headers, "accept-language").is_empty());
}

/// A result page is reached from the engine's own front page, and several front
/// doors serve a consent wall to a request that arrives from nowhere.
#[test]
fn a_backend_request_carries_a_referer_and_says_it_is_same_origin() {
    let headers = backend_headers("", "https://www.mojeek.com/");
    assert_eq!(value(&headers, "referer"), "https://www.mojeek.com/");
    assert_eq!(value(&headers, "sec-fetch-site"), "same-origin");
}

#[test]
fn a_configured_agent_suppresses_the_backend_extras_too() {
    let headers = backend_headers("DarkWire/0.5", "https://www.mojeek.com/");
    assert!(headers.get("referer").is_none());
    assert!(headers.get("sec-fetch-site").is_none());
}
