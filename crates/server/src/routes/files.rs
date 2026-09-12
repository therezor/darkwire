//! The workspace over HTTP: listing, upload, text, moves and signed media.
//!
//! Every path arrives as a workspace-relative string and goes through the
//! workspace jail before it reaches the filesystem — including the ones this
//! server signed itself. The jail returns the canonical path it verified and
//! that is the path used; nothing here re-derives one.
//!
//! `GET /api/media/:token` is the only route in the manifest whose credential
//! is the URL. The reasoning is in [`crate::signing`]; what belongs here is the
//! consequence: the response is served with `X-Content-Type-Options: nosniff`
//! and a `Content-Disposition` that refuses to render anything a browser would
//! execute in this origin. The workspace is a tree a language model writes to,
//! so "the agent produced an HTML file and the user opened it" is a path that
//! needs closing rather than a hypothetical.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Extension, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, body};
use garde::Validate;
use ghostai_core::ids::DEFAULT_WORKSPACE_ID;
use ghostai_core::{ErrorKind, GhostError, ensure_dir};
use ghostai_protocol::rest::{
    CreateDirectoryRequest, FileEntry, FileListResponse, FileTextResponse, FileWriteRequest,
    MoveFileRequest, SignedUrl, SignedUrlRequest, UploadResponse,
};
use ghostai_protocol::ws::ErrorCode;
use ghostai_security::jail::WorkspaceJail;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::io::ReaderStream;

use crate::errors::HttpError;
use crate::queries::{DeleteQuery, OptionalPathQuery, PathQuery};
use crate::routes::AppState;
use crate::schema::{parse_body, validated};
use crate::signing::{MEDIA_SECRET_NAME, MediaClaim, media_url, sign_media_token};
use crate::workspace::{entry_at, inline_safe, list_directory, mime_type_for, read_text};

/// The cap on one upload.
///
/// Enforced while the body is still arriving rather than after the whole thing
/// is in memory: the size hint is checked before a byte is read, so a request
/// that declares more than this is refused without buffering it. Raising a
/// global limit instead would let every other route buffer 25 MiB before
/// anything looked at it.
pub const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

/// The cap on one save.
///
/// Twice the read limit rather than equal to it: the body is JSON, so every
/// quote, backslash and newline in the file costs a second byte on the wire,
/// and a limit equal to the read limit would refuse to save a file the same
/// route had just agreed to open.
pub const MAX_TEXT_BODY_BYTES: usize = 2 * 512 * 1024;

/// The save cap is exactly twice the read cap, and the two are declared in
/// different integer types, so the relationship is asserted rather than
/// computed: moving the read limit fails this build instead of silently
/// leaving a save that refuses a file the read had just agreed to open.
const _: () = assert!(crate::workspace::MAX_TEXT_BYTES == 512 * 1024);

/// How long a media response may sit in a cache.
///
/// Private and short: the URL is a bearer credential, and a shared cache
/// holding the response would serve it to whoever asks next.
const MEDIA_CACHE_CONTROL: &str = "private, max-age=60";

// Handlers

/// `GET /api/files` — one workspace directory.
pub async fn list(
    State(state): State<AppState>,
    query: Result<Query<OptionalPathQuery>, QueryRejection>,
) -> Result<Response, HttpError> {
    let query = from_query("query", query)?;
    let jail = jail_for(&state, &query.workspace)?;

    blocking(move || {
        let absolute = jail.resolve(&query.path)?;
        let metadata = metadata_or_404(&absolute, &query.path)?;
        if !metadata.is_dir() {
            return Err(HttpError::bad_request(format!(
                "Not a directory: {}",
                query.path
            )));
        }

        let response = FileListResponse {
            // Echo the *relative* path the jail agreed to, not the input:
            // `./a/` and `a` are the same directory and a client keying on the
            // response should see one answer for both.
            path: relative(&jail, &absolute)?,
            entries: list_directory(&jail, &absolute),
        };
        Ok(Json(response).into_response())
    })
    .await
}

/// `DELETE /api/files` — one workspace file or directory.
pub async fn delete(
    State(state): State<AppState>,
    query: Result<Query<DeleteQuery>, QueryRejection>,
) -> Result<Response, HttpError> {
    let query = from_query("query", query)?;
    let jail = jail_for(&state, &query.workspace)?;

    blocking(move || {
        let absolute = jail.resolve(&query.path)?;
        let metadata = metadata_or_404(&absolute, &query.path)?;

        if !metadata.is_dir() {
            std::fs::remove_file(&absolute).map_err(internal)?;
            return Ok(StatusCode::NO_CONTENT.into_response());
        }

        // A recursive delete is a large, irreversible action, and it must never
        // be something a request *happens* to do — a mistyped path or a script
        // looping over names would otherwise empty a tree. So the contents go
        // only when the caller said the word that means exactly that; an empty
        // directory has no contents to lose and needs no ceremony.
        let count = std::fs::read_dir(&absolute)
            .map_err(internal)?
            .flatten()
            .count();
        if count > 0 && query.recursive != Some(true) {
            return Err(
                HttpError::conflict(format!("Directory is not empty: {}", query.path))
                    .with_detail("entryCount", count),
            );
        }

        std::fs::remove_dir_all(&absolute).map_err(internal)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// `POST /api/files/upload` — write a file into the workspace.
///
/// The body is raw bytes, not JSON: a browser sends a file as-is and a base64
/// envelope would inflate every upload by a third to describe what
/// `Content-Type` already says.
pub async fn upload(
    State(state): State<AppState>,
    query: Result<Query<PathQuery>, QueryRejection>,
    request: Body,
) -> Result<Response, HttpError> {
    let query = from_query("query", query)?;
    let jail = jail_for(&state, &query.workspace)?;
    let bytes = bounded(request, MAX_UPLOAD_BYTES).await?;
    if bytes.is_empty() {
        return Err(HttpError::bad_request("Upload body is empty"));
    }

    let signer = Signer::new(&state)?;
    let workspace = query.workspace.clone();
    blocking(move || {
        let absolute = jail.resolve(&query.path)?;
        if let Some(parent) = absolute.parent() {
            ensure_dir(parent)?;
        }
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        std::fs::write(&absolute, &bytes).map_err(internal)?;

        let path = relative(&jail, &absolute)?;
        let response = UploadResponse {
            mime_type: mime_type_for(&path).to_owned(),
            size_bytes: size,
            // Returned with the upload so a UI can render what it just sent
            // without a second round trip to ask permission to look at it.
            signed_url: Some(signer.sign(&path, &workspace)),
            path,
        };
        Ok((StatusCode::CREATED, Json(response)).into_response())
    })
    .await
}

/// `GET /api/files/text` — one workspace file, as text.
pub async fn read(
    State(state): State<AppState>,
    query: Result<Query<PathQuery>, QueryRejection>,
) -> Result<Response, HttpError> {
    let query = from_query("query", query)?;
    let jail = jail_for(&state, &query.workspace)?;

    blocking(move || {
        let absolute = jail.resolve(&query.path)?;
        let metadata = metadata_or_404(&absolute, &query.path)?;
        if metadata.is_dir() {
            return Err(HttpError::bad_request(format!(
                "Not a file: {}",
                query.path
            )));
        }

        // Decided from the bytes, not from the extension: the media-type table
        // is deliberately small, so an extension check would refuse `.py` and
        // `.ts` — the files a person most wants to open — while accepting a
        // `.txt` that happens to hold a binary blob.
        let Some(text) = read_text(&absolute, metadata.len())? else {
            return Err(HttpError::bad_request(format!(
                "Not a text file: {}",
                query.path
            )));
        };

        let response = FileTextResponse {
            path: relative(&jail, &absolute)?,
            content: text.content,
            size_bytes: metadata.len(),
            modified_at_ms: modified_at_ms(&metadata),
            truncated: text.truncated,
        };
        Ok(Json(response).into_response())
    })
    .await
}

/// `PUT /api/files/text` — write text to a workspace file.
pub async fn write(State(state): State<AppState>, request: Body) -> Result<Response, HttpError> {
    let body: FileWriteRequest = json_body(request, "body", MAX_TEXT_BODY_BYTES).await?;
    let jail = jail_for(&state, workspace_of(body.workspace_id.as_deref()))?;

    blocking(move || {
        let absolute = jail.resolve(&body.path)?;
        let before = std::fs::metadata(&absolute).ok();
        if before.as_ref().is_some_and(std::fs::Metadata::is_dir) {
            return Err(HttpError::bad_request(format!("Not a file: {}", body.path)));
        }

        // The workspace is a tree a language model writes to while somebody is
        // looking at it. An editor that loaded the file, sat open through a
        // turn, and then saved would silently delete whatever that turn wrote —
        // so a caller that says which version it read gets told when that is no
        // longer the version on disk. A caller that says nothing is creating a
        // file and has nothing to conflict with.
        if let Some(expected) = body.expected_modified_at_ms {
            match &before {
                None => {
                    return Err(HttpError::conflict(format!(
                        "Deleted since it was read: {}",
                        body.path
                    )));
                }
                Some(metadata) => {
                    let actual = modified_at_ms(metadata);
                    if actual != expected {
                        return Err(HttpError::conflict(format!(
                            "Changed since it was read: {}",
                            body.path
                        ))
                        .with_detail("modifiedAtMs", actual));
                    }
                }
            }
        }

        if let Some(parent) = absolute.parent() {
            ensure_dir(parent)?;
        }
        std::fs::write(&absolute, body.content.as_bytes()).map_err(internal)?;

        // Read the metadata again rather than computing the entry from the
        // content: the size on disk is the byte length after UTF-8 encoding,
        // and the modification time is the value the next save has to match.
        Ok(Json(entry_of(&jail, &absolute)?).into_response())
    })
    .await
}

/// `POST /api/files/directory` — create a workspace directory.
pub async fn mkdir(State(state): State<AppState>, request: Body) -> Result<Response, HttpError> {
    let body: CreateDirectoryRequest = json_body(request, "body", MAX_TEXT_BODY_BYTES).await?;
    let jail = jail_for(&state, workspace_of(body.workspace_id.as_deref()))?;

    blocking(move || {
        let absolute = jail.resolve(&body.path)?;
        // Not idempotent, deliberately: "New folder" that quietly returns an
        // existing one is how two things end up sharing a directory nobody
        // meant to share.
        if std::fs::metadata(&absolute).is_ok() {
            return Err(HttpError::conflict(format!(
                "Already exists: {}",
                body.path
            )));
        }

        std::fs::create_dir_all(&absolute).map_err(internal)?;
        Ok((StatusCode::CREATED, Json(entry_of(&jail, &absolute)?)).into_response())
    })
    .await
}

/// `POST /api/files/move` — rename or move a workspace entry.
pub async fn move_entry(
    State(state): State<AppState>,
    request: Body,
) -> Result<Response, HttpError> {
    let body: MoveFileRequest = json_body(request, "body", MAX_TEXT_BODY_BYTES).await?;
    let jail = jail_for(&state, workspace_of(body.workspace_id.as_deref()))?;

    blocking(move || {
        // Both ends through the jail, and the canonical paths it returns are
        // the ones used. A destination that climbs out of the workspace is
        // refused by the same code that refuses a source that does — which is
        // the whole reason this is one resolve call per side rather than a join
        // on the client's string.
        let source = jail.resolve(&body.from)?;
        let target = jail.resolve(&body.to)?;

        let metadata = metadata_or_404(&source, &body.from)?;

        if source == target {
            // Not an error and not a write: renaming a thing to its own name is
            // a no-op, and a rename onto itself is a silent success anyway.
            let entry = entry_at(&jail, &source, &metadata).ok_or_else(outside)?;
            return Ok(Json(entry).into_response());
        }

        // Refused rather than overwritten. A rename will happily replace a
        // file, and one that destroys whatever was already at the target is a
        // data loss the operator did not ask for and cannot see afterwards.
        if std::fs::metadata(&target).is_ok() {
            return Err(HttpError::conflict(format!("Already exists: {}", body.to)));
        }

        // A move into a folder that does not exist yet is a typo far more often
        // than it is an intention, and the operating system reports it as a
        // bare "no such file" that reads as "the file is missing" rather than
        // "the folder is".
        if let Some(parent) = target.parent()
            && std::fs::metadata(parent).is_err()
        {
            return Err(HttpError::bad_request(format!(
                "No such directory: {}",
                relative(&jail, parent)?
            )));
        }

        if let Err(error) = std::fs::rename(&source, &target) {
            // The one case worth naming: a directory cannot be moved inside
            // itself, and the raw error tells the operator nothing.
            if is_invalid_input(&error) {
                return Err(HttpError::bad_request(format!(
                    "Cannot move {} inside itself",
                    body.from
                )));
            }
            return Err(internal(error));
        }

        Ok(Json(entry_of(&jail, &target)?).into_response())
    })
    .await
}

/// `POST /api/files/signed-url` — mint a short-lived URL an `<img>` can load.
pub async fn sign(State(state): State<AppState>, request: Body) -> Result<Response, HttpError> {
    let body: SignedUrlRequest = json_body(request, "body", MAX_TEXT_BODY_BYTES).await?;
    let workspace = workspace_of(body.workspace_id.as_deref()).to_owned();
    let jail = jail_for(&state, &workspace)?;
    let signer = Signer::new(&state)?;

    blocking(move || {
        let absolute = jail.resolve(&body.path)?;
        // Signed after the file is known to exist: a URL that 404s later is a
        // worse answer than a 404 now, and the client is holding the path.
        metadata_or_404(&absolute, &body.path)?;
        let path = relative(&jail, &absolute)?;
        Ok(Json(signer.sign(&path, &workspace)).into_response())
    })
    .await
}

/// `GET /api/media/:token` — serve a workspace file to a signed URL.
///
/// The signature was verified before this ran, and that is also the only thing
/// that put a claim in the request.
pub async fn media(
    State(state): State<AppState>,
    Extension(claim): Extension<MediaClaim>,
) -> Result<Response, HttpError> {
    // The claim's workspace, not the default: the token names one, and the
    // whole point of signing it is that this is the workspace the URL was
    // authorised against. It deliberately does *not* consult the registry — a
    // live token against a workspace detached a minute ago keeps working until
    // it expires, which is consistent with detaching keeping files.
    let jail = state
        .runtime
        .agent(None)
        .and_then(|agent| agent.jail_for(&claim.workspace_id))
        .map_err(|_| HttpError::not_found("No such media"))?;

    // Checked again even though this server signed it, and through the jail's
    // full check rather than a containment test: containment reads an
    // already-absolute path without canonicalising, so it would happily serve a
    // file that became a symlink to `/etc/passwd` after the URL was minted. A
    // signature says who asked, not what the filesystem looks like now.
    let verdict = jail.check(&claim.path);
    let Some(accepted) = verdict.accepted() else {
        return Err(HttpError::not_found("No such media"));
    };
    let absolute = accepted.path.clone();

    let metadata = tokio::fs::metadata(&absolute)
        .await
        .map_err(|_| HttpError::not_found("No such media"))?;
    if metadata.is_dir() {
        return Err(HttpError::not_found("No such media"));
    }

    let inline = inline_safe(&claim.path);
    let content_type = if inline {
        mime_type_for(&claim.path)
    } else {
        crate::workspace::DEFAULT_MIME_TYPE
    };

    let file = tokio::fs::File::open(&absolute)
        .await
        .map_err(|_| HttpError::not_found("No such media"))?;
    // Streamed rather than read whole: the workspace holds whatever the agent
    // wrote to it, and a signed URL for a four-gigabyte file must not be a way
    // to make the server allocate four gigabytes.
    let body = Body::from_stream(ReaderStream::new(file));

    let mut response = Response::new(body);
    let headers = response.headers_mut();
    insert_header(headers, header::CONTENT_TYPE, content_type);
    insert_header(headers, header::CONTENT_LENGTH, &metadata.len().to_string());
    // Without this a browser may sniff a text file into HTML and run it.
    insert_header(headers, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    insert_header(
        headers,
        header::CONTENT_DISPOSITION,
        if inline { "inline" } else { "attachment" },
    );
    insert_header(headers, header::CACHE_CONTROL, MEDIA_CACHE_CONTROL);
    Ok(response)
}

// Shared pieces

/// Mints media tokens against the one signing secret.
///
/// Built before the blocking work starts, because reading the secret takes the
/// database lock and the signing itself is pure.
struct Signer {
    secret: String,
    ttl_ms: u64,
    now_ms: i64,
}

impl Signer {
    fn new(state: &AppState) -> Result<Signer, HttpError> {
        Ok(Signer {
            secret: state.auth.ensure_secret(MEDIA_SECRET_NAME)?,
            ttl_ms: state.runtime.config().server.auth.signed_url_ttl_ms,
            now_ms: state.clock.now_ms(),
        })
    }

    fn sign(&self, path: &str, workspace_id: &str) -> SignedUrl {
        let expires_at_ms = self
            .now_ms
            .saturating_add(i64::try_from(self.ttl_ms).unwrap_or(i64::MAX));
        let claim = MediaClaim {
            path: path.to_owned(),
            workspace_id: workspace_id.to_owned(),
            expires_at_ms,
        };
        SignedUrl {
            url: media_url(&sign_media_token(&self.secret, &claim)),
            expires_at_ms: u64::try_from(expires_at_ms).unwrap_or(0),
        }
    }
}

/// The jail for one workspace, refusing an id that names no workspace.
///
/// The registry lookup is what stops a crafted id from bringing a directory
/// into existence: the path resolver would happily accept any legal slug — that
/// is deliberate, so a *detached* workspace's sessions keep working — so the
/// boundary that decides "a user can still see this one" belongs here.
fn jail_for(state: &AppState, workspace_id: &str) -> Result<Arc<WorkspaceJail>, HttpError> {
    if state.runtime.workspaces().get(workspace_id)?.is_none() {
        return Err(HttpError::not_found(format!(
            "No such workspace: {workspace_id}"
        )));
    }
    Ok(state.runtime.agent(None)?.jail_for(workspace_id)?)
}

/// The workspace a body named, or the default one.
fn workspace_of(workspace_id: Option<&str>) -> &str {
    match workspace_id {
        Some(id) if !id.is_empty() => id,
        _ => DEFAULT_WORKSPACE_ID,
    }
}

/// A query string as one of the shapes in [`crate::queries`], validated.
fn from_query<T>(what: &str, result: Result<Query<T>, QueryRejection>) -> Result<T, HttpError>
where
    T: Validate<Context = ()>,
{
    match result {
        Ok(Query(value)) => validated(what, value),
        Err(rejection) => Err(HttpError::unprocessable(format!("Invalid {what}"))
            .with_detail("/", rejection.body_text())),
    }
}

/// A JSON body, bounded and then validated.
///
/// Malformed JSON is a 400 — the request is not something this server can act
/// on at all — while JSON of the wrong shape is the 422 the field-level detail
/// map belongs to.
async fn json_body<T>(request: Body, what: &str, limit: usize) -> Result<T, HttpError>
where
    T: DeserializeOwned + Validate<Context = ()>,
{
    let bytes = bounded(request, limit).await?;
    let raw: Value = serde_json::from_slice(&bytes)
        .map_err(|error| HttpError::bad_request(format!("Invalid JSON {what}: {error}")))?;
    parse_body(what, raw)
}

/// The whole body, refused if it declares or reaches more than `limit`.
async fn bounded(request: Body, limit: usize) -> Result<body::Bytes, HttpError> {
    body::to_bytes(request, limit).await.map_err(|_| {
        HttpError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::BadRequest,
            ErrorKind::InvalidInput,
            format!("Request body is larger than {limit} bytes"),
        )
    })
}

/// Filesystem metadata, with "does not exist" turned into the 404 it is.
fn metadata_or_404(absolute: &Path, relative_path: &str) -> Result<std::fs::Metadata, HttpError> {
    std::fs::metadata(absolute)
        .map_err(|_| HttpError::not_found(format!("No such file: {relative_path}")))
}

/// The entry one path produces, read fresh from disk.
fn entry_of(jail: &WorkspaceJail, absolute: &Path) -> Result<FileEntry, HttpError> {
    let metadata = std::fs::metadata(absolute).map_err(internal)?;
    entry_at(jail, absolute, &metadata).ok_or_else(outside)
}

/// The workspace-relative form of a path the jail returned.
fn relative(jail: &WorkspaceJail, absolute: &Path) -> Result<String, HttpError> {
    Ok(jail.relative(absolute)?)
}

/// Modification time in epoch milliseconds, floored.
fn modified_at_ms(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// A path the jail returned that no longer reads as inside it.
///
/// Unreachable for a path the jail itself produced a moment earlier, so it is
/// an invariant failure rather than something a caller did.
fn outside() -> HttpError {
    HttpError::from(GhostError::new(
        ErrorKind::Internal,
        "The resolved path is outside the workspace",
    ))
}

/// A filesystem failure nobody asked for.
///
/// The original error is kept as the source rather than only its text, so the
/// log line carries the errno the operator needs while the response carries the
/// opaque message a caller gets.
fn internal(error: std::io::Error) -> HttpError {
    let message = error.to_string();
    HttpError::from(GhostError::new(ErrorKind::Storage, message).with_source(error))
}

/// Whether the operating system refused the operation as nonsensical.
///
/// A directory moved inside itself is the one case that reaches here, and it
/// arrives as `EINVAL`. The raw number is checked beside the mapped kind
/// because the mapping is not guaranteed for every target.
fn is_invalid_input(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::InvalidInput || error.raw_os_error() == Some(22)
}

/// Sets one header, dropping a value that cannot be one.
///
/// Every value here is either a compile-time string or a decimal number, so the
/// fallible conversion cannot fail in practice — and a missing header is a
/// better outcome than a failed response for a case that cannot arise.
fn insert_header(headers: &mut axum::http::HeaderMap, name: header::HeaderName, value: &str) {
    if let Ok(value) = axum::http::HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

/// Runs blocking filesystem work off the async runtime.
///
/// Everything in this module touches the filesystem, and the files are whatever
/// the agent wrote — a directory listing or a half-megabyte read is not work to
/// do on a thread that is also serving the WebSocket.
async fn blocking<T, F>(work: F) -> Result<T, HttpError>
where
    F: FnOnce() -> Result<T, HttpError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(HttpError::from(GhostError::new(
            ErrorKind::Internal,
            format!("The filesystem task did not finish: {error}"),
        ))),
    }
}
