//! A remote tool descriptor, as a `Tool` the registry can hold.
//!
//! Shared with the extension host: one bridge, two name prefixes. The whole of
//! the MCP-specific reasoning is in three decisions:
//!
//! - **The `Tool` trait is implemented directly, not via the typed adapter.**
//!   See [`crate::schema`] for why the JSON Schema is not round-tripped
//!   through a struct.
//! - **A band, when the server says nothing, is not `safe`.** An MCP server is
//!   third-party code reached over a socket. `readOnlyHint: true` is the
//!   server saying it only reads, and that is the one claim worth taking at
//!   face value; silence is not the same claim. Bands are advisory — nothing
//!   reads one at call time — so this only has to be honest.
//! - **A remote failure is a result, never an `Err`.** `isError` on the wire
//!   becomes a flagged execution, so the model reads what went wrong and
//!   adapts instead of the turn dying; a transport failure becomes an
//!   execution with a `kind`, for the same reason.
//!
//! What is *not* here, because it already happens elsewhere and would be wrong
//! to repeat: truncation and prompt-injection fencing. The registry applies
//! both to every result, and the nonce module's header names the MCP server as
//! one of the parties whose output it is fencing.

use std::sync::Arc;

use darkwire_core::{Result, WireError};
use darkwire_protocol::json::Object;
use darkwire_protocol::{ToolDefinition, ToolRisk, ToolSource};
use darkwire_tools::{AnyTool, Tool, ToolContext, ToolExecution, assert_not_aborted};
use futures::future::BoxFuture;
use serde_json::Value;

use crate::names::flatten_tool_name;
use crate::schema::{ArgValidator, SchemaIssue, compile_validator, normalise_schema};
use crate::session::{McpCallOptions, McpCallResult, McpSession, McpToolDescriptor};

/// Where a bridged tool's calls go.
///
/// A session implements it directly; a connection implements it with a lookup
/// of whichever session it holds *now*, so a tool bridged before a reconnect
/// still reaches the server after one.
pub trait McpCallTarget: Send + Sync {
    /// One `tools/call` by upstream name.
    fn call(
        &self,
        upstream_name: &str,
        args: Object,
        options: McpCallOptions,
    ) -> BoxFuture<'_, Result<McpCallResult>>;
}

impl McpCallTarget for Arc<dyn McpSession> {
    fn call(
        &self,
        upstream_name: &str,
        args: Object,
        options: McpCallOptions,
    ) -> BoxFuture<'_, Result<McpCallResult>> {
        self.call_tool(upstream_name, args, options)
    }
}

/// How one descriptor becomes a tool.
#[derive(Debug, Clone)]
pub struct BridgeOptions {
    /// Whose tool this is: the MCP server id or the extension id.
    pub owner_id: String,
    /// Already flattened and deduplicated by [`crate::names`].
    pub advertised_name: String,
    /// Per-call cap in milliseconds. `0` disables it.
    pub tool_timeout_ms: u64,
    /// What the definition reports; the registry stamps its own on register.
    pub source: ToolSource,
}

impl BridgeOptions {
    /// Options for `descriptor` under `prefix`: `{prefix}_{owner}_{tool}`,
    /// no per-call cap, source `mcp`.
    pub fn new(prefix: &str, owner_id: &str, descriptor: &McpToolDescriptor) -> BridgeOptions {
        BridgeOptions {
            owner_id: owner_id.to_owned(),
            advertised_name: flatten_tool_name(prefix, owner_id, &descriptor.name),
            tool_timeout_ms: 0,
            source: ToolSource::Mcp,
        }
    }

    /// Replaces the advertised name, for a caller that broke collisions.
    #[must_use]
    pub fn advertised_as(mut self, name: impl Into<String>) -> BridgeOptions {
        self.advertised_name = name.into();
        self
    }

    /// Sets the per-call cap.
    #[must_use]
    pub fn timeout_ms(mut self, tool_timeout_ms: u64) -> BridgeOptions {
        self.tool_timeout_ms = tool_timeout_ms;
        self
    }

    /// Sets the source the definition reports.
    #[must_use]
    pub fn source(mut self, source: ToolSource) -> BridgeOptions {
        self.source = source;
        self
    }
}

/// One descriptor bridged, or the reason it could not be.
pub struct BridgedTool {
    /// Absent when the schema could not be advertised; `issues` says why.
    pub tool: Option<AnyTool>,
    /// The name the server knows the tool by.
    pub upstream_name: String,
    /// Sloppiness worth a warning beside the server, or the reason for
    /// refusal when `tool` is absent.
    pub issues: Vec<SchemaIssue>,
}

impl std::fmt::Debug for BridgedTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgedTool")
            .field("bridged", &self.tool.is_some())
            .field("upstream_name", &self.upstream_name)
            .field("issues", &self.issues)
            .finish()
    }
}

/// What band a tool gets, from what the server was willing to claim.
///
/// `destructiveHint` is the server volunteering that a call is not undoable, so
/// it earns the band an operator most wants to see before it happens.
/// Everything else that is not explicitly read-only is `network`, which is
/// true of every MCP call by construction.
fn risk_of(descriptor: &McpToolDescriptor) -> ToolRisk {
    let hints = descriptor.annotations.as_ref();
    if hints.and_then(|h| h.read_only_hint) == Some(true) {
        return ToolRisk::Safe;
    }
    if hints.and_then(|h| h.destructive_hint) == Some(true) {
        return ToolRisk::Exec;
    }
    ToolRisk::Network
}

/// The sentence that decides whether the model reaches for this tool.
///
/// A description is required of every tool in this repo, and a server is
/// allowed to omit one — so there is a fallback, and it says the two things a
/// model can still act on: what the tool is called upstream, and whose it is.
fn describe(descriptor: &McpToolDescriptor, options: &BridgeOptions) -> String {
    if let Some(own) = descriptor
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return own.to_owned();
    }
    if let Some(title) = descriptor
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return title.to_owned();
    }
    let whose = match options.source {
        ToolSource::Extension => format!("the {} extension", options.owner_id),
        _ => format!("the {} MCP server", options.owner_id),
    };
    format!("{}, from {whose}.", descriptor.name)
}

fn describe_binary(part: &crate::session::McpContentPart) -> String {
    let bytes = part.data.as_ref().map_or(0, |data| data.len() * 3 / 4);
    let kind = part.mime_type.as_deref().unwrap_or(&part.kind);
    format!("[{kind}, {bytes} bytes — not shown]")
}

/// Every part of a result as one string.
///
/// Binary parts become a placeholder rather than their base64: a model reading
/// a megabyte of base64 learns nothing from it and the bytes would consume the
/// whole output budget, evicting the text parts that do say something.
pub fn flatten_content(result: &McpCallResult) -> String {
    let mut lines = Vec::with_capacity(result.content.len());
    for part in &result.content {
        match part.kind.as_str() {
            "text" => lines.push(part.text.clone().unwrap_or_default()),
            "image" | "audio" => lines.push(describe_binary(part)),
            "resource" => {
                let embedded = part.resource.as_ref();
                lines.push(
                    embedded
                        .and_then(|r| r.text.clone())
                        .or_else(|| embedded.and_then(|r| r.uri.clone()))
                        .unwrap_or_else(|| "[resource — not shown]".to_owned()),
                );
            }
            "resource_link" => lines.push(
                part.uri
                    .clone()
                    .unwrap_or_else(|| "[resource link]".to_owned()),
            ),
            other => lines.push(format!("[{other} — not shown]")),
        }
    }
    if !lines.is_empty() {
        return lines.join("\n");
    }
    // A server answering with structured output and no content blocks is legal.
    // Showing the model nothing at all would look like a tool that silently did
    // nothing, which is the one outcome it cannot recover from.
    match &result.structured_content {
        Some(structured) => serde_json::to_string_pretty(structured).unwrap_or_default(),
        None => String::new(),
    }
}

struct RemoteTool {
    definition: ToolDefinition,
    validator: ArgValidator,
    upstream_name: String,
    tool_timeout_ms: u64,
    target: Arc<dyn McpCallTarget>,
}

impl Tool for RemoteTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn risk(&self) -> ToolRisk {
        self.definition.risk
    }

    fn execute<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolExecution> {
        Box::pin(async move {
            // Before validation, so a turn cancelled while this call was queued
            // unwinds as a cancellation rather than as an argument error.
            if let Err(aborted) = assert_not_aborted(&ctx.token, &self.definition.name) {
                return aborted.into();
            }
            let parsed = match self.validator.parse(Some(args)) {
                Ok(parsed) => parsed,
                Err(failure) => {
                    return WireError::from(failure)
                        .with_detail("tool", self.definition.name.as_str())
                        .into();
                }
            };
            let options = McpCallOptions {
                token: ctx.token.clone(),
                timeout_ms: self.tool_timeout_ms,
            };
            match self.target.call(&self.upstream_name, parsed, options).await {
                Err(error) => error.into(),
                Ok(result) => {
                    let content = flatten_content(&result);
                    let execution = if result.is_error == Some(true) {
                        ToolExecution::flagged(content)
                    } else {
                        ToolExecution::ok(content)
                    };
                    // Audit context, never shown to the model — which is the
                    // right home for `structuredContent`: it is the
                    // machine-readable twin of the text above and duplicating
                    // it into the prompt would double the cost of every call.
                    match result.structured_content {
                        Some(structured) => execution.with_detail("structuredContent", structured),
                        None => execution,
                    }
                }
            }
        })
    }
}

/// One descriptor as a tool, or the reason it cannot be one.
///
/// A schema this client cannot advertise drops that tool and leaves the rest of
/// the server working — the alternative, refusing the whole server because one
/// of its forty tools has a malformed schema, is a worse trade in every case.
pub fn bridge_tool(
    descriptor: &McpToolDescriptor,
    session: Arc<dyn McpCallTarget>,
    options: BridgeOptions,
) -> BridgedTool {
    let refused = |error: WireError| BridgedTool {
        tool: None,
        upstream_name: descriptor.name.clone(),
        issues: vec![SchemaIssue {
            tool: descriptor.name.clone(),
            message: error.message,
        }],
    };

    let normalised = match normalise_schema(&descriptor.name, &descriptor.input_schema) {
        Ok(normalised) => normalised,
        Err(error) => return refused(error),
    };
    let validator = match compile_validator(&options.advertised_name, &normalised.parameters) {
        Ok(validator) => validator,
        Err(error) => return refused(error),
    };
    let description = describe(descriptor, &options);
    let BridgeOptions {
        advertised_name,
        tool_timeout_ms,
        source,
        ..
    } = options;

    let definition = ToolDefinition {
        name: advertised_name,
        description,
        parameters: normalised.parameters,
        risk: risk_of(descriptor),
        source,
        annotations: descriptor.annotations.clone(),
    };

    BridgedTool {
        tool: Some(Arc::new(RemoteTool {
            definition,
            validator,
            upstream_name: descriptor.name.clone(),
            tool_timeout_ms,
            target: session,
        })),
        upstream_name: descriptor.name.clone(),
        issues: normalised.issues,
    }
}
