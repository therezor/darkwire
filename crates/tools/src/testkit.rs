//! A disposable workspace and the tool conformance suite, behind the `testkit`
//! feature.
//!
//! Every tool — built-in, extension-supplied, or bridged from an MCP server —
//! has to behave the same way at its edges, and those edges are where tools
//! break in ways that look like the *model* being broken. A schema that
//! advertises a required field it does not need produces hallucinated
//! arguments; a tool that strips an unknown key answers a question nobody
//! asked; one that ignores its token turns the Stop button into a suggestion.
//! None of that shows up in a test of the tool's happy path, so it is checked
//! here, once, for all of them.
//!
//! The suite is generated from the tool's own JSON Schema wherever it can be:
//! the coercion and wrong-type cases are derived by walking `parameters`, so a
//! tool that gains a numeric argument gains its coercion test without anyone
//! remembering to write one. It works through [`Tool::execute`] alone, because
//! two of the three kinds of tool above have no typed parser to reach.
#![allow(
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way, and `?` in a test hides which line gave up"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::ErrorKind;
use darkwire_protocol::{AgentSettings, ToolSource};
use darkwire_security::{JailOptions, WorkspaceJail};
use serde_json::{Map, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::registry::{ToolInvocation, ToolRegistry};
use crate::tool::{AnyTool, Tool, ToolContext, is_tool_name};

/// A temporary workspace with a jail and a context around it.
///
/// The temp directory is canonicalised by the jail, which is not optional:
/// macOS hands out `/var/folders/...`, a symlink to `/private/var/folders/...`,
/// and a jail that compared against the un-canonicalised form would reject
/// every path inside its own workspace.
pub struct TestWorkspace {
    dir: TempDir,
    jail: Arc<WorkspaceJail>,
    token: CancellationToken,
    context: ToolContext,
}

impl std::fmt::Debug for TestWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestWorkspace")
            .field("root", &self.jail.root())
            .finish_non_exhaustive()
    }
}

impl TestWorkspace {
    /// A fresh workspace under the default tools config.
    ///
    /// # Panics
    ///
    /// When the temp directory cannot be created — a failing test either way.
    pub fn new() -> TestWorkspace {
        TestWorkspace::with_config(AgentSettings::default())
    }

    /// A fresh workspace under `config`.
    ///
    /// # Panics
    ///
    /// When the temp directory cannot be created — a failing test either way.
    pub fn with_config(config: AgentSettings) -> TestWorkspace {
        let dir = tempfile::Builder::new()
            .prefix("darkwire-tools-")
            .tempdir()
            .unwrap_or_else(|error| panic!("temp dir: {error}"));
        let jail = Arc::new(
            WorkspaceJail::new(JailOptions::new(dir.path().join("workspace")))
                .unwrap_or_else(|error| panic!("jail: {error}")),
        );
        let token = CancellationToken::new();
        let context =
            ToolContext::new(Arc::clone(&jail), Arc::new(config)).with_token(token.clone());
        TestWorkspace {
            dir,
            jail,
            token,
            context,
        }
    }

    /// The canonical workspace root.
    pub fn root(&self) -> &Path {
        self.jail.root()
    }

    /// The directory *above* the workspace, for planting files outside it.
    pub fn outside(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    /// The jail.
    pub fn jail(&self) -> &Arc<WorkspaceJail> {
        &self.jail
    }

    /// The turn's token; cancel it to abort every context from here.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// The context.
    pub fn context(&self) -> &ToolContext {
        &self.context
    }

    /// A context with the same workspace and a config override.
    pub fn with(&self, edit: impl FnOnce(&mut AgentSettings)) -> ToolContext {
        let mut config = (*self.context.config).clone();
        edit(&mut config);
        self.context.clone().with_config(config)
    }
}

impl Default for TestWorkspace {
    fn default() -> TestWorkspace {
        TestWorkspace::new()
    }
}

/// What the conformance suite needs to know about one tool.
pub struct ToolConformance {
    /// The tool under test.
    pub tool: AnyTool,
    /// A fresh context per case — normally a new temporary workspace. Called
    /// once per case, so nothing a case writes can leak into the next. The
    /// workspace is returned beside the context so it outlives the call.
    pub context: Box<dyn Fn() -> (TestWorkspace, ToolContext) + Send + Sync>,
    /// Arguments the tool accepts and can execute against `context()`.
    pub valid_args: Map<String, Value>,
    /// Arguments producing more output than `config.max_output_chars`. Omit
    /// only if the tool cannot be made to produce a large result.
    pub large_output_args: Option<Map<String, Value>>,
}

struct Property {
    name: String,
    kind: String,
    required: bool,
    minimum: Option<f64>,
}

fn properties_of(tool: &dyn Tool) -> Vec<Property> {
    let parameters = &tool.definition().parameters;
    let required: Vec<&str> = parameters
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    parameters
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(name, schema)| Property {
                    name: name.clone(),
                    kind: schema
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                    required: required.contains(&name.as_str()),
                    minimum: schema.get("minimum").and_then(Value::as_f64),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A key no schema declares, used to prove unknown arguments are refused.
const UNKNOWN_KEY: &str = "__darkwire_conformance_unknown__";

/// The declaration checks: a provider-safe name, a strict object schema with a
/// description on every property, and a definition consistent with `risk()`.
///
/// # Panics
///
/// On the first check that fails, naming it.
pub fn assert_declaration(tool: &dyn Tool) {
    let definition = tool.definition();
    assert!(
        is_tool_name(&definition.name),
        "{} is not a provider-safe name",
        definition.name
    );
    assert!(
        !definition.description.trim().is_empty(),
        "{} has no description",
        definition.name
    );
    let parameters = &definition.parameters;
    assert_eq!(
        parameters.get("type").and_then(Value::as_str),
        Some("object"),
        "{} does not take an object",
        definition.name
    );
    assert_eq!(
        parameters.get("additionalProperties"),
        Some(&Value::Bool(false)),
        "{} does not refuse unknown properties",
        definition.name
    );
    assert!(
        parameters.get("$schema").is_none(),
        "{} carries a $schema annotation",
        definition.name
    );
    if let Some(properties) = parameters.get("properties").and_then(Value::as_object) {
        for (name, schema) in properties {
            assert!(
                schema
                    .get("description")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty()),
                "{}.{name} has no description",
                definition.name
            );
        }
    }
    assert_eq!(
        definition.risk,
        tool.risk(),
        "{} risk disagrees",
        definition.name
    );
}

async fn kind_of(tool: &dyn Tool, args: Value, ctx: &ToolContext) -> Option<ErrorKind> {
    tool.execute(args, ctx).await.kind
}

/// The argument checks, expressed through `execute`: valid arguments are not
/// refused as invalid; a non-object, an unknown key, a missing required
/// property and a number where a string is expected are; the string form of a
/// number is accepted.
///
/// # Panics
///
/// On the first check that fails, naming it.
pub async fn assert_argument_validation(suite: &ToolConformance) {
    let tool = suite.tool.as_ref();
    let name = tool.definition().name.clone();
    let invalid = Some(ErrorKind::InvalidInput);

    let (_ws, ctx) = (suite.context)();
    assert_ne!(
        kind_of(tool, Value::Object(suite.valid_args.clone()), &ctx).await,
        invalid,
        "{name} refused its valid arguments"
    );

    for raw in [
        Value::from("not an object"),
        Value::from(42),
        Value::Array(Vec::new()),
        Value::Bool(true),
    ] {
        let (_ws, ctx) = (suite.context)();
        assert_eq!(
            kind_of(tool, raw.clone(), &ctx).await,
            invalid,
            "{name} accepted {raw} in place of an argument object"
        );
    }

    let mut with_unknown = suite.valid_args.clone();
    with_unknown.insert(UNKNOWN_KEY.to_owned(), Value::Bool(true));
    let (_ws, ctx) = (suite.context)();
    let execution = tool.execute(Value::Object(with_unknown), &ctx).await;
    assert_eq!(
        execution.kind, invalid,
        "{name} stripped an unknown property rather than refusing it"
    );
    assert!(
        execution.content.contains(&name),
        "{name}'s refusal does not name the tool"
    );

    for property in properties_of(tool) {
        if property.required {
            let mut without = suite.valid_args.clone();
            without.remove(&property.name);
            let (_ws, ctx) = (suite.context)();
            assert_eq!(
                kind_of(tool, Value::Object(without), &ctx).await,
                invalid,
                "{name} accepted a call missing the required {}",
                property.name
            );
        }
        if property.kind == "integer" || property.kind == "number" {
            let minimum = property.minimum.unwrap_or(1.0);
            let mut coerced = suite.valid_args.clone();
            coerced.insert(property.name.clone(), Value::String(format!("{minimum}")));
            let (_ws, ctx) = (suite.context)();
            assert_ne!(
                kind_of(tool, Value::Object(coerced), &ctx).await,
                invalid,
                "{name} refused the string form of {}",
                property.name
            );
        }
        if property.kind == "string" {
            let mut wrong = suite.valid_args.clone();
            wrong.insert(property.name.clone(), Value::from(1));
            let (_ws, ctx) = (suite.context)();
            assert_eq!(
                kind_of(tool, Value::Object(wrong), &ctx).await,
                invalid,
                "{name} accepted a number where {} expects a string",
                property.name
            );
        }
    }
}

/// The execution checks: an already-cancelled token is honoured within 100 ms,
/// a failure is a typed kind and never a message prefix, and a large result is
/// truncated by the registry to the configured budget.
///
/// # Panics
///
/// On the first check that fails, naming it.
pub async fn assert_execution(suite: &ToolConformance) {
    let tool = &suite.tool;
    let name = tool.definition().name.clone();

    let (_ws, ctx) = (suite.context)();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let started = std::time::Instant::now();
    let execution = tool
        .execute(
            Value::Object(suite.valid_args.clone()),
            &ctx.with_token(cancelled),
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "{name} took {:?} to notice a cancelled token",
        started.elapsed()
    );
    assert_eq!(
        execution.kind,
        Some(ErrorKind::Aborted),
        "{name} did not report the cancellation as aborted"
    );

    let mut with_unknown = suite.valid_args.clone();
    with_unknown.insert(UNKNOWN_KEY.to_owned(), Value::Bool(true));
    let (_ws, ctx) = (suite.context)();
    let execution = tool.execute(Value::Object(with_unknown), &ctx).await;
    assert!(
        execution.is_error && execution.kind.is_some(),
        "{name} reported a failure without a kind"
    );

    if let Some(large) = &suite.large_output_args {
        let (_ws, ctx) = (suite.context)();
        let registry = ToolRegistry::new();
        registry
            .register(Arc::clone(tool), ToolSource::Builtin)
            .unwrap_or_else(|error| panic!("register: {error}"));
        let mut config = (*ctx.config).clone();
        config.max_output_chars = 200;
        let ctx = ctx.with_config(config);
        let call =
            ToolInvocation::with_json(&name, serde_json::to_string(large).unwrap_or_default());
        let execution = registry.execute_scoped(&call, &ctx, None).await;
        assert!(!execution.is_error, "{name} failed: {}", execution.content);
        assert!(execution.truncated, "{name} was not truncated");
        // The truncation marker names how much was dropped, so the result is
        // the budget plus that one line rather than exactly the budget.
        assert!(
            execution.content.encode_utf16().count() < 400,
            "{name} exceeded its budget"
        );
    }
}

/// The whole suite, in order.
///
/// # Panics
///
/// On the first check that fails, naming it.
pub async fn tool_conformance(suite: &ToolConformance) {
    assert_declaration(suite.tool.as_ref());
    assert_argument_validation(suite).await;
    assert_execution(suite).await;
}

/// A web port that answers from memory, for tests that must not touch a network.
///
/// Exists here rather than in one crate's `tests/` because `darkwire-agent`
/// wants it too once the resolver is wired: a turn that runs a web tool needs a
/// port, and a port that dials anything is a test that fails on an aeroplane.
#[derive(Debug)]
pub struct FakeWeb {
    /// The text every fetch returns.
    pub text: String,
    /// What a search returns, in order.
    pub hits: Vec<crate::web::SearchHit>,
    /// `false` makes the port report an agent with egress switched off.
    pub reachable: bool,
}

impl Default for FakeWeb {
    fn default() -> FakeWeb {
        FakeWeb {
            text: "The page said something worth reading, at length.".repeat(4),
            hits: vec![crate::web::SearchHit {
                title: "A result".to_owned(),
                url: "https://example.test/a".to_owned(),
                snippet: "What it is about.".to_owned(),
                source: String::new(),
            }],
            reachable: true,
        }
    }
}

impl FakeWeb {
    /// A port whose pages are `chars` long and whose result list is long enough
    /// to overflow a small budget on its own.
    ///
    /// Both halves matter. `web_fetch` overflows on the page; `web_search`
    /// computes a share and reads nothing when the budget is tiny, so the only
    /// way its result exceeds one is the list itself.
    pub fn of_length(chars: usize) -> FakeWeb {
        FakeWeb {
            text: "x".repeat(chars),
            hits: (0..12)
                .map(|index| crate::web::SearchHit {
                    title: format!("Result {index} with a title of a realistic length"),
                    url: format!("https://example.test/result-{index}"),
                    snippet: "A summary long enough to take a line of its own.".to_owned(),
                    source: String::new(),
                })
                .collect(),
            reachable: true,
        }
    }
}

impl crate::web::WebPort for FakeWeb {
    fn policy(&self) -> Option<&darkwire_security::NetworkPolicy> {
        // A leaked reference is the only way to hand out a borrow from a value
        // this type does not store; a test double may have one static policy.
        static OPEN: std::sync::OnceLock<darkwire_security::NetworkPolicy> =
            std::sync::OnceLock::new();
        self.reachable
            .then(|| OPEN.get_or_init(darkwire_security::NetworkPolicy::default))
    }

    fn read_timeout_ms(&self) -> u64 {
        15_000
    }

    fn backend_hosts(&self) -> Vec<String> {
        vec!["search.test".to_owned()]
    }

    fn fetch<'a>(
        &'a self,
        url: &'a str,
        _token: tokio_util::sync::CancellationToken,
    ) -> crate::tool::BoxFuture<'a, darkwire_core::Result<crate::web::Page>> {
        Box::pin(async move {
            Ok(crate::web::Page {
                url: url.to_owned(),
                title: "A page".to_owned(),
                text: self.text.clone(),
                kind: crate::web::PageKind::Article,
                content_type: "text/html".to_owned(),
                note: String::new(),
            })
        })
    }

    fn search<'a>(
        &'a self,
        _query: &'a crate::web::SearchQuery,
        _token: tokio_util::sync::CancellationToken,
    ) -> crate::tool::BoxFuture<'a, darkwire_core::Result<crate::web::SearchOutcome>> {
        Box::pin(async move {
            Ok(crate::web::SearchOutcome {
                hits: self.hits.clone(),
                problems: Vec::new(),
            })
        })
    }
}
