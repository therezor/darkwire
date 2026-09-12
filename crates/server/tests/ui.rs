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

use ghostai_server::ui::{INDEX_FILE, UiFile, UiRoot, may_fall_back};

/// A `dist/` as a bundler would leave one: a shell and a hashed asset.
fn bundle(root: &Path) {
    fs::write(
        root.join(INDEX_FILE),
        "<!doctype html><title>GhostAI</title>",
    )
    .unwrap();
    fs::create_dir_all(root.join("assets")).unwrap();
    fs::write(root.join("assets/app-abc123.js"), "console.log(1);\n").unwrap();
}

fn text(file: &UiFile) -> String {
    String::from_utf8(file.body.clone()).unwrap()
}

#[test]
fn a_directory_root_serves_the_shell() {
    let dir = tempfile::tempdir().unwrap();
    bundle(dir.path());
    let ui = UiRoot::Dir(dir.path().to_path_buf());

    let shell = ui.shell().expect("the shell is there");
    assert!(text(&shell).contains("<title>GhostAI</title>"));
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
        ghostai_core::workspace_files::DEFAULT_MIME_TYPE
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
