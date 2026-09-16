//! Environment policy: where an agent's built-in `exec` calls run.
//!
//! An environment is the place; a container is one kind of place. `kind` is
//! what says which, and today it has one arm, so every other field below
//! still describes a container and sits flat rather than inside the variant.
//! Folding them in is a second migration, and it buys nothing until there is a
//! second kind to fold them away from.

use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::json::{MAX_SAFE_INTEGER, coerce_f64, coerce_u64, literal, prefault};

/// The OCI runtime a container wants.
///
/// `runc` is the default everywhere. `runsc` (gVisor) trades syscall
/// compatibility for a real isolation boundary and is Linux-only; `kata` is a
/// microVM. Availability is probed when a container is first needed rather
/// than assumed, so a definition naming an absent runtime fails that turn with
/// a sentence instead of the whole install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContainerRuntime {
    /// The default OCI runtime.
    #[default]
    Runc,
    /// gVisor.
    Runsc,
    /// Kata Containers.
    Kata,
}

/// Linux capabilities the container drops and adds back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ContainerCaps {
    /// Almost always `["ALL"]`. Listed rather than assumed so a definition is
    /// readable.
    #[serde(default = "drop_all")]
    pub drop: Vec<String>,
    /// Added back one at a time, with a reason. `NET_ADMIN` is deliberately
    /// not grantable, because the egress gateway's rules live in a namespace
    /// the container shares and must not be able to flush them.
    #[serde(default)]
    pub add: Vec<String>,
}

fn drop_all() -> Vec<String> {
    vec!["ALL".to_owned()]
}

impl Default for ContainerCaps {
    fn default() -> Self {
        Self {
            drop: drop_all(),
            add: Vec::new(),
        }
    }
}

/// The seccomp profile a container runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SeccompProfile {
    /// The container engine's own profile.
    #[default]
    Default,
    /// No filter. Surfaced in the install review.
    Unconfined,
}

/// Container hardening.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ContainerSecurity {
    /// `no_new_privs`.
    #[serde(default = "crate::json::yes")]
    pub no_new_privileges: bool,
    /// The seccomp profile.
    #[serde(default)]
    pub seccomp: SeccompProfile,
    /// A read-only root filesystem.
    #[serde(default = "crate::json::yes")]
    pub read_only_root: bool,
    /// Mount specs, e.g. `/tmp:rw,nosuid,size=512m`.
    ///
    /// **Defaults to a writable `/tmp`, and that is load-bearing.** `exec`
    /// records its pid under `/tmp` so a timeout or a cancel has something to
    /// signal; under a read-only root with no tmpfs the redirect fails, the
    /// script carries on, and the kill script then reads an empty file and
    /// exits 0. Cancellation silently stops working while the command keeps
    /// running.
    ///
    /// 64m rather than the catalogue's 256m because a tmpfs is RAM and counts
    /// against the memory limit.
    #[serde(default = "default_tmpfs")]
    pub tmpfs: Vec<String>,
    /// Rootless build needs `/dev/fuse`; nothing else should ask for a device.
    #[serde(default)]
    pub devices: Vec<String>,
}

impl Default for ContainerSecurity {
    fn default() -> Self {
        Self {
            no_new_privileges: true,
            seccomp: SeccompProfile::Default,
            read_only_root: true,
            tmpfs: default_tmpfs(),
            devices: Vec::new(),
        }
    }
}

/// Resource limits. Each accepts a numeric string as well as a number, the
/// way a hand-written manifest tends to spell one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ContainerLimits {
    /// Memory, in megabytes.
    #[serde(default = "default_memory_mb", deserialize_with = "coerce_u64")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub memory_mb: u64,
    /// CPUs.
    #[serde(default = "default_cpus", deserialize_with = "coerce_f64")]
    #[garde(range(min = 0.0))]
    pub cpus: f64,
    /// The pid cap.
    #[serde(default = "default_pids_max", deserialize_with = "coerce_u64")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub pids_max: u64,
    /// `/dev/shm`, in megabytes. The engine's 64m default produces short
    /// writes in build and scan workloads.
    #[serde(default = "default_shm_size_mb", deserialize_with = "coerce_u64")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub shm_size_mb: u64,
}

/// A writable `/tmp`, without which cancellation silently stops working.
fn default_tmpfs() -> Vec<String> {
    vec!["/tmp:rw,nosuid,size=64m".to_owned()]
}

fn default_memory_mb() -> u64 {
    512
}

fn default_cpus() -> f64 {
    1.0
}

fn default_pids_max() -> u64 {
    256
}

fn default_shm_size_mb() -> u64 {
    64
}

impl Default for ContainerLimits {
    fn default() -> Self {
        Self {
            memory_mb: default_memory_mb(),
            cpus: default_cpus(),
            pids_max: default_pids_max(),
            shm_size_mb: default_shm_size_mb(),
        }
    }
}

literal! {
    /// The environment definition format tag.
    pub struct EnvironmentDefinitionTag = "darkwire.environment/1";
}

/// What kind of place an environment is.
///
/// One arm, and a closed set rather than a string, so adding a remote
/// environment is a variant and a match arm the compiler points at rather than
/// a comparison somebody has to remember to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentKind {
    /// An OCI container, described by the fields below.
    #[default]
    Container,
}

fn default_workdir() -> String {
    "/workspace".to_owned()
}

fn environment_user() -> String {
    "1000:1000".to_owned()
}

/// Where built-in command execution runs.
///
/// **There is no network here, deliberately.** Egress is the agent's own
/// request, configured in one place (`agents.list.<id>.environment.network`),
/// and the fields below are what decide whether a restricted egress gateway
/// can be built around it at all: a root or non-numeric `user`, missing
/// `noNewPrivileges` or a capability that can forge packets each make the
/// gateway refuse. So an operator writing a definition is fixing the *shape*
/// an agent's network request will be honoured in, not the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct EnvironmentDefinition {
    /// Always `darkwire.environment/1`.
    pub schema: EnvironmentDefinitionTag,
    /// What kind of place this is. Omitting it means a container.
    #[serde(default)]
    pub kind: EnvironmentKind,
    /// The name agents select, which is also its filename.
    #[schemars(regex(pattern = "^[a-z0-9][a-z0-9-]{0,63}$"))]
    pub name: String,
    /// Read by nothing. What the model is told about where its commands run is
    /// one section now, `agents.list.<id>.platform_prompt`, which says both
    /// where they run and what is installed there.
    ///
    /// Still parsed because every definition written before the change carries
    /// it. A definition setting it is reported on its row. It goes one release
    /// after that.
    ///
    /// `skip_serializing_if` is load-bearing: `save_environment` re-emits the
    /// whole definition, so without it every save from the editor would write
    /// the field back and the row would report itself forever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Must be digest-pinned: an immutable image ID or a registry digest. A
    /// tag is a mutable pointer, and a container installed once and then
    /// silently repointed would run code nobody chose.
    #[garde(length(utf16, min = 1))]
    pub image: String,
    /// Read by nothing. An environment is a place, and everything asking for
    /// the same place gets the same container: an instance is keyed on the
    /// workspace, the definition and the network, with neither the agent nor
    /// the session in it.
    ///
    /// Parsed and reported for the same reason `prompt` above is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<bool>,
    /// The OCI runtime.
    #[serde(default)]
    pub runtime: ContainerRuntime,
    /// Where the workspace is mounted inside the container.
    #[serde(default = "default_workdir")]
    pub workdir: String,
    /// `uid:gid` inside the container, non-root by default. Matching the host
    /// user is what keeps artefacts written into the workspace editable by the
    /// host's own tools — root-owned output is the most common complaint about
    /// this pattern.
    #[serde(default = "environment_user")]
    pub user: String,
    /// Capabilities.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub caps: ContainerCaps,
    /// Hardening.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub security: ContainerSecurity,
    /// Resource limits.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub limits: ContainerLimits,
    /// Host env names passed through. Never a secret — those go via the proxy.
    #[serde(default)]
    pub env: Vec<String>,
}
