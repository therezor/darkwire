//! Installed environment definitions: resolution, validation, and digests.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use crate::common;

use darkwire_core::ErrorKind;
use darkwire_security::PolicyStore;
use serde_json::{Value, json};

use common::message_of;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn definition() -> Value {
    json!({"schema": "darkwire.environment/1", "name": "dev", "image": DIGEST, "shared": true})
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

/// Writing a definition from the settings route.
///
/// The policy directory sits beside the workspace rather than inside it, so no
/// tool can reach these files; an authenticated operator is a different actor
/// from a prompt-injected `write`, and this is their door. What must not
/// change is the validation: a definition saved here clears exactly the checks
/// a hand-written one does.
mod saving {
    use super::*;

    /// Every optional field set to something that is not its default, so the
    /// round trip covers the nested objects and the lists rather than the four
    /// fields a minimal definition carries.
    fn full() -> Value {
        json!({
            "schema": "darkwire.environment/1",
            "kind": "container",
            "name": "dev",
            "prompt": "Rust and Node are installed.",
            "image": DIGEST,
            "shared": true,
            "runtime": "runsc",
            "workdir": "/srv/work",
            "user": "1001:1001",
            "caps": {"drop": ["ALL"], "add": ["CHOWN"]},
            "security": {
                "noNewPrivileges": false,
                "seccomp": "unconfined",
                "readOnlyRoot": false,
                "tmpfs": ["/tmp:rw,nosuid,size=512m"],
                "devices": ["/dev/fuse"],
            },
            "limits": {"memoryMb": 4096, "cpus": 1.5, "pidsMax": 1024, "shmSizeMb": 512},
            "env": ["CARGO_HOME", "RUSTUP_HOME"],
        })
    }

    fn empty() -> (tempfile::TempDir, PolicyStore) {
        let root = tempfile::tempdir().unwrap();
        let store = PolicyStore::new(root.path().to_path_buf());
        (root, store)
    }

    #[test]
    fn a_saved_definition_loads_back_field_for_field() {
        let (_root, store) = empty();
        let written: darkwire_protocol::environment::EnvironmentDefinition =
            serde_json::from_value(full()).unwrap();

        let digest = store.save_environment(&written).unwrap();
        let read = store.require_environment("dev").unwrap();

        assert_eq!(read.definition, written);
        // The digest the save reports is the digest of the bytes it left
        // behind, not of a re-serialisation somebody hopes matches.
        assert_eq!(read.digest, digest);
    }

    #[test]
    fn it_creates_the_environments_directory_on_the_first_save() {
        // An install that has never had a definition has no directory at all,
        // which is the state the Settings panel most often writes from.
        let (root, store) = empty();
        assert!(!root.path().join("environments").exists());

        let definition = serde_json::from_value(definition()).unwrap();
        store.save_environment(&definition).unwrap();

        assert!(root.path().join("environments/dev.yaml").is_file());
    }

    #[test]
    fn a_second_save_replaces_the_first_and_moves_the_digest() {
        let (_root, store) = empty();
        let mut definition: darkwire_protocol::environment::EnvironmentDefinition =
            serde_json::from_value(definition()).unwrap();
        let first = store.save_environment(&definition).unwrap();

        definition.limits.memory_mb = 8192;
        let second = store.save_environment(&definition).unwrap();

        assert_ne!(first, second);
        assert_eq!(store.require_environment("dev").unwrap().digest, second);
        assert_eq!(store.list_environments().len(), 1);
    }

    #[test]
    fn an_image_that_is_not_pinned_is_refused_and_nothing_is_written() {
        // The refusal that matters most: a tag is a mutable pointer, so a
        // definition installed once and then repointed runs code nobody chose.
        let (root, store) = empty();
        let mut definition = full();
        definition["image"] = json!("node:20");
        let definition = serde_json::from_value(definition).unwrap();

        let refusal = store.save_environment(&definition);
        let error = refusal.as_ref().unwrap_err();

        assert_eq!(error.kind, ErrorKind::Config);
        assert!(message_of(&refusal).contains("digest"));
        assert!(!root.path().join("environments/dev.yaml").exists());
    }

    #[test]
    fn a_forbidden_capability_is_refused_and_nothing_is_written() {
        // `NET_ADMIN` can flush the egress gateway's rules, which live in a
        // namespace the container shares.
        let (root, store) = empty();
        let mut definition = full();
        definition["caps"]["add"] = json!(["NET_ADMIN"]);
        let definition = serde_json::from_value(definition).unwrap();

        let refusal = store.save_environment(&definition);

        assert!(message_of(&refusal).contains("NET_ADMIN"));
        assert!(!root.path().join("environments/dev.yaml").exists());
    }

    #[test]
    fn a_name_that_is_not_a_slug_never_reaches_the_filesystem() {
        let (root, store) = empty();
        let mut definition = full();
        definition["name"] = json!("../escape");
        let definition = serde_json::from_value(definition).unwrap();

        let refusal = store.save_environment(&definition).unwrap_err();

        assert_eq!(refusal.kind, ErrorKind::InvalidInput);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_partial_write_leaves_no_file_behind() {
        // The temporary the rename works from is cleaned up on the way out of a
        // refusal, so a failed save does not leave a `.yaml.tmp` the directory
        // scan has to learn to ignore.
        let (root, store) = empty();
        let mut definition = full();
        definition["image"] = json!("node:20");
        let definition = serde_json::from_value(definition).unwrap();

        let _ = store.save_environment(&definition);

        assert!(!root.path().join("environments").exists());
    }
}

mod removing {
    use super::*;

    #[test]
    fn a_removed_definition_stops_being_installed() {
        let (root, store) = fixture();
        assert_eq!(store.list_environments().len(), 1);

        store.remove_environment("dev").unwrap();

        assert!(store.list_environments().is_empty());
        assert!(!root.path().join("environments/dev.yaml").exists());
    }

    #[test]
    fn removing_one_that_is_not_installed_says_so() {
        // Silent success would let a typo read as a deletion.
        let (_root, store) = fixture();
        let refusal = store.remove_environment("nope");
        assert!(message_of(&refusal).contains("No environment is installed"));
    }

    #[test]
    fn a_name_that_is_not_a_slug_never_reaches_the_filesystem() {
        let (_root, store) = fixture();
        let refusal = store.remove_environment("../dev").unwrap_err();
        assert_eq!(refusal.kind, ErrorKind::InvalidInput);
        assert_eq!(store.list_environments().len(), 1);
    }
}
