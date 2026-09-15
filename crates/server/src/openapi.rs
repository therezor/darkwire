//! The OpenAPI 3.1 document, emitted by hand from the manifest.
//!
//! Hand-emitted rather than derived from an annotation macro, because the
//! manifest already knows every route and the protocol crate already knows
//! every schema: a macro would be a third statement of both, and the way a
//! generated document starts lying is by having a source of its own.
//!
//! So this walks [`ROUTE_MANIFEST`], attaches each route's request and response
//! `$ref`s from the table below, and publishes the protocol crate's schemas
//! under `components.schemas`. A route references rather than restates —
//! restating is the other way a document starts lying.
//!
//! Parity with the TypeScript document is enforced by the protocol crate's
//! per-schema drift test plus the route manifest, not by diffing whole
//! documents: two documents can differ in key order and description wording
//! and still describe the same API, and a diff that fails on those is a gate
//! nobody can keep green.

use schemars::{JsonSchema, SchemaGenerator};
use serde_json::{Map, Value, json};

use crate::auth::SESSION_COOKIE;
use crate::manifest::{ROUTE_MANIFEST, Route, RouteAuth, RouteId, RouteMethod};
use crate::queries::{
    DeleteQuery, NotificationListQuery, OptionalPathQuery, PageQuery, PathQuery, SessionListQuery,
    TurnsQuery, WsQuery,
};
use crate::schema::{PROTOCOL_COMPONENTS, component_ref};
use crate::version::SERVER_VERSION;

/// A query shape one route accepts, named so the table can stay a `const`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryShape {
    /// A page of rows.
    PageQuery,
    /// The session listing's filters.
    SessionListQuery,
    /// The notification listing's filters.
    NotificationListQuery,
    /// The socket's parameters.
    WsQuery,
    /// A workspace-relative path.
    PathQuery,
    /// A workspace-relative path that defaults to the root.
    OptionalPathQuery,
    /// A delete, and whether it may recurse.
    DeleteQuery,
    /// The turn listing's bound.
    TurnsQuery,
}

/// A path-parameter shape one route accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathShape {
    /// Addressed by session key.
    SessionParams,
    /// Addressed by id.
    IdParams,
    /// Addressed by signed token.
    TokenParams,
}

/// What one route contributes to the document, beyond its manifest entry.
#[derive(Debug, Clone, Copy)]
pub struct RouteDoc {
    /// Which route.
    pub id: RouteId,
    /// The operation summary.
    pub summary: &'static str,
    /// The registered protocol schema a request body is, if it takes one.
    pub body: Option<&'static str>,
    /// The query shape, if it takes one.
    pub query: Option<QueryShape>,
    /// The path-parameter shape, if it takes one.
    pub params: Option<PathShape>,
    /// Status to registered protocol schema, for the answers that carry a body.
    pub responses: &'static [(u16, &'static str)],
}

/// Every route's contribution, in manifest order.
pub static ROUTE_DOCS: &[RouteDoc] = &[
    RouteDoc {
        id: RouteId::AgentsList,
        summary: "Every agent that can run a turn",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "AgentListResponse")],
    },
    RouteDoc {
        id: RouteId::AuthLogin,
        summary: "Exchange the username and password for a session",
        body: Some("LoginRequest"),
        query: None,
        params: None,
        responses: &[(200, "LoginResponse")],
    },
    RouteDoc {
        id: RouteId::AuthLogout,
        summary: "Revoke the presented session",
        body: None,
        query: None,
        params: None,
        responses: &[],
    },
    RouteDoc {
        id: RouteId::AuthMe,
        summary: "Whether the caller is authenticated, and until when",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "AuthSessionResponse")],
    },
    RouteDoc {
        id: RouteId::SetupStatus,
        summary: "Whether this install still has to be claimed",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "SetupStatusResponse")],
    },
    RouteDoc {
        id: RouteId::SetupClaim,
        summary: "Spend the one-time code printed at startup for a session",
        body: Some("SetupClaimRequest"),
        query: None,
        params: None,
        responses: &[(200, "LoginResponse")],
    },
    RouteDoc {
        id: RouteId::SetupPassword,
        summary: "Set the login password and name, finishing the claim or rotating both",
        body: Some("SetupPasswordRequest"),
        query: None,
        params: None,
        responses: &[(200, "LoginResponse")],
    },
    RouteDoc {
        id: RouteId::AutomationList,
        summary: "Every scheduled job",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "AutomationJobListResponse")],
    },
    RouteDoc {
        id: RouteId::AutomationCreate,
        summary: "Create a scheduled job",
        body: Some("CreateAutomationJob"),
        query: None,
        params: None,
        responses: &[(201, "AutomationJob")],
    },
    RouteDoc {
        id: RouteId::AutomationGet,
        summary: "One scheduled job",
        body: None,
        query: None,
        params: Some(PathShape::IdParams),
        responses: &[(200, "AutomationJob")],
    },
    RouteDoc {
        id: RouteId::AutomationUpdate,
        summary: "Change a scheduled job",
        body: Some("UpdateAutomationJob"),
        query: None,
        params: Some(PathShape::IdParams),
        responses: &[(200, "AutomationJob")],
    },
    RouteDoc {
        id: RouteId::AutomationDelete,
        summary: "Delete a scheduled job and its history",
        body: None,
        query: None,
        params: Some(PathShape::IdParams),
        responses: &[],
    },
    RouteDoc {
        id: RouteId::AutomationRun,
        summary: "Run a scheduled job now",
        body: None,
        query: None,
        params: Some(PathShape::IdParams),
        responses: &[(202, "AutomationRun")],
    },
    RouteDoc {
        id: RouteId::AutomationRuns,
        summary: "One job's run history, newest first",
        body: None,
        query: Some(QueryShape::PageQuery),
        params: Some(PathShape::IdParams),
        responses: &[(200, "AutomationRunListResponse")],
    },
    RouteDoc {
        id: RouteId::CommandsList,
        summary: "Every slash command extensions contribute",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "CommandListResponse")],
    },
    RouteDoc {
        id: RouteId::CommandsRun,
        summary: "Run one extension command",
        body: Some("RunCommandRequest"),
        query: None,
        params: None,
        responses: &[(200, "RunCommandResponse")],
    },
    RouteDoc {
        id: RouteId::ExtensionsList,
        summary: "Every installed extension and the state it is in",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "ExtensionListResponse")],
    },
    RouteDoc {
        id: RouteId::ExtensionsApprove,
        summary: "Approve the files an extension currently holds, and load it",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "ExtensionListResponse")],
    },
    RouteDoc {
        id: RouteId::ExtensionsRevoke,
        summary: "Forget an extension's approval and unload it",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "ExtensionListResponse")],
    },
    RouteDoc {
        id: RouteId::FilesList,
        summary: "One workspace directory",
        body: None,
        query: Some(QueryShape::OptionalPathQuery),
        params: None,
        responses: &[(200, "FileListResponse")],
    },
    RouteDoc {
        id: RouteId::FilesDelete,
        summary: "Delete one workspace file or directory",
        body: None,
        query: Some(QueryShape::DeleteQuery),
        params: None,
        responses: &[],
    },
    RouteDoc {
        id: RouteId::FilesUpload,
        summary: "Write a file into the workspace",
        body: None,
        query: Some(QueryShape::PathQuery),
        params: None,
        responses: &[(201, "UploadResponse")],
    },
    RouteDoc {
        id: RouteId::FilesRead,
        summary: "One workspace file, as text",
        body: None,
        query: Some(QueryShape::PathQuery),
        params: None,
        responses: &[(200, "FileTextResponse")],
    },
    RouteDoc {
        id: RouteId::FilesWrite,
        summary: "Write text to a workspace file",
        body: Some("FileWriteRequest"),
        query: None,
        params: None,
        responses: &[(200, "FileEntry")],
    },
    RouteDoc {
        id: RouteId::FilesMkdir,
        summary: "Create a workspace directory",
        body: Some("CreateDirectoryRequest"),
        query: None,
        params: None,
        responses: &[(201, "FileEntry")],
    },
    RouteDoc {
        id: RouteId::FilesMove,
        summary: "Rename or move a workspace entry",
        body: Some("MoveFileRequest"),
        query: None,
        params: None,
        responses: &[(200, "FileEntry")],
    },
    RouteDoc {
        id: RouteId::FilesSign,
        summary: "Mint a short-lived URL an <img> can load",
        body: Some("SignedUrlRequest"),
        query: None,
        params: None,
        responses: &[(200, "SignedUrl")],
    },
    RouteDoc {
        id: RouteId::MediaGet,
        summary: "Serve a workspace file to a signed URL",
        body: None,
        query: None,
        params: Some(PathShape::TokenParams),
        responses: &[],
    },
    RouteDoc {
        id: RouteId::McpList,
        summary: "Every configured MCP server and its connection state",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "McpStatusResponse")],
    },
    RouteDoc {
        id: RouteId::NotificationsList,
        summary: "Notifications, newest first",
        body: None,
        query: Some(QueryShape::NotificationListQuery),
        params: None,
        responses: &[(200, "NotificationListResponse")],
    },
    RouteDoc {
        id: RouteId::NotificationsRead,
        summary: "Mark one notification read",
        body: None,
        query: None,
        params: Some(PathShape::IdParams),
        responses: &[(200, "Notification")],
    },
    RouteDoc {
        id: RouteId::NotificationsReadAll,
        summary: "Mark every notification read",
        body: None,
        query: None,
        params: None,
        responses: &[],
    },
    RouteDoc {
        id: RouteId::NotificationsDeleteAll,
        summary: "Delete every notification",
        body: None,
        query: None,
        params: None,
        responses: &[],
    },
    RouteDoc {
        id: RouteId::NotificationsDelete,
        summary: "Delete one notification",
        body: None,
        query: None,
        params: Some(PathShape::IdParams),
        responses: &[],
    },
    RouteDoc {
        id: RouteId::ProvidersList,
        summary: "The provider catalogue, and every endpoint configured from it",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "ProvidersResponse")],
    },
    RouteDoc {
        id: RouteId::ProvidersTest,
        summary: "Ask one provider connection whether it answers, and with what",
        body: Some("ProviderTestRequest"),
        query: None,
        params: None,
        responses: &[(200, "ProviderTestResponse")],
    },
    RouteDoc {
        id: RouteId::ModelsList,
        summary: "Models available to the configured provider instances",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "ModelsResponse")],
    },
    RouteDoc {
        id: RouteId::ModelsRefresh,
        summary: "Re-fetch every provider instance model list, ignoring the cache",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "ModelsResponse")],
    },
    RouteDoc {
        id: RouteId::SessionsList,
        summary: "Sessions, newest activity first",
        body: None,
        query: Some(QueryShape::SessionListQuery),
        params: None,
        responses: &[(200, "SessionListResponse")],
    },
    RouteDoc {
        id: RouteId::SessionsCreate,
        summary: "Create a session, or return the existing one for a key",
        body: Some("CreateSessionRequest"),
        query: None,
        params: None,
        responses: &[(201, "SessionSummary")],
    },
    RouteDoc {
        id: RouteId::SessionsGet,
        summary: "One session",
        body: None,
        query: None,
        params: Some(PathShape::SessionParams),
        responses: &[(200, "SessionSummary")],
    },
    RouteDoc {
        id: RouteId::SessionsUpdate,
        summary: "Rename a session, or move it to another agent or workspace",
        body: Some("UpdateSessionRequest"),
        query: None,
        params: Some(PathShape::SessionParams),
        responses: &[(200, "SessionSummary")],
    },
    RouteDoc {
        id: RouteId::SessionsDelete,
        summary: "Delete a session and its messages",
        body: None,
        query: None,
        params: Some(PathShape::SessionParams),
        responses: &[],
    },
    RouteDoc {
        id: RouteId::SessionsMessages,
        summary: "A session transcript, oldest first",
        body: None,
        query: Some(QueryShape::PageQuery),
        params: Some(PathShape::SessionParams),
        responses: &[(200, "SessionMessagesResponse")],
    },
    RouteDoc {
        id: RouteId::SessionsClear,
        summary: "Drop a session transcript, keeping the session",
        body: None,
        query: None,
        params: Some(PathShape::SessionParams),
        responses: &[],
    },
    RouteDoc {
        id: RouteId::SessionsContext,
        summary: "What the agent would send to the model for this session",
        body: None,
        query: None,
        params: Some(PathShape::SessionParams),
        responses: &[(200, "ContextResponse")],
    },
    RouteDoc {
        id: RouteId::SessionsBranch,
        summary: "Fork a session at a point into a new one",
        body: Some("BranchSessionRequest"),
        query: None,
        params: Some(PathShape::SessionParams),
        responses: &[(201, "SessionSummary")],
    },
    RouteDoc {
        id: RouteId::SessionsTurns,
        summary: "What each turn in this session cost",
        body: None,
        query: Some(QueryShape::TurnsQuery),
        params: Some(PathShape::SessionParams),
        responses: &[(200, "TurnStatsResponse")],
    },
    RouteDoc {
        id: RouteId::SettingsGet,
        summary: "The settings tree, with credentials replaced by presence flags",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "SettingsResponse")],
    },
    RouteDoc {
        id: RouteId::SettingsPatch,
        summary: "Apply a settings patch and rebuild what depends on it",
        body: Some("SettingsPatchRequest"),
        query: None,
        params: None,
        responses: &[(200, "SettingsResponse")],
    },
    RouteDoc {
        id: RouteId::SettingsReload,
        summary: "Re-read config.yaml from disk and rebuild what depends on it",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "SettingsResponse")],
    },
    RouteDoc {
        id: RouteId::SettingsCredential,
        summary: "Store or clear one credential (write-only)",
        body: Some("SetCredentialRequest"),
        query: None,
        params: None,
        responses: &[],
    },
    RouteDoc {
        id: RouteId::SystemHealth,
        summary: "Liveness and the checks behind it",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "HealthResponse")],
    },
    RouteDoc {
        id: RouteId::SystemStatus,
        summary: "Version, uptime, and what a turn would use right now",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "StatusResponse")],
    },
    RouteDoc {
        id: RouteId::SystemOpenapi,
        summary: "The generated OpenAPI 3.1 document",
        body: None,
        query: None,
        params: None,
        responses: &[],
    },
    RouteDoc {
        id: RouteId::ToolboxesList,
        summary: "Toolboxes installed on this machine",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "ToolboxListResponse")],
    },
    RouteDoc {
        id: RouteId::ContainersList,
        summary: "Container definitions installed on this machine",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "ContainerListResponse")],
    },
    RouteDoc {
        id: RouteId::SandboxesList,
        summary: "Live sandbox instances",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "SandboxListResponse")],
    },
    RouteDoc {
        id: RouteId::SandboxesManage,
        summary: "Manage approved sandbox instances",
        body: Some("SandboxRequest"),
        query: None,
        params: None,
        // No typed response: this route forwards whatever op it was given and
        // answers with the service's own JSON, so `stop` and `health` each
        // return a different shape. Naming one of them would document a
        // guarantee the route does not make.
        responses: &[],
    },
    RouteDoc {
        id: RouteId::ToolsList,
        summary: "Every tool the registry holds, whoever may call it",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "ToolListResponse")],
    },
    RouteDoc {
        id: RouteId::WorkspacesList,
        summary: "Every workspace, the default first",
        body: None,
        query: None,
        params: None,
        responses: &[(200, "WorkspaceListResponse")],
    },
    RouteDoc {
        id: RouteId::WorkspacesCreate,
        summary: "Create a workspace and its folder",
        body: Some("CreateWorkspaceRequest"),
        query: None,
        params: None,
        responses: &[(201, "WorkspaceSummary")],
    },
    RouteDoc {
        id: RouteId::WorkspacesUpdate,
        summary: "Rename a workspace, move its folder, or both",
        body: Some("UpdateWorkspaceRequest"),
        query: None,
        params: Some(PathShape::IdParams),
        responses: &[(200, "WorkspaceSummary")],
    },
    RouteDoc {
        id: RouteId::WorkspacesDelete,
        summary: "Detach a workspace, keeping its files",
        body: None,
        query: None,
        params: Some(PathShape::IdParams),
        responses: &[],
    },
    RouteDoc {
        id: RouteId::WorkspacesMoveSessions,
        summary: "Move every session in a workspace to another one",
        body: Some("MoveSessionsRequest"),
        query: None,
        params: Some(PathShape::IdParams),
        responses: &[(200, "MoveSessionsResponse")],
    },
    RouteDoc {
        id: RouteId::WsConnect,
        summary: "Upgrade to the GhostAI WebSocket protocol",
        body: None,
        query: Some(QueryShape::WsQuery),
        params: None,
        responses: &[],
    },
];

/// The whole document.
pub fn openapi_document() -> Value {
    let mut paths: Map<String, Value> = Map::new();
    for route in ROUTE_MANIFEST {
        let Some(doc) = ROUTE_DOCS.iter().find(|entry| entry.id == route.id) else {
            // A `test-hooks` route, which is deliberately not part of the API a
            // client may rely on.
            continue;
        };
        let entry = paths
            .entry(document_path(route.path))
            .or_insert_with(|| json!({}));
        if let Some(object) = entry.as_object_mut() {
            object.insert(method_key(route.method).to_owned(), operation(route, doc));
        }
    }

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "GhostAI",
            "version": SERVER_VERSION,
            "description": "Generated from the schemas in ghostai-protocol.",
        },
        "paths": Value::Object(paths),
        "components": {
            // The `$defs` pool: every protocol schema, so a route references
            // rather than restates.
            "schemas": Value::Object(
                PROTOCOL_COMPONENTS
                    .iter()
                    .map(|(name, schema)| (name.clone(), schema.clone()))
                    .collect(),
            ),
            "securitySchemes": {
                "cookieAuth": {"type": "apiKey", "in": "cookie", "name": SESSION_COOKIE},
                "bearerAuth": {"type": "http", "scheme": "bearer"},
            },
        },
    })
}

/// One operation object.
fn operation(route: &Route, doc: &RouteDoc) -> Value {
    let mut operation = Map::new();
    operation.insert("operationId".to_owned(), json!(route.id.as_str()));
    operation.insert("summary".to_owned(), json!(doc.summary));

    // Either carrier satisfies it, which is what a list of two alternatives
    // means in OpenAPI. A signed route lists neither: its credential is the
    // path, and a document that named a scheme here would tell a client to
    // attach one that is not accepted.
    operation.insert(
        "security".to_owned(),
        match route.auth {
            RouteAuth::Required => json!([{"cookieAuth": []}, {"bearerAuth": []}]),
            RouteAuth::Public | RouteAuth::Signed => json!([]),
        },
    );

    let mut parameters = Vec::new();
    if let Some(shape) = doc.params {
        parameters.extend(parameters_from(&path_schema(shape), "path"));
    }
    if let Some(shape) = doc.query {
        parameters.extend(parameters_from(&query_schema(shape), "query"));
    }
    if !parameters.is_empty() {
        operation.insert("parameters".to_owned(), Value::Array(parameters));
    }

    if let Some(name) = doc.body {
        operation.insert(
            "requestBody".to_owned(),
            json!({
                "required": true,
                "content": {"application/json": {"schema": component_ref(name)}},
            }),
        );
    }

    let mut responses = Map::new();
    for (status, name) in doc.responses {
        responses.insert(
            status.to_string(),
            json!({
                "description": "OK",
                "content": {"application/json": {"schema": component_ref(name)}},
            }),
        );
    }
    if responses.is_empty() {
        // Every operation must carry at least one response, and a route that
        // answers with no body still answers.
        responses.insert("200".to_owned(), json!({"description": "OK"}));
    }
    responses.insert(
        "default".to_owned(),
        json!({
            "description": "Error",
            "content": {
                "application/json": {"schema": component_ref("ErrorResponse")},
            },
        }),
    );
    operation.insert("responses".to_owned(), Value::Object(responses));

    Value::Object(operation)
}

/// The manifest's `:key` is the document's `{key}`.
fn document_path(path: &str) -> String {
    path.split('/')
        .map(|segment| match segment.strip_prefix(':') {
            Some(name) => format!("{{{name}}}"),
            None => segment.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The lowercase method key an operation hangs off.
fn method_key(method: RouteMethod) -> &'static str {
    match method {
        RouteMethod::GET => "get",
        RouteMethod::POST => "post",
        RouteMethod::PATCH => "patch",
        RouteMethod::PUT => "put",
        RouteMethod::DELETE => "delete",
    }
}

/// One object schema flattened into OpenAPI parameter objects.
///
/// Every field becomes one parameter, carrying the schema the route actually
/// enforces — including the cap and the default, so a client knows the bound
/// before it is refused rather than after.
fn parameters_from(schema: &Value, location: &str) -> Vec<Value> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    properties
        .iter()
        .map(|(name, property)| {
            json!({
                "name": name,
                "in": location,
                // A path parameter is required by definition: there is no
                // request that reaches the route without it.
                "required": location == "path" || required.contains(&name.as_str()),
                "schema": property,
            })
        })
        .collect()
}

/// The generated schema for one query shape, with the same settings the
/// protocol pool uses so a parameter reads like the rest of the document.
fn query_schema(shape: QueryShape) -> Value {
    let mut generator = ghostai_protocol::schemas::protocol_generator();
    match shape {
        QueryShape::PageQuery => schema_of::<PageQuery>(&mut generator),
        QueryShape::SessionListQuery => schema_of::<SessionListQuery>(&mut generator),
        QueryShape::NotificationListQuery => schema_of::<NotificationListQuery>(&mut generator),
        QueryShape::WsQuery => schema_of::<WsQuery>(&mut generator),
        QueryShape::PathQuery => schema_of::<PathQuery>(&mut generator),
        QueryShape::OptionalPathQuery => schema_of::<OptionalPathQuery>(&mut generator),
        QueryShape::DeleteQuery => schema_of::<DeleteQuery>(&mut generator),
        QueryShape::TurnsQuery => schema_of::<TurnsQuery>(&mut generator),
    }
}

/// The generated schema for one path-parameter shape.
fn path_schema(shape: PathShape) -> Value {
    let mut generator = ghostai_protocol::schemas::protocol_generator();
    match shape {
        PathShape::SessionParams => schema_of::<crate::queries::SessionParams>(&mut generator),
        PathShape::IdParams => schema_of::<crate::queries::IdParams>(&mut generator),
        PathShape::TokenParams => schema_of::<crate::queries::TokenParams>(&mut generator),
    }
}

fn schema_of<T: JsonSchema>(generator: &mut SchemaGenerator) -> Value {
    let mut schema = generator.subschema_for::<T>().to_value();
    // A flattened shape resolves through `$defs`, and a parameter list has
    // nowhere to put one.
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or_default().to_owned();
        if let Some(resolved) = generator.definitions().get(&name) {
            schema = resolved.clone();
        }
    }
    schema
}
