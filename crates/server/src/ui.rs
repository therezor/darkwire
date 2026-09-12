//! Serving the built single-page app, and the fallback that goes with it.
//!
//! A single-page app owns its own URLs — `/settings`, `/session/abc` — and none
//! of them exist on the server, so a reload of any page but `/` is a 404 unless
//! the shell is served for it. That is the whole of the fallback, and its one
//! rule is the exclusion below: an unknown `/api` path is a client bug, and
//! answering it with HTML makes it surface as a JSON parse error somewhere else
//! entirely.
//!
//! The bundle is normally compiled in. Serving it from memory is not only a
//! packaging convenience: it removes the "restart the server after a UI build"
//! gotcha, because there is no longer a directory whose contents could have
//! moved under a running process. `--ui <dir>` is the escape for developing the
//! UI itself, and a build with neither is honest rather than broken — `/api`
//! works and `GET /` is a JSON 404.

use std::path::{Component, Path, PathBuf};

/// Paths that must 404 as JSON rather than falling back to the shell.
pub const API_PREFIXES: [&str; 2] = ["/api", "/ws"];

/// The document a client-routed path falls back to.
pub const INDEX_FILE: &str = "index.html";

/// The bundle compiled into this build, when there is one.
///
/// `rust-embed` reads the folder at compile time, so a checkout that has never
/// run `pnpm build` fails to compile the crate rather than the request.
/// `build.rs` sets `headless_ui` only when a headless build was asked for —
/// `GHOSTAI_HEADLESS_BUILD=1`, which is how the workspace CI builds without the
/// bundle, or `embed-ui` switched off. A bundle that is merely missing is an
/// error there, not a quiet fallback.
#[cfg(all(feature = "embed-ui", not(headless_ui)))]
#[derive(rust_embed::Embed)]
#[folder = "../../packages/web/dist"]
struct Bundle;

/// Where the served UI comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UiRoot {
    /// The bundle compiled into this binary.
    Embedded,
    /// A directory on disk, which is what `--ui <dir>` selects.
    Dir(PathBuf),
    /// No UI at all: `/api` works and every other `GET` is a JSON 404.
    #[default]
    None,
}

/// One file the UI layer can answer with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiFile {
    /// The bytes.
    pub body: Vec<u8>,
    /// The media type, from the file's own extension.
    pub content_type: &'static str,
}

impl UiRoot {
    /// Whether this build can serve a UI at all.
    pub fn is_serving(&self) -> bool {
        !matches!(self, UiRoot::None)
    }

    /// One asset by its request path, or `None` when there is no such file.
    ///
    /// The path is taken as a *request* path, not a filesystem one: every
    /// `..` and every absolute root is dropped before it is joined, so a
    /// request for `/../../etc/passwd` can only ever name something inside the
    /// bundle. The embedded case cannot escape at all — it is a lookup in a map
    /// compiled into the binary — and the directory case is the one that needs
    /// the rule.
    pub fn asset(&self, request_path: &str) -> Option<UiFile> {
        let relative = sanitise(request_path)?;
        match self {
            UiRoot::None => None,
            UiRoot::Dir(root) => {
                let file = root.join(&relative);
                let body = std::fs::read(&file).ok()?;
                Some(UiFile {
                    body,
                    content_type: content_type_for(&relative),
                })
            }
            UiRoot::Embedded => embedded(&relative),
        }
    }

    /// The shell, for a path only the client knows about.
    pub fn shell(&self) -> Option<UiFile> {
        self.asset(INDEX_FILE)
    }
}

/// Whether a request may fall back to the shell.
///
/// Only a `GET`, and never under `/api` or `/ws`: a `POST` to an unknown path
/// is a client bug, and so is an unknown API path, and answering either with
/// HTML hides the mistake behind a parse error somewhere unrelated.
pub fn may_fall_back(method: &str, path: &str) -> bool {
    method == "GET" && !API_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

/// A request path reduced to a relative path that cannot leave the bundle.
fn sanitise(request_path: &str) -> Option<String> {
    let mut out = PathBuf::new();
    for component in Path::new(request_path.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => out.push(part),
            // Every other component is a way out of the tree, and there is no
            // legitimate asset path that contains one.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
            Component::CurDir => {}
        }
    }
    let relative = out.to_str()?.to_owned();
    if relative.is_empty() {
        None
    } else {
        Some(relative)
    }
}

#[cfg(all(feature = "embed-ui", not(headless_ui)))]
fn embedded(relative: &str) -> Option<UiFile> {
    let file = Bundle::get(relative)?;
    Some(UiFile {
        body: file.data.into_owned(),
        content_type: content_type_for(relative),
    })
}

/// No bundle was compiled in, so `UiRoot::Embedded` has nothing to answer with.
#[cfg(not(all(feature = "embed-ui", not(headless_ui))))]
fn embedded(relative: &str) -> Option<UiFile> {
    let _ = relative;
    None
}

/// Whether this build carries an embedded bundle.
///
/// The caller resolving `--ui` needs it to tell "no flag on a headless build"
/// from "no flag on a normal build", which are a JSON 404 and a served shell.
pub fn has_embedded_bundle() -> bool {
    cfg!(all(feature = "embed-ui", not(headless_ui)))
}

/// The media type for one bundle file.
///
/// A short table rather than a database: a Vite `dist` holds a shell, hashed
/// JavaScript and CSS, a font, an icon and a couple of images, and a wrong
/// answer for anything outside that list is a file the bundle does not contain.
fn content_type_for(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        // A source map is JSON, and a browser fetches it only when the tools
        // are open.
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        _ => ghostai_core::workspace_files::DEFAULT_MIME_TYPE,
    }
}
