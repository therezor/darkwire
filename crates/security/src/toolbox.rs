//! Toolboxes: parsing, policy, and the ceiling.
//!
//! A toolbox is authorised by **content hash**, not by a signature. The question
//! asked at resolution is only ever "are these exact bytes approved?", and an
//! operator answers it once by installing the toolbox. That choice is worth
//! stating because the alternative looks stronger and is not: an Ed25519
//! signature answers *who authored this policy*, which matters when a manifest
//! arrives from somewhere else and proves nothing when the key sits on the same
//! disk as the file it signs. The approval check is a single predicate over
//! bytes so a signature path can be added later as a second way to satisfy the
//! same question, without disturbing anything that calls it.
//!
//! Two refusals here are absolute, and the difference between them is the design:
//!
//!  - **An image must be digest-pinned.** A tag is a mutable pointer, so a
//!    toolbox approved once and then repointed is the approval gate defeated
//!    while every hash still matches. Nothing in the review would show it.
//!  - **`NET_ADMIN` is never grantable.** The egress gateway's rules live in a
//!    network namespace the sandbox *shares*; a sandbox holding `NET_ADMIN` can
//!    flush them. This is refused rather than surfaced because it breaks an
//!    invariant the rest of the system relies on, and no operator reviewing a
//!    manifest could be expected to reconstruct that.
//!
//! `seccomp: unconfined` is deliberately *not* in that list. It is genuinely
//! risky and genuinely required for rootless builds inside a container, so it is
//! surfaced in the install review and left to the operator. The rule of thumb:
//! refuse what silently breaks the machinery, surface what is merely dangerous.

use std::sync::LazyLock;

use garde::Validate;
use ghostai_core::{ErrorKind, GhostError, Result};
pub use ghostai_protocol::BUILTIN_TOOL_NAMES;
use ghostai_protocol::{
    AgentToolboxNetwork, SeccompProfile, Toolbox, ToolboxNetworkMode, ToolboxRuntime,
};
use regex::Regex;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::ip::parse_cidr;
use crate::random::hex_lower;

/// The two immutable ways to name an image.
///
/// `name@sha256:<64 hex>` is a registry digest. A bare `sha256:<64 hex>` is a
/// local image **ID**, which is what `docker build` produces and what a toolbox
/// built on this machine has to reference — there is no registry digest until
/// something is pushed. Both are content addresses, so both are as unrepointable
/// as the other; a tag is neither.
///
/// **Anchored at both ends deliberately.** A pattern anchored only at the end
/// accepts `-v/:/hostfs@sha256:<64 hex>`, and the image is pushed to `docker run`
/// as a bare argv token — so a manifest could smuggle a flag past the check and
/// bind host root into the sandbox. That is not currently exploitable, because the
/// token after the image happens to be one docker rejects, but it survives by
/// accident of argument order rather than by design.
static IMAGE_DIGEST_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9][a-z0-9._\-/:]*@sha256:[0-9a-f]{64}$|^sha256:[0-9a-f]{64}$")
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// Capabilities a toolbox may never request. See the module header.
const FORBIDDEN_CAPABILITIES: &[&str] = &["NET_ADMIN", "SYS_ADMIN", "SYS_MODULE"];

/// Ordered weakest to strongest, which is what makes the ceiling a `min`.
fn rank(mode: ToolboxNetworkMode) -> u8 {
    match mode {
        ToolboxNetworkMode::None => 0,
        ToolboxNetworkMode::Allowlist => 1,
        ToolboxNetworkMode::Open => 2,
    }
}

fn mode_name(mode: ToolboxNetworkMode) -> &'static str {
    match mode {
        ToolboxNetworkMode::None => "none",
        ToolboxNetworkMode::Allowlist => "allowlist",
        ToolboxNetworkMode::Open => "open",
    }
}

/// The sha256 of a manifest's exact bytes.
///
/// Over the bytes, never over a re-serialisation of the parsed object: a toolbox
/// that round-trips through a formatter gains and loses whitespace and key
/// order, and an approval keyed on that would break on a formatter rather than on
/// a change of meaning.
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
    let json: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        GhostError::new(ErrorKind::Config, format!("{what} is not valid JSON")).with_source(error)
    })?;
    let parsed: T = serde_path_to_error::deserialize(json).map_err(|error| {
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

/// Parses manifest bytes, with the schema's own errors turned into a sentence.
pub fn parse_toolbox(bytes: &[u8]) -> Result<Toolbox> {
    parse_manifest(bytes, "Profile manifest")
}

fn policy_error(toolbox: &Toolbox, message: String) -> GhostError {
    GhostError::new(ErrorKind::Config, message).with_detail("toolbox", toolbox.name.as_str())
}

/// Refuses a toolbox the machinery cannot honour.
///
/// Separate from parsing because a manifest can be perfectly well-formed and
/// still ask for something that would quietly disable a guarantee elsewhere.
pub fn assert_toolbox_policy(toolbox: &Toolbox) -> Result<()> {
    if !IMAGE_DIGEST_PATTERN.is_match(&toolbox.image) {
        return Err(policy_error(
            toolbox,
            format!(
                "Toolbox \"{}\" must pin its image by digest, not by tag: {}\n  A tag can be repointed after approval, which would leave the recorded hash\n  matching an image nobody reviewed. Use name@sha256:<digest>.",
                toolbox.name, toolbox.image
            ),
        )
        .with_detail("image", toolbox.image.as_str()));
    }

    for capability in &toolbox.caps.add {
        let upper = capability.to_uppercase();
        let name = upper.strip_prefix("CAP_").unwrap_or(&upper);
        if FORBIDDEN_CAPABILITIES.contains(&name) {
            return Err(policy_error(
                toolbox,
                format!(
                    "Toolbox \"{}\" asks for {capability}, which is never granted.\n  The egress gateway's rules live in a namespace the sandbox shares, and a\n  sandbox holding NET_ADMIN could flush them.",
                    toolbox.name
                ),
            )
            .with_detail("capability", capability.as_str()));
        }
    }

    // A declared entry becomes a callable under `expose: tools`, and one named
    // `read_file` would shadow the jailed built-in with an unjailed shell command.
    // Refused rather than surfaced: no operator reading a manifest would spot that
    // a program name is also a tool name.
    for entry in &toolbox.tools {
        if BUILTIN_TOOL_NAMES.contains(&entry.name.as_str()) {
            return Err(policy_error(
                toolbox,
                format!(
                    "Toolbox \"{}\" declares a program called \"{}\", which is the\n  name of a built-in tool. Exposed as a callable it would shadow that tool.",
                    toolbox.name, entry.name
                ),
            )
            .with_detail("entry", entry.name.as_str()));
        }
    }

    for host in &toolbox.network.proxy_allow_hosts {
        if host.trim().is_empty() {
            return Err(policy_error(
                toolbox,
                format!("Toolbox \"{}\" has an empty proxy host entry", toolbox.name),
            ));
        }
    }
    Ok(())
}

/// Refuses an agent asking for more network than its toolbox permits.
///
/// Raised at agent resolution rather than clamped at turn time. Silently
/// narrowing would leave the config saying one thing while the sandbox did
/// another, and the operator who wrote `open` would have no way to discover it.
pub fn assert_network_within_ceiling(
    toolbox: &Toolbox,
    requested: &AgentToolboxNetwork,
    agent_id: &str,
) -> Result<()> {
    let maximum = toolbox.network.max_mode;
    if rank(requested.mode) > rank(maximum) {
        return Err(GhostError::new(
            ErrorKind::Config,
            format!(
                "Agent \"{agent_id}\" asks for network \"{}\", but toolbox \"{}\" permits at most \"{}\".",
                mode_name(requested.mode),
                toolbox.name,
                mode_name(maximum)
            ),
        )
        .with_detail("agentId", agent_id)
        .with_detail("toolbox", toolbox.name.as_str())
        .with_detail("requested", mode_name(requested.mode))
        .with_detail("maximum", mode_name(maximum)));
    }
    if requested.mode == ToolboxNetworkMode::Allowlist && requested.allow.is_empty() {
        return Err(GhostError::new(
            ErrorKind::Config,
            format!(
                "Agent \"{agent_id}\" asks for an allow-list with no entries, which reaches nothing.\n  Use mode \"none\" if that is the intent."
            ),
        )
        .with_detail("agentId", agent_id)
        .with_detail("toolbox", toolbox.name.as_str()));
    }
    for entry in &requested.allow {
        if parse_cidr(entry).is_none() {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Agent \"{agent_id}\" has an egress entry that is not a CIDR block: {entry}\n  Hostnames are refused here because DNS rebinding defeats them. Use 10.0.0.0/8."
                ),
            )
            .with_detail("agentId", agent_id)
            .with_detail("entry", entry.as_str()));
        }
    }
    Ok(())
}

/// What a toolbox and an agent's request resolve to together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveNetwork {
    /// The resolved mode, never above the toolbox ceiling.
    pub mode: ToolboxNetworkMode,
    /// The agent's CIDRs, only when the mode is `allowlist`.
    pub allow: Vec<String>,
    /// The toolbox's resolvers.
    pub dns: Vec<String>,
    /// The toolbox's proxy hosts.
    pub proxy_allow_hosts: Vec<String>,
}

/// The intersection of a toolbox's ceiling and an agent's request.
///
/// Defensive even though [`assert_network_within_ceiling`] has usually already
/// run: this is the value the runner turns into flags, and a `min` here means no
/// ordering of calls can produce a container with more reach than its toolbox
/// allows. The property worth testing is that it never widens.
pub fn effective_network(toolbox: &Toolbox, requested: &AgentToolboxNetwork) -> EffectiveNetwork {
    let maximum = toolbox.network.max_mode;
    let mode = if rank(requested.mode) < rank(maximum) {
        requested.mode
    } else {
        maximum
    };
    EffectiveNetwork {
        mode,
        allow: if mode == ToolboxNetworkMode::Allowlist {
            requested.allow.clone()
        } else {
            Vec::new()
        },
        dns: toolbox.network.dns.clone(),
        proxy_allow_hosts: toolbox.network.proxy_allow_hosts.clone(),
    }
}

/// Everything about a toolbox that grants more than the defaults do.
///
/// The two fields that actually reach the host are the ones worth naming:
/// `security.devices` becomes `--device=…`, so `/dev/sda:/dev/sda:rwm` is raw
/// disk access, and `user: "0:0"` runs as root inside. A manifest asking for
/// both passes [`assert_toolbox_policy`], so a summary of image, network and
/// limits alone would present a total escape as a clean toolbox. Neither is
/// *refused*, because a device is legitimate for a rootless builder; both are
/// named loudly.
pub fn weakened_in(toolbox: &Toolbox) -> Vec<String> {
    let mut weakened = Vec::new();
    if !toolbox.security.devices.is_empty() {
        weakened.push(format!(
            "devices    {}  (host device access)",
            toolbox.security.devices.join(", ")
        ));
    }
    if toolbox.user.is_empty() || toolbox.user.starts_with("0:") {
        let user = if toolbox.user.is_empty() {
            "image default"
        } else {
            toolbox.user.as_str()
        };
        weakened.push(format!("user       {user}  (may be root)"));
    }
    if toolbox.security.seccomp != SeccompProfile::Default {
        weakened.push("seccomp    unconfined".to_owned());
    }
    if !toolbox.security.read_only_root {
        weakened.push("rootfs     writable".to_owned());
    }
    match toolbox.runtime {
        ToolboxRuntime::Runc => {}
        ToolboxRuntime::Runsc => weakened.push("runtime    runsc".to_owned()),
        ToolboxRuntime::Kata => weakened.push("runtime    kata".to_owned()),
    }
    if toolbox.workdir == "/" {
        weakened.push("workdir    / (mounts the workspace over the root)".to_owned());
    }
    weakened
}
