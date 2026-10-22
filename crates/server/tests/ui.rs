//! Serving the built UI, and the one rule the fallback has.
//!
//! A single-page app owns URLs the server has never heard of, so anything the
//! router did not match has to become the shell — except under `/api` and
//! `/ws`, where a 404 is a client bug and answering it with HTML turns "no such
//! route" into "the JSON parser failed" somewhere unrelated.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;
use std::path::Path;

use darkwire_server::ui::{
    CACHE_IMMUTABLE, CACHE_REVALIDATE, INDEX_FILE, UiFile, UiRoot, accepts, is_build_asset,
    may_fall_back, not_modified,
};

/// A `dist/` as a bundler would leave one: a shell and a hashed asset.
fn bundle(root: &Path) {
    fs::write(
        root.join(INDEX_FILE),
        "<!doctype html><title>DarkWire</title>",
    )
    .unwrap();
    fs::create_dir_all(root.join("assets")).unwrap();
    fs::write(root.join("assets/app-abc123.js"), "console.log(1);\n").unwrap();
}

fn text(file: &UiFile) -> String {
    String::from_utf8(file.body.to_vec()).unwrap()
}

/// The copies the web build writes beside each hashed asset.
fn compressed(root: &Path) {
    fs::write(root.join("assets/app-abc123.js.br"), "brotli bytes").unwrap();
    fs::write(root.join("assets/app-abc123.js.gz"), "gzip bytes").unwrap();
}

#[test]
fn a_directory_root_serves_the_shell() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    let shell = ui.shell().expect("the shell is there");
    assert!(text(&shell).contains("<title>DarkWire</title>"));
    assert_eq!(shell.content_type, "text/html; charset=utf-8");
}

#[test]
fn a_directory_root_serves_the_asset_bundle() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    let asset = ui
        .asset("/assets/app-abc123.js")
        .expect("the asset is there");
    assert_eq!(text(&asset), "console.log(1);\n");
    assert_eq!(asset.content_type, "text/javascript; charset=utf-8");
}

#[test]
fn a_path_that_names_nothing_is_absent_rather_than_empty() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    assert!(ui.asset("/assets/gone.js").is_none());
}

#[test]
fn a_traversal_cannot_leave_the_bundle() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    let outside = dir.path().join("secret.txt");
    fs::write(&outside, "not yours").unwrap();
    let inner = dir.path().join("dist");
    fs::create_dir_all(&inner).unwrap();
    bundle(&inner);
    let ui = UiRoot::Dir(inner);

    // Every `..` is refused outright rather than normalised, because there is
    // no legitimate asset path that contains one.
    assert!(ui.asset("/../secret.txt").is_none());
    assert!(ui.asset("/assets/../../secret.txt").is_none());
}

#[test]
fn an_absolute_path_is_refused_rather_than_joined() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    // Leading slashes are the request path's, not a filesystem root: they are
    // trimmed, and what is left has to name something in the bundle.
    assert!(ui.asset("//etc/passwd").is_none());
    assert!(ui.asset("/").is_none());
}

#[test]
fn no_ui_serves_nothing_at_all() {
    let ui = UiRoot::None;
    assert!(!ui.is_serving());
    assert!(ui.shell().is_none());
    assert!(ui.asset("/assets/app.js").is_none());
}

#[test]
fn a_directory_root_is_serving() {
    assert!(UiRoot::Dir("/tmp".into()).is_serving());
    assert!(UiRoot::Embedded.is_serving());
}

#[test]
fn only_a_get_outside_the_api_may_fall_back_to_the_shell() {
    assert!(may_fall_back("GET", "/"));
    assert!(may_fall_back("GET", "/settings"));
    assert!(may_fall_back("GET", "/session/abc-123"));

    // An unknown API path is a client bug, and answering it with HTML makes it
    // surface as a JSON parse error somewhere else entirely.
    assert!(!may_fall_back("GET", "/api/nope"));
    assert!(!may_fall_back("GET", "/ws"));
    // A POST to an unknown path is a client bug too.
    assert!(!may_fall_back("POST", "/anything"));
    assert!(!may_fall_back("DELETE", "/anything"));
}

#[test]
fn the_media_type_comes_from_the_extension() {
    let dir = tempfile::tempdir().unwrap();
    for (name, expected) in [
        ("a.css", "text/css; charset=utf-8"),
        ("a.json", "application/json"),
        ("a.map", "application/json"),
        ("a.svg", "image/svg+xml"),
        ("a.woff2", "font/woff2"),
        ("a.png", "image/png"),
        ("a.webmanifest", "application/manifest+json"),
    ] {
        fs::write(dir.path().join(name), "x").unwrap();
        let ui = UiRoot::Dir(dir.path().to_path_buf());
        assert_eq!(
            ui.asset(&format!("/{name}")).expect(name).content_type,
            expected,
            "{name}"
        );
    }
}

#[test]
fn an_unknown_extension_falls_back_to_the_default_media_type() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("blob.qqq"), "x").unwrap();
    let ui = UiRoot::Dir(dir.path().to_path_buf());
    assert_eq!(
        ui.asset("/blob.qqq").unwrap().content_type,
        darkwire_core::workspace_files::DEFAULT_MIME_TYPE
    );
}

#[test]
fn a_file_with_no_extension_still_serves() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("LICENSE"), "MIT").unwrap();
    let ui = UiRoot::Dir(dir.path().to_path_buf());
    let file = ui.asset("/LICENSE").expect("served");
    assert_eq!(text(&file), "MIT");
}

/// A build that claims an embedded bundle must be able to answer with it.
///
/// `build.rs` refuses a build whose `packages/web/dist` is missing, so the only
/// way to reach `has_embedded_bundle()` returning true is with a real bundle
/// compiled in — and the one file the fallback cannot do without is the shell.
/// This is the assertion that turns "the embed was configured" into "the embed
/// has something in it", which an empty or half-written `dist` would not.
#[test]
fn an_embedded_build_can_serve_its_shell() {
    if !darkwire_server::ui::has_embedded_bundle() {
        return;
    }
    let shell = UiRoot::Embedded.shell().expect("a shell to fall back to");
    assert!(!shell.body.is_empty());
    assert_eq!(shell.content_type, "text/html; charset=utf-8");
}

// Compressed copies and caching

#[test]
fn a_hashed_asset_goes_out_as_the_best_copy_the_client_takes() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    compressed(dir.path());
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    let brotli = ui
        .negotiate("/assets/app-abc123.js", "gzip, deflate, br")
        .unwrap();
    assert_eq!(brotli.encoding, Some("br"));
    assert_eq!(text(&brotli), "brotli bytes");
    // The type is the file's, not the copy's.
    assert_eq!(brotli.content_type, "text/javascript; charset=utf-8");

    // A browser offers Brotli only over HTTPS, so plain HTTP on a LAN gets gzip.
    let gzip = ui
        .negotiate("/assets/app-abc123.js", "gzip, deflate")
        .unwrap();
    assert_eq!(gzip.encoding, Some("gzip"));

    let plain = ui.negotiate("/assets/app-abc123.js", "").unwrap();
    assert_eq!(plain.encoding, None);
    assert_eq!(text(&plain), "console.log(1);\n");
}

#[test]
fn a_refused_encoding_is_not_sent() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    compressed(dir.path());
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    let file = ui
        .negotiate("/assets/app-abc123.js", "br;q=0, gzip")
        .unwrap();
    assert_eq!(file.encoding, Some("gzip"));
}

#[test]
fn an_asset_without_copies_goes_out_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    let file = ui.negotiate("/assets/app-abc123.js", "br, gzip").unwrap();
    assert_eq!(file.encoding, None);
    assert!(file.varies);
}

#[test]
fn a_hashed_asset_is_cached_for_good_and_the_shell_is_revalidated() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    let asset = ui.asset("/assets/app-abc123.js").unwrap();
    assert_eq!(asset.cache_control, CACHE_IMMUTABLE);
    assert_eq!(asset.etag, None);

    let shell = ui.shell().unwrap();
    assert_eq!(shell.cache_control, CACHE_REVALIDATE);
    assert!(!shell.varies);
    let etag = shell.etag.unwrap();
    assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
}

#[test]
fn the_shell_validator_follows_its_contents() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    let before = ui.shell().unwrap().etag;
    assert_eq!(ui.shell().unwrap().etag, before);

    fs::write(
        dir.path().join(INDEX_FILE),
        "<!doctype html><title>New</title>",
    )
    .unwrap();
    assert_ne!(ui.shell().unwrap().etag, before);
}

#[test]
fn a_file_outside_assets_has_no_compressed_copy() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    fs::write(dir.path().join("boot.js.br"), "brotli bytes").unwrap();
    fs::write(dir.path().join("boot.js"), "boot").unwrap();
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    let file = ui.negotiate("/boot.js", "br").unwrap();
    assert_eq!(file.encoding, None);
    assert_eq!(text(&file), "boot");
}

#[test]
fn only_build_output_is_a_build_asset() {
    assert!(is_build_asset("/assets/app-abc123.js"));
    assert!(!is_build_asset("/settings"));
    assert!(!is_build_asset("/assetsy"));
}

#[test]
fn if_none_match_is_compared_weakly_and_as_a_list() {
    let etag = "\"abc\"";
    assert!(not_modified("\"abc\"", etag));
    assert!(not_modified("W/\"abc\"", etag));
    assert!(not_modified("\"x\", \"abc\"", etag));
    assert!(not_modified("*", etag));
    assert!(!not_modified("\"abd\"", etag));
    assert!(!not_modified("", etag));
}

#[test]
fn accept_encoding_honours_names_wildcards_and_zero_weights() {
    assert!(accepts("gzip, deflate, br", "br"));
    assert!(accepts("GZIP", "gzip"));
    assert!(accepts("*", "br"));
    assert!(accepts("br;q=0.5", "br"));
    assert!(!accepts("br;q=0", "br"));
    assert!(!accepts("gzip", "br"));
    assert!(!accepts("", "gzip"));
    // Named, the refusal stands whatever the wildcard says.
    assert!(!accepts("gzip;q=0, *", "gzip"));
    assert!(accepts("gzip;q=0, *", "br"));
}
