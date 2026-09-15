//! Toolboxes: resolving a grant list, and the constraints on an operation.
//!
//! A toolbox names reusable operation definitions rather than carrying them, so
//! one reviewed `git-status` is shared by every toolbox that grants it. That
//! makes the identity of a toolbox wider than one file: [`resolve_bundle`] reads
//! the toolbox and each definition it names **once**, hashes those same bytes
//! together, and returns the values it parsed from them. Hashing the bundle is
//! what makes editing a shared definition move the digest of every toolbox that
//! reaches it; hashing the bytes it actually parsed is what stops a definition
//! being read twice and changing in between.
//!
//! The constraints on an operation are all about the same thing: an operation
//! is a fixed program and a reviewed argument mapping, never a shell string.
//! `/usr/bin/git` with `["diff", {input: "path", workspacePath: true}]` is an
//! operation; `git diff $PATH` is not expressible, because there is nowhere to
//! put it. So the checks below are not a filter over dangerous commands — there
//! is no command to filter — they are what keeps the argv shape honest:
//!
//!  - **The schema must be self-contained.** A `$ref` would make validation
//!    fetch something, at resolution time and again at call time, and the two
//!    could differ.
//!  - **The executable is absolute and outside the workspace.** A relative path
//!    resolves against a working directory nobody stated, and one inside the
//!    workspace is a file `write_file` can replace between resolution and call.
//!  - **Every argv input is a required scalar.** Optional means the argv has a
//!    hole at a position the operator counted on being filled; a non-scalar
//!    means one input becomes several arguments, which is the shell-injection
//!    shape wearing a JSON hat.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ghostai_core::{ErrorKind, GhostError, Result};
use ghostai_protocol::toolbox::{
    OperationArgument, OperationImplementation, ToolOperation, Toolbox,
};
use ghostai_protocol::{ContainerNetwork, NetworkMode, ToolPermission};
use serde_json::Value;

use crate::container::{manifest_hash, parse_manifest};
use crate::ip::parse_cidr;
use crate::{WorkspaceJail, parse_ip_literal};

/// Construct an operator-actionable policy error.
pub fn invalid(message: impl Into<String>) -> GhostError {
    GhostError::new(ErrorKind::Config, message)
}

/// Ensure a reference names a file within the operator policy directory.
///
/// The character class is the whole check: no separator, no dot, so no
/// reference can leave the directory it is resolved in. A name that fails this
/// never reaches the filesystem at all.
pub fn assert_slug(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || name.starts_with('-')
    {
        return Err(invalid(format!("Invalid policy reference: {name}")));
    }
    Ok(())
}

/// Parses toolbox bytes, with the schema's own errors turned into a sentence.
pub fn parse_toolbox(bytes: &[u8]) -> Result<Toolbox> {
    parse_manifest(bytes, "Toolbox manifest")
}

/// A toolbox and every operation it grants, resolved together.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedToolbox {
    /// The manifest.
    pub toolbox: Toolbox,
    /// Each grant's name mapped to the definition it resolved to.
    pub operations: BTreeMap<String, ToolOperation>,
    /// The digest over the toolbox and every definition it names.
    pub digest: String,
}

fn read_dependency(root: &Path, folder: &str, name: &str, bundle: &mut Vec<u8>) -> Result<Vec<u8>> {
    assert_slug(name)?;
    let path = root.join(folder).join(format!("{name}.yaml"));
    let bytes = std::fs::read(&path)
        .map_err(|e| invalid(format!("Cannot read {}: {e}", path.display())))?;
    // Length framing prevents ambiguous concatenations across dependencies: two
    // definitions whose bytes could be split differently must not hash alike.
    bundle.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    bundle.extend_from_slice(&bytes);
    Ok(bytes)
}

/// Read every dependency once, then hash and return those same bytes.
pub fn resolve_bundle(root: &Path, bytes: &[u8]) -> Result<ResolvedToolbox> {
    let toolbox = parse_toolbox(bytes)?;
    assert_slug(&toolbox.name)?;
    let mut bundle = (bytes.len() as u64).to_be_bytes().to_vec();
    bundle.extend_from_slice(bytes);
    let mut operations = BTreeMap::new();
    for grant in &toolbox.tools {
        let bytes = read_dependency(root, "tool-definitions", &grant.definition, &mut bundle)?;
        let operation: ToolOperation = serde_yaml_ng::from_slice(&bytes)
            .map_err(|e| invalid(format!("{}: {e}", grant.definition)))?;
        validate_operation(&operation)?;
        if operations.insert(grant.name.clone(), operation).is_some() {
            return Err(invalid(format!("Duplicate tool grant: {}", grant.name)));
        }
    }
    Ok(ResolvedToolbox {
        toolbox,
        operations,
        digest: manifest_hash(&bundle),
    })
}

/// Remote references are forbidden: validation must never fetch schemas.
fn reject_references(value: &Value) -> Result<()> {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(key.as_str(), "$ref" | "$dynamicRef" | "$recursiveRef") {
                    return Err(invalid(
                        "Operation schemas must be self-contained (no references)",
                    ));
                }
                reject_references(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                reject_references(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Validate an operation's schema and argument mapping before it is granted.
pub fn validate_operation(operation: &ToolOperation) -> Result<()> {
    reject_references(&operation.parameters)?;
    if operation.parameters.get("type") != Some(&Value::from("object"))
        || operation.parameters.get("additionalProperties") != Some(&Value::from(false))
    {
        return Err(invalid(
            "Operation parameters must be an object with additionalProperties:false",
        ));
    }
    jsonschema::validator_for(&operation.parameters).map_err(|e| invalid(e.to_string()))?;
    let OperationImplementation::Command {
        executable,
        argv,
        argv_input,
    } = &operation.implementation
    else {
        return Ok(());
    };
    if !executable.starts_with('/')
        || executable.contains('\0')
        || executable.split('/').any(|p| p == "..")
    {
        return Err(invalid(
            "Operation executables must be absolute operator-controlled paths",
        ));
    }
    if executable == "/workspace" || executable.starts_with("/workspace/") {
        return Err(invalid(
            "Operation executables cannot come from the writable workspace",
        ));
    }
    let properties = operation
        .parameters
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("Operation needs properties"))?;
    let required: BTreeSet<&str> = operation
        .parameters
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    for argument in argv {
        match argument {
            OperationArgument::Literal(value) if value.contains('\0') => {
                return Err(invalid("NUL in fixed argument"));
            }
            OperationArgument::Input(input) => {
                let property = properties
                    .get(&input.input)
                    .ok_or_else(|| invalid("Unknown argv input"))?;
                if !required.contains(input.input.as_str())
                    || !matches!(
                        property.get("type").and_then(Value::as_str),
                        Some("string" | "integer" | "number" | "boolean")
                    )
                {
                    return Err(invalid("argv inputs must be required scalar properties"));
                }
                if input.workspace_path && property.get("type") != Some(&Value::from("string")) {
                    return Err(invalid("Workspace paths must be strings"));
                }
            }
            OperationArgument::Literal(_) => {}
        }
    }
    if let Some(input) = argv_input {
        let property = properties
            .get(input)
            .ok_or_else(|| invalid("Unknown argvInput"))?;
        if !required.contains(input.as_str())
            || property.get("type") != Some(&Value::from("array"))
            || property.pointer("/items/type") != Some(&Value::from("string"))
        {
            return Err(invalid("argvInput must be a required array of strings"));
        }
    }
    Ok(())
}

/// Validate one call against the exact granted input schema.
pub fn validate_input(operation: &ToolOperation, input: &Value) -> Result<()> {
    let validator =
        jsonschema::validator_for(&operation.parameters).map_err(|e| invalid(e.to_string()))?;
    validator
        .validate(input)
        .map_err(|e| GhostError::new(ErrorKind::InvalidInput, e.to_string()))
}

/// Build argv using constants and scalar values; no shell string is built.
pub fn command_argv(
    operation: &ToolOperation,
    input: &Value,
    jail: &WorkspaceJail,
) -> Result<Vec<String>> {
    validate_input(operation, input)?;
    let OperationImplementation::Command {
        executable,
        argv,
        argv_input,
    } = &operation.implementation
    else {
        return Err(invalid("Not a command operation"));
    };
    if jail.contains(Path::new(executable)) {
        return Err(invalid("Executable is inside the writable workspace"));
    }
    let mut command = vec![executable.clone()];
    for argument in argv {
        command.push(match argument {
            OperationArgument::Literal(value) => value.clone(),
            OperationArgument::Input(field) => {
                let value = &input[&field.input];
                if field.workspace_path {
                    jail.resolve(value.as_str().ok_or_else(|| invalid("Invalid path"))?)?
                        .to_string_lossy()
                        .into_owned()
                } else {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                }
            }
        });
    }
    if let Some(field) = argv_input {
        command.extend(
            input[field]
                .as_array()
                .ok_or_else(|| invalid("Invalid argv"))?
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_owned()),
        );
    }
    if command.iter().any(|v| v.contains('\0')) {
        return Err(invalid("NUL in command argument"));
    }
    Ok(command)
}

/// Intersect a requested permission with an immutable grant ceiling.
pub fn narrow_permission(ceiling: ToolPermission, requested: ToolPermission) -> ToolPermission {
    match (ceiling, requested) {
        (ToolPermission::Deny, _) | (_, ToolPermission::Deny) => ToolPermission::Deny,
        (ToolPermission::Ask, _) | (_, ToolPermission::Ask) => ToolPermission::Ask,
        _ => ToolPermission::Allow,
    }
}

/// Refuses an egress request nothing could enforce.
///
/// Raised where the agent is resolved rather than clamped when a container
/// starts. Silently narrowing would leave the config saying one thing while the
/// container did another, and the operator who wrote it would have no way to
/// discover it.
pub fn assert_container_network(network: &ContainerNetwork, agent_id: &str) -> Result<()> {
    if network.mode != NetworkMode::Allowlist {
        if !network.allow.is_empty() || !network.hosts.is_empty() || !network.dns.is_empty() {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Agent \"{agent_id}\" lists egress entries but its network mode is not \"allowlist\".\n  They would have no effect. Set the mode, or clear the entries."
                ),
            )
            .with_detail("agentId", agent_id));
        }
        return Ok(());
    }
    if network.allow.is_empty() && network.hosts.is_empty() {
        return Err(GhostError::new(
            ErrorKind::Config,
            format!(
                "Agent \"{agent_id}\" asks for an allow-list with no entries, which reaches nothing.\n  Use mode \"none\" if that is the intent."
            ),
        )
        .with_detail("agentId", agent_id));
    }
    if !network.allow.is_empty() && !network.hosts.is_empty() {
        return Err(GhostError::new(
            ErrorKind::Config,
            format!(
                "Agent \"{agent_id}\" lists both CIDRs and hosts. Choose one.\n  CIDRs are enforced by the gateway's packet filter and hosts by the egress\n  proxy, and a request enforced in two places is enforced in neither."
            ),
        )
        .with_detail("agentId", agent_id));
    }
    for entry in &network.allow {
        if parse_cidr(entry).is_none() {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Agent \"{agent_id}\" has an egress entry that is not a CIDR block: {entry}\n  Hostnames go in `hosts`, where the proxy sees the name rather than an address\n  DNS rebinding chose. Use 10.0.0.0/8 here."
                ),
            )
            .with_detail("agentId", agent_id)
            .with_detail("entry", entry.as_str()));
        }
    }
    for host in &network.hosts {
        if host.is_empty()
            || host.len() > 253
            || host.starts_with('.')
            || host.ends_with('.')
            || !host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
        {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Agent \"{agent_id}\" has an egress host that is not an exact DNS name: {host}\n  Wildcards are not accepted: the proxy matches the name it was given."
                ),
            )
            .with_detail("agentId", agent_id)
            .with_detail("host", host.as_str()));
        }
    }
    for resolver in &network.dns {
        if parse_ip_literal(resolver).is_none() {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Agent \"{agent_id}\" has a DNS resolver that is not an IP literal: {resolver}\n  A name cannot be resolved by something that has to be resolved first."
                ),
            )
            .with_detail("agentId", agent_id)
            .with_detail("resolver", resolver.as_str()));
        }
    }
    if !network.allow.is_empty() && network.dns.is_empty() {
        return Err(GhostError::new(
            ErrorKind::Config,
            format!(
                "Agent \"{agent_id}\" scopes egress by CIDR but names no DNS resolver.\n  Nothing in the container could resolve a hostname, so every name would fail.\n  Add a resolver reachable inside the allow-list, or scope by `hosts` instead."
            ),
        )
        .with_detail("agentId", agent_id));
    }
    Ok(())
}
