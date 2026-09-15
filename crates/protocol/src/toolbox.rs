//! Toolboxes and containers: what an agent may call, and where it runs.
//!
//! Two manifests, approved separately, that deliberately do not know about
//! each other:
//!
//!  - A **toolbox** is the complete set of operations one agent may call. It
//!    names reusable operation definitions and the permission ceiling for
//!    each. It holds no image, no capabilities and no network, so nothing in
//!    it can widen a boundary.
//!  - A **container** is where command operations run: an image, the
//!    hardening around it, its resource budget, and whether agents share one
//!    instance. It holds no tool grants, so approving a place to run commands
//!    is not approving any particular command.
//!
//! Both live in an operator-installed policy directory rather than in
//! `agents.list.<id>`, because an agent's config is *editable* — through the
//! settings route, through a hand-edited file, and through anything that later
//! gains the ability to propose a patch. An agent carries a toolbox name, a
//! container name and its own egress request; every value that decides what an
//! image is or what privileges it holds has no representation in the config
//! tree at all.
//!
//! **Why not "tool".** That word is taken: a tool is a function the model can
//! call, with a schema and a risk band. A toolbox is the *set* of those an
//! agent was granted, and an operation is the reviewed definition behind one.
//!
//! An operation is a fixed program and a reviewed argument mapping, never a
//! shell string. The JSON Schema it publishes is the same one its inputs are
//! validated against before a process starts, so "what the model was told it
//! could send" and "what the sandbox accepts" cannot drift apart.

use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::json::{MAX_SAFE_INTEGER, coerce_f64, coerce_u64, literal, prefault};
use crate::tools::ToolPermission;

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
    #[serde(default)]
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
    /// The toolbox manifest format tag. Refused when unrecognised.
    pub struct ToolboxSchemaTag = "ghostai.toolbox/1";
}
literal! {
    /// The reusable operation manifest format tag.
    pub struct ToolOperationTag = "ghostai.tool/1";
}
literal! {
    /// The container definition format tag.
    pub struct ContainerDefinitionTag = "ghostai.container/1";
}

fn ask() -> ToolPermission {
    ToolPermission::Ask
}

fn default_version() -> String {
    "0.0.0".to_owned()
}

fn default_workdir() -> String {
    "/workspace".to_owned()
}

fn container_user() -> String {
    "1000:1000".to_owned()
}

/// An approved toolbox is the complete callable surface of one agent.
///
/// Every grant names an operation definition installed beside it rather than
/// carrying the definition inline, so one reviewed `git-status` is shared by
/// every toolbox that grants it and is reviewed once. The approval hash covers
/// the toolbox *and* every definition it names, so editing a shared definition
/// revokes each toolbox that reaches it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct Toolbox {
    /// Always `ghostai.toolbox/1`.
    pub schema: ToolboxSchemaTag,
    /// The name agents refer to it by, which is also its filename.
    #[schemars(regex(pattern = "^[a-z0-9][a-z0-9-]{0,63}$"))]
    pub name: String,
    /// Shown in the UI. Empty falls back to the name.
    #[serde(default)]
    pub label: String,
    /// The manifest's own version string.
    #[serde(default = "default_version")]
    pub version: String,
    /// Caveats about the set as a whole, appended to the prompt section. Model
    /// guidance, never an authorisation rule.
    #[serde(default)]
    pub notes: String,
    /// The operations this toolbox grants, resolved against the operator's
    /// tool-definitions directory.
    #[garde(dive)]
    pub tools: Vec<ToolGrant>,
}

/// One operation the toolbox permits an agent to invoke.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct ToolGrant {
    /// Public callable name, local to this toolbox.
    #[schemars(regex(pattern = "^[A-Za-z0-9_-]{1,64}$"))]
    pub name: String,
    /// The operator-installed definition this name resolves to.
    #[schemars(regex(pattern = "^[a-z0-9][a-z0-9-]{0,63}$"))]
    pub definition: String,
    /// A ceiling: an agent's own `toolbox.tools` map may tighten this to
    /// `ask` or `deny`, never widen it.
    #[serde(default = "ask")]
    pub permission: ToolPermission,
}

/// A reusable, operator-installed operation.
///
/// The JSON Schema is validated offline at approval time and enforced again
/// before every call, so it can never reference anything the validator would
/// have to fetch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolOperation {
    /// Always `ghostai.tool/1`.
    pub schema: ToolOperationTag,
    /// Description advertised to the model.
    pub description: String,
    /// Self-contained JSON Schema enforced before invocation.
    #[schemars(with = "serde_json::Map<String, serde_json::Value>")]
    pub parameters: serde_json::Value,
    /// The implementation that receives validated inputs.
    pub implementation: OperationImplementation,
}

/// Implementations supported by an approved operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum OperationImplementation {
    /// Read bounded output from the agent's current container instance.
    Transcript,
    /// Execute a fixed program with a reviewed argument mapping.
    Command {
        /// Absolute executable path outside the writable workspace.
        #[schemars(regex(pattern = "^\\/.*"))]
        executable: String,
        /// Fixed arguments and individual validated inputs.
        #[serde(default)]
        argv: Vec<OperationArgument>,
        /// Explicit permission for arbitrary argv; absent for narrow
        /// operations.
        #[serde(default, rename = "argvInput", skip_serializing_if = "Option::is_none")]
        argv_input: Option<String>,
    },
    /// Call an installed built-in, MCP, or extension tool.
    Registered {
        /// Exact registered tool name.
        tool: String,
        /// SHA-256 of the installed tool definition, including source
        /// identity.
        digest: String,
    },
}

/// One argv element, without shell interpolation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OperationArgument {
    /// Operator-supplied constant.
    Literal(String),
    /// A validated input value.
    Input(OperationInput),
}

/// Maps a named scalar input to one argv element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationInput {
    /// Required property from the operation input schema.
    pub input: String,
    /// Resolve this input through the workspace jail.
    #[serde(default)]
    pub workspace_path: bool,
}

/// Where command operations run, chosen independently of the toolbox.
///
/// **There is no network here, deliberately.** Egress is the agent's own
/// request, configured in one place (`agents.list.<id>.container.network`),
/// and the fields below are what decide whether a restricted egress gateway
/// can be built around it at all: a root or non-numeric `user`, missing
/// `noNewPrivileges` or a capability that can forge packets each make the
/// gateway refuse. So an operator approving a definition is approving the
/// *shape* an agent's network request will be honoured in, not the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct ContainerDefinition {
    /// Always `ghostai.container/1`.
    pub schema: ContainerDefinitionTag,
    /// The name agents select, which is also its filename.
    #[schemars(regex(pattern = "^[a-z0-9][a-z0-9-]{0,63}$"))]
    pub name: String,
    /// Must be digest-pinned: an immutable image ID or a registry digest. A
    /// tag is a mutable pointer, and a container approved once and then
    /// silently repointed is the approval gate defeated.
    #[garde(length(utf16, min = 1))]
    pub image: String,
    /// Share one instance across agents and conversations in a workspace that
    /// ask for the same egress.
    #[serde(default)]
    pub shared: bool,
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
    #[serde(default = "container_user")]
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
