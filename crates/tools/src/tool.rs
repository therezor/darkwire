//! The `Tool` trait, what a call receives, what it returns, and the typed
//! adapter every built-in is written against.
//!
//! One declaration per tool is the point of [`TypedTool`]. The argument type
//! is a struct; schemars derives the JSON Schema the model is shown from it,
//! `jsonschema` validates every call against that same schema before serde
//! turns the value into the struct, and the handler receives the struct. There
//! is one copy of the shape, so a field added to the struct is advertised,
//! validated and delivered without anyone touching three places — and a schema
//! the handler does not follow is a compile error rather than a provider 400.
//!
//! Three decisions are load-bearing:
//!
//!  - **The schema must refuse unknown keys.** Rejecting them is not pedantry:
//!    a model that adds `recursive: true` to `read` has misunderstood the
//!    tool, and silently stripping the key runs a command it did not ask for
//!    and returns an answer to a question it did not pose. Every argument
//!    struct carries `deny_unknown_fields`, which is what puts
//!    `additionalProperties: false` in the emitted schema, and the constructor
//!    checks the emitted schema so the rule holds however it was built.
//!
//!  - **Optional means omittable, never nullable.** A field the model may
//!    leave out is advertised as absent from `required`, not as accepting
//!    `null`; advertising the latter tells the model it must supply every
//!    optional argument, which is both wrong and a reliable source of invented
//!    values.
//!
//!  - **Numeric strings are coerced, deliberately, and booleans are not.**
//!    Models emit `"10"` for a number argument often enough that refusing it
//!    is a self-inflicted failure mode. `"false"` for a boolean is refused,
//!    because the only coercion available reads it as `true`, and on a flag
//!    deciding between one replacement and every replacement that inverts the
//!    model's stated intent.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};

use darkwire_core::{Clock, ErrorKind, Result, SystemClock, WireError};
use darkwire_protocol::json::Object;
use darkwire_protocol::{
    AgentSettings, ToolAnnotations, ToolDefinition, ToolRisk, ToolSource, protocol_generator,
};
use darkwire_security::{WorkspaceJail, WrappedToolOutput};
use regex::Regex;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::automation::AutomationPort;
use crate::discovery::ToolDiscovery;
use crate::runner::{CommandRunner, LocalRunner};
use crate::tasks::TaskPort;

/// A boxed, sendable future borrowing its inputs for `'a`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The name shape every provider accepts, as a pattern: `^[A-Za-z0-9_-]{1,64}$`.
///
/// OpenAI, and every gateway speaking its wire, restricts function names to
/// this set. A tool named `web search` is not rejected at registration by the
/// provider — it is rejected mid-turn, as a 400 that reads like the model is
/// broken. The MCP flattening (`mcp_{server}_{tool}`) exists partly to keep
/// remote names inside it.
pub const TOOL_NAME_PATTERN: &str = "^[A-Za-z0-9_-]{1,64}$";

static TOOL_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(TOOL_NAME_PATTERN).unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// Whether a name matches [`TOOL_NAME_PATTERN`].
pub fn is_tool_name(name: &str) -> bool {
    TOOL_NAME.is_match(name)
}

/// Config as the schema defines it, for tests and for a caller with no file.
pub fn default_tools_config() -> AgentSettings {
    AgentSettings::default()
}

/// Everything a tool may reach, supplied per call rather than captured at
/// definition time.
///
/// A tool is a value, not a closure over a workspace: the registry is built
/// once and the same `read` serves every session, so the jail, the config
/// and — critically — the turn's cancellation token arrive with the invocation.
/// Capturing them would mean a registry per session and a token one turn stale.
#[derive(Clone)]
pub struct ToolContext {
    /// The only thing permitted to judge an agent-supplied path.
    pub jail: Arc<WorkspaceJail>,
    /// The turn's cancellation, already combined with any per-call timeout by
    /// the registry. Every tool must observe it before doing work and while
    /// doing it.
    pub token: CancellationToken,
    /// The calling agent's settings: the `exec` rules, the result budget and
    /// the approval timeout all live there, per agent.
    pub config: Arc<AgentSettings>,
    /// Wall-clock and monotonic time.
    pub clock: Arc<dyn Clock>,
    /// Source for the exec env allow-list. Defaults to this process's
    /// environment.
    pub env: Arc<HashMap<String, String>>,
    /// Where a guarded command runs. Defaults to [`LocalRunner`] — a child
    /// process on this machine, which is what `exec` has always done. The seam
    /// an agent's `sandbox` setting reaches the tool layer through.
    pub runner: Arc<dyn CommandRunner>,
    /// Whether `runner` confines the command to a container mounting only the
    /// workspace.
    ///
    /// Separate from `runner` being set, because the two answer different
    /// questions: `runner` is *where*, and this is *whether the guard's
    /// host-shaped assumptions still hold*. A future runner that ran commands
    /// on another host without confining them would set one and not the other.
    pub sandboxed: bool,
    /// The turn's placement, so an operation forwarded to the sandbox service
    /// can name the agent, workspace, session and container it belongs to.
    /// `None` is a turn with no container, which executes locally instead.
    pub placement: Option<crate::PlacementRequest>,
    /// Where a scheduled job gets written, already scoped to this turn's agent
    /// and session. `None` is a build with no scheduler — the tool then refuses
    /// rather than pretending.
    pub automation: Option<Arc<dyn AutomationPort>>,
    /// What `tool_search` may find and reveal, already scoped to this turn's
    /// agent and session. `None` is an install with lazy discovery off, where
    /// the tool is not registered and would refuse if it were.
    pub discovery: Option<Arc<dyn ToolDiscovery>>,
    /// Where the `todo` tool writes, already scoped to this turn's session.
    /// `None` is a call with no conversation behind it — a one-shot path or a
    /// test — where the tool refuses rather than writing nowhere.
    pub tasks: Option<Arc<dyn TaskPort>>,
    /// This turn's tool-output nonce. When present the registry fences every
    /// result in it; `None` is the bare registry — the CLI's one-shot paths and
    /// tests — where nothing is sent to a model.
    pub nonce: Option<String>,
    /// What this turn may reach on the web, already scoped to its agent's
    /// egress policy. `None` is an install with no web layer, where both web
    /// tools refuse rather than pretending; an agent whose egress is switched
    /// off is a port whose `policy()` is `None`, which is a different sentence.
    pub web: Option<Arc<dyn crate::web::port::WebPort>>,
}

impl ToolContext {
    /// A context for `jail` under `config`, with every other seam at its
    /// default: a fresh token, the host clock, this process's environment, the
    /// local runner, no sandbox, no scheduler and no nonce.
    pub fn new(jail: Arc<WorkspaceJail>, config: Arc<AgentSettings>) -> ToolContext {
        ToolContext {
            jail,
            token: CancellationToken::new(),
            config,
            clock: Arc::new(SystemClock),
            env: Arc::new(std::env::vars().collect()),
            runner: Arc::new(LocalRunner::default()),
            sandboxed: false,
            placement: None,
            automation: None,
            discovery: None,
            tasks: None,
            nonce: None,
            web: None,
        }
    }

    /// The same context with a different cancellation token.
    #[must_use]
    pub fn with_token(mut self, token: CancellationToken) -> ToolContext {
        self.token = token;
        self
    }

    /// The same context with a different configuration.
    #[must_use]
    pub fn with_config(mut self, config: AgentSettings) -> ToolContext {
        self.config = Arc::new(config);
        self
    }
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("jail", &self.jail.root())
            .field("cancelled", &self.token.is_cancelled())
            .field("sandboxed", &self.sandboxed)
            .field("automation", &self.automation.is_some())
            .field("discovery", &self.discovery.is_some())
            .field("tasks", &self.tasks.is_some())
            .field("nonce", &self.nonce.is_some())
            .field("web", &self.web.is_some())
            .finish_non_exhaustive()
    }
}

/// What a handler returns.
///
/// `is_error` is a flag rather than a prefix on the text. A tool whose
/// legitimate output starts with the word "Error" — `grep` over a log file,
/// say — must not be recorded as a failed call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolOutput {
    /// What the model reads.
    pub content: String,
    /// Whether the call failed. Independent of the content.
    pub is_error: bool,
    /// Structured context for the audit log. Never shown to the model.
    pub details: Map<String, Value>,
}

impl ToolOutput {
    /// A successful result with no details.
    pub fn text(content: impl Into<String>) -> ToolOutput {
        ToolOutput {
            content: content.into(),
            is_error: false,
            details: Map::new(),
        }
    }

    /// A failure the model should read and recover from.
    pub fn error(content: impl Into<String>) -> ToolOutput {
        ToolOutput {
            content: content.into(),
            is_error: true,
            details: Map::new(),
        }
    }

    /// Adds one audit detail.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> ToolOutput {
        self.details.insert(key.into(), value.into());
        self
    }
}

impl From<String> for ToolOutput {
    fn from(content: String) -> ToolOutput {
        ToolOutput::text(content)
    }
}

impl From<&str> for ToolOutput {
    fn from(content: &str) -> ToolOutput {
        ToolOutput::text(content)
    }
}

/// The outcome of one call, ready to become a `tool` message.
///
/// One type serves two layers. A tool fills `content`, `is_error`, `kind` and
/// `details` — [`ToolExecution::ok`] and [`ToolExecution::error`] are the two
/// constructors it needs. The registry fills the rest: it stamps `name`,
/// measures `duration_ms`, truncates `content` to the configured budget and,
/// when the context carries a nonce, fences the truncated text into
/// `envelope`. The agent must not repeat either step: truncating an envelope
/// cuts its closing delimiter off, and a tool result the model cannot see the
/// end of is one it reads as continuing into the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecution {
    /// The tool that ran. Empty until the registry stamps it.
    pub name: String,
    /// The plain result text, truncated to the budget. What the UI shows.
    pub content: String,
    /// Whether the call failed.
    pub is_error: bool,
    /// Present only on failure. `Aborted` is a cancellation, not an error to
    /// show.
    pub kind: Option<ErrorKind>,
    /// Whether `content` was cut down to `config.max_output_chars`.
    pub truncated: bool,
    /// Wall time the call took, from the injected clock.
    pub duration_ms: u64,
    /// Audit context from the handler. Not sent to the model.
    pub details: Map<String, Value>,
    /// `content` fenced in this turn's delimiter, with what detection found.
    /// The `text` is what the `tool` message carries; `None` when the context
    /// had no nonce.
    pub envelope: Option<WrappedToolOutput>,
}

impl ToolExecution {
    /// A successful result.
    pub fn ok(content: impl Into<String>) -> ToolExecution {
        ToolExecution {
            name: String::new(),
            content: content.into(),
            is_error: false,
            kind: None,
            truncated: false,
            duration_ms: 0,
            details: Map::new(),
            envelope: None,
        }
    }

    /// A failure of `kind`, in words the model can act on.
    pub fn error(kind: ErrorKind, content: impl Into<String>) -> ToolExecution {
        ToolExecution {
            kind: Some(kind),
            is_error: true,
            ..ToolExecution::ok(content)
        }
    }

    /// A failure flagged by the handler with no taxonomy kind — the tool ran
    /// and reported that what it did failed, as `grep` exiting 1 does.
    pub fn flagged(content: impl Into<String>) -> ToolExecution {
        ToolExecution {
            is_error: true,
            ..ToolExecution::ok(content)
        }
    }

    /// Replaces the audit details.
    #[must_use]
    pub fn with_details(mut self, details: Map<String, Value>) -> ToolExecution {
        self.details = details;
        self
    }

    /// Adds one audit detail.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> ToolExecution {
        self.details.insert(key.into(), value.into());
        self
    }

    /// Whether this is the cancellation the caller asked for.
    pub fn is_aborted(&self) -> bool {
        self.kind == Some(ErrorKind::Aborted)
    }
}

impl From<ToolOutput> for ToolExecution {
    fn from(output: ToolOutput) -> ToolExecution {
        let execution = if output.is_error {
            ToolExecution::flagged(output.content)
        } else {
            ToolExecution::ok(output.content)
        };
        execution.with_details(output.details)
    }
}

impl From<WireError> for ToolExecution {
    fn from(error: WireError) -> ToolExecution {
        ToolExecution::error(error.kind, error.message).with_details(error.details)
    }
}

impl From<Result<ToolOutput>> for ToolExecution {
    fn from(result: Result<ToolOutput>) -> ToolExecution {
        match result {
            Ok(output) => output.into(),
            Err(error) => error.into(),
        }
    }
}

/// Something the model can call.
///
/// Three methods, and nothing about validation: that is the implementor's,
/// because a bridged MCP tool validates against the schema its server sent
/// while a built-in validates against one derived from a struct. What every
/// implementor promises is that `execute` **never fails**: an invalid argument,
/// a refused path, a cancelled turn all come back as a [`ToolExecution`] with
/// `is_error` set and a `kind` from the taxonomy, because a failed tool call is
/// a legal history entry the model needs to see to recover.
pub trait Tool: Send + Sync {
    /// The definition advertised to the model. Its `source` is whatever the
    /// tool was built with; the registry stamps the one it registered under.
    fn definition(&self) -> &ToolDefinition;

    /// What it can do, worst case.
    fn risk(&self) -> ToolRisk;

    /// Validates, then runs. Never fails; see the trait docs.
    fn execute<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolExecution>;
}

/// A tool with its argument type erased, for storage and iteration.
pub type AnyTool = Arc<dyn Tool>;

/// Returns the taxonomy's `aborted` once `token` has fired.
pub fn assert_not_aborted(token: &CancellationToken, what: &str) -> Result<()> {
    if token.is_cancelled() {
        return Err(WireError::aborted(what));
    }
    Ok(())
}

/// One argument that failed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgIssue {
    /// Dotted path to the offending property, or `""` for the object itself.
    pub path: String,
    /// The validator's sentence.
    pub message: String,
}

/// The handler half of a [`TypedTool`]: an argument type and what to do with
/// a value of it.
pub trait ToolHandler: Send + Sync + 'static {
    /// The arguments, as validated and deserialised from the model's JSON.
    type Args: DeserializeOwned + JsonSchema + Send + 'static;

    /// Runs the tool on validated arguments.
    fn execute<'a>(
        &'a self,
        args: Self::Args,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>>;
}

/// What a tool says about itself, beside its handler.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// The name the model calls it by. Must match [`TOOL_NAME_PATTERN`].
    pub name: String,
    /// The sentence that decides whether the model reaches for it.
    pub description: String,
    /// Defaults to `safe`; the approval policy per band lives in config.
    pub risk: ToolRisk,
    /// MCP-style effect hints.
    pub annotations: Option<ToolAnnotations>,
}

impl ToolSpec {
    /// A `safe` tool with no annotations.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: description.into(),
            risk: ToolRisk::Safe,
            annotations: None,
        }
    }

    /// Sets the risk band.
    #[must_use]
    pub fn risk(mut self, risk: ToolRisk) -> ToolSpec {
        self.risk = risk;
        self
    }

    /// Sets the effect hints.
    #[must_use]
    pub fn annotations(mut self, annotations: ToolAnnotations) -> ToolSpec {
        self.annotations = Some(annotations);
        self
    }
}

/// A rewrite of the raw argument value before validation.
///
/// The one place a tool may be lenient about shape: a container program's `args`
/// arriving as a string is coerced into the array the schema asks for. The
/// advertised type does not change — coercion is a backstop, not a contract.
pub type Preprocess = Arc<dyn Fn(&mut Value) + Send + Sync>;

/// A tool defined from an argument struct and a handler.
///
/// Everything derivable is derived here and once: the JSON Schema and its
/// compiled validator are computed when the tool is built, not per turn and
/// not per call.
pub struct TypedTool<H: ToolHandler> {
    definition: ToolDefinition,
    validator: jsonschema::Validator,
    preprocess: Option<Preprocess>,
    handler: H,
}

impl<H: ToolHandler> std::fmt::Debug for TypedTool<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedTool")
            .field("name", &self.definition.name)
            .field("risk", &self.definition.risk)
            .finish_non_exhaustive()
    }
}

fn config_error(tool: &str, message: String) -> WireError {
    WireError::new(ErrorKind::Config, message).with_detail("tool", tool)
}

/// The JSON Schema for `A`, as a parameter object.
///
/// `$schema` and `title` are document-level annotations. Providers accept the
/// object as a parameter schema, not as a standalone document, and some
/// gateways reject unknown top-level keys outright.
pub fn parameters_for<A: JsonSchema>() -> Result<Object> {
    let mut generator = protocol_generator();
    let schema = generator.root_schema_for::<A>().to_value();
    let mut object: Object = serde_json::from_value(schema).map_err(|error| {
        WireError::new(ErrorKind::Config, "Argument schema is not an object").with_source(error)
    })?;
    object.shift_remove("$schema");
    object.shift_remove("title");
    Ok(object)
}

/// Refuses a parameter schema that is not a strict object.
fn assert_strict_object(name: &str, parameters: &Object) -> Result<()> {
    if parameters.get("type").and_then(Value::as_str) != Some("object") {
        return Err(config_error(
            name,
            format!(
                "Tool {name} must take an object, not {}",
                parameters
                    .get("type")
                    .map_or_else(|| "nothing".to_owned(), Value::to_string)
            ),
        ));
    }
    if parameters.get("additionalProperties") != Some(&Value::Bool(false)) {
        return Err(config_error(
            name,
            format!(
                "Tool {name} must refuse unknown arguments (deny_unknown_fields) rather than strip them"
            ),
        ));
    }
    Ok(())
}

fn compile(name: &str, parameters: &Object) -> Result<jsonschema::Validator> {
    let value = serde_json::to_value(parameters).map_err(|error| {
        config_error(name, format!("Tool {name} has a schema with no JSON form")).with_source(error)
    })?;
    jsonschema::validator_for(&value).map_err(|error| {
        config_error(
            name,
            format!("Tool {name} has a schema with no JSON Schema form: {error}"),
        )
    })
}

/// Whether a property schema advertises a number.
fn is_numeric(schema: &Value) -> bool {
    let numeric = |t: &Value| matches!(t.as_str(), Some("integer" | "number"));
    match schema.get("type") {
        Some(Value::Array(types)) => types.iter().any(numeric),
        Some(single) => numeric(single),
        None => false,
    }
}

/// Turns `"10"` into `10` wherever the schema asks for a number.
///
/// Top-level properties only, which is where every built-in's numbers live,
/// and strings only: a boolean is never coerced, for the reason the module
/// docs give.
fn coerce_numeric_strings(parameters: &Object, value: &mut Value) {
    let Some(Value::Object(properties)) = parameters.get("properties") else {
        return;
    };
    let Value::Object(fields) = value else {
        return;
    };
    for (name, schema) in properties {
        if !is_numeric(schema) {
            continue;
        }
        if let Some(Value::String(text)) = fields.get(name)
            && let Some(parsed) = parse_number(text.trim())
        {
            fields.insert(name.clone(), Value::Number(parsed));
        }
    }
}

/// `"15"` as an integer, `"1.5"` as a float; anything else is left alone.
fn parse_number(text: &str) -> Option<serde_json::Number> {
    if let Ok(integer) = text.parse::<i64>() {
        return Some(serde_json::Number::from(integer));
    }
    text.parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
}

fn dotted(location: &jsonschema::paths::Location) -> String {
    location
        .as_str()
        .trim_start_matches('/')
        .replace('/', ".")
        .replace("~1", "/")
        .replace("~0", "~")
}

impl<H: ToolHandler> TypedTool<H> {
    /// Defines a tool from its spec and handler.
    ///
    /// Fails on a name a provider would refuse, an empty description, or an
    /// argument type whose schema is not a strict object.
    pub fn new(spec: ToolSpec, handler: H) -> Result<TypedTool<H>> {
        let parameters = parameters_for::<H::Args>()?;
        TypedTool::with_parameters(spec, parameters, handler)
    }

    /// Defines a tool advertising `parameters` in place of the derived schema.
    ///
    /// For a tool whose schema carries text only known at runtime — a container
    /// entry's own description of its `args`. The handler's argument type must
    /// still deserialise from anything the schema admits.
    pub fn with_parameters(spec: ToolSpec, parameters: Object, handler: H) -> Result<TypedTool<H>> {
        if !is_tool_name(&spec.name) {
            return Err(config_error(
                &spec.name,
                format!("Tool name must match {TOOL_NAME_PATTERN}: {}", spec.name),
            ));
        }
        if spec.description.trim().is_empty() {
            return Err(config_error(
                &spec.name,
                format!("Tool {} has no description", spec.name),
            ));
        }
        assert_strict_object(&spec.name, &parameters)?;
        let validator = compile(&spec.name, &parameters)?;
        Ok(TypedTool {
            definition: ToolDefinition {
                name: spec.name,
                description: spec.description,
                parameters,
                risk: spec.risk,
                source: ToolSource::Builtin,
                annotations: spec.annotations,
            },
            validator,
            preprocess: None,
            handler,
        })
    }

    /// Installs a rewrite applied to the raw value before validation.
    #[must_use]
    pub fn with_preprocess(mut self, preprocess: Preprocess) -> TypedTool<H> {
        self.preprocess = Some(preprocess);
        self
    }

    /// The name the model calls it by.
    pub fn name(&self) -> &str {
        &self.definition.name
    }

    /// The advertised parameter schema.
    pub fn parameters(&self) -> &Object {
        &self.definition.parameters
    }

    /// Validates the model's raw arguments and turns them into `H::Args`.
    ///
    /// A model calling a no-argument tool commonly emits `""`, `"{}"` or
    /// nothing at all, which arrives here as `null`. Treating it as an empty
    /// object lets the schema's own `required` list decide, instead of failing
    /// every such call with "expected object, received null".
    pub fn parse_args(&self, raw: Value) -> Result<H::Args> {
        let mut candidate = if raw.is_null() {
            Value::Object(Map::new())
        } else {
            raw
        };
        if let Some(preprocess) = &self.preprocess {
            preprocess(&mut candidate);
        }
        coerce_numeric_strings(&self.definition.parameters, &mut candidate);

        let issues: Vec<ArgIssue> = self
            .validator
            .iter_errors(&candidate)
            .map(|error| ArgIssue {
                path: dotted(error.instance_path()),
                message: error.to_string(),
            })
            .collect();
        if !issues.is_empty() {
            return Err(self.invalid(&issues));
        }

        serde_json::from_value::<H::Args>(candidate).map_err(|error| {
            self.invalid(&[ArgIssue {
                path: String::new(),
                message: error.to_string(),
            }])
        })
    }

    fn invalid(&self, issues: &[ArgIssue]) -> WireError {
        let detail = issues
            .iter()
            .map(|issue| {
                if issue.path.is_empty() {
                    issue.message.clone()
                } else {
                    format!("{}: {}", issue.path, issue.message)
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        let listed: Vec<Value> = issues
            .iter()
            .map(|issue| serde_json::json!({ "path": issue.path, "message": issue.message }))
            .collect();
        WireError::new(
            ErrorKind::InvalidInput,
            format!("Invalid arguments for {}: {detail}", self.definition.name),
        )
        .with_detail("tool", self.definition.name.as_str())
        .with_detail("issues", Value::Array(listed))
    }
}

impl<H: ToolHandler> Tool for TypedTool<H> {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn risk(&self) -> ToolRisk {
        self.definition.risk
    }

    fn execute<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolExecution> {
        Box::pin(async move {
            let parsed = match self.parse_args(args) {
                Ok(parsed) => parsed,
                Err(error) => return error.into(),
            };
            self.handler.execute(parsed, ctx).await.into()
        })
    }
}
