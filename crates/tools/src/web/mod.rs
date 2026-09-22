//! Reading the web, for the `web_fetch` and `web_search` tools.
//!
//! Everything here sits on `darkwire_security::guarded_fetch`, which resolves a
//! host itself, pins the answers into a client that never consults DNS again,
//! revalidates every redirect hop and caps the body as it streams. Nothing in
//! this module makes a request any other way, and nothing in it decides what may
//! be reached: that is the `NetworkPolicy` the turn arrives with.
//!
//! `extract` and `budget` are deliberately free of `ToolContext` and of
//! tool-shaped prose, so they stay testable against fixtures alone.

pub mod budget;
pub mod cache;
pub mod extract;
pub mod headers;
pub mod live;
pub mod port;
pub mod search;

pub use budget::{MIN_EXTRACT, RESERVE, cut_at_line, share, width};
pub use cache::WebCache;
pub use extract::{Page, PageKind, decode, extract};
pub use headers::{CHROME_MAJOR, backend_headers, browser_headers, chrome_user_agent};
pub use live::{LiveWeb, LiveWebResolver, WebSettings};
pub use port::{Recency, SearchHit, SearchOutcome, SearchQuery, WebPort, WebResolver};
