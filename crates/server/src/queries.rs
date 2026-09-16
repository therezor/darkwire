//! Query-string and path-parameter shapes.
//!
//! These live here rather than in `ghostai-protocol` for one reason: a query
//! string carries only strings, so `limit=50` arrives as `"50"` and reading it
//! as a number is a coercion. The protocol crate's types describe what a client
//! *sends* as JSON, where a number is already a number; these describe the same
//! shapes as they arrive on a URL, and the coercion lives on this side of the
//! boundary, next to the transport that makes it necessary.
//!
//! They are not DTOs and are not registered in `PROTOCOL_SCHEMAS`: a query
//! shape always inlines into the OpenAPI document as parameters, so a `$ref`
//! would have nothing to point at.
//!
//! **The page fields are restated on each shape rather than flattened in.**
//! `#[serde(flatten)]` forces the deserialiser to buffer every value as a
//! string, which destroys exactly the coercion these types exist for: `limit=50`
//! then fails as "invalid type: string, expected u32". It is also the more
//! faithful shape — a query string has no nesting, so the wire was always flat
//! and the composition would have been a Rust-side convenience that the wire
//! cannot express.

use garde::Validate;
use ghostai_core::ids::DEFAULT_WORKSPACE_ID;
use schemars::JsonSchema;
use serde::Deserialize;

/// The bounds the protocol's pagination shape states, restated once.
pub const MAX_PAGE_LIMIT: u32 = 200;

/// Rows per page when a request names none.
pub const DEFAULT_PAGE_LIMIT: u32 = 50;

fn default_page_limit() -> u32 {
    DEFAULT_PAGE_LIMIT
}

fn default_workspace() -> String {
    DEFAULT_WORKSPACE_ID.to_owned()
}

fn default_dot() -> String {
    ".".to_owned()
}

/// How a client asks for one page.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PageQuery {
    /// Rows per page.
    #[serde(default = "default_page_limit")]
    #[garde(range(min = 1, max = MAX_PAGE_LIMIT))]
    pub limit: u32,
    /// Opaque; echoed back from `nextCursor`.
    #[serde(default)]
    #[garde(skip)]
    pub cursor: Option<String>,
    /// Rows to skip from the top, for a numbered pager.
    ///
    /// Left optional rather than defaulted to `0`: zero and "absent" have to
    /// stay distinguishable, or a request carrying only a cursor arrives with
    /// an offset it never sent and trips the guard that refuses both at once.
    #[serde(default)]
    #[garde(skip)]
    pub offset: Option<u32>,
}

/// The columns `GET /api/sessions` will order by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SessionSort {
    /// Most recently active first.
    Updated,
    /// Newest first.
    Created,
    /// Alphabetical.
    Title,
}

/// `GET /api/sessions`.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
pub struct SessionListQuery {
    /// Rows per page.
    #[serde(default = "default_page_limit")]
    #[garde(range(min = 1, max = MAX_PAGE_LIMIT))]
    pub limit: u32,
    /// Opaque; echoed back from `nextCursor`.
    #[serde(default)]
    #[garde(skip)]
    pub cursor: Option<String>,
    /// Rows to skip from the top, for a numbered pager.
    ///
    /// Left optional rather than defaulted to `0`: zero and "absent" have to
    /// stay distinguishable, or a request carrying only a cursor arrives with
    /// an offset it never sent and trips the guard that refuses both at once.
    #[serde(default)]
    #[garde(skip)]
    pub offset: Option<u32>,
    /// `web`, `telegram`, `automation`, an extension id. Absent means every
    /// origin.
    #[serde(default)]
    #[garde(inner(length(min = 1)))]
    pub origin: Option<String>,
    /// One origin to leave out. Absent means none is.
    ///
    /// The sidebar sends `subagent`: a shortlist of thirty is a list of
    /// conversations, and a delegated run is a step inside one. Excluded here
    /// rather than dropped from the response, so the thirty stay thirty.
    #[serde(default)]
    #[garde(inner(length(min = 1)))]
    pub exclude_origin: Option<String>,
    /// Absent means every workspace, which is what the unscoped sidebar asks
    /// for.
    #[serde(default)]
    #[garde(inner(length(min = 1)))]
    pub workspace: Option<String>,
    /// A title substring.
    ///
    /// No minimum length: an empty box is a legal thing for a client to send,
    /// and the store already treats blank as "no filter". Refusing it would
    /// make clearing the search field a 422.
    #[serde(default)]
    #[garde(skip)]
    pub q: Option<String>,
    /// Which column to order by.
    #[serde(default)]
    #[garde(skip)]
    pub sort: Option<SessionSort>,
    /// Descending rather than ascending.
    #[serde(default)]
    #[garde(skip)]
    pub desc: Option<bool>,
}

/// `GET /api/notifications`.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NotificationListQuery {
    /// Rows per page.
    #[serde(default = "default_page_limit")]
    #[garde(range(min = 1, max = MAX_PAGE_LIMIT))]
    pub limit: u32,
    /// Opaque; echoed back from `nextCursor`.
    #[serde(default)]
    #[garde(skip)]
    pub cursor: Option<String>,
    /// Rows to skip from the top, for a numbered pager.
    ///
    /// Left optional rather than defaulted to `0`: zero and "absent" have to
    /// stay distinguishable, or a request carrying only a cursor arrives with
    /// an offset it never sent and trips the guard that refuses both at once.
    #[serde(default)]
    #[garde(skip)]
    pub offset: Option<u32>,
    /// Only the unread ones.
    #[serde(default)]
    #[garde(skip)]
    pub unread: Option<bool>,
}

/// The socket's query parameters.
///
/// Validated before the upgrade rather than after, so a client that sends
/// `?session=` gets an error it can read instead of a socket that opens, mints
/// a session it did not ask for, and looks like it lost the conversation.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
pub struct WsQuery {
    /// The session to open on. Absent asks the hub to mint one.
    #[serde(default)]
    #[garde(inner(length(min = 1)))]
    pub session: Option<String>,
    /// The agent a session *created* by this connection is bound to.
    ///
    /// A default for the connection, not an override: a frame may name its own,
    /// and a session that already exists keeps the agent it was created with.
    /// It is here because a tab connects before it has sent anything, and the
    /// store holds no row until the first message lands.
    #[serde(default)]
    #[garde(inner(length(min = 1)))]
    pub agent: Option<String>,
}

/// A workspace-relative path in a query string.
///
/// Never validated for safety here — that is the jail's job and only its job.
/// This says the parameter is present and is a string; whether it is a legal
/// path is decided in one place, for every caller.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PathQuery {
    /// The path, relative to the workspace root.
    #[garde(length(min = 1))]
    pub path: String,
    /// Which workspace the path is relative to.
    ///
    /// A query parameter rather than a header or a `/api/workspaces/:id/files`
    /// prefix. A header would be invisible in the generated document and in a
    /// pasted `curl`, and would need a `Vary`; a path prefix would double the
    /// file surface for no gain over a parameter the document already
    /// describes.
    ///
    /// It is authorised, not authorising. There is one principal here and it
    /// can already reach the whole tree, so naming a workspace is not a
    /// privilege decision — what the routes must do with it is refuse an id
    /// with no registry row, so a crafted value cannot bring a directory into
    /// existence.
    #[serde(default = "default_workspace")]
    #[garde(length(min = 1))]
    pub workspace: String,
}

/// The directory listing's path, where absent means the workspace root.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
pub struct OptionalPathQuery {
    /// The directory. Defaults to the root.
    #[serde(default = "default_dot")]
    #[garde(skip)]
    pub path: String,
    /// Which workspace the path is relative to.
    #[serde(default = "default_workspace")]
    #[garde(length(min = 1))]
    pub workspace: String,
}

/// A delete, and whether it may take a directory's contents with it.
///
/// The flag exists so that emptying a tree cannot be something a request
/// *happens* to do. A bare `DELETE /api/files?path=notes` removes an empty
/// directory and refuses a full one — so a mistyped path, a stale bookmark or a
/// script looping over names cannot recurse, and the caller that means it has
/// to say a word that only means that.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
pub struct DeleteQuery {
    /// The path to remove.
    #[garde(length(min = 1))]
    pub path: String,
    /// Which workspace the path is relative to.
    #[serde(default = "default_workspace")]
    #[garde(length(min = 1))]
    pub workspace: String,
    /// Whether a non-empty directory may go with it.
    #[serde(default)]
    #[garde(skip)]
    pub recursive: Option<bool>,
}

/// `GET /api/sessions/:key/turns`.
///
/// Deliberately not [`PageQuery`]. There is no turn cursor — `turn_stats` is
/// read newest-first off one index and a conversation has orders of magnitude
/// fewer turns than messages. Accepting a `cursor` that is then ignored would
/// put a parameter in the OpenAPI document that the server does not honour,
/// which is a lie the document cannot recover from.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
pub struct TurnsQuery {
    /// Rows per page.
    #[serde(default = "default_page_limit")]
    #[garde(range(min = 1, max = MAX_PAGE_LIMIT))]
    pub limit: u32,
}

/// A route addressed by session key.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
pub struct SessionParams {
    /// The conversation.
    #[garde(length(min = 1))]
    pub key: String,
}

/// A route addressed by id.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
pub struct IdParams {
    /// The thing.
    #[garde(length(min = 1))]
    pub id: String,
}

/// A route addressed by an operator-chosen name rather than a generated id.
///
/// Separate from [`IdParams`] because the parameter is spelled `name` in the
/// path, and an OpenAPI document that called it `id` would describe a request
/// nothing sends.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
pub struct NameParams {
    /// The thing, by the name its operator gave it.
    #[garde(length(min = 1))]
    pub name: String,
}

/// The signed-media route's token.
#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
pub struct TokenParams {
    /// The signed, expiring token naming one workspace path.
    #[garde(length(min = 1))]
    pub token: String,
}
