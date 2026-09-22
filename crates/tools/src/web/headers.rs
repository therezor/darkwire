//! What a request looks like on the wire, and why it looks like a browser.
//!
//! A great many sites serve a challenge page to anything that looks automated,
//! and a fetch that returns "please enable JavaScript" has spent a turn to learn
//! nothing. So the default request carries a current Chrome user agent and the
//! header set that goes with it.
//!
//! **The client hints have to agree with the user agent.** A `sec-ch-ua` naming
//! a different Chrome major than the UA string is a contradiction a fingerprinter
//! reads in one line, and is a stronger signal than sending no hints at all.
//! [`CHROME_MAJOR`] is therefore one constant used by both, bumped in its own
//! commit like a pinned dependency.
//!
//! **`accept-encoding` is deliberately absent.** reqwest adds it from the
//! `gzip`, `deflate` and `brotli` features and decodes the response; setting it
//! by hand turns that decoding off and hands the extractor bytes it cannot read.
//!
//! **What this does not do is TLS.** rustls presents a `ClientHello` that is not
//! Chrome's, and no amount of header work changes that, so a site behind a
//! fingerprinting CDN will refuse these requests however they are dressed. That
//! is a deliberate limit: one HTTP stack is worth more than the sites it loses,
//! and the refusal says so rather than inviting a retry.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// The Chrome major both the user agent and the client hints claim.
pub const CHROME_MAJOR: u32 = 141;

/// The user agent sent when an operator has configured none.
pub fn chrome_user_agent() -> String {
    format!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/{CHROME_MAJOR}.0.0.0 Safari/537.36"
    )
}

fn insert(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(HeaderName::from_static(name), value);
    }
}

/// The headers one page request carries.
///
/// `configured` is the operator's own user agent. When it is set it is sent
/// verbatim and **without** client hints, because a hint set naming Chrome
/// beside a custom agent is exactly the contradiction the default set exists to
/// avoid, and an operator who chose to be honest should not be given away by a
/// header they did not write.
pub fn browser_headers(configured: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let honest = !configured.trim().is_empty();

    let default_agent = chrome_user_agent();
    insert(
        &mut headers,
        "user-agent",
        if honest {
            configured.trim()
        } else {
            &default_agent
        },
    );
    insert(
        &mut headers,
        "accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,\
         image/avif,image/webp,image/apng,*/*;q=0.8",
    );
    insert(&mut headers, "accept-language", "en-US,en;q=0.9");

    if honest {
        return headers;
    }

    insert(
        &mut headers,
        "sec-ch-ua",
        &format!(
            "\"Chromium\";v=\"{CHROME_MAJOR}\", \"Google Chrome\";v=\"{CHROME_MAJOR}\", \
             \"Not?A_Brand\";v=\"99\""
        ),
    );
    insert(&mut headers, "sec-ch-ua-mobile", "?0");
    insert(&mut headers, "sec-ch-ua-platform", "\"macOS\"");
    insert(&mut headers, "upgrade-insecure-requests", "1");
    insert(&mut headers, "sec-fetch-dest", "document");
    insert(&mut headers, "sec-fetch-mode", "navigate");
    // A fetch is always a typed URL rather than a click, so `none` is the honest
    // constant. A real browser rewrites this to `cross-site` after a redirect
    // changes origin; correcting that needs a per-hop hook in the guard, and is
    // not worth one.
    insert(&mut headers, "sec-fetch-site", "none");
    insert(&mut headers, "sec-fetch-user", "?1");
    headers
}

/// The headers a search backend request carries.
///
/// A result page is fetched the way a browser fetches one it navigated to from
/// the engine's own front page, so `sec-fetch-site` says `same-origin` and a
/// referer is sent. Without them several front doors serve a consent wall.
pub fn backend_headers(configured: &str, referer: &str) -> HeaderMap {
    let mut headers = browser_headers(configured);
    if configured.trim().is_empty() {
        insert(&mut headers, "sec-fetch-site", "same-origin");
        insert(&mut headers, "referer", referer);
    }
    headers
}
