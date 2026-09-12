//! The toolbox approval ledger, and its table against `fixtures/sqlite`.

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
use ghostai_security::ToolboxStore;
use serde_json::{Value, json};

use common::{kind_of, message_of, read_fixture, temp_base, write};

const DIGEST: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

struct Setup {
    _dir: tempfile::TempDir,
    base: PathBuf,
    db: Database,
    store: ToolboxStore,
}

fn setup() -> Setup {
    let (dir, base) = temp_base();
    let db = Database::in_memory().unwrap();
    let store = ToolboxStore::new(
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

fn install(base: &Path, name: &str, overrides: &Value) {
    let mut manifest = json!({"schema": "ghostai.toolbox/1", "name": name, "image": DIGEST});
    for (key, value) in overrides.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    write(
        &base.join(name).join("toolbox.json"),
        serde_json::to_string(&manifest).unwrap(),
    );
}

fn table_sql(db: &Database, table: &str) -> String {
    db.lock()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?",
            [table],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
}

#[test]
fn the_table_matches_the_sqlite_master_fixture() {
    let s = setup();
    let fixture = read_fixture("sqlite/sqlite_master.json");
    let row = fixture["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "toolbox_approvals")
        .expect("fixture row");
    assert_eq!(
        table_sql(&s.db, "toolbox_approvals"),
        row["sql"].as_str().unwrap()
    );
    // Opening a second store over the same database is a no-op.
    ToolboxStore::new(s.db.clone(), &s.base, Arc::new(ManualClock::at(0))).unwrap();
    assert!(format!("{:?}", s.store).contains("ToolboxStore"));
}

#[test]
fn distinguishes_not_installed_from_not_approved_from_edited() {
    let s = setup();
    assert!(message_of(&s.store.require("research")).contains("No toolbox is installed"));
    install(&s.base, "research", &json!({}));
    assert!(message_of(&s.store.require("research")).contains("never been approved"));
    s.store.approve("research").unwrap();
    assert_eq!(
        s.store.require("research").unwrap().toolbox.name,
        "research"
    );
    install(&s.base, "research", &json!({"version": "2.0.0"}));
    let error = s.store.require("research").unwrap_err();
    assert!(error.message.contains("has changed since it was approved"));
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.details["approved"].is_string());
}

#[test]
fn reports_the_manifest_path_and_the_hash() {
    let s = setup();
    install(&s.base, "research", &json!({}));
    let approved = s.store.approve("research").unwrap();
    let required = s.store.require("research").unwrap();
    assert_eq!(
        required.manifest_path,
        s.base.join("research").join("toolbox.json")
    );
    assert_eq!(required.manifest_sha256, approved.manifest_sha256);
    assert_eq!(required, approved);
    assert_eq!(
        s.store.manifest_path_for("research").unwrap(),
        s.base.join("research").join("toolbox.json")
    );
}

#[test]
fn refuses_an_image_pinned_by_tag_even_after_approval() {
    let s = setup();
    install(&s.base, "research", &json!({"image": "alpine:3.21"}));
    assert!(message_of(&s.store.approve("research")).contains("digest"));
    assert!(message_of(&s.store.require("research")).contains("digest"));
    install(&s.base, "local", &json!({"image": DIGEST}));
    s.store.approve("local").unwrap();
    assert_eq!(s.store.require("local").unwrap().toolbox.image, DIGEST);
}

#[test]
fn refuses_a_toolbox_name_that_is_not_a_slug() {
    let s = setup();
    for name in ["../../etc", "Research", "", "a".repeat(65).as_str()] {
        let error = s.store.require(name).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{name}");
        assert!(error.message.contains("Not a toolbox name"));
    }
}

#[test]
fn approve_is_idempotent_and_re_approves_in_place() {
    let s = setup();
    install(&s.base, "research", &json!({}));
    let first = s.store.approve("research").unwrap();
    install(&s.base, "research", &json!({"version": "2.0.0"}));
    let second = s.store.approve("research").unwrap();
    assert_ne!(second.manifest_sha256, first.manifest_sha256);
    assert_eq!(
        s.store.require("research").unwrap().toolbox.version,
        "2.0.0"
    );
    let rows: i64 =
        s.db.lock()
            .query_row("SELECT COUNT(*) FROM toolbox_approvals", [], |row| {
                row.get(0)
            })
            .unwrap();
    assert_eq!(rows, 1);
    let approved_at: i64 =
        s.db.lock()
            .query_row("SELECT approved_at_ms FROM toolbox_approvals", [], |row| {
                row.get(0)
            })
            .unwrap();
    assert_eq!(approved_at, 1_700_000_000_000);
    assert!(message_of(&s.store.approve("nope")).contains("No toolbox is installed"));
}

#[test]
fn revoke_leaves_the_manifest_but_stops_it_resolving() {
    let s = setup();
    install(&s.base, "research", &json!({}));
    s.store.approve("research").unwrap();
    s.store.revoke("research").unwrap();
    assert!(message_of(&s.store.require("research")).contains("never been approved"));
    assert_eq!(
        s.store
            .list()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["research"]
    );
}

#[test]
fn list_reports_every_directory_usable_or_not() {
    let s = setup();
    assert!(s.store.list().is_empty());
    let absent = ToolboxStore::new(
        s.db.clone(),
        s.base.join("nowhere"),
        Arc::new(ManualClock::at(0)),
    )
    .unwrap();
    assert!(absent.list().is_empty());

    install(&s.base, "research", &json!({}));
    install(&s.base, "kali", &json!({}));
    s.store.approve("kali").unwrap();
    write(&s.base.join("broken").join("toolbox.json"), "not json");
    std::fs::create_dir_all(s.base.join("empty")).unwrap();
    write(&s.base.join("a-file"), "x");
    write(&s.base.join("Shouted").join("toolbox.json"), "{}");

    let listing = s.store.list();
    let summary: Vec<(String, bool, Option<String>)> = listing
        .iter()
        .map(|entry| (entry.name.clone(), entry.approved, entry.problem.clone()))
        .collect();
    assert_eq!(
        summary,
        [
            (
                "Shouted".to_owned(),
                false,
                Some("Not a toolbox name: Shouted".to_owned())
            ),
            (
                "broken".to_owned(),
                false,
                Some("Profile manifest is not valid JSON".to_owned())
            ),
            ("empty".to_owned(), false, Some("no manifest".to_owned())),
            ("kali".to_owned(), true, None),
            (
                "research".to_owned(),
                false,
                Some("not approved, or changed since approval".to_owned())
            ),
        ]
    );
    let kali = listing.iter().find(|entry| entry.name == "kali").unwrap();
    assert_eq!(kali.toolbox.as_ref().unwrap().name, "kali");
    assert_eq!(kali.manifest_path, s.base.join("kali").join("toolbox.json"));
    assert!(
        listing
            .iter()
            .find(|entry| entry.name == "broken")
            .unwrap()
            .toolbox
            .is_none()
    );
    assert_eq!(listing[0], listing[0].clone());
}

#[test]
fn files_beside_the_manifest_are_ignored() {
    let s = setup();
    install(&s.base, "research", &json!({}));
    write(&s.base.join("research").join("NOTES.md"), "first");
    let approved = s.store.approve("research").unwrap();
    write(&s.base.join("research").join("NOTES.md"), "second");
    assert_eq!(
        s.store.require("research").unwrap().manifest_sha256,
        approved.manifest_sha256
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_manifest_is_a_config_error_not_absence() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup();
    install(&s.base, "research", &json!({}));
    let manifest = s.base.join("research").join("toolbox.json");
    std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o000)).unwrap();
    let outcome = s.store.require("research");
    std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(kind_of(&outcome), "config");
    assert!(message_of(&outcome).contains("could not be read"));
}
