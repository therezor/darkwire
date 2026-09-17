//! One `tools.mcpServers.<id>` entry, resolved into something connectable.
//!
//! The config schema is deliberately permissive — every field has a default,
//! and `type` is optional because the shape of the entry already implies it.
//! This is where that permissiveness is turned into a decision, once, so that
//! nothing downstream has to ask "is this a stdio server?" by looking at
//! whether `command` happens to be empty.
//!
//! Two rules are worth stating because they are choices rather than
//! derivations:
//!
//! - **`sse` is never inferred.** The legacy transport is deprecated upstream
//!   and Streamable HTTP serves the same URL shape, so an entry with a `url`
//!   and no `type` gets the transport it almost certainly wants. Reaching the
//!   legacy one is an explicit `"type": "sse"`.
//! - **A URL is checked, not guarded.** See [`parse_url`].
//!
//! ## Why neither guard applies
//!
//! A stdio `command` does **not** go through the exec guard, and a `url` does
//! **not** go through the SSRF-guarded fetch. Both guards exist to constrain
//! what a *model* chose: the exec guard refuses absolute paths and shell
//! binaries and classifies every path-shaped argument against the workspace
//! jail; the fetch guard refuses loopback and private ranges. An MCP entry is
//! operator configuration in `config.yaml`, in the same trust class as
//! `providers.<id>.apiBase` or a container image. Its command is almost always
//! `npx`, `uvx`, `docker` or an absolute path to a binary outside the
//! workspace on purpose, and the single most common MCP deployment is a server
//! on loopback — every one of which the guards refuse by design. Running them
//! through would make the feature unusable while protecting against nothing:
//! anyone able to edit `config.yaml` can already add a binary to an agent's
//! `exec.allowedBinaries`. The model's reach stops at a bridged tool's
//! *arguments*, which are JSON over a pipe and never become argv.
//!
//! The OAuth endpoints are the exception, and they are guarded — see
//! [`crate::oauth`]. Discovery can move the token exchange to a host the
//! operator never typed, and what is being handed to it is a credential.

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{McpOAuthConfig, McpServerConfig, McpTransport};
use indexmap::IndexMap;
use reqwest::Url;
use serde_json::json;

/// How a resolved server is reached, with nothing left to infer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransportSpec {
    /// A child process speaking JSON-RPC over its pipes.
    Stdio {
        /// The program. Never a shell.
        command: String,
        /// Its arguments.
        args: Vec<String>,
        /// Added to a minimal inherited environment; never the whole of ours.
        env: IndexMap<String, String>,
    },
    /// An HTTP endpoint, over either HTTP transport.
    Http {
        /// `StreamableHttp` unless the entry said `sse` by name.
        kind: McpTransport,
        /// The endpoint, normalised.
        url: String,
        /// Sent with every request, including the event stream's own.
        headers: IndexMap<String, String>,
        /// OAuth, when the server needs it.
        oauth: Option<McpOAuthConfig>,
    },
}

/// One entry, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConnectionSpec {
    /// The config key.
    pub server_id: String,
    /// Which process or endpoint this is.
    pub transport: McpTransportSpec,
    /// Per-call cap in milliseconds. `0` disables it.
    pub tool_timeout_ms: u64,
    /// Which upstream tools this install advertises.
    pub enabled_tools: Vec<String>,
}

impl McpConnectionSpec {
    /// The transport, as the status row reports it.
    pub fn kind(&self) -> McpTransport {
        match &self.transport {
            McpTransportSpec::Stdio { .. } => McpTransport::Stdio,
            McpTransportSpec::Http { kind, .. } => *kind,
        }
    }

    /// The OAuth block, for an HTTP server that carries one.
    pub fn oauth(&self) -> Option<&McpOAuthConfig> {
        match &self.transport {
            McpTransportSpec::Http { oauth, .. } => oauth.as_ref(),
            McpTransportSpec::Stdio { .. } => None,
        }
    }

    /// The endpoint, for an HTTP server.
    pub fn url(&self) -> Option<&str> {
        match &self.transport {
            McpTransportSpec::Http { url, .. } => Some(url),
            McpTransportSpec::Stdio { .. } => None,
        }
    }
}

fn refuse(server_id: &str, message: &str) -> WireError {
    WireError::new(
        ErrorKind::Config,
        format!("MCP server \"{server_id}\": {message}"),
    )
    .with_detail("server", server_id)
}

/// Whether a URL is one this client will dial: a scheme this client speaks,
/// and a URL that parses. The same check `providers.<id>.apiBase` gets, for
/// the reason the module docs give.
fn parse_url(server_id: &str, url: &str) -> Result<Url> {
    let parsed = Url::parse(url).map_err(|_| {
        refuse(server_id, &format!("\"{url}\" is not a URL")).with_detail("url", url)
    })?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(refuse(
            server_id,
            &format!(
                "only http and https URLs are supported, not \"{}:\"",
                parsed.scheme()
            ),
        )
        .with_detail("url", url));
    }
    Ok(parsed)
}

/// The transport an entry names, or the one its shape implies.
///
/// An explicit `type` is still cross-checked against the fields it needs, so
/// `{"type": "stdio"}` with no `command` fails here rather than as an
/// unexplained spawn error a minute later.
pub fn resolve_spec(server_id: &str, config: &McpServerConfig) -> Result<McpConnectionSpec> {
    let command = config.command.trim();
    let url = config.url.trim();

    let kind = config.kind.or(if !command.is_empty() {
        Some(McpTransport::Stdio)
    } else if !url.is_empty() {
        Some(McpTransport::StreamableHttp)
    } else {
        None
    });

    let Some(kind) = kind else {
        return Err(refuse(server_id, "names neither a command nor a url"));
    };
    if !command.is_empty() && !url.is_empty() {
        return Err(refuse(
            server_id,
            "names both a command and a url; a server is one or the other",
        ));
    }

    let transport = if kind == McpTransport::Stdio {
        if command.is_empty() {
            return Err(refuse(server_id, "is a stdio server with no command"));
        }
        McpTransportSpec::Stdio {
            command: command.to_owned(),
            args: config.args.clone(),
            env: config.env.clone(),
        }
    } else {
        if url.is_empty() {
            let name = match kind {
                McpTransport::Sse => "sse",
                _ => "streamableHttp",
            };
            return Err(refuse(
                server_id,
                &format!("is a {name} server with no url"),
            ));
        }
        McpTransportSpec::Http {
            kind,
            url: parse_url(server_id, url)?.to_string(),
            headers: config.headers.clone(),
            oauth: config.oauth.clone(),
        }
    };

    Ok(McpConnectionSpec {
        server_id: server_id.to_owned(),
        transport,
        tool_timeout_ms: config.tool_timeout_ms,
        enabled_tools: config.enabled_tools.clone(),
    })
}

/// Everything that decides *which process or endpoint* this is.
///
/// Changing any of it has to bounce the connection. Kept apart from
/// [`exposure_fingerprint`] so that narrowing `enabledTools` — the edit an
/// operator makes most — does not kill and respawn a subprocess to re-filter a
/// list this client already holds.
pub fn transport_fingerprint(spec: &McpConnectionSpec) -> String {
    let value = match &spec.transport {
        McpTransportSpec::Stdio { command, args, env } => json!(["stdio", command, args, env]),
        McpTransportSpec::Http {
            kind,
            url,
            headers,
            oauth,
        } => json!([kind, url, headers, oauth]),
    };
    value.to_string()
}

/// Everything that decides which of a live server's tools reach the registry.
pub fn exposure_fingerprint(spec: &McpConnectionSpec) -> String {
    json!([spec.enabled_tools, spec.tool_timeout_ms]).to_string()
}
