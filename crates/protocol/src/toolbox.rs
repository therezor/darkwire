//! A toolbox: an image, the tools inside it, and the policy for running it.
//!
//! These live in one operator-installed manifest rather than in an agent's
//! config because an agent's config is *editable* — through the settings
//! route, through a hand-edited file, and through anything that later gains
//! the ability to propose a patch. An image reference and a capability set are
//! not settings; they are the boundary that makes everything else safe. So
//! they live here, outside the config tree, and an agent carries a toolbox
//! *name* and nothing else that could widen it.
//!
//! **Why not "tool".** That word is taken: a tool is a function the model can
//! call, with a schema and a risk band. A toolbox is the *environment* those
//! calls run in — a box of programs `exec` can reach.
//!
//! Four fields are load-bearing: `image` must be digest-pinned (a tag is a
//! mutable pointer, and a toolbox approved once and silently repointed is the
//! approval gate defeated); `network.max_mode` is a ceiling an agent's own
//! request is intersected with, never unioned; `tools` is the toolset
//! advertisement as a list a UI can render and a prompt can compose; `notes`
//! is for what a list cannot say. What is deliberately absent is the
//! boilerplate every toolbox would otherwise repeat — that a shell is
//! available, that only the workspace is mounted — which is composed where it
//! is always true rather than copied into every manifest where it can drift.

use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::json::{MAX_SAFE_INTEGER, coerce_f64, coerce_u64, literal, prefault};
use crate::tools::ToolPermission;

/// How much network a toolbox is willing to permit at most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolboxNetworkMode {
    /// No network at all.
    #[default]
    None,
    /// Only the CIDRs an agent lists.
    Allowlist,
    /// Anything.
    Open,
}

/// The OCI runtime a toolbox wants.
///
/// `runc` is the default everywhere. `runsc` (gVisor) trades syscall
/// compatibility for a real isolation boundary and is Linux-only; `kata` is a
/// microVM. Availability is probed when a container is first needed rather
/// than assumed, so a toolbox naming an absent runtime fails that turn with a
/// sentence instead of the whole install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolboxRuntime {
    /// The default OCI runtime.
    #[default]
    Runc,
    /// gVisor.
    Runsc,
    /// Kata Containers.
    Kata,
}

/// One program in the box, as the model is told about it.
///
/// Four fields rather than one paragraph, because they land in three different
/// places: `use` becomes the tool's own description — imperative, "Search the
/// web", not a definition; `args` becomes the description of the `args` field,
/// which is the text a model reads while deciding what to put there; `example`
/// is a concrete argv, which models copy far more reliably than they follow
/// prose; `requires_args` makes the schema itself refuse an empty call, after a
/// model called `fetch` with no URL, got a usage error and gave up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolboxEntry {
    /// The program.
    #[garde(length(utf16, min = 1))]
    pub name: String,
    /// One imperative sentence. Becomes the tool's description.
    #[serde(default)]
    pub r#use: String,
    /// What the arguments mean. Becomes the `args` field's own description.
    #[serde(default)]
    pub args: String,
    /// A concrete argv the model can copy.
    #[serde(default)]
    pub example: Vec<String>,
    /// When true, a call with no arguments is refused by the schema.
    #[serde(default)]
    pub requires_args: bool,
    /// What this program should be allowed to do, as the box's author sees it.
    ///
    /// A **default, not a ceiling** — unlike `network.max_mode`, an agent's own
    /// `tools` map overrides it in either direction. A toolbox that marked
    /// `nmap` as `ask` and could not be overridden would be a manifest edit,
    /// and therefore a re-approval, every time an operator wanted their own
    /// scanner to run unattended. `ask` by default because these are all
    /// `exec` underneath.
    #[serde(default = "ask")]
    pub permission: ToolPermission,
}

fn ask() -> ToolPermission {
    ToolPermission::Ask
}

/// Linux capabilities the container drops and adds back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolboxCaps {
    /// Almost always `["ALL"]`. Listed rather than assumed so a manifest is
    /// readable.
    #[serde(default = "drop_all")]
    pub drop: Vec<String>,
    /// Added back one at a time, with a reason. `NET_RAW` is what `nmap -sS`
    /// needs; `NET_ADMIN` is deliberately not grantable, because the egress
    /// gateway's rules live in a namespace the container shares and must not
    /// be able to flush.
    #[serde(default)]
    pub add: Vec<String>,
}

fn drop_all() -> Vec<String> {
    vec!["ALL".to_owned()]
}

impl Default for ToolboxCaps {
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
    /// Docker's own profile.
    #[default]
    Default,
    /// No filter. Surfaced in the install review.
    Unconfined,
}

/// Container hardening.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolboxSecurity {
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
    #[serde(default)]
    pub tmpfs: Vec<String>,
    /// Rootless build needs `/dev/fuse`; nothing else should ask for a device.
    #[serde(default)]
    pub devices: Vec<String>,
}

impl Default for ToolboxSecurity {
    fn default() -> Self {
        Self {
            no_new_privileges: true,
            seccomp: SeccompProfile::Default,
            read_only_root: true,
            tmpfs: Vec::new(),
            devices: Vec::new(),
        }
    }
}

/// Resource limits. Each accepts a numeric string as well as a number, the
/// way a hand-written manifest tends to spell one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolboxLimits {
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
    /// `/dev/shm`, in megabytes. Docker's 64m default produces short writes in
    /// build and scan workloads.
    #[serde(default = "default_shm_size_mb", deserialize_with = "coerce_u64")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub shm_size_mb: u64,
}

fn default_memory_mb() -> u64 {
    2048
}

fn default_cpus() -> f64 {
    2.0
}

fn default_pids_max() -> u64 {
    512
}

fn default_shm_size_mb() -> u64 {
    256
}

impl Default for ToolboxLimits {
    fn default() -> Self {
        Self {
            memory_mb: default_memory_mb(),
            cpus: default_cpus(),
            pids_max: default_pids_max(),
            shm_size_mb: default_shm_size_mb(),
        }
    }
}

/// The network ceiling and the gateway's plumbing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolboxNetwork {
    /// The most this toolbox ever permits; an agent may ask for less.
    #[serde(default)]
    pub max_mode: ToolboxNetworkMode,
    /// Resolvers the gateway permits on port 53. Without one, a CIDR
    /// allow-list makes every hostname unresolvable — `127.0.0.11` is
    /// Docker's embedded resolver and lives in the namespace the container
    /// shares with the gateway.
    #[serde(default = "default_dns")]
    pub dns: Vec<String>,
    /// Hostnames the credential/egress proxy permits, for a toolbox whose
    /// traffic is all HTTP(S). Useless for raw scanning, which is why a pentest
    /// toolbox scopes by CIDR instead.
    #[serde(default)]
    pub proxy_allow_hosts: Vec<String>,
}

fn default_dns() -> Vec<String> {
    vec!["127.0.0.11".to_owned()]
}

impl Default for ToolboxNetwork {
    fn default() -> Self {
        Self {
            max_mode: ToolboxNetworkMode::None,
            dns: default_dns(),
            proxy_allow_hosts: Vec::new(),
        }
    }
}

/// How the model is told what is in a toolbox.
///
/// `prompt` is one section of about forty tokens, whatever the box holds, and
/// relies on the model reading its instructions. `tools` additionally
/// materialises every entry as a real callable schema beside `read_file` and
/// `exec` — roughly 60–80 tokens each, every request of every turn, and worth
/// it for a model that reads its tool list far more attentively than its prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolboxExposure {
    /// A prompt section only.
    #[default]
    Prompt,
    /// A prompt section and one callable tool per entry.
    Tools,
}

literal! {
    /// The manifest format tag. Bumped only for a breaking change; refused when
    /// unrecognised.
    pub struct ToolboxSchemaTag = "ghostai.toolbox/1";
}

/// A toolbox manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct Toolbox {
    /// Always `ghostai.toolbox/1`.
    pub schema: ToolboxSchemaTag,
    /// The name agents refer to it by.
    #[garde(length(utf16, min = 1, max = 64))]
    pub name: String,
    /// The manifest's own version string.
    #[serde(default = "default_version")]
    pub version: String,
    /// Shown in the UI. Empty falls back to the name.
    #[serde(default)]
    pub label: String,
    /// What is in the box. See the module docs on why this is a list.
    #[serde(default)]
    #[garde(dive)]
    pub tools: Vec<ToolboxEntry>,
    /// Caveats about the box as a whole, appended to the prompt section.
    #[serde(default)]
    pub notes: String,
    /// How the model is told what is in here.
    #[serde(default)]
    pub expose: ToolboxExposure,
    /// Must be digest-pinned. Checked by the security layer.
    #[garde(length(utf16, min = 1))]
    pub image: String,
    /// The OCI runtime.
    #[serde(default)]
    pub runtime: ToolboxRuntime,
    /// Where the workspace is mounted inside the container.
    #[serde(default = "default_workdir")]
    pub workdir: String,
    /// `uid:gid` inside the container. Matching the host user is what keeps
    /// artefacts written into the workspace editable by the host's own tools —
    /// root-owned output is the most common complaint about this pattern.
    #[serde(default)]
    pub user: String,
    /// Capabilities.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub caps: ToolboxCaps,
    /// Hardening.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub security: ToolboxSecurity,
    /// Resource limits.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub limits: ToolboxLimits,
    /// The network ceiling.
    #[serde(default)]
    #[schemars(transform = prefault)]
    #[garde(dive)]
    pub network: ToolboxNetwork,
    /// Host env names passed through. Never a secret — those go via the proxy.
    #[serde(default)]
    pub env: Vec<String>,
}

fn default_version() -> String {
    "0.0.0".to_owned()
}

fn default_workdir() -> String {
    "/workspace".to_owned()
}
