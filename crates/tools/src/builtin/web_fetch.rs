//! `web_fetch`: one URL as the least text that still answers a question.
//!
//! The rule this file is written around, stated once:
//!
//! > `Err` is "there was no body to work with". `Ok` with `is_error` is "there
//! > was a body and it did not answer".
//!
//! They need different advice, and the model can only tell them apart if the
//! tool does. A blocked host, a timeout and a refused redirect are the first;
//! a bot wall, a PDF and a client-rendered shell are the second, and each of
//! those carries the sentence naming what to try instead.

use std::fmt::Write as _;

use darkwire_core::Result;
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::builtin::built;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};
use crate::web::budget::{RESERVE, cut_at_line, width};
use crate::web::extract::PageKind;

/// What comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
enum Format {
    /// The article, as markdown.
    Markdown,
    /// The body as the server sent it.
    Raw,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WebFetchArgs {
    #[schemars(
        length(min = 1),
        description = "The page to read. A bare host is read as https."
    )]
    url: String,
    #[schemars(
        description = "markdown extracts the article and drops navigation, which is what you want for a page. raw returns the body untouched, which is what you want for a JSON or plain-text API. Defaults to markdown."
    )]
    format: Option<Format>,
    #[schemars(
        range(min = 500),
        description = "Cap the text returned. Capped in turn by this agent's result budget, so a larger number has no effect."
    )]
    max_chars: Option<u64>,
}

struct WebFetch;

/// The sentence for a body that arrived and did not answer.
fn unreadable(page: &crate::web::extract::Page) -> Option<String> {
    match page.kind {
        PageKind::Pdf => Some(format!(
            "This is a PDF ({}). No PDF text extraction is built in.\n\
             Look for an HTML version of the same document, or if you have exec and the\n\
             image ships a PDF tool, download it there and read it.",
            crate::builtin::format_bytes(page.text.len() as u64)
        )),
        PageKind::Binary => Some(format!(
            "The body is {}, not text. There is nothing to read here.",
            if page.content_type.is_empty() {
                "not text"
            } else {
                page.content_type.as_str()
            }
        )),
        PageKind::Empty => Some(
            "The page returned markup but no text. It is almost certainly rendered\n\
             client-side, and there is no JavaScript engine here. Look for an API, an\n\
             RSS feed, or a <noscript> fallback."
                .to_owned(),
        ),
        _ => None,
    }
}

impl ToolHandler for WebFetch {
    type Args = WebFetchArgs;

    fn execute<'a>(
        &'a self,
        args: WebFetchArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "web_fetch")?;

            let Some(web) = ctx.web.as_ref() else {
                return Ok(ToolOutput::error(
                    "There is no web access configured on this install.",
                ));
            };
            if web.policy().is_none() {
                return Ok(ToolOutput::error(format!(
                    "This agent has no network access, so it cannot reach {}.\n\
                     An operator grants it on the agent's environment.",
                    args.url
                )));
            }

            let page = web.fetch(&args.url, ctx.token.clone()).await?;
            if let Some(sentence) = unreadable(&page) {
                return Ok(ToolOutput::error(sentence)
                    .with_detail("url", page.url.as_str())
                    .with_detail("kind", format!("{:?}", page.kind).to_lowercase()));
            }

            // The registry truncates to the budget by keeping the head *and*
            // the tail, so a page that overflows comes back with its middle
            // removed. Cutting here, at a line boundary, is what keeps the
            // result readable and recoverable.
            let budget = usize::try_from(ctx.config.max_output_chars)
                .unwrap_or(usize::MAX)
                .saturating_sub(RESERVE);
            let asked = args
                .max_chars
                .map_or(budget, |chars| usize::try_from(chars).unwrap_or(budget));
            let limit = asked.min(budget);

            let raw = args.format == Some(Format::Raw);
            let mut out = String::new();
            if !page.title.is_empty() && !raw {
                let _ = writeln!(out, "# {}", page.title);
            }
            let _ = writeln!(out, "<{}>\n", page.url);
            if !page.note.is_empty() {
                let _ = writeln!(out, "> note: {}\n", page.note);
            }
            let (body, dropped) = cut_at_line(&page.text, limit.saturating_sub(width(&out)));
            out.push_str(&body);
            if dropped > 0 {
                let _ = write!(
                    out,
                    "\n\n[{dropped} characters not shown. web_fetch {} for the rest, \
                     or narrow what you are looking for.]",
                    page.url
                );
            }

            Ok(ToolOutput::text(out)
                .with_detail("url", page.url.as_str())
                .with_detail("kind", format!("{:?}", page.kind).to_lowercase())
                .with_detail("contentType", page.content_type.as_str())
                .with_detail("truncated", dropped > 0))
        })
    }
}

/// The `web_fetch` tool.
pub fn web_fetch_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "web_fetch",
            "Read a web page. Returns the main content as markdown with navigation, \
             adverts and boilerplate removed, so a documentation page costs a fraction \
             of what its markup would. Use format \"raw\" for an API that returns JSON \
             or plain text. There is no JavaScript engine, so a page rendered in the \
             browser comes back empty and says so.",
        )
        .risk(ToolRisk::Network)
        .annotations(ToolAnnotations {
            title: Some("Read a web page".to_owned()),
            read_only_hint: Some(true),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        WebFetch,
    ))
}
