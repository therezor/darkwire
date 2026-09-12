//! The extension approval ledger, and its table against `fixtures/sqlite`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ghostai_core::testkit::ManualClock;
use ghostai_core::{Database, ErrorKind};
use ghostai_security::{ExtensionResolutionState, ExtensionStore};
use serde_json::{Value, json};

use common::{read_fixture, temp_base, write};

struct Setup {
    _dir: tempfile::TempDir,
    base: PathBuf,
    db: Database,
    store: ExtensionStore,
}

fn setup() -> Setup {
    let (dir, base) = temp_base();
    let db = Database::in_memory().unwrap();
    let store = ExtensionStore::new(
        db.clone(),
        &base,
        Arc::new(ManualClock::at(1_700_000_000_000)),
    )
    .unwrap();
    Setup {
        _dir: dir,
        base,
        db,
        store,
    }
}

fn install(root: &Path, id: &str, overrides: &Value) -> PathBuf {
    let dir = root.join(id);
    let mut manifest = json!({"schema": "ghostai.extension/1", "id": id});
    for (key, value) in overrides.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    write(
        &dir.join("ghostai.extension.json"),
        serde_json::to_string(&manifest).unwrap(),
    );
    write(
        &dir.join("dist").join("index.js"),
        "export const extension = {};\n",
    );
    dir
}

#[test]
fn the_table_matches_the_sqlite_master_fixture() {
    let s = setup();
    let fixture = read_fixture("sqlite/sqlite_master.json");
    let row = fixture["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "extension_approvals")
        .expect("fixture row");
    let sql: String =
        s.db.lock()
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'extension_approvals'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(sql, row["sql"].as_str().unwrap());
    assert!(format!("{:?}", s.store).contains("ExtensionStore"));
}

#[test]
fn answers_rather_than_erroring_so_one_bad_row_cannot_end_the_sweep() {
    let s = setup();
    install(&s.base, "broken", &json!({"entry": "dist/missing.js"}));
    let resolution = s.store.resolve("broken").unwrap();
    assert_eq!(resolution.state, ExtensionResolutionState::Failed);
    assert!(resolution.problem.is_some());
    assert!(resolution.manifest.is_none());
    assert_eq!(resolution.digest, "");
}

#[test]
fn distinguishes_never_approved_from_changed_since_approved() {
    let s = setup();
    let dir = install(&s.base, "slack", &json!({}));

    let unapproved = s.store.resolve("slack").unwrap();
    assert_eq!(unapproved.state, ExtensionResolutionState::Unapproved);
    assert!(unapproved.problem.unwrap().contains("never been approved"));
    assert_eq!(unapproved.approved_at_ms, None);

    let approved = s.store.approve("slack").unwrap();
    assert_eq!(approved.state, ExtensionResolutionState::Approved);
    assert_eq!(approved.approved_at_ms, Some(1_700_000_000_000));
    let resolved = s.store.resolve("slack").unwrap();
    assert_eq!(resolved, approved);
    assert_eq!(resolved.digest.len(), 64);

    write(
        &dir.join("dist").join("index.js"),
        "export const extension = 1;\n",
    );
    let drifted = s.store.resolve("slack").unwrap();
    assert_eq!(drifted.state, ExtensionResolutionState::Drifted);
    assert!(
        drifted
            .problem
            .unwrap()
            .contains("changed since it was approved")
    );
    assert_eq!(drifted.approved_at_ms, Some(1_700_000_000_000));

    // Adding a file revokes the approval just as editing one does.
    s.store.approve("slack").unwrap();
    write(&dir.join("dist").join("other.js"), "export const y = 1;\n");
    assert_eq!(
        s.store.resolve("slack").unwrap().state,
        ExtensionResolutionState::Drifted
    );
}

#[test]
fn a_directory_with_no_manifest_is_failed_not_absent() {
    let s = setup();
    std::fs::create_dir_all(s.base.join("empty")).unwrap();
    assert_eq!(
        s.store.resolve("empty").unwrap().state,
        ExtensionResolutionState::Failed
    );
}

#[test]
fn a_digest_that_cannot_be_computed_is_failed_with_the_manifest_kept() {
    let s = setup();
    let dir = install(&s.base, "huge", &json!({}));
    let many = dir.join("many");
    for index in 0..=ghostai_security::MAX_EXTENSION_FILES {
        write(&many.join(format!("{index}.txt")), "x");
    }
    let resolution = s.store.resolve("huge").unwrap();
    assert_eq!(resolution.state, ExtensionResolutionState::Failed);
    assert!(resolution.manifest.is_some());
    assert!(resolution.problem.unwrap().contains("too large"));
}

#[test]
fn approve_errors_where_resolve_answers() {
    let s = setup();
    assert!(s.store.approve("missing").is_err());
    let error = s.store.dir_for("../evil").unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("Not an extension id"));
    assert!(s.store.resolve("../evil").is_err());
    assert_eq!(s.store.dir_for("slack").unwrap(), s.base.join("slack"));
}

#[test]
fn re_approves_in_place_and_revokes_without_touching_files() {
    let s = setup();
    let dir = install(&s.base, "slack", &json!({}));
    s.store.approve("slack").unwrap();
    write(
        &dir.join("dist").join("index.js"),
        "export const extension = 2;\n",
    );
    s.store.approve("slack").unwrap();
    assert_eq!(
        s.store.resolve("slack").unwrap().state,
        ExtensionResolutionState::Approved
    );
    let rows: i64 =
        s.db.lock()
            .query_row("SELECT COUNT(*) FROM extension_approvals", [], |row| {
                row.get(0)
            })
            .unwrap();
    assert_eq!(rows, 1);

    s.store.revoke("slack").unwrap();
    let resolution = s.store.resolve("slack").unwrap();
    assert_eq!(resolution.state, ExtensionResolutionState::Unapproved);
    assert_eq!(resolution.manifest.unwrap().id, "slack");
    assert!(dir.join("dist").join("index.js").exists());
}

#[test]
fn lists_installed_ids_sorted_and_skips_what_cannot_be_one() {
    let s = setup();
    install(&s.base, "zulip", &json!({}));
    install(&s.base, "slack", &json!({}));
    std::fs::create_dir_all(s.base.join("node_modules")).unwrap();
    std::fs::create_dir_all(s.base.join(".cache")).unwrap();
    write(&s.base.join("a-file"), "x");
    assert_eq!(s.store.installed_ids(), ["slack", "zulip"]);

    let absent = ExtensionStore::new(
        s.db.clone(),
        s.base.join("nowhere"),
        Arc::new(ManualClock::at(0)),
    )
    .unwrap();
    assert!(absent.installed_ids().is_empty());
}

#[test]
fn resolves_an_extension_from_an_explicit_path() {
    let s = setup();
    let (_other, elsewhere) = temp_base();
    let dir = install(&elsewhere, "corp", &json!({}));
    let resolution = s.store.resolve_path(&dir).unwrap().unwrap();
    assert_eq!(resolution.id, "corp");
    assert_eq!(resolution.state, ExtensionResolutionState::Unapproved);
    assert_eq!(resolution.dir, dir);

    write(&s.base.join("a-file"), "x");
    assert!(
        s.store
            .resolve_path(&s.base.join("a-file"))
            .unwrap()
            .is_none()
    );
    assert!(
        s.store
            .resolve_path(&s.base.join("nothing-here"))
            .unwrap()
            .is_none()
    );

    let bad = s.base.join("bad");
    write(&bad.join("ghostai.extension.json"), "{");
    let failed = s.store.resolve_path(&bad).unwrap().unwrap();
    assert_eq!(failed.state, ExtensionResolutionState::Failed);
    assert_eq!(failed.id, bad.to_string_lossy());
}
