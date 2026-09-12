//! The one version lives in the root `Cargo.toml` and the root `package.json`, and
//! they must agree: the web bundle and the binary ship together.

// `allow-unwrap-in-tests` covers a `#[test]` body; the fixture helpers beside
// them are ordinary functions, and a helper that returned `Result` would make
// every case carry a `?` for a temporary directory that cannot fail in a way
// the case could act on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture helpers in an integration test"
)]

use std::path::Path;

#[test]
fn workspace_version_matches_root_package_json() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join("package.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
}
