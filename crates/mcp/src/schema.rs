//! A remote `inputSchema`, made into something this crate will advertise and
//! validate against.
//!
//! Every other tool in DarkWire is declared with one argument struct and has its
//! JSON Schema *derived* from it. An MCP server hands over the JSON Schema
//! directly, so the derivation runs the other way, and the temptation is to
//! convert it into a struct so the typed adapter can be reused. It cannot be:
//! the shape is only known at runtime, and any conversion is lossy on `$ref`,
//! `oneOf`, `patternProperties` and `format`, so the model would be told a
//! shape the server did not describe — and the call would then fail *at the
//! server*, which reads as the model being broken.
//!
//! So the schema is passed through, normalised, and validated as the JSON
//! Schema it is. Two things the validator will not do:
//!
//! - **Fetch anything.** A schema is untrusted input from a socket, and a
//!   `$ref` to `https://…` or `file:///…` must not make this process reach for
//!   the network or the disk. Every validator is built with a retriever that
//!   refuses, so such a tool is dropped with a sentence instead.
//! - **Refuse `"10"` where a number is wanted.** Models emit the string form
//!   often enough that refusing it is a self-inflicted failure mode; the
//!   built-ins coerce for the same reason. Only in the number direction:
//!   coercing `1` into `"1"` for a string argument would hide a model that has
//!   genuinely misunderstood the tool.

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::json::Object;
use darkwire_tools::ArgIssue;
use jsonschema::error::ValidationErrorKind;
use jsonschema::{Retrieve, Uri, Validator};
use serde_json::{Map, Value};

/// A problem with what a server advertised. Never fatal on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaIssue {
    /// The upstream tool name.
    pub tool: String,
    /// What is wrong, for the status row.
    pub message: String,
}

/// The advertised parameter schema for one remote tool.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalisedSchema {
    /// Advertised as-is to the model and the browser.
    pub parameters: Object,
    /// Sloppiness worth a warning beside the server.
    pub issues: Vec<SchemaIssue>,
}

fn unusable(tool_name: &str, message: String) -> WireError {
    WireError::new(ErrorKind::InvalidInput, message).with_detail("tool", tool_name)
}

/// Normalises a raw `inputSchema`.
///
/// Fails only for a schema that cannot be advertised at all; everything else
/// is an issue the caller surfaces beside the server. A tool whose schema is
/// merely sloppy still works.
pub fn normalise_schema(tool_name: &str, raw: &Value) -> Result<NormalisedSchema> {
    let Value::Object(raw) = raw else {
        return Err(unusable(
            tool_name,
            format!("Tool {tool_name} advertises no input schema"),
        ));
    };

    // Cloned key by key rather than borrowed: the object travels to a provider
    // and to the browser, and what is advertised must be exactly what was
    // validated against, not a view onto something the session may replace.
    let mut parameters: Object = raw.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut issues = Vec::new();

    // A document-level annotation. Providers take this object as a *parameter*
    // schema rather than a standalone document, and some gateways reject
    // unknown top-level keys outright.
    parameters.shift_remove("$schema");

    if parameters.get("type").and_then(Value::as_str) != Some("object") {
        let declared = parameters.get("type").map_or_else(
            || "undefined".to_owned(),
            |t| match t {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            },
        );
        return Err(unusable(
            tool_name,
            format!("Tool {tool_name} must take an object, not {declared}"),
        ));
    }

    match parameters.get("additionalProperties") {
        None => {
            // The repo's position: a model that adds a key the tool does not
            // declare has misunderstood the tool, and stripping the key
            // silently answers a question nobody asked. Most servers simply
            // omit this, so the default is where it lands.
            parameters.insert("additionalProperties".to_owned(), Value::Bool(false));
        }
        Some(Value::Bool(false)) => {}
        Some(_) => {
            // Left as the server wrote it. A tool that genuinely takes a
            // free-form bag exists, and refusing to advertise it would be this
            // client deciding the server is wrong about its own arguments.
            issues.push(SchemaIssue {
                tool: tool_name.to_owned(),
                message:
                    "accepts undeclared arguments, so a mistyped argument name will not be refused"
                        .to_owned(),
            });
        }
    }

    if let Some(Value::Object(properties)) = parameters.get("properties") {
        for (name, schema) in properties {
            let described = matches!(schema.get("description"), Some(Value::String(_)));
            if described {
                continue;
            }
            // Reported, never invented. A description this client made up is a
            // sentence the model will believe and the server never wrote.
            issues.push(SchemaIssue {
                tool: tool_name.to_owned(),
                message: format!(
                    "argument \"{name}\" has no description, so the model is guessing what it is for"
                ),
            });
        }
    }

    Ok(NormalisedSchema { parameters, issues })
}

/// Refuses every `$ref` that leaves the schema document.
///
/// A server's schema is data that arrived over a socket. Following a reference
/// out of it would let that data direct this process at a URL or a file, and
/// nothing about validating a tool call needs that.
#[derive(Debug)]
struct RefuseRemote;

/// The error a refused reference surfaces as.
#[derive(Debug)]
struct RefusedReference(String);

impl std::fmt::Display for RefusedReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refuses to fetch {}: a tool schema may not reference outside itself",
            self.0
        )
    }
}

impl std::error::Error for RefusedReference {}

impl Retrieve for RefuseRemote {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err(Box::new(RefusedReference(uri.to_string())))
    }
}

/// Which JSON types a property declares, for coercion and for wording.
fn declared_types(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(many)) => many
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// The JSON type name of a value, in the words the wording uses.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `"10"` where a number was asked for, one level deep.
fn coerce(types: &[String], value: &Value) -> Option<Value> {
    let Value::String(text) = value else {
        return None;
    };
    if text.trim().is_empty() {
        return None;
    }
    let wants_integer = types.iter().any(|t| t == "integer");
    if !wants_integer && !types.iter().any(|t| t == "number") {
        return None;
    }
    let parsed: f64 = text.trim().parse().ok()?;
    if !parsed.is_finite() {
        return None;
    }
    if wants_integer && parsed.fract() != 0.0 {
        return None;
    }
    if parsed.fract() == 0.0 && parsed.abs() < 9_007_199_254_740_992.0 {
        // Kept integral so an `integer` schema accepts it and the server sees
        // `3`, not `3.0`.
        #[allow(clippy::cast_possible_truncation)] // bounded by the check above
        return Some(Value::from(parsed as i64));
    }
    serde_json::Number::from_f64(parsed).map(Value::Number)
}

/// A validator for one tool's advertised parameters.
///
/// Built once per tool, at bridge time, so a turn making twenty calls does not
/// re-walk the schema twenty times.
pub struct ArgValidator {
    tool_name: String,
    properties: Map<String, Value>,
    validator: Validator,
}

impl std::fmt::Debug for ArgValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArgValidator")
            .field("tool_name", &self.tool_name)
            .finish_non_exhaustive()
    }
}

/// Why a call was refused before it reached the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgFailure {
    /// One sentence naming the tool and every problem.
    pub message: String,
    /// The problems, one per argument.
    pub issues: Vec<ArgIssue>,
}

impl From<ArgFailure> for WireError {
    fn from(failure: ArgFailure) -> WireError {
        let listed: Vec<Value> = failure
            .issues
            .iter()
            .map(|issue| serde_json::json!({ "path": issue.path, "message": issue.message }))
            .collect();
        WireError::new(ErrorKind::InvalidInput, failure.message)
            .with_detail("issues", Value::Array(listed))
    }
}

/// Compiles the validator for one normalised schema.
///
/// Fails for a schema `jsonschema` cannot compile — a malformed keyword, or a
/// reference outside the document.
pub fn compile_validator(tool_name: &str, parameters: &Object) -> Result<ArgValidator> {
    let value = serde_json::to_value(parameters).map_err(|error| {
        unusable(
            tool_name,
            format!("Tool {tool_name} advertises an input schema that is not JSON"),
        )
        .with_source(error)
    })?;
    let validator = jsonschema::options()
        .with_retriever(RefuseRemote)
        .build(&value)
        .map_err(|error| {
            unusable(
                tool_name,
                format!("Tool {tool_name} advertises a schema this client cannot compile: {error}"),
            )
        })?;
    let properties = match parameters.get("properties") {
        Some(Value::Object(properties)) => properties.clone(),
        _ => Map::new(),
    };
    Ok(ArgValidator {
        tool_name: tool_name.to_owned(),
        properties,
        validator,
    })
}

impl ArgValidator {
    /// Validates the model's raw arguments.
    ///
    /// A model calling a no-argument tool emits `""`, `"{}"` or nothing at all,
    /// which arrives as `None` or `null`. The schema's own `required` list is
    /// the right thing to judge it, so both become an empty object.
    pub fn parse(&self, raw: Option<Value>) -> std::result::Result<Object, ArgFailure> {
        let candidate = match raw {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(value) => value,
        };
        let Value::Object(mut fields) = candidate else {
            let received = match &candidate {
                Value::Array(_) => "an array".to_owned(),
                other => format!("a {}", type_name(other)),
            };
            return Err(self.fail(vec![ArgIssue {
                path: String::new(),
                message: format!("expected an object of arguments, received {received}"),
            }]));
        };

        for (name, schema) in &self.properties {
            let types = declared_types(schema);
            if let Some(value) = fields.get(name)
                && let Some(coerced) = coerce(&types, value)
            {
                fields.insert(name.clone(), coerced);
            }
        }

        let candidate = Value::Object(fields);
        let issues: Vec<ArgIssue> = self
            .validator
            .iter_errors(&candidate)
            .flat_map(|error| self.describe(&error))
            .collect();
        if !issues.is_empty() {
            return Err(self.fail(issues));
        }

        match candidate {
            Value::Object(fields) => Ok(fields.into_iter().collect()),
            // Unreachable by construction; an empty object is the honest answer
            // rather than a panic.
            _ => Ok(Object::new()),
        }
    }

    /// One validator error as issues in this repo's wording.
    fn describe(&self, error: &jsonschema::ValidationError<'_>) -> Vec<ArgIssue> {
        let path = dotted(error.instance_path());
        match error.kind() {
            ValidationErrorKind::Required { property } => vec![ArgIssue {
                path: join(&path, property.as_str().unwrap_or_default()),
                message: "is required".to_owned(),
            }],
            ValidationErrorKind::AdditionalProperties { unexpected } => unexpected
                .iter()
                .map(|name| ArgIssue {
                    path: join(&path, name),
                    message: "is not an argument of this tool".to_owned(),
                })
                .collect(),
            ValidationErrorKind::Enum { options } => {
                let listed = options.as_array().map_or_else(
                    || options.to_string(),
                    |entries| {
                        entries
                            .iter()
                            .map(Value::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                );
                vec![ArgIssue {
                    path,
                    message: format!("must be one of {listed}"),
                }]
            }
            ValidationErrorKind::Type { .. } => {
                let declared = path
                    .split('.')
                    .next()
                    .and_then(|name| self.properties.get(name))
                    .map(declared_types)
                    .unwrap_or_default();
                let expected = if declared.is_empty() {
                    error.to_string()
                } else {
                    format!("expected {}", declared.join(" or "))
                };
                vec![ArgIssue {
                    path,
                    message: format!("{expected}, received {}", type_name(error.instance())),
                }]
            }
            _ => vec![ArgIssue {
                path,
                message: error.to_string(),
            }],
        }
    }

    fn fail(&self, issues: Vec<ArgIssue>) -> ArgFailure {
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
        ArgFailure {
            message: format!("Invalid arguments for {}: {detail}", self.tool_name),
            issues,
        }
    }
}

/// A JSON pointer as a dotted path: `/a/b` becomes `a.b`.
fn dotted(location: &jsonschema::paths::Location) -> String {
    location
        .as_str()
        .trim_start_matches('/')
        .replace('/', ".")
        .replace("~1", "/")
        .replace("~0", "~")
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}.{name}")
    }
}
