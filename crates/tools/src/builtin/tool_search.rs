//! `tool_search`: the door to the tools lazy discovery hides.
//!
//! With `tools.lazy_discovery` on, the model is sent this tool and the pinned
//! ones, and nothing else. Everything else it may call is reachable by name
//! through here: `query` finds tools, `activate` adds them to the list for the
//! rest of the session.
//!
//! ## What a result carries, and what it deliberately does not
//!
//! A search answers with names and one line each, never a schema. An
//! activation answers with the names and nothing else: the schema is in the
//! tools array of the very next request, and a copy in the transcript would be
//! paid for on every request after it. The whole point of the tool is to spend
//! fewer tokens on definitions, so nothing here writes one into history.
//!
//! ## Matching
//!
//! Plain substring scoring over the name, the description, the annotation
//! title and the parameter names and descriptions. An exact name wins, a name
//! containing the whole query comes next, and after that every query word found
//! anywhere in the text scores the same. Words are split on spaces and on the
//! separators tool names use, so a guessed name finds its neighbours by parts. No stemming, no fuzziness: a model
//! that misses refines the query, and a clever matcher that guessed wrong would
//! hand it a tool it did not ask for.
//!
//! The parameter prose is in the haystack because MCP servers put most of their
//! vocabulary there: a tool named `create` under a server prefix says what it
//! creates in its arguments, not in its name.

use darkwire_core::Result;
use darkwire_protocol::json::js_trim;
use darkwire_protocol::{ToolAnnotations, ToolDefinition, ToolRisk};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::builtin::built;
use crate::discovery::Activation;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

/// The tool's name, spelled once.
pub const TOOL_SEARCH_NAME: &str = "tool_search";

/// How many hits a search shows. The rest is a count the model can act on by
/// refining, which costs less than listing them.
pub const MAX_SEARCH_RESULTS: usize = 10;

/// Longest description line a hit carries. A hit is a pointer, not the tool.
const MAX_HIT_DESCRIPTION_CHARS: usize = 160;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ToolSearchArgs {
    #[schemars(
        description = "Words to look for in tool names and descriptions. Returns matching tool names with one line each."
    )]
    query: Option<String>,
    #[schemars(
        description = "Exact tool names to add to your tool list, taken from search results. They are callable from your next step on and stay for the rest of the session."
    )]
    activate: Option<Vec<String>>,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The tool's name, exactly as it is called.
    pub name: String,
    /// The first line of its description, shortened.
    pub description: String,
    /// Higher is a better match. Ties break by name.
    pub score: u32,
}

/// What a search found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    /// At most [`MAX_SEARCH_RESULTS`], best first.
    pub hits: Vec<Hit>,
    /// How many matches were cut from the end.
    pub more: usize,
}

/// The text of a definition a query is matched against, lower-cased.
fn haystack(tool: &ToolDefinition) -> String {
    let mut text = String::new();
    text.push_str(&tool.description);
    if let Some(title) = tool.annotations.as_ref().and_then(|a| a.title.as_deref()) {
        text.push(' ');
        text.push_str(title);
    }
    if let Some(properties) = tool.parameters.get("properties").and_then(Value::as_object) {
        for (name, schema) in properties {
            text.push(' ');
            text.push_str(name);
            if let Some(description) = schema.get("description").and_then(Value::as_str) {
                text.push(' ');
                text.push_str(description);
            }
        }
    }
    text.to_lowercase()
}

fn score(tool: &ToolDefinition, query: &str, words: &[&str]) -> u32 {
    let name = tool.name.to_lowercase();
    if name == query {
        return 100;
    }
    if name.contains(query) {
        return 50;
    }
    let text = haystack(tool);
    words
        .iter()
        .filter(|word| name.contains(**word) || text.contains(**word))
        .count()
        .try_into()
        .map_or(u32::MAX, |found: u32| found.saturating_mul(10))
}

/// The first line of a description, cut to [`MAX_HIT_DESCRIPTION_CHARS`].
fn one_line(description: &str) -> String {
    let line = description.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= MAX_HIT_DESCRIPTION_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(MAX_HIT_DESCRIPTION_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// The tools in `corpus` that match `query`, best first.
///
/// A blank query matches nothing. Scoring is described in the module header.
pub fn search(corpus: &[ToolDefinition], query: &str) -> SearchResults {
    let query = js_trim(query).to_lowercase();
    if query.is_empty() {
        return SearchResults::default();
    }
    // Split on the separators tool names use as well as on spaces, so a
    // guessed name such as `github_issue` still finds `mcp_github_create_issue`
    // by its parts.
    let words: Vec<&str> = query
        .split(|c: char| c.is_whitespace() || matches!(c, '_' | '-' | '.' | '/' | ':'))
        .filter(|word| !word.is_empty())
        .collect();
    let mut hits: Vec<Hit> = corpus
        .iter()
        .filter_map(|tool| {
            let score = score(tool, &query, &words);
            (score > 0).then(|| Hit {
                name: tool.name.clone(),
                description: one_line(&tool.description),
                score,
            })
        })
        .collect();
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
    let more = hits.len().saturating_sub(MAX_SEARCH_RESULTS);
    hits.truncate(MAX_SEARCH_RESULTS);
    SearchResults { hits, more }
}

/// The text a search answers with.
///
/// Names that are already in the model's list say so, so a model that searched
/// for one is told it can call it now rather than being sent round again.
pub fn render_search(query: &str, results: &SearchResults, visible: &[String]) -> String {
    if results.hits.is_empty() {
        return format!(
            "No tools match \"{}\". Try other words: a tool is found by its name, what it does, or the names of its arguments.",
            js_trim(query)
        );
    }
    let mut text = if results.more > 0 {
        format!(
            "Tools matching \"{}\" ({} shown, {} more; refine the query to see the rest):\n",
            js_trim(query),
            results.hits.len(),
            results.more
        )
    } else {
        format!("Tools matching \"{}\":\n", js_trim(query))
    };
    for hit in &results.hits {
        text.push_str("- ");
        text.push_str(&hit.name);
        if visible.contains(&hit.name) {
            text.push_str(" (already in your tool list)");
        }
        if !hit.description.is_empty() {
            text.push_str(": ");
            text.push_str(&hit.description);
        }
        text.push('\n');
    }
    text.push_str(
        "To use one that is not in your list yet, call tool_search with activate set to its exact name. Several names at once is fine.",
    );
    text
}

/// The text an activation answers with. No schema: it is in the tool list.
///
/// A call that activated nothing and knew none of the names is an error, so a
/// model that guessed sees the failure flag as well as the suggestions.
pub fn render_activation(outcome: &Activation, corpus: &[ToolDefinition]) -> ToolOutput {
    let mut lines: Vec<String> = Vec::new();
    if !outcome.activated.is_empty() {
        lines.push(format!(
            "Activated: {}. They are in your tool list from your next step on; call them directly.",
            outcome.activated.join(", ")
        ));
    }
    if !outcome.already_visible.is_empty() {
        lines.push(format!(
            "Already in your tool list: {}.",
            outcome.already_visible.join(", ")
        ));
    }
    for name in &outcome.unknown {
        let suggestions: Vec<String> = search(corpus, name)
            .hits
            .into_iter()
            .take(3)
            .map(|hit| hit.name)
            .collect();
        if suggestions.is_empty() {
            lines.push(format!(
                "Unknown tool \"{name}\". Use the exact name from a search result."
            ));
        } else {
            lines.push(format!(
                "Unknown tool \"{name}\". Did you mean: {}?",
                suggestions.join(", ")
            ));
        }
    }
    let failed = outcome.activated.is_empty() && outcome.already_visible.is_empty();
    let text = lines.join("\n");
    if failed {
        ToolOutput::error(text)
    } else {
        ToolOutput::text(text)
    }
}

/// The names in `activate` worth acting on: trimmed, non-empty, first
/// occurrence only.
fn requested(names: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for name in names {
        let name = js_trim(name);
        if !name.is_empty() && !seen.iter().any(|s| s == name) {
            seen.push(name.to_owned());
        }
    }
    seen
}

struct ToolSearch;

impl ToolHandler for ToolSearch {
    type Args = ToolSearchArgs;

    fn execute<'a>(
        &'a self,
        args: ToolSearchArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, TOOL_SEARCH_NAME)?;

            let Some(port) = &ctx.discovery else {
                return Ok(ToolOutput::error(
                    "Refused: lazy tool discovery is off for this install, so every tool you may use is already in your list.",
                ));
            };

            let names = args.activate.as_deref().map(requested).unwrap_or_default();
            if !names.is_empty() {
                let outcome = port.activate(&names);
                let corpus = if outcome.unknown.is_empty() {
                    Vec::new()
                } else {
                    port.corpus()
                };
                return Ok(render_activation(&outcome, &corpus)
                    .with_detail("activated", outcome.activated.clone()));
            }

            let query = args.query.as_deref().map(js_trim).unwrap_or_default();
            if query.is_empty() {
                return Ok(ToolOutput::error(
                    "Provide query to search for tools, or activate with the exact names to add.",
                ));
            }
            let results = search(&port.corpus(), query);
            Ok(
                ToolOutput::text(render_search(query, &results, &port.visible()))
                    .with_detail("matched", results.hits.len() + results.more),
            )
        })
    }
}

/// The `tool_search` tool.
pub fn tool_search_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            TOOL_SEARCH_NAME,
            "Find and activate tools that are not in your tool list. Use query to search by words; use activate with exact names from the results to add them to your list for the rest of the session. Activate several at once when you know what you need.",
        )
        .risk(ToolRisk::Safe)
        .annotations(ToolAnnotations {
            title: Some("Find tools".to_owned()),
            read_only_hint: Some(true),
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        ToolSearch,
    ))
}
