//! `web_search`: find pages, and read them.
//!
//! **Reading is the default, and that is the decision this file turns on.**
//! With reading opt-in, a model handed six one-line snippets answers from the
//! snippets: asked which small model is best at tool calling, it summarised six
//! link *titles* and offered to dig deeper. The instruction to ask for the pages
//! was in the tool description and was simply not taken. A research tool whose
//! useful mode has to be requested has its default the wrong way round.
//!
//! Three pages, because one source is an opinion and two that agree is a
//! coincidence.
//!
//! Two other things are load-bearing. The budget is computed rather than fixed,
//! because the registry truncates by keeping the head *and* the tail, so a
//! result that overflows comes back with its middle removed and reads as a
//! document with a hole in it. And every result that could not be read is
//! **named**: a model that sees two extracts where it expected three needs to
//! know the third was unreadable rather than judged irrelevant, or it fetches it
//! again one at a time.

use std::fmt::Write as _;
use std::sync::Arc;

use darkwire_core::Result;
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use futures::stream::{self, StreamExt};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::builtin::built;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};
use crate::web::budget::{cut_at_line, share, width};
use crate::web::extract::PageKind;
use crate::web::port::{Recency, SearchQuery, WebPort};

/// How many extra results may be tried to reach the reads that were asked for.
///
/// Results include pages there is no point fetching: a video page has no text
/// without JavaScript, a news site bot-walls, a PDF is out of scope. "Read the
/// top three" meaning "attempt the first three" spends the budget on those and
/// returns nothing. Bounded, so a page of dead links cannot turn one search into
/// nine fetches.
const SLACK: usize = 3;

/// How many reads run at once. Polite to an origin, and a bound on how many
/// blocking extractors are live.
const CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
enum Window {
    Day,
    Week,
    Month,
    Year,
}

impl From<Window> for Recency {
    fn from(window: Window) -> Recency {
        match window {
            Window::Day => Recency::Day,
            Window::Week => Recency::Week,
            Window::Month => Recency::Month,
            Window::Year => Recency::Year,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WebSearchArgs {
    #[schemars(
        length(min = 1),
        description = "The search terms, as plain words. Do not quote the phrase."
    )]
    query: String,
    #[schemars(
        range(min = 1, max = 20),
        description = "How many results to list. Defaults to 6."
    )]
    count: Option<u64>,
    #[schemars(
        range(min = 0, max = 5),
        description = "How many of the result pages to read and include. Defaults to 3. Use 0 for the list of links alone, which is rarely what you want: titles do not answer questions."
    )]
    read: Option<u64>,
    #[schemars(
        description = "Only results from the last day, week, month or year. Best effort: some backends ignore it."
    )]
    recent: Option<Window>,
    #[schemars(description = "Restrict to one domain, for example docs.python.org.")]
    site: Option<String>,
    #[schemars(description = "Bias to a region, for example uk-en. Best effort.")]
    region: Option<String>,
}

struct WebSearch;

/// The refusal when an allow-listed agent cannot reach any backend.
fn no_backend(web: &dyn WebPort) -> String {
    let hosts = web.backend_hosts();
    if hosts.is_empty() {
        return "This agent's search provider is not configured.".to_owned();
    }
    format!(
        "This agent's network allow-list does not include a search backend.\n\
         An operator adds one of: {}.",
        hosts.join(", ")
    )
}

impl ToolHandler for WebSearch {
    type Args = WebSearchArgs;

    fn execute<'a>(
        &'a self,
        args: WebSearchArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "web_search")?;

            let Some(web) = ctx.web.as_ref() else {
                return Ok(ToolOutput::error(
                    "There is no web access configured on this install.",
                ));
            };
            if web.policy().is_none() {
                return Ok(ToolOutput::error(
                    "This agent has no network access, so it cannot search.\n\
                     An operator grants it on the agent's environment.",
                ));
            }

            // Stripped, because the instruction not to quote is not always
            // followed and a phrase search for "sqlite wal" matches almost
            // nothing. Cheaper to accept the input than to be right about it.
            let mut terms = args
                .query
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .trim()
                .to_owned();
            if let Some(site) = args
                .site
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                terms = format!("site:{site} {terms}");
            }
            if terms.is_empty() {
                return Ok(ToolOutput::error("The query is empty."));
            }

            let count = usize::try_from(args.count.unwrap_or(6)).unwrap_or(6);
            let asked_reads = usize::try_from(args.read.unwrap_or(3)).unwrap_or(3);
            let query = SearchQuery {
                terms,
                count: count.max(asked_reads),
                recent: args.recent.map(Recency::from),
                region: args
                    .region
                    .clone()
                    .filter(|region| !region.trim().is_empty()),
            };

            let outcome = web.search(&query, ctx.token.clone()).await?;
            if outcome.hits.is_empty() {
                let mut message = if outcome.problems.is_empty() {
                    "Nothing found.".to_owned()
                } else {
                    format!("Nothing found. {}.", outcome.problems.join("; "))
                };
                // The advice differs by reason, which is why the reasons are
                // kept rather than counted.
                message.push_str(
                    "\nIf the reason above is a block or a rate limit, the query was fine.\n\
                     Do not re-ask it: fetch a likely URL with web_fetch, or answer without\n\
                     searching and say you could not.",
                );
                if web.backend_hosts().iter().any(|host| {
                    outcome
                        .problems
                        .iter()
                        .any(|problem| problem.contains(host.as_str()))
                }) {
                    message.push('\n');
                    message.push_str(&no_backend(web.as_ref()));
                }
                return Ok(ToolOutput::error(message));
            }

            let listing = render_listing(&outcome.hits);
            let budget = usize::try_from(ctx.config.max_output_chars).unwrap_or(usize::MAX);
            let (each, reads) = share(budget, width(&listing), asked_reads);

            let mut out = listing;
            if reads == 0 {
                if asked_reads > 0 {
                    out.push_str(
                        "\n[no budget left to read any of these. Ask for fewer results, \
                         or web_fetch one of the URLs above.]\n",
                    );
                }
                return Ok(finish(out, &outcome, 0));
            }
            if reads < asked_reads {
                let _ = writeln!(out, "\n[budget for {reads} extracts, not {asked_reads}.]");
            }

            let printed = read_into(&mut out, web, &outcome.hits, reads, each, ctx).await?;
            if printed == 0 && asked_reads > 0 {
                out.push_str(
                    "\n[none of the result pages could be read. Answer from the snippets \
                     above, or search again with different words.]\n",
                );
            }

            Ok(finish(out, &outcome, printed))
        })
    }
}

/// The numbered list, which is a real answer even when nothing can be read.
fn render_listing(hits: &[crate::web::port::SearchHit]) -> String {
    let mut listing = String::new();
    for (index, hit) in hits.iter().enumerate() {
        let tag = if hit.source.is_empty() {
            String::new()
        } else {
            format!("  [{}]", hit.source)
        };
        let _ = writeln!(listing, "{}. {}{tag}\n   {}", index + 1, hit.title, hit.url);
        if !hit.snippet.is_empty() {
            let _ = writeln!(listing, "   {}", hit.snippet);
        }
    }
    listing
}

/// Reads until `reads` pages have produced text, and names every one that did
/// not. Returns how many were printed.
///
/// Fanned out because every one is waiting on a round trip, and all of them race
/// the turn's one token. `SLACK` extra are attempted because "read the top
/// three" means three that answered, not three attempts.
async fn read_into(
    out: &mut String,
    web: &Arc<dyn WebPort>,
    hits: &[crate::web::port::SearchHit],
    reads: usize,
    each: usize,
    ctx: &ToolContext,
) -> Result<usize> {
    let targets: Vec<String> = hits
        .iter()
        .take(reads + SLACK)
        .map(|hit| hit.url.clone())
        .collect();
    let pages = stream::iter(targets.into_iter().map(|url| {
        let port = Arc::clone(web);
        let token = ctx.token.clone();
        async move {
            let page = port.fetch(&url, token).await;
            (url, page)
        }
    }))
    .buffered(CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let mut printed = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    for (url, page) in pages {
        assert_not_aborted(&ctx.token, "web_search")?;
        if printed >= reads {
            break;
        }
        match page {
            Ok(page) if page.ok() && !page.text.trim().is_empty() => {
                printed += 1;
                let _ = writeln!(out, "\n===== [{printed}] {} =====", page.url);
                if !page.title.is_empty() {
                    let _ = writeln!(out, "# {}\n", page.title);
                }
                if !page.note.is_empty() {
                    let _ = writeln!(out, "> note: {}\n", page.note);
                }
                let (body, dropped) = cut_at_line(&page.text, each);
                out.push_str(&body);
                if dropped > 0 {
                    let _ = writeln!(
                        out,
                        "\n[{dropped} characters not shown. web_fetch {} for the rest.]",
                        page.url
                    );
                }
                out.push('\n');
            }
            Ok(page) => skipped.push(format!("{url} ({})", why(page.kind))),
            Err(error) => skipped.push(format!("{url} ({})", short(&error.message))),
        }
    }

    // Named rather than dropped: a model that sees two extracts where it
    // expected three needs to know the third was unreadable, not judged
    // irrelevant, or it fetches it again one at a time.
    if !skipped.is_empty() {
        let _ = writeln!(
            out,
            "\n[not readable, skipped: {}]",
            skipped
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(printed)
}

/// Why a page was not printed, in the words the model needs.
fn why(kind: PageKind) -> &'static str {
    match kind {
        PageKind::Pdf => "PDF",
        PageKind::Binary => "not text",
        PageKind::Empty => "rendered client-side",
        _ => "empty",
    }
}

fn short(message: &str) -> String {
    message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}

/// The list is a real answer even when nothing could be read, so a search that
/// found results never reports as a failed call.
fn finish(text: String, outcome: &crate::web::port::SearchOutcome, printed: usize) -> ToolOutput {
    ToolOutput::text(text)
        .with_detail("results", outcome.hits.len() as u64)
        .with_detail("read", printed as u64)
        .with_detail(
            "problems",
            serde_json::Value::Array(
                outcome
                    .problems
                    .iter()
                    .map(|problem| serde_json::Value::from(problem.as_str()))
                    .collect(),
            ),
        )
}

/// The `web_search` tool.
pub fn web_search_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "web_search",
            "Search the web and read the top results. Returns a numbered list of \
             results and, by default, the text of the first three pages, so one call \
             usually answers the question. Titles and snippets alone rarely do, which \
             is why reading is the default rather than something to ask for.",
        )
        .risk(ToolRisk::Network)
        .annotations(ToolAnnotations {
            title: Some("Search the web".to_owned()),
            read_only_hint: Some(true),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        WebSearch,
    ))
}
