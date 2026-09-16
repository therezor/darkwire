//! Container definitions: parsing, policy, and what an egress gateway needs.
//!
//! The definition file *is* the policy: installing it is the decision, the same
//! way writing `config.yaml` is. The hash taken over its bytes is identity, not
//! consent — two definitions that differ never share a warm instance, and one
//! edited under a running command cancels it. What keeps the file trustworthy
//! is that the policy directory sits outside the workspace jail, so nothing a
//! tool can write reaches it. That is worth stating because the alternative
//! looks stronger and is not: an Ed25519 signature answers *who authored this
//! policy*, which matters when a definition arrives from somewhere else and
//! proves nothing when the key sits on the same disk as the file it signs.
//!
//! Two refusals here are absolute, and the difference between them is the
//! design:
//!
//!  - **An image must be digest-pinned.** A tag is a mutable pointer, so a
//!    container installed once and then repointed runs code nobody chose while
//!    every hash still matches. Nothing in the definition would show it.
//!  - **`NET_ADMIN` is never grantable.** The egress gateway's rules live in a
//!    network namespace the container *shares*; a container holding
//!    `NET_ADMIN` can flush them. This is refused rather than surfaced because
//!    it breaks an invariant the rest of the system relies on, and no operator
//!    reviewing a definition could be expected to reconstruct that.
//!
//! `seccomp: unconfined` is deliberately *not* in that list. It is genuinely
//! risky and genuinely required for rootless builds inside a container, so it
//! is surfaced when the definition is read and left to the operator. The rule of
//! thumb: refuse what silently breaks the machinery, surface what is merely
//! dangerous.
//!
//! A third group is refused only when it matters. A root uid, missing
//! `no_new_privileges` or a packet-forging capability are all legitimate for a
//! container with no network, and each of them defeats a restricted egress
//! gateway. So they are checked by [`assert_gateway_compatible`], which runs
//! when an agent asks that container for an allow-list, rather than at install
//! — and the container list reports the sentence in advance, so the choice is
//! visible before a save fails.

use std::sync::LazyLock;

use garde::Validate;
use ghostai_core::{ErrorKind, GhostError, Result};
pub use ghostai_protocol::BUILTIN_TOOL_NAMES;
use ghostai_protocol::environment::{ContainerRuntime, EnvironmentDefinition, SeccompProfile};
use ghostai_protocol::{EnvironmentNetwork, NetworkMode};
use regex::Regex;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::random::hex_lower;
use crate::{parse_cidr, parse_ip_literal};

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

/// The two immutable ways to name an image.
///
/// `name@sha256:<64 hex>` is a registry digest. A bare `sha256:<64 hex>` is a
/// local image **ID**, which is what `docker build` produces and what a
/// container built on this machine has to reference — there is no registry
/// digest until something is pushed. Both are content addresses, so both are as
/// unrepointable as the other; a tag is neither.
///
/// **Anchored at both ends deliberately.** A pattern anchored only at the end
/// accepts `-v/:/hostfs@sha256:<64 hex>`, and the image is pushed to the engine
/// as a bare argv token — so a definition could smuggle a flag past the check
/// and bind host root into the container. That is not currently exploitable,
/// because the token after the image happens to be one the engine rejects, but
/// it survives by accident of argument order rather than by design.
static IMAGE_DIGEST_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9][a-z0-9._\-/:]*@sha256:[0-9a-f]{64}$|^sha256:[0-9a-f]{64}$")
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// Capabilities a container may never request. See the module header.
const FORBIDDEN_CAPABILITIES: &[&str] = &["NET_ADMIN", "SYS_ADMIN", "SYS_MODULE"];

/// Capabilities that defeat a restricted egress gateway.
///
/// `NET_RAW` forges packets past a filter that matches on the socket's owner;
/// `SETUID` and `SETGID` reach the proxy's own uid, which the filter accepts
/// unconditionally.
const GATEWAY_INCOMPATIBLE_CAPABILITIES: &[&str] = &["NET_RAW", "SETUID", "SETGID"];

/// The sha256 of a manifest's exact bytes.
///
/// Over the bytes, never over a re-serialisation of the parsed object: a
/// definition that round-trips through a formatter gains and loses whitespace
/// and key order, and an identity keyed on that would move on a formatter
/// rather than on a change of meaning.
pub fn manifest_hash(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

/// Lowercase hex of the sha256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

/// Parses manifest bytes into `T`, with the schema's own errors turned into a
/// sentence that names the field. Shared with the extension manifest.
pub(crate) fn parse_manifest<T: DeserializeOwned + Validate<Context = ()>>(
    bytes: &[u8],
    what: &str,
) -> Result<T> {
    let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_slice(bytes).map_err(|error| {
        GhostError::new(ErrorKind::Config, format!("{what} is not valid YAML")).with_source(error)
    })?;
    let parsed: T = serde_path_to_error::deserialize(yaml).map_err(|error| {
        let path = error.path().to_string();
        let field = if path == "." { "(root)" } else { path.as_str() };
        GhostError::new(
            ErrorKind::Config,
            format!("{what} is not valid: {field}: {}", error.inner()),
        )
    })?;
    if let Err(report) = parsed.validate() {
        let detail = report
            .iter()
            .map(|(path, error)| {
                let path = path.to_string();
                let field = if path.is_empty() {
                    "(root)"
                } else {
                    path.as_str()
                };
                format!("{field}: {error}")
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(GhostError::new(
            ErrorKind::Config,
            format!("{what} is not valid: {detail}"),
        ));
    }
    Ok(parsed)
}

/// Parses definition bytes, with the schema's own errors turned into a
/// sentence.
pub fn parse_environment(bytes: &[u8]) -> Result<EnvironmentDefinition> {
    parse_manifest(bytes, "Container definition")
}

/// Refuses an egress request nothing could enforce.
///
/// Raised where the agent is resolved rather than clamped when a container
/// starts. Silently narrowing would leave the config saying one thing while the
/// container did another, and the operator who wrote it would have no way to
/// discover it.
pub fn assert_environment_network(network: &EnvironmentNetwork, agent_id: &str) -> Result<()> {
    if network.mode != NetworkMode::Allowlist {
        if !network.allow.is_empty() || !network.hosts.is_empty() || !network.dns.is_empty() {
            return Err(invalid(format!(
                "Agent \"{agent_id}\" lists egress entries but its network mode is not \"allowlist\".\n  They would have no effect. Set the mode, or clear the entries."
            ))
            .with_detail("agentId", agent_id));
        }
        return Ok(());
    }
    if network.allow.is_empty() && network.hosts.is_empty() {
        return Err(invalid(format!(
            "Agent \"{agent_id}\" asks for an allow-list with no entries, which reaches nothing.\n  Use mode \"none\" if that is the intent."
        ))
        .with_detail("agentId", agent_id));
    }
    if !network.allow.is_empty() && !network.hosts.is_empty() {
        return Err(invalid(format!(
            "Agent \"{agent_id}\" lists both CIDRs and hosts. Choose one.\n  CIDRs are enforced by the gateway's packet filter and hosts by the egress\n  proxy, and a request enforced in two places is enforced in neither."
        ))
        .with_detail("agentId", agent_id));
    }
    for entry in &network.allow {
        if parse_cidr(entry).is_none() {
            return Err(invalid(format!(
                "Agent \"{agent_id}\" has an egress entry that is not a CIDR block: {entry}\n  Hostnames go in `hosts`, where the proxy sees the name rather than an address\n  DNS rebinding chose. Use 10.0.0.0/8 here."
            ))
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
            return Err(invalid(format!(
                "Agent \"{agent_id}\" has an egress host that is not an exact DNS name: {host}\n  Wildcards are not accepted: the proxy matches the name it was given."
            ))
            .with_detail("agentId", agent_id)
            .with_detail("host", host.as_str()));
        }
    }
    for resolver in &network.dns {
        if parse_ip_literal(resolver).is_none() {
            return Err(invalid(format!(
                "Agent \"{agent_id}\" has a DNS resolver that is not an IP literal: {resolver}\n  A name cannot be resolved by something that has to be resolved first."
            ))
            .with_detail("agentId", agent_id)
            .with_detail("resolver", resolver.as_str()));
        }
    }
    if !network.allow.is_empty() && network.dns.is_empty() {
        return Err(invalid(format!(
            "Agent \"{agent_id}\" scopes egress by CIDR but names no DNS resolver.\n  Nothing in the container could resolve a hostname, so every name would fail.\n  Add a resolver reachable inside the allow-list, or scope by `hosts` instead."
        ))
        .with_detail("agentId", agent_id));
    }
    Ok(())
}

fn policy_error(environment: &EnvironmentDefinition, message: String) -> GhostError {
    GhostError::new(ErrorKind::Config, message)
        .with_detail("environment", environment.name.as_str())
}

/// Refuses a container the machinery cannot honour.
///
/// Separate from parsing because a definition can be perfectly well-formed and
/// still ask for something that would quietly disable a guarantee elsewhere.
pub fn assert_environment_policy(container: &EnvironmentDefinition) -> Result<()> {
    if !IMAGE_DIGEST_PATTERN.is_match(&container.image) {
        return Err(policy_error(
            container,
            format!(
                "Container \"{}\" must pin its image by digest, not by tag: {}\n  A tag can be repointed later, which would leave the recorded digest\n  matching an image nobody chose. Use name@sha256:<digest>.",
                container.name, container.image
            ),
        )
        .with_detail("image", container.image.as_str()));
    }

    for capability in &container.caps.add {
        let upper = capability.to_uppercase();
        let name = upper.strip_prefix("CAP_").unwrap_or(&upper);
        if FORBIDDEN_CAPABILITIES.contains(&name) {
            return Err(policy_error(
                container,
                format!(
                    "Container \"{}\" asks for {capability}, which is never granted.\n  The egress gateway's rules live in a namespace the container shares, and a\n  container holding NET_ADMIN could flush them.",
                    container.name
                ),
            )
            .with_detail("capability", capability.as_str()));
        }
    }

    if container.workdir == "/" || !container.workdir.starts_with('/') {
        return Err(policy_error(
            container,
            format!(
                "Container \"{}\" must mount the workspace at an absolute path other than \"/\".\n  Mounting it over the root would bury the image's own filesystem.",
                container.name
            ),
        )
        .with_detail("workdir", container.workdir.as_str()));
    }

    Ok(())
}

/// Whether a restricted egress gateway can be built around this container.
///
/// Not part of [`assert_environment_policy`], because every condition here is
/// legitimate for a container that reaches nothing: a root uid is how a
/// rootless builder works, and `NET_RAW` is what `nmap -sS` needs. They are
/// refused only when an agent asks *this* container for an allow-list, which is
/// the moment the gateway has to filter by the socket's owner and would be
/// filtering something that can rewrite itself.
pub fn assert_gateway_compatible(container: &EnvironmentDefinition) -> Result<()> {
    let uid = container.user.split(':').next().unwrap_or_default();
    if uid.is_empty() || uid == "0" || !uid.bytes().all(|c| c.is_ascii_digit()) {
        return Err(policy_error(
            container,
            format!(
                "Container \"{}\" runs as \"{}\", so a restricted allow-list cannot be enforced in it.\n  The gateway filters by the socket's owning uid, which needs a non-root numeric one.\n  Use network mode \"none\" or \"open\", or select a container with a numeric user.",
                container.name,
                if container.user.is_empty() {
                    "the image default"
                } else {
                    container.user.as_str()
                }
            ),
        ));
    }
    if uid == crate::egress::PROXY_UID {
        return Err(policy_error(
            container,
            format!(
                "Container \"{}\" runs as uid {}, which the egress proxy reserves for itself.\n  Traffic from it would be accepted unfiltered. Choose another uid.",
                container.name,
                crate::egress::PROXY_UID
            ),
        ));
    }
    if !container.security.no_new_privileges {
        return Err(policy_error(
            container,
            format!(
                "Container \"{}\" disables no-new-privileges, so a restricted allow-list cannot be enforced in it.\n  A process that can gain privileges can become the uid the gateway trusts.",
                container.name
            ),
        ));
    }
    for capability in &container.caps.add {
        let upper = capability.to_uppercase();
        let name = upper.strip_prefix("CAP_").unwrap_or(&upper);
        if GATEWAY_INCOMPATIBLE_CAPABILITIES.contains(&name) {
            return Err(policy_error(
                container,
                format!(
                    "Container \"{}\" holds {capability}, so a restricted allow-list cannot be enforced in it.\n  It can forge packets or change uid past a filter that matches on either.\n  Use network mode \"none\" or \"open\", or select a container without it.",
                    container.name
                ),
            )
            .with_detail("capability", capability.as_str()));
        }
    }
    Ok(())
}

/// Everything about a container that grants more than the defaults do.
///
/// The two fields that actually reach the host are the ones worth naming:
/// `security.devices` becomes `--device=…`, so `/dev/sda:/dev/sda:rwm` is raw
/// disk access, and `user: "0:0"` runs as root inside. A definition asking for
/// both passes [`assert_environment_policy`], so a summary of image and limits
/// alone would present a total escape as a clean container. Neither is
/// *refused*, because a device is legitimate for a rootless builder; both are
/// named loudly.
pub fn weakened_in(container: &EnvironmentDefinition) -> Vec<String> {
    let mut weakened = Vec::new();
    if !container.security.devices.is_empty() {
        weakened.push(format!(
            "devices    {}  (host device access)",
            container.security.devices.join(", ")
        ));
    }
    if container.user.is_empty() || container.user.starts_with("0:") {
        let user = if container.user.is_empty() {
            "image default"
        } else {
            container.user.as_str()
        };
        weakened.push(format!("user       {user}  (may be root)"));
    }
    if container.security.seccomp != SeccompProfile::Default {
        weakened.push("seccomp    unconfined".to_owned());
    }
    if !container.security.read_only_root {
        weakened.push("rootfs     writable".to_owned());
    }
    if !container.security.no_new_privileges {
        weakened.push("privileges may be gained (no-new-privileges off)".to_owned());
    }
    match container.runtime {
        ContainerRuntime::Runc => {}
        ContainerRuntime::Runsc => weakened.push("runtime    runsc".to_owned()),
        ContainerRuntime::Kata => weakened.push("runtime    kata".to_owned()),
    }

    // Two fields that still parse and are read by nothing. Reported beside the
    // hardening rather than refused, for the same reason: an operator is told
    // what their file says that no longer does anything, and gets to fix it in
    // their own time. Silence would be the file quietly meaning less than it
    // says.
    if container
        .prompt
        .as_deref()
        .is_some_and(|text| !text.is_empty())
    {
        weakened.push(
            "prompt     set, and no longer placed (say it in the agent's platformPrompt)"
                .to_owned(),
        );
    }
    if container.shared.is_some() {
        weakened
            .push("shared     set, and no longer read (every environment is shared)".to_owned());
    }
    weakened
}
