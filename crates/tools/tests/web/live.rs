//! The live checks, which are `#[ignore]`d on purpose.
//!
//! **These are what a maintainer runs when a backend is reported broken.** They
//! reach the real internet, so they cannot be part of the suite: they fail on an
//! aeroplane, they fail when a front door rate-limits this address, and a red
//! test that means "the web changed" teaches nobody anything on a pull request.
//! The fixture tests beside them are the regression net; these are the ones that
//! tell you the net is out of date.
//!
//! ```text
//! cargo test -p darkwire-tools --test web -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::print_stdout, reason = "a manual check")]

use std::sync::Arc;

use darkwire_core::SystemClock;
use darkwire_protocol::config::Config;
use darkwire_security::{HickoryResolver, OsRandom};
use darkwire_tools::web::live::{LiveWebResolver, WebSettings};
use darkwire_tools::web::{SearchQuery, WebResolver};
use tokio_util::sync::CancellationToken;

fn resolver() -> LiveWebResolver {
    let config = Arc::new(Config::default());
    LiveWebResolver::new(
        Arc::new(move || Arc::clone(&config)),
        WebSettings::default(),
        Arc::new(HickoryResolver::new().unwrap()),
        Arc::new(SystemClock),
        Arc::new(OsRandom),
    )
}

#[tokio::test]
#[ignore = "reaches the real internet"]
async fn fetches_a_real_page_as_markdown() {
    let port = resolver().for_agent("default").unwrap();
    let page = port
        .fetch("https://www.sqlite.org/wal.html", CancellationToken::new())
        .await
        .unwrap();
    println!(
        "kind={:?} title={:?} chars={}",
        page.kind,
        page.title,
        page.text.len()
    );
    println!("{}", &page.text[..page.text.len().min(600)]);
    assert!(page.ok());
    assert!(page.text.len() > 1_000, "suspiciously short");
}

#[tokio::test]
#[ignore = "reaches the real internet"]
async fn searches_the_real_web() {
    let port = resolver().for_agent("default").unwrap();
    let outcome = port
        .search(
            &SearchQuery {
                terms: "sqlite wal mode tradeoffs".to_owned(),
                count: 6,
                recent: None,
                region: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    for problem in &outcome.problems {
        println!("problem: {problem}");
    }
    for hit in &outcome.hits {
        println!("{} -> {}", hit.title, hit.url);
    }
    assert!(!outcome.hits.is_empty(), "every backend failed");
}
