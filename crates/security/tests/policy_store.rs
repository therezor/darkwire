//! Installed definitions: what resolves, what refuses, and what the digest is for.

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

fn manifest() -> Value {
    json!({
        "schema": "ghostai.toolbox/1",
        "name": "research",
        "tools": [{"name": "status", "definition": "status", "permission": "ask"}],
    })
}

fn operation() -> Value {
    json!({
        "schema": "ghostai.tool/1",
        "description": "Repository status",
        "implementation": {
            "kind": "command",
            "executable": "/usr/bin/git",
            "argv": ["status", "--porcelain"],
        },
        "parameters": {"type": "object", "properties": {}, "additionalProperties": false},
    })
}

fn definition() -> Value {
    json!({"schema": "ghostai.container/1", "name": "dev", "image": DIGEST, "shared": true})
}

fn fixture() -> (tempfile::TempDir, PolicyStore) {
    let root = tempfile::tempdir().unwrap();
    for directory in ["toolboxes", "tool-definitions", "containers"] {
        std::fs::create_dir_all(root.path().join(directory)).unwrap();
    }
    write(&root, "toolboxes/research.yaml", &manifest());
    write(&root, "tool-definitions/status.yaml", &operation());
    write(&root, "containers/dev.yaml", &definition());
    let store = PolicyStore::new(root.path().to_path_buf());
    (root, store)
}

fn write(root: &tempfile::TempDir, relative: &str, value: &Value) {
    std::fs::write(root.path().join(relative), value.to_string()).unwrap();
}

#[test]
fn an_installed_definition_is_usable_with_no_second_step() {
    let (root, store) = fixture();
    assert!(store.require_toolbox("research").is_ok());
    assert!(store.require_container("dev").is_ok());

    // The file is the whole policy. Nothing beside it records a decision, so
    // there is no sidecar for an operator to forget.
    assert!(
        !root
            .path()
            .join("toolboxes/research.approval.sha256")
            .exists()
    );
    assert!(!root.path().join("containers/dev.approval.sha256").exists());
}

#[test]
fn the_digest_is_the_bytes_on_disk_and_a_second_store_agrees() {
    let (root, store) = fixture();
    let installed = store.require_toolbox("research").unwrap();
    assert_eq!(installed.digest().len(), 64);
    assert_eq!(installed.path, root.path().join("toolboxes/research.yaml"));

    // The sandbox service is a separate process over the same directory, and
    // holds no state of its own that could disagree.
    let service = PolicyStore::new(root.path().to_path_buf());
    assert_eq!(
        service.require_toolbox("research").unwrap().digest(),
        installed.digest()
    );
}

#[test]
fn editing_a_shared_definition_moves_every_digest_that_covers_it() {
    let (root, store) = fixture();
    let before = store
        .require_toolbox("research")
        .unwrap()
        .digest()
        .to_owned();

    let mut changed = operation();
    changed["implementation"]["argv"] = json!(["push"]);
    write(&root, "tool-definitions/status.yaml", &changed);

    // Still usable — the edit is the operator's decision — but it is visibly a
    // different toolbox, which is what cancels a command running under the old
    // one and stops a warm container being reused for it.
    let after = store.require_toolbox("research").unwrap();
    assert_ne!(after.digest(), before);
}

#[test]
fn a_toolbox_and_a_container_have_unrelated_digests() {
    let (root, store) = fixture();
    let toolbox = store
        .require_toolbox("research")
        .unwrap()
        .digest()
        .to_owned();
    let container = store.require_container("dev").unwrap().digest;
    assert_ne!(toolbox, container);

    let mut changed = definition();
    changed["shared"] = json!(false);
    write(&root, "containers/dev.yaml", &changed);

    assert_ne!(store.require_container("dev").unwrap().digest, container);
    assert_eq!(store.require_toolbox("research").unwrap().digest(), toolbox);
}

#[test]
fn a_definition_that_is_not_installed_says_so_rather_than_failing_to_parse() {
    let (_root, store) = fixture();
    let toolbox = message_of(&store.require_toolbox("absent"));
    assert!(toolbox.contains("No toolbox is installed"), "{toolbox}");
    assert!(toolbox.contains("ghostai preset install"), "{toolbox}");

    let container = message_of(&store.require_container("absent"));
    assert!(
        container.contains("No container is installed"),
        "{container}"
    );
    assert!(container.contains("Settings"), "{container}");
}

#[test]
fn a_name_that_is_not_a_slug_never_reaches_the_filesystem() {
    let (_root, store) = fixture();
    for name in ["../outside", "sub/dir", "Research", "", &"x".repeat(65)] {
        let error = store.require_toolbox(name).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{name}");
        assert!(error.message.contains("Not a toolbox name"), "{name}");
        assert_eq!(
            store.require_container(name).unwrap_err().kind,
            ErrorKind::InvalidInput
        );
        assert!(store.toolbox_path(name).is_err());
        assert!(store.container_path(name).is_err());
    }
    assert_eq!(
        store.toolbox_path("research").unwrap(),
        store.root().join("toolboxes/research.yaml")
    );
    assert_eq!(
        store.container_path("dev").unwrap(),
        store.root().join("containers/dev.yaml")
    );
}

#[test]
fn a_manifest_must_name_itself_after_its_file() {
    let (root, store) = fixture();
    let mut renamed = manifest();
    renamed["name"] = json!("other");
    write(&root, "toolboxes/research.yaml", &renamed);
    assert!(message_of(&store.require_toolbox("research")).contains("names itself \"other\""));

    let mut container = definition();
    container["name"] = json!("other");
    write(&root, "containers/dev.yaml", &container);
    assert!(message_of(&store.require_container("dev")).contains("names itself \"other\""));
}

#[test]
fn a_container_that_breaks_install_policy_is_refused_rather_than_resolved() {
    let (root, store) = fixture();
    let mut tagged = definition();
    tagged["image"] = json!("alpine:latest");
    write(&root, "containers/dev.yaml", &tagged);
    assert!(message_of(&store.require_container("dev")).contains("digest"));
    assert_eq!(store.list_containers()[0].value, None);
    assert!(store.list_containers()[0].problem.is_some());
}

#[test]
fn a_broken_definition_is_listed_with_its_problem_rather_than_hidden() {
    let (root, store) = fixture();
    std::fs::write(root.path().join("toolboxes/broken.yaml"), "{").unwrap();
    // Not a `.yaml` file, so not a definition at all.
    std::fs::write(root.path().join("toolboxes/notes.txt"), "ignored").unwrap();

    let listed = store.list_toolboxes();
    assert_eq!(
        listed
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["broken", "research"]
    );
    let broken = &listed[0];
    assert_eq!(broken.value, None);
    assert!(
        broken
            .problem
            .as_deref()
            .unwrap()
            .contains("not valid YAML")
    );

    // One that parses has nothing to report. There is no second file it could
    // be out of step with any more.
    let research = &listed[1];
    assert!(research.value.is_some());
    assert_eq!(research.problem, None);
}

#[test]
fn an_install_with_no_policy_directory_lists_nothing() {
    let root = tempfile::tempdir().unwrap();
    let store = PolicyStore::new(root.path().join("policy"));
    assert!(store.list_toolboxes().is_empty());
    assert!(store.list_containers().is_empty());
    assert!(store.require_toolbox("research").is_err());
}
