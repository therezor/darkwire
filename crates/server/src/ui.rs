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
//!
//! Everything under `assets/` has a content hash in its name, so it is cached
//! for a year and never asked for again. The build also writes a Brotli and a
//! gzip copy of each, and the one the browser accepts goes out instead: a small
//! board serves a page of compressed bytes rather than compressing on every
//! request. The few files outside `assets/` keep their names across builds, so
//! they carry an `ETag` and are revalidated on every load.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest as _, Sha256};

/// Paths that must 404 as JSON rather than falling back to the shell.
pub const API_PREFIXES: [&str; 2] = ["/api", "/ws"];

/// The document a client-routed path falls back to.
pub const INDEX_FILE: &str = "index.html";

/// Where the build puts files whose names change with their contents.
pub const ASSETS_DIR: &str = "assets/";

/// For a file whose name is its version.
pub const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// For a file whose name stays the same when it changes: keep it, but ask
/// first. The `ETag` makes the asking cheap.
pub const CACHE_REVALIDATE: &str = "no-cache";

/// The encodings the build writes, best first, with the suffix of each copy.
const ENCODINGS: [(&str, &str); 2] = [("br", ".br"), ("gzip", ".gz")];

/// The bundle compiled into this build, when there is one.
///
/// `rust-embed` reads the folder at compile time, so a checkout that has never
/// run `pnpm build` fails to compile the crate rather than the request.
/// `build.rs` sets `headless_ui` only when a headless build was asked for —
/// `DARKWIRE_HEADLESS_BUILD=1`, which is how the workspace CI builds without the
/// bundle, or `embed-ui` switched off. A bundle that is merely missing is an
/// error there, not a quiet fallback.
///
/// Source maps stay out. They are most of the bundle's bytes, a browser asks
/// for them only with its tools open, and `--ui <dir>` still serves them.
#[cfg(all(feature = "embed-ui", not(headless_ui)))]
#[derive(rust_embed::Embed)]
#[folder = "../../packages/web/dist"]
#[exclude = "*.map"]
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
    /// The bytes, borrowed from the binary when the bundle is compiled in.
    pub body: Cow<'static, [u8]>,
    /// The media type, from the file's own extension.
    pub content_type: &'static str,
    /// `br` or `gzip` when `body` is a compressed copy, `None` for the file.
    pub encoding: Option<&'static str>,
    /// What a browser may do with it before asking again.
    pub cache_control: &'static str,
    /// Whether the answer depended on `Accept-Encoding`, which a cache in the
    /// middle has to know.
    pub varies: bool,
    /// A validator, for a file whose name does not change with its contents.
    pub etag: Option<String>,
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
        self.negotiate(request_path, "")
    }

    /// One asset, as the smallest copy the client accepts.
    ///
    /// `accept_encoding` is the request's header, empty when it sent none. Only
    /// files under `assets/` have compressed copies.
    pub fn negotiate(&self, request_path: &str, accept_encoding: &str) -> Option<UiFile> {
        let relative = sanitise(request_path)?;
        let content_type = content_type_for(&relative);
        let hashed = relative.starts_with(ASSETS_DIR);

        if hashed {
            for (encoding, suffix) in ENCODINGS {
                if !accepts(accept_encoding, encoding) {
                    continue;
                }
                if let Some(found) = self.read(&format!("{relative}{suffix}")) {
                    return Some(UiFile {
                        body: found.body,
                        content_type,
                        encoding: Some(encoding),
                        cache_control: CACHE_IMMUTABLE,
                        varies: true,
                        etag: None,
                    });
                }
            }
        }

        let found = self.read(&relative)?;
        let etag = if hashed {
            None
        } else {
            Some(etag_of(found.hash.unwrap_or_else(|| sha256(&found.body))))
        };
        Some(UiFile {
            body: found.body,
            content_type,
            encoding: None,
            cache_control: if hashed {
                CACHE_IMMUTABLE
            } else {
                CACHE_REVALIDATE
            },
            varies: hashed,
            etag,
        })
    }

    fn read(&self, relative: &str) -> Option<Found> {
        match self {
            UiRoot::None => None,
            UiRoot::Dir(root) => std::fs::read(root.join(relative)).ok().map(|body| Found {
                body: Cow::Owned(body),
                hash: None,
            }),
            UiRoot::Embedded => embedded(relative),
        }
    }

    /// The shell, for a path only the client knows about.
    pub fn shell(&self) -> Option<UiFile> {
        self.asset(INDEX_FILE)
    }
}

/// One file's bytes, and its hash when the build already worked it out.
struct Found {
    body: Cow<'static, [u8]>,
    hash: Option<[u8; 32]>,
}

/// Whether a path names build output, which never falls back to the shell.
///
/// A tab left open across an upgrade asks for chunks the new build does not
/// have. The shell in their place is HTML parsed as a script, which fails with
/// a message about syntax rather than a missing file.
pub fn is_build_asset(path: &str) -> bool {
    path.trim_start_matches('/').starts_with(ASSETS_DIR)
}

/// Whether `If-None-Match` names this `ETag`, so the answer can be a 304.
///
/// A weak comparison, as the header asks for: `W/` is ignored on both sides.
pub fn not_modified(if_none_match: &str, etag: &str) -> bool {
    let bare = |tag: &str| tag.trim().trim_start_matches("W/").to_owned();
    let wanted = bare(etag);
    if_none_match
        .split(',')
        .any(|tag| tag.trim() == "*" || bare(tag) == wanted)
}

/// Whether `Accept-Encoding` allows `coding`. An entry with `q=0` refuses it,
/// and an entry naming the coding outranks a `*`.
pub fn accepts(accept_encoding: &str, coding: &str) -> bool {
    let mut wildcard = false;
    for entry in accept_encoding.split(',') {
        let mut parts = entry.split(';');
        let name = parts.next().unwrap_or_default().trim();
        let allowed = parts.all(|parameter| {
            let parameter = parameter.trim();
            parameter
                .strip_prefix("q=")
                .or_else(|| parameter.strip_prefix("Q="))
                .is_none_or(|q| q.trim().parse::<f32>().is_ok_and(|q| q > 0.0))
        });
        if name.eq_ignore_ascii_case(coding) {
            return allowed;
        }
        if name == "*" {
            wildcard = allowed;
        }
    }
    wildcard
}

fn sha256(body: &[u8]) -> [u8; 32] {
    Sha256::digest(body).into()
}

/// A strong `ETag` from a content hash. Half of it is plenty to tell builds
/// apart.
fn etag_of(hash: [u8; 32]) -> String {
    let mut tag = String::with_capacity(34);
    tag.push('"');
    for byte in &hash[..16] {
        let _ = write!(tag, "{byte:02x}");
    }
    tag.push('"');
    tag
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
fn embedded(relative: &str) -> Option<Found> {
    let file = Bundle::get(relative)?;
    Some(Found {
        hash: Some(file.metadata.sha256_hash()),
        body: file.data,
    })
}

/// No bundle was compiled in, so `UiRoot::Embedded` has nothing to answer with.
#[cfg(not(all(feature = "embed-ui", not(headless_ui))))]
fn embedded(relative: &str) -> Option<Found> {
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
        _ => darkwire_core::workspace_files::DEFAULT_MIME_TYPE,
    }
}
