//! The approval ledger: two files, and every way they can disagree.

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
    write(&root, "toolboxes/research.json", &manifest());
    write(&root, "tool-definitions/status.json", &operation());
    write(&root, "containers/dev.json", &definition());
    let store = PolicyStore::new(root.path().to_path_buf());
    (root, store)
}

fn write(root: &tempfile::TempDir, relative: &str, value: &Value) {
    std::fs::write(root.path().join(relative), value.to_string()).unwrap();
}

#[test]
fn an_installed_definition_is_not_an_approved_one() {
    let (_root, store) = fixture();
    let never = store.require_toolbox("research").unwrap_err();
    assert_eq!(never.kind, ErrorKind::Config);
    assert!(never.message.contains("never been approved"));
    assert!(never.message.contains("ghostai toolbox approve research"));

    let container = store.require_container("dev").unwrap_err();
    assert!(container.message.contains("never been approved"));
    assert!(container.message.contains("ghostai container approve dev"));

    store.approve_toolbox("research").unwrap();
    store.approve_container("dev").unwrap();
    assert!(store.require_toolbox("research").is_ok());
    assert!(store.require_container("dev").is_ok());
}

#[test]
fn the_approval_is_a_file_a_second_store_reads_the_same_way() {
    let (root, store) = fixture();
    let approved = store.approve_toolbox("research").unwrap();
    assert_eq!(approved.sha256().len(), 64);
    assert_eq!(approved.path, root.path().join("toolboxes/research.json"));
    assert!(
        root.path()
            .join("toolboxes/research.approval.sha256")
            .is_file()
    );

    // The sandbox service is a separate process over the same directory, and
    // holds no state of its own that could disagree.
    let service = PolicyStore::new(root.path().to_path_buf());
    assert_eq!(
        service.require_toolbox("research").unwrap().sha256(),
        approved.sha256()
    );
}

#[test]
fn editing_a_shared_definition_revokes_every_toolbox_that_names_it() {
    let (root, store) = fixture();
    store.approve_toolbox("research").unwrap();
    let mut changed = operation();
    changed["implementation"]["argv"] = json!(["push"]);
    write(&root, "tool-definitions/status.json", &changed);

    let drifted = store.require_toolbox("research").unwrap_err();
    assert!(drifted.message.contains("changed since it was approved"));
    assert!(drifted.details["approved"].is_string());
    assert!(drifted.details["actual"].is_string());
    assert_ne!(drifted.details["approved"], drifted.details["actual"]);

    store.approve_toolbox("research").unwrap();
    assert!(store.require_toolbox("research").is_ok());
}

#[test]
fn the_two_approvals_are_independent() {
    let (root, store) = fixture();
    let toolbox = store.approve_toolbox("research").unwrap();
    let container = store.approve_container("dev").unwrap();
    assert_ne!(toolbox.sha256(), container.sha256);

    let mut changed = definition();
    changed["shared"] = json!(false);
    write(&root, "containers/dev.json", &changed);

    assert!(store.require_container("dev").is_err());
    assert_eq!(
        store.require_toolbox("research").unwrap().sha256(),
        toolbox.sha256()
    );
    assert!(!store.list_containers()[0].approved);
    assert!(store.list_toolboxes()[0].approved);
}

#[test]
fn revoking_forgets_the_approval_and_keeps_the_definition() {
    let (root, store) = fixture();
    store.approve_toolbox("research").unwrap();
    store.approve_container("dev").unwrap();

    store.revoke_toolbox("research").unwrap();
    store.revoke_container("dev").unwrap();
    assert!(store.require_toolbox("research").is_err());
    assert!(store.require_container("dev").is_err());
    assert!(root.path().join("toolboxes/research.json").is_file());
    assert!(root.path().join("containers/dev.json").is_file());

    // Revoking twice is not an error: the second call has nothing to forget.
    assert!(store.revoke_toolbox("research").is_ok());
    assert!(store.revoke_container("dev").is_ok());
}

#[test]
fn a_definition_that_is_not_installed_says_so_rather_than_failing_to_parse() {
    let (_root, store) = fixture();
    for message in [
        message_of(&store.require_toolbox("absent")),
        message_of(&store.approve_toolbox("absent")),
    ] {
        assert!(message.contains("No toolbox is installed"), "{message}");
        assert!(message.contains("ghostai preset install"), "{message}");
    }
    for message in [
        message_of(&store.require_container("absent")),
        message_of(&store.approve_container("absent")),
    ] {
        assert!(message.contains("No container is installed"), "{message}");
    }
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
        store.root().join("toolboxes/research.json")
    );
    assert_eq!(
        store.container_path("dev").unwrap(),
        store.root().join("containers/dev.json")
    );
}

#[test]
fn a_manifest_must_name_itself_after_its_file() {
    let (root, store) = fixture();
    let mut renamed = manifest();
    renamed["name"] = json!("other");
    write(&root, "toolboxes/research.json", &renamed);
    assert!(message_of(&store.require_toolbox("research")).contains("names itself \"other\""));

    let mut container = definition();
    container["name"] = json!("other");
    write(&root, "containers/dev.json", &container);
    assert!(message_of(&store.require_container("dev")).contains("names itself \"other\""));
}

#[test]
fn a_container_that_breaks_install_policy_is_refused_before_it_can_be_approved() {
    let (root, store) = fixture();
    let mut tagged = definition();
    tagged["image"] = json!("alpine:latest");
    write(&root, "containers/dev.json", &tagged);
    assert!(message_of(&store.approve_container("dev")).contains("digest"));
    assert_eq!(store.list_containers()[0].value, None);
    assert!(store.list_containers()[0].problem.is_some());
}

#[test]
fn a_broken_definition_is_listed_with_its_problem_rather_than_hidden() {
    let (root, store) = fixture();
    std::fs::write(root.path().join("toolboxes/broken.json"), "not json").unwrap();
    // Not a `.json` file, so not a definition at all.
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
            .contains("not valid JSON")
    );
    assert!(!broken.approved);

    let research = &listed[1];
    assert!(research.value.is_some());
    assert!(!research.approved);
    assert_eq!(
        research.problem.as_deref(),
        Some("not approved, or changed since approval")
    );
}

#[test]
fn an_install_with_no_policy_directory_lists_nothing() {
    let root = tempfile::tempdir().unwrap();
    let store = PolicyStore::new(root.path().join("policy"));
    assert!(store.list_toolboxes().is_empty());
    assert!(store.list_containers().is_empty());
    assert!(store.require_toolbox("research").is_err());
}
