//! Every route the server serves, and what it takes to reach it.
//!
//! The router is built *from* this array rather than beside it, which is the
//! whole reason it exists: the auth-matrix test iterates the same list, so a
//! route that appears here in any state other than the one it was written for
//! fails a test, and a route that does not appear here is not served at all.
//! "Remembered to add the auth check" is not a property a codebase can hold
//! onto across sixty-odd routes; "cannot be registered without saying which it
//! is" is.
//!
//! [`RouteId`] is what ties an entry to its implementation. The router
//! `match`es on it exhaustively, so a manifest entry with no handler and a
//! handler with no manifest entry are both compile errors rather than a 404
//! found later.

/// What it takes to reach a route.
///
/// - [`RouteAuth::Public`] — no credential needed, ever. Four routes qualify:
///   the liveness probe, the login that mints the credential, and the two
///   first-run setup routes that stop existing once a password is set.
/// - [`RouteAuth::Required`] — a valid session cookie or bearer token, unless
///   authentication is disabled for the whole server.
/// - [`RouteAuth::Signed`] — the credential is in the URL: an HMAC-signed,
///   expiring token naming one workspace path. Exactly one route uses it,
///   because `<img src>` can carry neither a header nor, reliably, a
///   `SameSite=Strict` cookie. A session is *not* accepted there and a
///   signature is not accepted anywhere else, so neither carrier widens the
///   other's reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteAuth {
    /// No credential, ever.
    Public,
    /// A session cookie or bearer token.
    Required,
    /// An HMAC-signed, expiring token in the URL.
    Signed,
}

/// The methods the manifest uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(
    clippy::upper_case_acronyms,
    reason = "these are HTTP method names, which are spelled in capitals"
)]
pub enum RouteMethod {
    /// `GET`.
    GET,
    /// `POST`.
    POST,
    /// `PATCH`.
    PATCH,
    /// `PUT`.
    PUT,
    /// `DELETE`.
    DELETE,
}

impl RouteMethod {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            RouteMethod::GET => "GET",
            RouteMethod::POST => "POST",
            RouteMethod::PATCH => "PATCH",
            RouteMethod::PUT => "PUT",
            RouteMethod::DELETE => "DELETE",
        }
    }
}

/// One served route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    /// What ties this entry to its handler.
    pub id: RouteId,
    /// The method, which is part of the identity: two entries may share a path.
    pub method: RouteMethod,
    /// The path, in the `:param` spelling the TypeScript manifest used and the
    /// OpenAPI document reports. [`Route::axum_path`] rewrites it for the
    /// router.
    pub path: &'static str,
    /// What it takes to reach it.
    pub auth: RouteAuth,
}

impl Route {
    /// The path in axum's spelling: `:key` becomes `{key}`.
    ///
    /// The manifest keeps the colon form because that is what the OpenAPI
    /// document and `docs/api.md` say, and a router syntax is not a reason to
    /// change the documented surface. axum 0.8 refuses a colon outright, so the
    /// rewrite happens once, here, on the way into the router.
    pub fn axum_path(&self) -> String {
        self.path
            .split('/')
            .map(|segment| match segment.strip_prefix(':') {
                Some(name) => format!("{{{name}}}"),
                None => segment.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// Every route, as an identifier the router matches on exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteId {
    /// `GET /api/health`
    SystemHealth,
    /// `GET /api/status`
    SystemStatus,
    /// `GET /api/openapi.json`
    SystemOpenapi,
    /// `GET /ws`
    WsConnect,
    /// `POST /api/auth/login`
    AuthLogin,
    /// `POST /api/auth/logout`
    AuthLogout,
    /// `GET /api/auth/me`
    AuthMe,
    /// `GET /api/setup`
    SetupStatus,
    /// `POST /api/setup/claim`
    SetupClaim,
    /// `POST /api/setup/password`
    SetupPassword,
    /// `GET /api/settings`
    SettingsGet,
    /// `PATCH /api/settings`
    SettingsPatch,
    /// `PUT /api/settings/credentials`
    SettingsCredential,
    /// `POST /api/settings/reload`
    SettingsReload,
    /// `GET /api/providers`
    ProvidersList,
    /// `POST /api/providers/test`
    ProvidersTest,
    /// `GET /api/models`
    ModelsList,
    /// `POST /api/models/refresh`
    ModelsRefresh,
    /// `GET /api/sessions`
    SessionsList,
    /// `POST /api/sessions`
    SessionsCreate,
    /// `GET /api/sessions/:key`
    SessionsGet,
    /// `PATCH /api/sessions/:key`
    SessionsUpdate,
    /// `DELETE /api/sessions/:key`
    SessionsDelete,
    /// `GET /api/sessions/:key/messages`
    SessionsMessages,
    /// `DELETE /api/sessions/:key/messages`
    SessionsClear,
    /// `GET /api/sessions/:key/context`
    SessionsContext,
    /// `POST /api/sessions/:key/branch`
    SessionsBranch,
    /// `GET /api/sessions/:key/turns`
    SessionsTurns,
    /// `GET /api/agents`
    AgentsList,
    /// `GET /api/tools`
    ToolsList,
    /// `GET /api/containers`
    ContainersList,
    /// `GET /api/sandboxes`.
    SandboxesList,
    /// `POST /api/sandboxes`.
    SandboxesManage,
    /// `GET /api/mcp`
    McpList,
    /// `GET /api/extensions`
    ExtensionsList,
    /// `POST /api/extensions/:id/approve`
    ExtensionsApprove,
    /// `POST /api/extensions/:id/revoke`
    ExtensionsRevoke,
    /// `GET /api/commands`
    CommandsList,
    /// `POST /api/commands/:id`
    CommandsRun,
    /// `GET /api/files`
    FilesList,
    /// `DELETE /api/files`
    FilesDelete,
    /// `POST /api/files/upload`
    FilesUpload,
    /// `GET /api/files/text`
    FilesRead,
    /// `PUT /api/files/text`
    FilesWrite,
    /// `POST /api/files/directory`
    FilesMkdir,
    /// `POST /api/files/move`
    FilesMove,
    /// `POST /api/files/signed-url`
    FilesSign,
    /// `GET /api/media/:token`
    MediaGet,
    /// `GET /api/workspaces`
    WorkspacesList,
    /// `POST /api/workspaces`
    WorkspacesCreate,
    /// `PATCH /api/workspaces/:id`
    WorkspacesUpdate,
    /// `DELETE /api/workspaces/:id`
    WorkspacesDelete,
    /// `POST /api/workspaces/:id/sessions/move`
    WorkspacesMoveSessions,
    /// `GET /api/notifications`
    NotificationsList,
    /// `POST /api/notifications/read`
    NotificationsReadAll,
    /// `POST /api/notifications/:id/read`
    NotificationsRead,
    /// `DELETE /api/notifications/:id`
    NotificationsDelete,
    /// `DELETE /api/notifications`
    NotificationsDeleteAll,
    /// `GET /api/automation/jobs`
    AutomationList,
    /// `POST /api/automation/jobs`
    AutomationCreate,
    /// `GET /api/automation/jobs/:id`
    AutomationGet,
    /// `PATCH /api/automation/jobs/:id`
    AutomationUpdate,
    /// `DELETE /api/automation/jobs/:id`
    AutomationDelete,
    /// `POST /api/automation/jobs/:id/run`
    AutomationRun,
    /// `GET /api/automation/jobs/:id/runs`
    AutomationRuns,

    // The end-to-end suite's seams. In the manifest like every other route, so
    // the auth-matrix test covers them, and `Required` like the routes they
    // stand in for: a hook that skipped authentication would be a hole the
    // moment a build shipped with the feature on. Excluded from the OpenAPI
    // document, because they are not part of the API a client may rely on.
    #[cfg(feature = "test-hooks")]
    /// `POST /api/_test/sessions`
    TestSessions,
    #[cfg(feature = "test-hooks")]
    /// `POST /api/_test/notifications`
    TestNotifications,
    #[cfg(feature = "test-hooks")]
    /// `POST /api/_test/automation/runs`
    TestAutomationRun,
    #[cfg(feature = "test-hooks")]
    /// `POST /api/_test/automation/runs/:id/finish`
    TestAutomationRunFinish,
    #[cfg(feature = "test-hooks")]
    /// `POST /api/_test/scheduler/tick`
    TestSchedulerTick,
}

impl RouteId {
    /// The dotted name the TypeScript manifest used, which is what the OpenAPI
    /// document reports as `operationId` and what a log line names.
    pub fn as_str(self) -> &'static str {
        match self {
            RouteId::SystemHealth => "system.health",
            RouteId::SystemStatus => "system.status",
            RouteId::SystemOpenapi => "system.openapi",
            RouteId::WsConnect => "ws.connect",
            RouteId::AuthLogin => "auth.login",
            RouteId::AuthLogout => "auth.logout",
            RouteId::AuthMe => "auth.me",
            RouteId::SetupStatus => "setup.status",
            RouteId::SetupClaim => "setup.claim",
            RouteId::SetupPassword => "setup.password",
            RouteId::SettingsGet => "settings.get",
            RouteId::SettingsPatch => "settings.patch",
            RouteId::SettingsCredential => "settings.credential",
            RouteId::SettingsReload => "settings.reload",
            RouteId::ProvidersList => "providers.list",
            RouteId::ProvidersTest => "providers.test",
            RouteId::ModelsList => "models.list",
            RouteId::ModelsRefresh => "models.refresh",
            RouteId::SessionsList => "sessions.list",
            RouteId::SessionsCreate => "sessions.create",
            RouteId::SessionsGet => "sessions.get",
            RouteId::SessionsUpdate => "sessions.update",
            RouteId::SessionsDelete => "sessions.delete",
            RouteId::SessionsMessages => "sessions.messages",
            RouteId::SessionsClear => "sessions.clear",
            RouteId::SessionsContext => "sessions.context",
            RouteId::SessionsBranch => "sessions.branch",
            RouteId::SessionsTurns => "sessions.turns",
            RouteId::AgentsList => "agents.list",
            RouteId::ToolsList => "tools.list",
            RouteId::ContainersList => "containers.list",
            RouteId::SandboxesList => "sandboxes.list",
            RouteId::SandboxesManage => "sandboxes.manage",
            RouteId::McpList => "mcp.list",
            RouteId::ExtensionsList => "extensions.list",
            RouteId::ExtensionsApprove => "extensions.approve",
            RouteId::ExtensionsRevoke => "extensions.revoke",
            RouteId::CommandsList => "commands.list",
            RouteId::CommandsRun => "commands.run",
            RouteId::FilesList => "files.list",
            RouteId::FilesDelete => "files.delete",
            RouteId::FilesUpload => "files.upload",
            RouteId::FilesRead => "files.read",
            RouteId::FilesWrite => "files.write",
            RouteId::FilesMkdir => "files.mkdir",
            RouteId::FilesMove => "files.move",
            RouteId::FilesSign => "files.sign",
            RouteId::MediaGet => "media.get",
            RouteId::WorkspacesList => "workspaces.list",
            RouteId::WorkspacesCreate => "workspaces.create",
            RouteId::WorkspacesUpdate => "workspaces.update",
            RouteId::WorkspacesDelete => "workspaces.delete",
            RouteId::WorkspacesMoveSessions => "workspaces.moveSessions",
            RouteId::NotificationsList => "notifications.list",
            RouteId::NotificationsReadAll => "notifications.readAll",
            RouteId::NotificationsRead => "notifications.read",
            RouteId::NotificationsDelete => "notifications.delete",
            RouteId::NotificationsDeleteAll => "notifications.deleteAll",
            RouteId::AutomationList => "automation.list",
            RouteId::AutomationCreate => "automation.create",
            RouteId::AutomationGet => "automation.get",
            RouteId::AutomationUpdate => "automation.update",
            RouteId::AutomationDelete => "automation.delete",
            RouteId::AutomationRun => "automation.run",
            RouteId::AutomationRuns => "automation.runs",
            #[cfg(feature = "test-hooks")]
            RouteId::TestSessions => "test.sessions",
            #[cfg(feature = "test-hooks")]
            RouteId::TestNotifications => "test.notifications",
            #[cfg(feature = "test-hooks")]
            RouteId::TestAutomationRun => "test.automation.run",
            #[cfg(feature = "test-hooks")]
            RouteId::TestAutomationRunFinish => "test.automation.runFinish",
            #[cfg(feature = "test-hooks")]
            RouteId::TestSchedulerTick => "test.scheduler.tick",
        }
    }
}

/// The routes every build serves.
const BASE: [Route; 65] = [
    // Status and health
    Route {
        id: RouteId::SystemHealth,
        method: RouteMethod::GET,
        path: "/api/health",
        auth: RouteAuth::Public,
    },
    Route {
        id: RouteId::SystemStatus,
        method: RouteMethod::GET,
        path: "/api/status",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SystemOpenapi,
        method: RouteMethod::GET,
        path: "/api/openapi.json",
        auth: RouteAuth::Required,
    },
    // The socket. In the manifest like everything else, and `Required` like
    // almost everything else: an unauthenticated upgrade is a shell-capable
    // agent that anyone who can reach the port may drive, which is the exact
    // failure `assert_boot_policy` refuses to start for.
    Route {
        id: RouteId::WsConnect,
        method: RouteMethod::GET,
        path: "/ws",
        auth: RouteAuth::Required,
    },
    // Auth
    Route {
        id: RouteId::AuthLogin,
        method: RouteMethod::POST,
        path: "/api/auth/login",
        auth: RouteAuth::Public,
    },
    Route {
        id: RouteId::AuthLogout,
        method: RouteMethod::POST,
        path: "/api/auth/logout",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::AuthMe,
        method: RouteMethod::GET,
        path: "/api/auth/me",
        auth: RouteAuth::Required,
    },
    // First-run setup. Two of the three are `Public`, which is deliberate and
    // is the only widening of the public surface since it was two routes:
    // `setup.status` answers one bit an attacker learns anyway by watching
    // every login fail, and `setup.claim` is the login for an install that has
    // no password yet — it spends a single-use code that only the operator's
    // own terminal ever saw. Both stop existing the moment a password is set.
    Route {
        id: RouteId::SetupStatus,
        method: RouteMethod::GET,
        path: "/api/setup",
        auth: RouteAuth::Public,
    },
    Route {
        id: RouteId::SetupClaim,
        method: RouteMethod::POST,
        path: "/api/setup/claim",
        auth: RouteAuth::Public,
    },
    Route {
        id: RouteId::SetupPassword,
        method: RouteMethod::POST,
        path: "/api/setup/password",
        auth: RouteAuth::Required,
    },
    // Settings and credentials
    Route {
        id: RouteId::SettingsGet,
        method: RouteMethod::GET,
        path: "/api/settings",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SettingsPatch,
        method: RouteMethod::PATCH,
        path: "/api/settings",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SettingsCredential,
        method: RouteMethod::PUT,
        path: "/api/settings/credentials",
        auth: RouteAuth::Required,
    },
    // A POST because it is not idempotent in the way that matters: it rebuilds
    // the provider, the loops and the tool registry, and doing that twice is
    // two rebuilds. Under `/api/settings` rather than `/api/system` because
    // what it re-reads is the settings file — the process it belongs to keeps
    // running.
    Route {
        id: RouteId::SettingsReload,
        method: RouteMethod::POST,
        path: "/api/settings/reload",
        auth: RouteAuth::Required,
    },
    // Providers and models
    Route {
        id: RouteId::ProvidersList,
        method: RouteMethod::GET,
        path: "/api/providers",
        auth: RouteAuth::Required,
    },
    // A POST because it opens a socket to somewhere else, and not
    // `/api/providers/:id/test` because the thing most worth testing is a
    // connection that has not been saved yet — an id-shaped route would force
    // a write before the check the check exists to precede.
    Route {
        id: RouteId::ProvidersTest,
        method: RouteMethod::POST,
        path: "/api/providers/test",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::ModelsList,
        method: RouteMethod::GET,
        path: "/api/models",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::ModelsRefresh,
        method: RouteMethod::POST,
        path: "/api/models/refresh",
        auth: RouteAuth::Required,
    },
    // Sessions, messages, context
    Route {
        id: RouteId::SessionsList,
        method: RouteMethod::GET,
        path: "/api/sessions",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SessionsCreate,
        method: RouteMethod::POST,
        path: "/api/sessions",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SessionsGet,
        method: RouteMethod::GET,
        path: "/api/sessions/:key",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SessionsUpdate,
        method: RouteMethod::PATCH,
        path: "/api/sessions/:key",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SessionsDelete,
        method: RouteMethod::DELETE,
        path: "/api/sessions/:key",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SessionsMessages,
        method: RouteMethod::GET,
        path: "/api/sessions/:key/messages",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SessionsClear,
        method: RouteMethod::DELETE,
        path: "/api/sessions/:key/messages",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SessionsContext,
        method: RouteMethod::GET,
        path: "/api/sessions/:key/context",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SessionsBranch,
        method: RouteMethod::POST,
        path: "/api/sessions/:key/branch",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SessionsTurns,
        method: RouteMethod::GET,
        path: "/api/sessions/:key/turns",
        auth: RouteAuth::Required,
    },
    // Agents and tools. Both read-only: an agent is a subtree of the settings
    // tree, so it is created, edited, deleted *and renamed* through
    // `settings.patch` — the last of those carries a `renameAgents` field,
    // because a key move on its own cannot say whether it means "rename" or
    // "delete and create", and those differ on what happens to the old id's
    // conversations and standing approvals.
    Route {
        id: RouteId::AgentsList,
        method: RouteMethod::GET,
        path: "/api/agents",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::ToolsList,
        method: RouteMethod::GET,
        path: "/api/tools",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::ContainersList,
        method: RouteMethod::GET,
        path: "/api/containers",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SandboxesList,
        method: RouteMethod::GET,
        path: "/api/sandboxes",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::SandboxesManage,
        method: RouteMethod::POST,
        path: "/api/sandboxes",
        auth: RouteAuth::Required,
    },
    // Live connection state, which `GET /api/settings` cannot carry: that
    // response is the settings tree, and the tree is what gets written back to
    // `config.yaml`. `GET /api/tools` cannot carry it either — it answers with
    // flattened names and no server, and recovering "whose is
    // `mcp_github_create-issue`?" by splitting a string in the browser is
    // ambiguous the moment a server id contains an underscore.
    Route {
        id: RouteId::McpList,
        method: RouteMethod::GET,
        path: "/api/mcp",
        auth: RouteAuth::Required,
    },
    // Extensions, and the two writes that are not settings patches. An
    // approval records the digest of the files on disk *now*; putting it in
    // `config.yaml` would make it survive an edit to the very files it was
    // about. POST for both because neither is idempotent across such an edit.
    Route {
        id: RouteId::ExtensionsList,
        method: RouteMethod::GET,
        path: "/api/extensions",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::ExtensionsApprove,
        method: RouteMethod::POST,
        path: "/api/extensions/:id/approve",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::ExtensionsRevoke,
        method: RouteMethod::POST,
        path: "/api/extensions/:id/revoke",
        auth: RouteAuth::Required,
    },
    // The one command surface that is not a table compiled into a client: an
    // extension's command has one definition and three places it has to
    // appear.
    Route {
        id: RouteId::CommandsList,
        method: RouteMethod::GET,
        path: "/api/commands",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::CommandsRun,
        method: RouteMethod::POST,
        path: "/api/commands/:id",
        auth: RouteAuth::Required,
    },
    // Files, upload and signed media
    Route {
        id: RouteId::FilesList,
        method: RouteMethod::GET,
        path: "/api/files",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::FilesDelete,
        method: RouteMethod::DELETE,
        path: "/api/files",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::FilesUpload,
        method: RouteMethod::POST,
        path: "/api/files/upload",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::FilesRead,
        method: RouteMethod::GET,
        path: "/api/files/text",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::FilesWrite,
        method: RouteMethod::PUT,
        path: "/api/files/text",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::FilesMkdir,
        method: RouteMethod::POST,
        path: "/api/files/directory",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::FilesMove,
        method: RouteMethod::POST,
        path: "/api/files/move",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::FilesSign,
        method: RouteMethod::POST,
        path: "/api/files/signed-url",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::MediaGet,
        method: RouteMethod::GET,
        path: "/api/media/:token",
        auth: RouteAuth::Signed,
    },
    // Workspaces
    Route {
        id: RouteId::WorkspacesList,
        method: RouteMethod::GET,
        path: "/api/workspaces",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::WorkspacesCreate,
        method: RouteMethod::POST,
        path: "/api/workspaces",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::WorkspacesUpdate,
        method: RouteMethod::PATCH,
        path: "/api/workspaces/:id",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::WorkspacesDelete,
        method: RouteMethod::DELETE,
        path: "/api/workspaces/:id",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::WorkspacesMoveSessions,
        method: RouteMethod::POST,
        path: "/api/workspaces/:id/sessions/move",
        auth: RouteAuth::Required,
    },
    // Notifications
    Route {
        id: RouteId::NotificationsList,
        method: RouteMethod::GET,
        path: "/api/notifications",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::NotificationsReadAll,
        method: RouteMethod::POST,
        path: "/api/notifications/read",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::NotificationsRead,
        method: RouteMethod::POST,
        path: "/api/notifications/:id/read",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::NotificationsDelete,
        method: RouteMethod::DELETE,
        path: "/api/notifications/:id",
        auth: RouteAuth::Required,
    },
    // Before the `:id` form would be ambiguous — it is not, because the paths
    // differ in segment count — but it is listed after it to read in the order
    // a person would expect: the one, then all of them.
    Route {
        id: RouteId::NotificationsDeleteAll,
        method: RouteMethod::DELETE,
        path: "/api/notifications",
        auth: RouteAuth::Required,
    },
    // Automation. `/api/automation/jobs` rather than `/api/jobs`: "job" alone
    // is too general a noun to own a top-level surface — a turn is a job and an
    // upload is a job — and the prefix is where the heartbeat's own routes go
    // next. `automation.get` exists although the listing is unpaged, because
    // the editor lives at its own URL and a deep link must resolve without
    // fetching every job the install has.
    Route {
        id: RouteId::AutomationList,
        method: RouteMethod::GET,
        path: "/api/automation/jobs",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::AutomationCreate,
        method: RouteMethod::POST,
        path: "/api/automation/jobs",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::AutomationGet,
        method: RouteMethod::GET,
        path: "/api/automation/jobs/:id",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::AutomationUpdate,
        method: RouteMethod::PATCH,
        path: "/api/automation/jobs/:id",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::AutomationDelete,
        method: RouteMethod::DELETE,
        path: "/api/automation/jobs/:id",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::AutomationRun,
        method: RouteMethod::POST,
        path: "/api/automation/jobs/:id/run",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::AutomationRuns,
        method: RouteMethod::GET,
        path: "/api/automation/jobs/:id/runs",
        auth: RouteAuth::Required,
    },
];

/// The routes only a `test-hooks` build serves.
#[cfg(feature = "test-hooks")]
const HOOKS: [Route; 5] = [
    Route {
        id: RouteId::TestSessions,
        method: RouteMethod::POST,
        path: "/api/_test/sessions",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::TestNotifications,
        method: RouteMethod::POST,
        path: "/api/_test/notifications",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::TestAutomationRun,
        method: RouteMethod::POST,
        path: "/api/_test/automation/runs",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::TestAutomationRunFinish,
        method: RouteMethod::POST,
        path: "/api/_test/automation/runs/:id/finish",
        auth: RouteAuth::Required,
    },
    Route {
        id: RouteId::TestSchedulerTick,
        method: RouteMethod::POST,
        path: "/api/_test/scheduler/tick",
        auth: RouteAuth::Required,
    },
];

/// `BASE` followed by `HOOKS`, in const so the manifest stays a `&[Route]`.
#[cfg(feature = "test-hooks")]
const fn with_hooks(base: &[Route; 65], hooks: &[Route; 5]) -> [Route; 70] {
    let mut all = [base[0]; 70];
    let mut i = 0;
    while i < base.len() {
        all[i] = base[i];
        i += 1;
    }
    let mut j = 0;
    while j < 5 {
        all[base.len() + j] = hooks[j];
        j += 1;
    }
    all
}

#[cfg(feature = "test-hooks")]
const ALL: [Route; 70] = with_hooks(&BASE, &HOOKS);

/// Every route this build serves, and the only path to one.
#[cfg(not(feature = "test-hooks"))]
pub static ROUTE_MANIFEST: &[Route] = &BASE;

/// Every route this build serves, and the only path to one.
#[cfg(feature = "test-hooks")]
pub static ROUTE_MANIFEST: &[Route] = &ALL;
