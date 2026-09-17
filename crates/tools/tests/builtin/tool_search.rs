//! The `tool_search` tool, over a stub port, and the pure search beneath it.
//!
//! The port decides what a session may see; that is tested where it is
//! enforced, in the agent crate. What is asserted here is the matching, the
//! wording a model reads back, and that no schema ever reaches the transcript.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_core::ErrorKind;
use darkwire_protocol::{ToolAnnotations, ToolDefinition, ToolRisk, ToolSource};
use darkwire_tools::testkit::{TestWorkspace, ToolConformance, tool_conformance};
use darkwire_tools::{
    Activation, MAX_SEARCH_RESULTS, TOOL_SEARCH_NAME, ToolContext, ToolDiscovery, ToolExecution,
    render_activation, render_search, search, tool_search_tool,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

fn parameters(value: Value) -> darkwire_protocol::json::Object {
    serde_json::from_value(value).unwrap()
}

fn tool(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: parameters(
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        ),
        risk: ToolRisk::Safe,
        source: ToolSource::Builtin,
        annotations: None,
    }
}

fn corpus() -> Vec<ToolDefinition> {
    let mut issue = tool(
        "mcp_github_create_issue",
        "Create an issue in a repository.\nThe body is Markdown.",
    );
    issue.parameters = parameters(json!({
        "type": "object",
        "properties": {
            "repository": {"type": "string", "description": "owner/name of the repository"},
            "labels": {"type": "array", "description": "Labels to attach to the new bug report"}
        },
        "additionalProperties": false
    }));
    let mut clock = tool("clock", "The current time.");
    clock.annotations = Some(ToolAnnotations {
        title: Some("Wall clock".to_owned()),
        ..ToolAnnotations::default()
    });
    vec![
        tool("read_file", "Read a file from the workspace."),
        tool("write_file", "Write a file into the workspace."),
        issue,
        clock,
        tool("exec", "Run a command."),
    ]
}

fn names(results: &darkwire_tools::SearchResults) -> Vec<&str> {
    results.hits.iter().map(|hit| hit.name.as_str()).collect()
}

#[test]
fn an_exact_name_outranks_a_name_that_contains_the_query_which_outranks_prose() {
    let results = search(&corpus(), "read_file");
    assert_eq!(results.hits[0].name, "read_file");
    assert_eq!(results.hits[0].score, 100);

    let results = search(&corpus(), "file");
    assert_eq!(names(&results), vec!["read_file", "write_file"]);
    assert!(results.hits.iter().all(|hit| hit.score == 50));

    let results = search(&corpus(), "workspace");
    assert_eq!(names(&results), vec!["read_file", "write_file"]);
    assert!(results.hits.iter().all(|hit| hit.score == 10));
}

#[test]
fn every_query_word_found_scores_and_ties_break_by_name() {
    let results = search(&corpus(), "workspace read");
    assert_eq!(names(&results), vec!["read_file", "write_file"]);
    assert_eq!(results.hits[0].score, 20);
    assert_eq!(results.hits[1].score, 10);
}

#[test]
fn parameter_names_and_descriptions_and_the_title_are_searched() {
    assert_eq!(
        names(&search(&corpus(), "bug report")),
        vec!["mcp_github_create_issue"]
    );
    assert_eq!(
        names(&search(&corpus(), "repository")),
        vec!["mcp_github_create_issue"]
    );
    assert_eq!(names(&search(&corpus(), "wall")), vec!["clock"]);
}

#[test]
fn matching_ignores_case_and_a_blank_query_matches_nothing() {
    assert_eq!(
        names(&search(&corpus(), "  GITHUB ")),
        vec!["mcp_github_create_issue"]
    );
    assert!(search(&corpus(), "   ").hits.is_empty());
    assert!(search(&corpus(), "nothing-like-this").hits.is_empty());
}

#[test]
fn results_are_capped_and_the_rest_is_a_count() {
    let many: Vec<ToolDefinition> = (0..25)
        .map(|index| tool(&format!("widget_{index:02}"), "A widget."))
        .collect();
    let results = search(&many, "widget");
    assert_eq!(results.hits.len(), MAX_SEARCH_RESULTS);
    assert_eq!(results.more, 15);
    assert_eq!(results.hits[0].name, "widget_00");

    let text = render_search("widget", &results, &[]);
    assert!(text.contains("10 shown, 15 more"));
}

#[test]
fn a_hit_carries_one_shortened_line_and_never_a_schema() {
    let results = search(&corpus(), "issue");
    let hit = &results.hits[0];
    assert_eq!(hit.description, "Create an issue in a repository.");
    let text = render_search("issue", &results, &[]);
    assert!(!text.contains("properties"));
    assert!(!text.contains("owner/name"));

    let long = tool("verbose", &"word ".repeat(100));
    let results = search(&[long], "verbose");
    assert!(results.hits[0].description.chars().count() <= 160);
    assert!(results.hits[0].description.ends_with('…'));
}

#[test]
fn a_search_marks_the_tools_already_in_the_list_and_says_how_to_activate() {
    let results = search(&corpus(), "file");
    let text = render_search("file", &results, &["read_file".to_owned()]);
    assert!(text.contains("- read_file (already in your tool list): Read a file"));
    assert!(text.contains("- write_file: Write a file"));
    assert!(text.contains("activate"));

    let text = render_search("zzz", &search(&corpus(), "zzz"), &[]);
    assert!(text.starts_with("No tools match \"zzz\""));
}

#[test]
fn an_activation_names_what_changed_and_carries_no_schema() {
    let outcome = Activation {
        activated: vec!["mcp_github_create_issue".to_owned()],
        already_visible: vec!["read_file".to_owned()],
        unknown: vec![],
    };
    let output = render_activation(&outcome, &corpus());
    assert!(!output.is_error);
    assert!(
        output
            .content
            .contains("Activated: mcp_github_create_issue.")
    );
    assert!(
        output
            .content
            .contains("Already in your tool list: read_file.")
    );
    assert!(!output.content.contains("properties"));
}

#[test]
fn an_unknown_name_gets_suggestions_and_is_an_error_when_nothing_else_happened() {
    let outcome = Activation {
        activated: vec![],
        already_visible: vec![],
        unknown: vec!["github_issue".to_owned(), "qzx".to_owned()],
    };
    let output = render_activation(&outcome, &corpus());
    assert!(output.is_error);
    assert!(
        output
            .content
            .contains("Unknown tool \"github_issue\". Did you mean: mcp_github_create_issue?")
    );
    assert!(
        output
            .content
            .contains("Unknown tool \"qzx\". Use the exact name")
    );

    let mixed = Activation {
        activated: vec!["exec".to_owned()],
        already_visible: vec![],
        unknown: vec!["qzx".to_owned()],
    };
    assert!(!render_activation(&mixed, &corpus()).is_error);
}

// The tool over a stub port.

#[derive(Default)]
struct Stub {
    visible: Vec<String>,
    activated: Mutex<Vec<Vec<String>>>,
}

impl ToolDiscovery for Stub {
    fn corpus(&self) -> Vec<ToolDefinition> {
        corpus()
    }

    fn visible(&self) -> Vec<String> {
        self.visible.clone()
    }

    fn activate(&self, names: &[String]) -> Activation {
        self.activated.lock().push(names.to_vec());
        let known = corpus();
        let mut outcome = Activation::default();
        for name in names {
            if self.visible.contains(name) {
                outcome.already_visible.push(name.clone());
            } else if known.iter().any(|tool| tool.name == *name) {
                outcome.activated.push(name.clone());
            } else {
                outcome.unknown.push(name.clone());
            }
        }
        outcome
    }
}

fn with_port(ws: &TestWorkspace, stub: Arc<Stub>) -> ToolContext {
    let mut ctx = ws.context().clone();
    ctx.discovery = Some(stub);
    ctx
}

async fn run(ctx: &ToolContext, args: Value) -> ToolExecution {
    tool_search_tool().execute(args, ctx).await
}

#[tokio::test]
async fn refuses_when_the_install_has_no_discovery_port() {
    let ws = TestWorkspace::new();
    let execution = run(ws.context(), json!({"query": "file"})).await;
    assert!(execution.is_error);
    assert!(execution.content.contains("lazy tool discovery is off"));
}

#[tokio::test]
async fn searches_the_port_corpus_and_marks_what_is_visible() {
    let ws = TestWorkspace::new();
    let stub = Arc::new(Stub {
        visible: vec!["read_file".to_owned()],
        ..Stub::default()
    });
    let execution = run(&with_port(&ws, stub), json!({"query": "file"})).await;
    assert!(!execution.is_error);
    assert!(
        execution
            .content
            .contains("read_file (already in your tool list)")
    );
    assert!(execution.content.contains("- write_file"));
    assert_eq!(execution.details.get("matched"), Some(&json!(2)));
}

#[tokio::test]
async fn activates_trimmed_distinct_names_and_activate_wins_over_query() {
    let ws = TestWorkspace::new();
    let stub = Arc::new(Stub::default());
    let execution = run(
        &with_port(&ws, Arc::clone(&stub)),
        json!({"query": "file", "activate": [" exec ", "exec", "", "read_file"]}),
    )
    .await;
    assert!(!execution.is_error);
    assert_eq!(
        stub.activated.lock().as_slice(),
        &[vec!["exec".to_owned(), "read_file".to_owned()]]
    );
    assert!(execution.content.starts_with("Activated: exec, read_file."));
    assert!(!execution.content.contains("Tools matching"));
    assert_eq!(
        execution.details.get("activated"),
        Some(&json!(["exec", "read_file"]))
    );
}

#[tokio::test]
async fn an_unknown_activation_is_answered_with_suggestions() {
    let ws = TestWorkspace::new();
    let execution = run(
        &with_port(&ws, Arc::new(Stub::default())),
        json!({"activate": ["github"]}),
    )
    .await;
    assert!(execution.is_error);
    assert!(
        execution
            .content
            .contains("Did you mean: mcp_github_create_issue?")
    );
}

#[tokio::test]
async fn asks_for_an_argument_when_both_are_missing_or_blank() {
    let ws = TestWorkspace::new();
    let ctx = with_port(&ws, Arc::new(Stub::default()));
    for args in [
        json!({}),
        json!({"query": "  "}),
        json!({"activate": []}),
        json!({"activate": [" "]}),
    ] {
        let execution = run(&ctx, args).await;
        assert!(execution.is_error);
        assert!(execution.content.contains("Provide query"));
    }
}

#[tokio::test]
async fn a_bad_argument_shape_is_invalid_input() {
    let ws = TestWorkspace::new();
    let ctx = with_port(&ws, Arc::new(Stub::default()));
    let execution = run(&ctx, json!({"activate": "exec"})).await;
    assert_eq!(execution.kind, Some(ErrorKind::InvalidInput));
    let execution = run(&ctx, json!({"activate": [1]})).await;
    assert_eq!(execution.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn conforms() {
    tool_conformance(&ToolConformance {
        tool: tool_search_tool(),
        context: Box::new(|| {
            let ws = TestWorkspace::new();
            let ctx = with_port(&ws, Arc::new(Stub::default()));
            (ws, ctx)
        }),
        valid_args: json!({"query": "file"}).as_object().cloned().unwrap(),
        large_output_args: None,
    })
    .await;
    assert_eq!(tool_search_tool().definition().name, TOOL_SEARCH_NAME);
}
