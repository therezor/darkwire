//! Installed environment definitions: resolution, validation, and digests.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use ghostai_core::ErrorKind;
use ghostai_security::PolicyStore;
use serde_json::{Value, json};

use common::message_of;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn definition() -> Value {
    json!({"schema": "ghostai.environment/1", "name": "dev", "image": DIGEST, "shared": true})
}

fn fixture() -> (tempfile::TempDir, PolicyStore) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("environments")).unwrap();
    write(&root, "environments/dev.yaml", &definition());
    let store = PolicyStore::new(root.path().to_path_buf());
    (root, store)
}

fn write(root: &tempfile::TempDir, relative: &str, value: &Value) {
    std::fs::write(root.path().join(relative), value.to_string()).unwrap();
}

#[test]
fn an_installed_definition_is_usable_with_no_second_step() {
    let (root, store) = fixture();
    assert!(store.require_environment("dev").is_ok());
    assert!(
        !root
            .path()
            .join("environments/dev.approval.sha256")
            .exists()
    );
}

#[test]
fn the_digest_is_the_bytes_on_disk_and_a_second_store_agrees() {
    let (root, store) = fixture();
    let installed = store.require_environment("dev").unwrap();
    assert_eq!(installed.digest.len(), 64);
    assert!(root.path().join("environments/dev.yaml").exists());

    let service = PolicyStore::new(root.path().to_path_buf());
    assert_eq!(
        service.require_environment("dev").unwrap().digest,
        installed.digest
    );
}

#[test]
fn editing_a_definition_moves_its_digest() {
    let (root, store) = fixture();
    let before = store.require_environment("dev").unwrap().digest;
    let mut changed = definition();
    changed["shared"] = json!(false);
    write(&root, "environments/dev.yaml", &changed);
    assert_ne!(store.require_environment("dev").unwrap().digest, before);
}

#[test]
fn a_definition_that_is_not_installed_says_so() {
    let (_root, store) = fixture();
    let message = message_of(&store.require_environment("absent"));
    assert!(message.contains("No environment is installed"), "{message}");
    assert!(message.contains("Settings"), "{message}");
}

#[test]
fn a_name_that_is_not_a_slug_never_reaches_the_filesystem() {
    let (_root, store) = fixture();
    for name in ["../outside", "sub/dir", "Research", "", &"x".repeat(65)] {
        assert_eq!(
            store.require_environment(name).unwrap_err().kind,
            ErrorKind::InvalidInput
        );
        assert!(store.environment_path(name).is_err());
    }
    assert_eq!(
        store.environment_path("dev").unwrap(),
        store.root().join("environments/dev.yaml")
    );
}

#[test]
fn a_manifest_must_name_itself_after_its_file() {
    let (root, store) = fixture();
    let mut environment = definition();
    environment["name"] = json!("other");
    write(&root, "environments/dev.yaml", &environment);
    assert!(message_of(&store.require_environment("dev")).contains("names itself \"other\""));
}

#[test]
fn an_environment_that_breaks_install_policy_is_refused_rather_than_resolved() {
    let (root, store) = fixture();
    let mut tagged = definition();
    tagged["image"] = json!("alpine:latest");
    write(&root, "environments/dev.yaml", &tagged);
    assert!(message_of(&store.require_environment("dev")).contains("digest"));
    assert_eq!(store.list_environments()[0].value, None);
    assert!(store.list_environments()[0].problem.is_some());
}

#[test]
fn a_broken_definition_is_listed_with_its_problem_rather_than_hidden() {
    let (root, store) = fixture();
    std::fs::write(root.path().join("environments/broken.yaml"), "{").unwrap();
    std::fs::write(root.path().join("environments/notes.txt"), "ignored").unwrap();

    let listed = store.list_environments();
    assert_eq!(
        listed
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["broken", "dev"]
    );
    assert_eq!(listed[0].value, None);
    assert!(
        listed[0]
            .problem
            .as_deref()
            .unwrap()
            .contains("not valid YAML")
    );
    assert!(listed[1].value.is_some());
    assert_eq!(listed[1].problem, None);
}

#[test]
fn an_install_with_no_policy_directory_lists_nothing() {
    let root = tempfile::tempdir().unwrap();
    let store = PolicyStore::new(root.path().join("policy"));
    assert!(store.list_environments().is_empty());
    assert!(store.require_environment("dev").is_err());
}
