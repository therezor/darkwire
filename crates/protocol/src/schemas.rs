//! A registry of every schema this crate publishes.
//!
//! Two jobs. The drift test iterates it, so the guarantee that every mirrored
//! type still matches the browser's schema cannot quietly stop covering a type
//! someone added later — a new `JsonSchema` type with no entry here fails the
//! completeness test beside it. And it is the `$defs` pool the HTTP layer
//! publishes under `components.schemas` in the generated OpenAPI document.
//!
//! The generator settings are the crate's schema policy, stated once:
//!
//! - **Draft 2020-12, deserialise contract.** A field with a default or an
//!   `Option` type is not required, because a client may omit it; that is what
//!   the browser's schemas say of a request body, and the direction it writes.
//! - **Every subschema inline.** The document maps to the source one type at a
//!   time, so a `$ref` would name something the other side never emits.
//! - **`Option<T>` means "may be omitted", not "may be null".** serde reads a
//!   `null` into an `Option` as absent, and the schema says only what the wire
//!   contract promises: the field may be left out. A field that is genuinely
//!   nullable names [`Nullable`](crate::json::Nullable), which is spelled as a
//!   `oneOf` so this policy can tell it apart and leave it alone.

use schemars::generate::{Contract, SchemaSettings};
use schemars::transform::{Transform, transform_subschemas};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde_json::Value;

use crate::{
    automation, config, environment, extension, messages, rest, subagent, tasks, tools, ws,
};

/// One published schema: the name the browser's registry uses, and how to
/// generate it.
#[derive(Debug, Clone, Copy)]
pub struct RegisteredSchema {
    /// The registry key, identical on both sides of the wire.
    pub name: &'static str,
    /// Produces the schema with the crate's generator settings applied.
    pub schema: fn(&mut SchemaGenerator) -> Schema,
}

impl RegisteredSchema {
    /// The schema, generated fresh.
    pub fn generate(&self) -> Schema {
        let mut generator = protocol_generator();
        (self.schema)(&mut generator)
    }
}

/// Strips the `null` that serde's `Option<T>` would also accept, everywhere
/// except inside a `oneOf` — which is how [`Nullable`](crate::json::Nullable)
/// marks a field that is nullable on purpose.
#[derive(Debug, Clone, Copy)]
struct OptionMeansOmittable;

impl Transform for OptionMeansOmittable {
    fn transform(&mut self, schema: &mut Schema) {
        transform_subschemas(self, schema);
        let Some(object) = schema.as_object_mut() else {
            return;
        };
        if let Some(Value::Array(types)) = object.get_mut("type") {
            types.retain(|t| t != "null");
            if types.len() == 1 {
                let only = types.remove(0);
                object.insert("type".into(), only);
            }
        }
        if let Some(Value::Array(values)) = object.get_mut("enum") {
            values.retain(|v| !v.is_null());
        }
        if let Some(Value::Array(members)) = object.get_mut("anyOf") {
            members.retain(|m| {
                m.as_object()
                    .is_none_or(|o| o.len() != 1 || o.get("type") != Some(&Value::from("null")))
            });
            if members.len() == 1 {
                let only = members.remove(0);
                object.remove("anyOf");
                if let Value::Object(inner) = only {
                    for (key, value) in inner {
                        object.entry(key).or_insert(value);
                    }
                }
            }
        }
    }
}

/// A closed set of string literals is an `enum`, however its variants were
/// documented. The derive spells a documented variant as `{const, description}`
/// and the union of those as `oneOf`; the browser spells the same set as one
/// `enum`, and so does this document.
#[derive(Debug, Clone, Copy)]
struct ConstUnionAsEnum;

impl Transform for ConstUnionAsEnum {
    fn transform(&mut self, schema: &mut Schema) {
        transform_subschemas(self, schema);
        let Some(object) = schema.as_object_mut() else {
            return;
        };
        let Some(Value::Array(members)) = object.get("oneOf") else {
            return;
        };
        let literals: Option<Vec<Value>> = members
            .iter()
            .map(|member| {
                let m = member.as_object()?;
                let plain = m
                    .keys()
                    .all(|k| matches!(k.as_str(), "type" | "const" | "description"));
                (plain && m.get("type") == Some(&Value::from("string")))
                    .then(|| m.get("const").cloned())?
            })
            .collect();
        let annotations_only = object.keys().all(|k| {
            matches!(
                k.as_str(),
                "oneOf" | "$schema" | "title" | "description" | "default"
            )
        });
        if let Some(literals) = literals
            && annotations_only
        {
            object.remove("oneOf");
            object.insert("type".into(), Value::from("string"));
            object.insert("enum".into(), Value::Array(literals));
        }
    }
}

/// The generator every published schema comes from. See the module docs for
/// what the settings mean.
pub fn protocol_generator() -> SchemaGenerator {
    let mut settings = SchemaSettings::draft2020_12();
    settings.contract = Contract::Deserialize;
    settings.inline_subschemas = true;
    settings.transforms.push(Box::new(ConstUnionAsEnum));
    settings.transforms.push(Box::new(OptionMeansOmittable));
    SchemaGenerator::new(settings)
}

fn schema_for<T: JsonSchema>(generator: &mut SchemaGenerator) -> Schema {
    generator.root_schema_for::<T>()
}

/// A schema whose root carries a default — a type alias in Rust, which has
/// nowhere to hang one.
#[allow(
    dead_code,
    reason = "used by the config entries added with that module"
)]
fn with_default<T: JsonSchema>(default: Value) -> impl Fn(&mut SchemaGenerator) -> Schema {
    move |generator| {
        let mut schema = generator.root_schema_for::<T>();
        schema.insert("default".into(), default.clone());
        schema
    }
}

fn workspaces_path(generator: &mut SchemaGenerator) -> Schema {
    with_default::<config::WorkspacesPath>(Value::from(""))(generator)
}

fn providers_config(generator: &mut SchemaGenerator) -> Schema {
    with_default::<config::ProvidersConfig>(Value::Object(serde_json::Map::new()))(generator)
}

/// Every published schema, in the order the browser's registry lists them.
pub static PROTOCOL_SCHEMAS: &[RegisteredSchema] = &[
    // messages
    RegisteredSchema {
        name: "ChatRole",
        schema: schema_for::<messages::ChatRole>,
    },
    RegisteredSchema {
        name: "TextPart",
        schema: schema_for::<messages::TextPart>,
    },
    RegisteredSchema {
        name: "ImagePart",
        schema: schema_for::<messages::ImagePart>,
    },
    RegisteredSchema {
        name: "FilePart",
        schema: schema_for::<messages::FilePart>,
    },
    RegisteredSchema {
        name: "ContentPart",
        schema: schema_for::<messages::ContentPart>,
    },
    RegisteredSchema {
        name: "ToolCall",
        schema: schema_for::<messages::ToolCall>,
    },
    RegisteredSchema {
        name: "SystemMessage",
        schema: schema_for::<messages::SystemMessage>,
    },
    RegisteredSchema {
        name: "UserMessage",
        schema: schema_for::<messages::UserMessage>,
    },
    RegisteredSchema {
        name: "AssistantMessage",
        schema: schema_for::<messages::AssistantMessage>,
    },
    RegisteredSchema {
        name: "ToolMessage",
        schema: schema_for::<messages::ToolMessage>,
    },
    RegisteredSchema {
        name: "ChatMessage",
        schema: schema_for::<messages::ChatMessage>,
    },
    RegisteredSchema {
        name: "StoredMessage",
        schema: schema_for::<messages::StoredMessage>,
    },
    RegisteredSchema {
        name: "Usage",
        schema: schema_for::<messages::Usage>,
    },
    RegisteredSchema {
        name: "StopReason",
        schema: schema_for::<messages::StopReason>,
    },
    // tools
    RegisteredSchema {
        name: "ToolRisk",
        schema: schema_for::<tools::ToolRisk>,
    },
    RegisteredSchema {
        name: "ToolPermission",
        schema: schema_for::<tools::ToolPermission>,
    },
    RegisteredSchema {
        name: "ToolPermissions",
        schema: schema_for::<tools::ToolPermissions>,
    },
    RegisteredSchema {
        name: "ToolSource",
        schema: schema_for::<tools::ToolSource>,
    },
    RegisteredSchema {
        name: "ToolAnnotations",
        schema: schema_for::<tools::ToolAnnotations>,
    },
    RegisteredSchema {
        name: "ToolDefinition",
        schema: schema_for::<tools::ToolDefinition>,
    },
    RegisteredSchema {
        name: "ApprovalScope",
        schema: schema_for::<tools::ApprovalScope>,
    },
    RegisteredSchema {
        name: "ToolPromptOverride",
        schema: schema_for::<tools::ToolPromptOverride>,
    },
    RegisteredSchema {
        name: "ToolPromptOverrides",
        schema: schema_for::<tools::ToolPromptOverrides>,
    },
    // config
    RegisteredSchema {
        name: "ReasoningEffort",
        schema: schema_for::<config::ReasoningEffort>,
    },
    RegisteredSchema {
        name: "PromptMode",
        schema: schema_for::<config::PromptMode>,
    },
    RegisteredSchema {
        name: "ReasoningDisplay",
        schema: schema_for::<config::ReasoningDisplay>,
    },
    RegisteredSchema {
        name: "AgentSettings",
        schema: schema_for::<config::AgentSettings>,
    },
    RegisteredSchema {
        name: "WorkspacesPath",
        schema: workspaces_path,
    },
    RegisteredSchema {
        name: "AgentsConfig",
        schema: schema_for::<config::AgentsConfig>,
    },
    RegisteredSchema {
        name: "ProviderConfig",
        schema: schema_for::<config::ProviderConfig>,
    },
    RegisteredSchema {
        name: "ProvidersConfig",
        schema: providers_config,
    },
    RegisteredSchema {
        name: "AuthConfig",
        schema: schema_for::<config::AuthConfig>,
    },
    RegisteredSchema {
        name: "ServerConfig",
        schema: schema_for::<config::ServerConfig>,
    },
    RegisteredSchema {
        name: "ExecToolConfig",
        schema: schema_for::<config::ExecToolConfig>,
    },
    RegisteredSchema {
        name: "McpOAuthConfig",
        schema: schema_for::<config::McpOAuthConfig>,
    },
    RegisteredSchema {
        name: "McpTransport",
        schema: schema_for::<config::McpTransport>,
    },
    RegisteredSchema {
        name: "McpServerConfig",
        schema: schema_for::<config::McpServerConfig>,
    },
    RegisteredSchema {
        name: "ToolsConfig",
        schema: schema_for::<config::ToolsConfig>,
    },
    RegisteredSchema {
        name: "EnvironmentNetwork",
        schema: schema_for::<config::EnvironmentNetwork>,
    },
    RegisteredSchema {
        name: "AgentEnvironment",
        schema: schema_for::<config::AgentEnvironment>,
    },
    RegisteredSchema {
        name: "SubagentRef",
        schema: schema_for::<config::SubagentRef>,
    },
    RegisteredSchema {
        name: "SubagentRunRef",
        schema: schema_for::<subagent::SubagentRunRef>,
    },
    RegisteredSchema {
        name: "TaskStatus",
        schema: schema_for::<tasks::TaskStatus>,
    },
    RegisteredSchema {
        name: "TaskItem",
        schema: schema_for::<tasks::TaskItem>,
    },
    RegisteredSchema {
        name: "AgentEntry",
        schema: schema_for::<config::AgentEntry>,
    },
    RegisteredSchema {
        name: "SchedulerConfig",
        schema: schema_for::<config::SchedulerConfig>,
    },
    RegisteredSchema {
        name: "ChannelsConfig",
        schema: schema_for::<config::ChannelsConfig>,
    },
    RegisteredSchema {
        name: "ExtensionsConfig",
        schema: schema_for::<config::ExtensionsConfig>,
    },
    RegisteredSchema {
        name: "UiConfig",
        schema: schema_for::<config::UiConfig>,
    },
    RegisteredSchema {
        name: "Config",
        schema: schema_for::<config::Config>,
    },
    RegisteredSchema {
        name: "ConfigPatch",
        schema: schema_for::<config::ConfigPatch>,
    },
    // environment
    RegisteredSchema {
        name: "EnvironmentDefinition",
        schema: schema_for::<environment::EnvironmentDefinition>,
    },
    RegisteredSchema {
        name: "NetworkMode",
        schema: schema_for::<config::NetworkMode>,
    },
    RegisteredSchema {
        name: "EnvironmentKind",
        schema: schema_for::<environment::EnvironmentKind>,
    },
    RegisteredSchema {
        name: "ContainerRuntime",
        schema: schema_for::<environment::ContainerRuntime>,
    },
    RegisteredSchema {
        name: "ContainerCaps",
        schema: schema_for::<environment::ContainerCaps>,
    },
    RegisteredSchema {
        name: "ContainerSecurity",
        schema: schema_for::<environment::ContainerSecurity>,
    },
    RegisteredSchema {
        name: "ContainerLimits",
        schema: schema_for::<environment::ContainerLimits>,
    },
    // extension
    RegisteredSchema {
        name: "ExtensionContribution",
        schema: schema_for::<extension::ExtensionContribution>,
    },
    RegisteredSchema {
        name: "ExtensionSchemaVersion",
        schema: schema_for::<extension::ExtensionSchemaVersion>,
    },
    RegisteredSchema {
        name: "ExtensionMaxTokensParam",
        schema: schema_for::<extension::ExtensionMaxTokensParam>,
    },
    RegisteredSchema {
        name: "ExtensionProviderSpec",
        schema: schema_for::<extension::ExtensionProviderSpec>,
    },
    RegisteredSchema {
        name: "ExtensionManifest",
        schema: schema_for::<extension::ExtensionManifest>,
    },
    // automation
    RegisteredSchema {
        name: "AtSchedule",
        schema: schema_for::<automation::AtSchedule>,
    },
    RegisteredSchema {
        name: "EverySchedule",
        schema: schema_for::<automation::EverySchedule>,
    },
    RegisteredSchema {
        name: "CronSchedule",
        schema: schema_for::<automation::CronSchedule>,
    },
    RegisteredSchema {
        name: "AutomationSchedule",
        schema: schema_for::<automation::AutomationSchedule>,
    },
    RegisteredSchema {
        name: "AutomationDelivery",
        schema: schema_for::<automation::AutomationDelivery>,
    },
    RegisteredSchema {
        name: "ScheduledPayload",
        schema: schema_for::<automation::ScheduledPayload>,
    },
    RegisteredSchema {
        name: "HeartbeatPayload",
        schema: schema_for::<automation::HeartbeatPayload>,
    },
    RegisteredSchema {
        name: "AutomationPayload",
        schema: schema_for::<automation::AutomationPayload>,
    },
    RegisteredSchema {
        name: "RunStatus",
        schema: schema_for::<automation::RunStatus>,
    },
    RegisteredSchema {
        name: "AutomationJobState",
        schema: schema_for::<automation::AutomationJobState>,
    },
    RegisteredSchema {
        name: "AutomationJob",
        schema: schema_for::<automation::AutomationJob>,
    },
    RegisteredSchema {
        name: "AutomationRun",
        schema: schema_for::<automation::AutomationRun>,
    },
    RegisteredSchema {
        name: "CreateAutomationJob",
        schema: schema_for::<automation::CreateAutomationJob>,
    },
    RegisteredSchema {
        name: "UpdateAutomationJob",
        schema: schema_for::<automation::UpdateAutomationJob>,
    },
    // websocket
    RegisteredSchema {
        name: "Attachment",
        schema: schema_for::<ws::Attachment>,
    },
    RegisteredSchema {
        name: "PingMessage",
        schema: schema_for::<ws::PingMessage>,
    },
    RegisteredSchema {
        name: "UserMessageRequest",
        schema: schema_for::<ws::UserMessageRequest>,
    },
    RegisteredSchema {
        name: "RegenerateMessage",
        schema: schema_for::<ws::RegenerateMessage>,
    },
    RegisteredSchema {
        name: "EditMessage",
        schema: schema_for::<ws::EditMessage>,
    },
    RegisteredSchema {
        name: "StopTurnMessage",
        schema: schema_for::<ws::StopTurnMessage>,
    },
    RegisteredSchema {
        name: "NewSessionMessage",
        schema: schema_for::<ws::NewSessionMessage>,
    },
    RegisteredSchema {
        name: "SwitchSessionMessage",
        schema: schema_for::<ws::SwitchSessionMessage>,
    },
    RegisteredSchema {
        name: "ResumeSessionMessage",
        schema: schema_for::<ws::ResumeSessionMessage>,
    },
    RegisteredSchema {
        name: "ToolApproveMessage",
        schema: schema_for::<ws::ToolApproveMessage>,
    },
    RegisteredSchema {
        name: "SteerMessage",
        schema: schema_for::<ws::SteerMessage>,
    },
    RegisteredSchema {
        name: "ClientMessage",
        schema: schema_for::<ws::ClientMessage>,
    },
    RegisteredSchema {
        name: "ConnectedEvent",
        schema: schema_for::<ws::ConnectedEvent>,
    },
    RegisteredSchema {
        name: "PongEvent",
        schema: schema_for::<ws::PongEvent>,
    },
    RegisteredSchema {
        name: "ErrorCode",
        schema: schema_for::<ws::ErrorCode>,
    },
    RegisteredSchema {
        name: "ErrorEvent",
        schema: schema_for::<ws::ErrorEvent>,
    },
    RegisteredSchema {
        name: "MessageAckEvent",
        schema: schema_for::<ws::MessageAckEvent>,
    },
    RegisteredSchema {
        name: "MessageQueuedEvent",
        schema: schema_for::<ws::MessageQueuedEvent>,
    },
    RegisteredSchema {
        name: "TurnStartEvent",
        schema: schema_for::<ws::TurnStartEvent>,
    },
    RegisteredSchema {
        name: "AssistantDeltaEvent",
        schema: schema_for::<ws::AssistantDeltaEvent>,
    },
    RegisteredSchema {
        name: "ReasoningDeltaEvent",
        schema: schema_for::<ws::ReasoningDeltaEvent>,
    },
    RegisteredSchema {
        name: "ToolCallEvent",
        schema: schema_for::<ws::ToolCallEvent>,
    },
    RegisteredSchema {
        name: "ToolProgressEvent",
        schema: schema_for::<ws::ToolProgressEvent>,
    },
    RegisteredSchema {
        name: "ToolResultEvent",
        schema: schema_for::<ws::ToolResultEvent>,
    },
    RegisteredSchema {
        name: "ToolApprovalRequestEvent",
        schema: schema_for::<ws::ToolApprovalRequestEvent>,
    },
    RegisteredSchema {
        name: "NoticeKind",
        schema: schema_for::<ws::NoticeKind>,
    },
    RegisteredSchema {
        name: "NoticeEvent",
        schema: schema_for::<ws::NoticeEvent>,
    },
    RegisteredSchema {
        name: "TurnEndEvent",
        schema: schema_for::<ws::TurnEndEvent>,
    },
    RegisteredSchema {
        name: "NestedAgentEvent",
        schema: schema_for::<ws::NestedAgentEvent>,
    },
    RegisteredSchema {
        name: "SubagentEvent",
        schema: schema_for::<ws::SubagentEvent>,
    },
    RegisteredSchema {
        name: "ContextUsageEvent",
        schema: schema_for::<ws::ContextUsageEvent>,
    },
    RegisteredSchema {
        name: "SessionStatusEvent",
        schema: schema_for::<ws::SessionStatusEvent>,
    },
    RegisteredSchema {
        name: "SessionResetEvent",
        schema: schema_for::<ws::SessionResetEvent>,
    },
    RegisteredSchema {
        name: "SessionReplayEvent",
        schema: schema_for::<ws::SessionReplayEvent>,
    },
    RegisteredSchema {
        name: "SessionTruncatedEvent",
        schema: schema_for::<ws::SessionTruncatedEvent>,
    },
    RegisteredSchema {
        name: "NotificationEvent",
        schema: schema_for::<ws::NotificationEvent>,
    },
    RegisteredSchema {
        name: "ToolsChangedEvent",
        schema: schema_for::<ws::ToolsChangedEvent>,
    },
    RegisteredSchema {
        name: "SteerEvent",
        schema: schema_for::<ws::SteerEvent>,
    },
    RegisteredSchema {
        name: "ServerMessage",
        schema: schema_for::<ws::ServerMessage>,
    },
    // rest
    RegisteredSchema {
        name: "ErrorResponse",
        schema: schema_for::<rest::ErrorResponse>,
    },
    RegisteredSchema {
        name: "PaginationQuery",
        schema: schema_for::<rest::PaginationQuery>,
    },
    RegisteredSchema {
        name: "StatusResponse",
        schema: schema_for::<rest::StatusResponse>,
    },
    RegisteredSchema {
        name: "HealthCheck",
        schema: schema_for::<rest::HealthCheck>,
    },
    RegisteredSchema {
        name: "HealthResponse",
        schema: schema_for::<rest::HealthResponse>,
    },
    RegisteredSchema {
        name: "ConfigWarning",
        schema: schema_for::<rest::ConfigWarning>,
    },
    RegisteredSchema {
        name: "AgentRename",
        schema: schema_for::<rest::AgentRename>,
    },
    RegisteredSchema {
        name: "ChannelStatus",
        schema: schema_for::<rest::ChannelStatus>,
    },
    RegisteredSchema {
        name: "SettingsPatchRequest",
        schema: schema_for::<rest::SettingsPatchRequest>,
    },
    RegisteredSchema {
        name: "SettingsResponse",
        schema: schema_for::<rest::SettingsResponse>,
    },
    RegisteredSchema {
        name: "SetCredentialRequest",
        schema: schema_for::<rest::SetCredentialRequest>,
    },
    RegisteredSchema {
        name: "ProviderInfo",
        schema: schema_for::<rest::ProviderInfo>,
    },
    RegisteredSchema {
        name: "ProviderInstanceInfo",
        schema: schema_for::<rest::ProviderInstanceInfo>,
    },
    RegisteredSchema {
        name: "ProvidersResponse",
        schema: schema_for::<rest::ProvidersResponse>,
    },
    RegisteredSchema {
        name: "ModelInfo",
        schema: schema_for::<rest::ModelInfo>,
    },
    RegisteredSchema {
        name: "ModelsResponse",
        schema: schema_for::<rest::ModelsResponse>,
    },
    RegisteredSchema {
        name: "ProviderTestRequest",
        schema: schema_for::<rest::ProviderTestRequest>,
    },
    RegisteredSchema {
        name: "ProviderTestResponse",
        schema: schema_for::<rest::ProviderTestResponse>,
    },
    RegisteredSchema {
        name: "SessionSummary",
        schema: schema_for::<rest::SessionSummary>,
    },
    RegisteredSchema {
        name: "SessionListResponse",
        schema: schema_for::<rest::SessionListResponse>,
    },
    RegisteredSchema {
        name: "SessionMessagesResponse",
        schema: schema_for::<rest::SessionMessagesResponse>,
    },
    RegisteredSchema {
        name: "CreateSessionRequest",
        schema: schema_for::<rest::CreateSessionRequest>,
    },
    RegisteredSchema {
        name: "UpdateSessionRequest",
        schema: schema_for::<rest::UpdateSessionRequest>,
    },
    RegisteredSchema {
        name: "ContextResponse",
        schema: schema_for::<rest::ContextResponse>,
    },
    RegisteredSchema {
        name: "TurnStats",
        schema: schema_for::<rest::TurnStats>,
    },
    RegisteredSchema {
        name: "TurnStatsResponse",
        schema: schema_for::<rest::TurnStatsResponse>,
    },
    RegisteredSchema {
        name: "BranchSessionRequest",
        schema: schema_for::<rest::BranchSessionRequest>,
    },
    RegisteredSchema {
        name: "AgentSummary",
        schema: schema_for::<rest::AgentSummary>,
    },
    RegisteredSchema {
        name: "AgentListResponse",
        schema: schema_for::<rest::AgentListResponse>,
    },
    RegisteredSchema {
        name: "ToolListResponse",
        schema: schema_for::<rest::ToolListResponse>,
    },
    RegisteredSchema {
        name: "TasksResponse",
        schema: schema_for::<rest::TasksResponse>,
    },
    RegisteredSchema {
        name: "EnvironmentSummary",
        schema: schema_for::<rest::EnvironmentSummary>,
    },
    RegisteredSchema {
        name: "EnvironmentListResponse",
        schema: schema_for::<rest::EnvironmentListResponse>,
    },
    RegisteredSchema {
        name: "SandboxInstanceSummary",
        schema: schema_for::<rest::SandboxInstanceSummary>,
    },
    RegisteredSchema {
        name: "SandboxListResponse",
        schema: schema_for::<rest::SandboxListResponse>,
    },
    RegisteredSchema {
        name: "SandboxRequest",
        schema: schema_for::<rest::SandboxRequest>,
    },
    RegisteredSchema {
        name: "ResolveImageResponse",
        schema: schema_for::<rest::ResolveImageResponse>,
    },
    RegisteredSchema {
        name: "McpServerState",
        schema: schema_for::<rest::McpServerState>,
    },
    RegisteredSchema {
        name: "McpServerStatus",
        schema: schema_for::<rest::McpServerStatus>,
    },
    RegisteredSchema {
        name: "McpStatusResponse",
        schema: schema_for::<rest::McpStatusResponse>,
    },
    RegisteredSchema {
        name: "FileEntry",
        schema: schema_for::<rest::FileEntry>,
    },
    RegisteredSchema {
        name: "FileListResponse",
        schema: schema_for::<rest::FileListResponse>,
    },
    RegisteredSchema {
        name: "SignedUrl",
        schema: schema_for::<rest::SignedUrl>,
    },
    RegisteredSchema {
        name: "SignedUrlRequest",
        schema: schema_for::<rest::SignedUrlRequest>,
    },
    RegisteredSchema {
        name: "UploadResponse",
        schema: schema_for::<rest::UploadResponse>,
    },
    RegisteredSchema {
        name: "FileTextResponse",
        schema: schema_for::<rest::FileTextResponse>,
    },
    RegisteredSchema {
        name: "FileWriteRequest",
        schema: schema_for::<rest::FileWriteRequest>,
    },
    RegisteredSchema {
        name: "CreateDirectoryRequest",
        schema: schema_for::<rest::CreateDirectoryRequest>,
    },
    RegisteredSchema {
        name: "MoveFileRequest",
        schema: schema_for::<rest::MoveFileRequest>,
    },
    RegisteredSchema {
        name: "WorkspaceSummary",
        schema: schema_for::<rest::WorkspaceSummary>,
    },
    RegisteredSchema {
        name: "WorkspaceListResponse",
        schema: schema_for::<rest::WorkspaceListResponse>,
    },
    RegisteredSchema {
        name: "CreateWorkspaceRequest",
        schema: schema_for::<rest::CreateWorkspaceRequest>,
    },
    RegisteredSchema {
        name: "UpdateWorkspaceRequest",
        schema: schema_for::<rest::UpdateWorkspaceRequest>,
    },
    RegisteredSchema {
        name: "MoveSessionsRequest",
        schema: schema_for::<rest::MoveSessionsRequest>,
    },
    RegisteredSchema {
        name: "MoveSessionsResponse",
        schema: schema_for::<rest::MoveSessionsResponse>,
    },
    RegisteredSchema {
        name: "Notification",
        schema: schema_for::<rest::Notification>,
    },
    RegisteredSchema {
        name: "NotificationListResponse",
        schema: schema_for::<rest::NotificationListResponse>,
    },
    RegisteredSchema {
        name: "AutomationJobListResponse",
        schema: schema_for::<rest::AutomationJobListResponse>,
    },
    RegisteredSchema {
        name: "AutomationRunListResponse",
        schema: schema_for::<rest::AutomationRunListResponse>,
    },
    RegisteredSchema {
        name: "Username",
        schema: schema_for::<rest::Username>,
    },
    RegisteredSchema {
        name: "NewPassword",
        schema: schema_for::<rest::NewPassword>,
    },
    RegisteredSchema {
        name: "PresentedPassword",
        schema: schema_for::<rest::PresentedPassword>,
    },
    RegisteredSchema {
        name: "LoginRequest",
        schema: schema_for::<rest::LoginRequest>,
    },
    RegisteredSchema {
        name: "LoginResponse",
        schema: schema_for::<rest::LoginResponse>,
    },
    RegisteredSchema {
        name: "AuthSessionResponse",
        schema: schema_for::<rest::AuthSessionResponse>,
    },
    RegisteredSchema {
        name: "SetupStatusResponse",
        schema: schema_for::<rest::SetupStatusResponse>,
    },
    RegisteredSchema {
        name: "SetupClaimRequest",
        schema: schema_for::<rest::SetupClaimRequest>,
    },
    RegisteredSchema {
        name: "SetupPasswordRequest",
        schema: schema_for::<rest::SetupPasswordRequest>,
    },
    RegisteredSchema {
        name: "ExtensionState",
        schema: schema_for::<rest::ExtensionState>,
    },
    RegisteredSchema {
        name: "ExtensionStatus",
        schema: schema_for::<rest::ExtensionStatus>,
    },
    RegisteredSchema {
        name: "ExtensionListResponse",
        schema: schema_for::<rest::ExtensionListResponse>,
    },
    RegisteredSchema {
        name: "ExtensionCommand",
        schema: schema_for::<rest::ExtensionCommand>,
    },
    RegisteredSchema {
        name: "CommandListResponse",
        schema: schema_for::<rest::CommandListResponse>,
    },
    RegisteredSchema {
        name: "RunCommandRequest",
        schema: schema_for::<rest::RunCommandRequest>,
    },
    RegisteredSchema {
        name: "RunCommandResponse",
        schema: schema_for::<rest::RunCommandResponse>,
    },
];

/// The registered schema of that name, if any.
pub fn registered(name: &str) -> Option<&'static RegisteredSchema> {
    PROTOCOL_SCHEMAS.iter().find(|entry| entry.name == name)
}
