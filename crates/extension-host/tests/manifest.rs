//! Discovery, precedence, and the version gate in front of both.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use ghostai_core::{Database, SystemClock};
use ghostai_extension_host::{
    V1_UNSUPPORTED, discover, refuses_version, schema_on_disk, settings_for,
};
use ghostai_protocol::{ExtensionSchemaVersion, ExtensionsConfig};
use ghostai_security::ExtensionStore;
use serde_json::json;
use tempfile::TempDir;

/// An install directory holding a manifest and the script it names.
fn install(root: &std::path::Path, id: &str, manifest: &serde_json::Value) -> std::path::PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ghostai.extension.yaml"),
        serde_json::to_string(manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("index.mjs"), "process.exit(0);\n").unwrap();
    dir
}

fn v2(id: &str) -> serde_json::Value {
    json!({"schema": "ghostai.extension/2", "id": id, "command": ["node", "index.mjs"]})
}

fn store(root: &std::path::Path) -> ExtensionStore {
    ExtensionStore::new(Database::in_memory().unwrap(), root, Arc::new(SystemClock)).unwrap()
}

#[test]
fn the_schema_is_readable_from_a_manifest_that_did_not_parse() {
    let temp = TempDir::new().unwrap();
    let one = install(temp.path(), "one", &v2("one"));
    assert_eq!(schema_on_disk(&one), Some(ExtensionSchemaVersion::V2));

    // The point of reading it raw: this manifest is missing `id`, so nothing
    // would parse it — and the row still needs to say which contract it is on.
    let two = temp.path().join("two");
    std::fs::create_dir_all(&two).unwrap();
    std::fs::write(
        two.join("ghostai.extension.yaml"),
        r#"{"schema":"ghostai.extension/1"}"#,
    )
    .unwrap();
    assert_eq!(schema_on_disk(&two), Some(ExtensionSchemaVersion::V1));

    // And the three ways there is nothing to read.
    let three = temp.path().join("three");
    std::fs::create_dir_all(&three).unwrap();
    assert_eq!(schema_on_disk(&three), None);
    std::fs::write(three.join("ghostai.extension.yaml"), "not yaml").unwrap();
    assert_eq!(schema_on_disk(&three), None);
    std::fs::write(
        three.join("ghostai.extension.yaml"),
        r#"{"schema":"ghostai.extension/9"}"#,
    )
    .unwrap();
    assert_eq!(schema_on_disk(&three), None);
}

#[test]
fn a_v1_bundle_is_refused_whatever_the_store_thinks_of_it() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    install(root, "two", &v2("two"));
    // A v1 whose policy also fails, so its resolution carries no manifest at
    // all — the case where the version has to be read off the disk.
    let one = root.join("one");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::write(
        one.join("ghostai.extension.yaml"),
        r#"{"schema":"ghostai.extension/1","id":"one","entry":"dist/missing.js"}"#,
    )
    .unwrap();

    let store = store(root);
    assert!(refuses_version(&store.resolve("one").unwrap()));
    assert!(!refuses_version(&store.resolve("two").unwrap()));

    // The sentence names the fix, not the host's limitation.
    assert!(V1_UNSUPPORTED.contains("ghostai.extension/2"));
    assert!(V1_UNSUPPORTED.contains("Rebuild"));
}

#[test]
fn discovery_is_the_scan_plus_what_load_names() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("extensions");
    std::fs::create_dir_all(&root).unwrap();
    install(&root, "alpha", &v2("alpha"));
    install(&root, "beta", &v2("beta"));
    // Not an extension id, so the scan skips it rather than reporting a row
    // nobody can act on.
    std::fs::create_dir_all(root.join("node_modules")).unwrap();

    let elsewhere = temp.path().join("checkouts");
    install(&elsewhere, "gamma", &v2("gamma"));

    let store = store(&root);
    let mut config = ExtensionsConfig::default();
    config
        .load
        .push(elsewhere.join("gamma").to_string_lossy().into_owned());
    // A path that is not a directory is warned about and skipped: a typo is
    // not an extension that is failing.
    config
        .load
        .push(temp.path().join("nowhere").to_string_lossy().into_owned());

    let found = discover(&store, &config);
    let ids: Vec<&str> = found.keys().map(String::as_str).collect();
    // Sorted, so two machines with the same extensions agree on the order.
    assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn an_explicit_path_wins_over_an_installed_extension_of_the_same_id() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("extensions");
    std::fs::create_dir_all(&root).unwrap();
    install(&root, "alpha", &v2("alpha"));

    let elsewhere = temp.path().join("checkouts");
    let shadow = install(&elsewhere, "alpha", &v2("alpha"));

    let store = store(&root);
    let mut config = ExtensionsConfig::default();
    config.load.push(shadow.to_string_lossy().into_owned());

    // The more specific statement wins: a scan is what happens to be there and
    // a `load` entry is something an operator wrote down.
    let found = discover(&store, &config);
    assert_eq!(found.len(), 1);
    assert_eq!(
        std::fs::canonicalize(&found["alpha"].dir).unwrap(),
        std::fs::canonicalize(&shadow).unwrap()
    );

    // And with `allowOverride` the shadowing is expected rather than warned at.
    config.allow_override = true;
    assert_eq!(discover(&store, &config).len(), 1);
}

#[test]
fn settings_are_this_extensions_block_and_nothing_else() {
    let mut config = ExtensionsConfig::default();
    config.settings.insert(
        "alpha".to_owned(),
        serde_json::from_value(json!({"greeting": "Ahoy"})).unwrap(),
    );

    let block = settings_for(&config, "alpha");
    assert_eq!(block["greeting"], json!("Ahoy"));
    // An extension with no block gets an empty one, never someone else's.
    assert!(settings_for(&config, "beta").is_empty());
}
