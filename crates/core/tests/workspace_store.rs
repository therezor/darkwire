//! `WorkspaceStore`: the registry, its directory handling, and the invariant
//! it shares with `sessions`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{NOW, make_store_on};
use ghostai_core::ids::{
    DEFAULT_WORKSPACE_ID, RESERVED_WORKSPACE_IDS, derive_workspace_id, is_workspace_id,
};
use ghostai_core::paths::{GhostPaths, ResolveGhostPaths, shared_dir_for, workspace_dir_for};
use ghostai_core::session_store::{CreateSession, ListSessions};
use ghostai_core::testkit::ManualClock;
use ghostai_core::workspace_store::{CreateWorkspace, WorkspaceStore};
use ghostai_core::{Database, ErrorKind};
use proptest::prelude::*;
use serde_json::json;

struct Fixture {
    root: tempfile::TempDir,
    paths: GhostPaths,
    db: Database,
    clock: Arc<ManualClock>,
    store: WorkspaceStore,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let paths = GhostPaths::resolve(ResolveGhostPaths {
        root: Some(root.path().to_string_lossy().into_owned()),
        home: Some(root.path().to_path_buf()),
        ..ResolveGhostPaths::default()
    })
    .unwrap();
    std::fs::create_dir_all(&paths.workspace).unwrap();
    let db = Database::in_memory().unwrap();
    let clock = Arc::new(ManualClock::at(NOW));
    let store = WorkspaceStore::new(db.clone(), paths.clone(), clock.clone()).unwrap();
    Fixture {
        root,
        paths,
        db,
        clock,
        store,
    }
}

impl Fixture {
    fn create(&self, name: &str) -> ghostai_core::workspace_store::WorkspaceRecord {
        self.store
            .create(CreateWorkspace {
                name: name.to_owned(),
                ..CreateWorkspace::default()
            })
            .unwrap()
    }

    fn create_with_id(
        &self,
        name: &str,
        id: &str,
    ) -> ghostai_core::Result<ghostai_core::workspace_store::WorkspaceRecord> {
        self.store.create(CreateWorkspace {
            name: name.to_owned(),
            id: Some(id.to_owned()),
            metadata: None,
        })
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.paths.workspace.join(id)
    }

    fn ids(&self) -> Vec<String> {
        self.store
            .list()
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect()
    }
}

fn kind_of<T>(result: ghostai_core::Result<T>) -> ErrorKind {
    result.err().map(|e| e.kind).expect("expected an error")
}

// ids

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]
    /// Every name derives to something legal that can only ever name a child of
    /// the default workspace.
    #[test]
    fn always_derives_something_legal_for_any_name_at_all(name in ".*") {
        // Resolving touches no filesystem, so one fixed root serves every case.
        let paths = GhostPaths::resolve(ResolveGhostPaths {
            root: Some("/ghostai-home".to_owned()),
            home: Some(PathBuf::from("/home/nobody")),
            ..ResolveGhostPaths::default()
        })
        .unwrap();
        let slug = derive_workspace_id(&name);
        prop_assert!(is_workspace_id(&slug));
        prop_assert_eq!(workspace_dir_for(&paths, &slug).unwrap(), paths.workspace.join(&slug));
    }
}

#[test]
fn refuses_uppercase_because_case_folding_filesystems_would_share_one_directory() {
    assert!(!is_workspace_id("Work"));
    assert_eq!(derive_workspace_id("Work"), "work");
}

// the store

#[test]
fn bootstraps_a_default_that_is_marked_as_such() {
    let f = fixture();
    let rows = f.store.list().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, DEFAULT_WORKSPACE_ID);
    assert!(rows[0].is_default);
    assert_eq!(rows[0].name, "Default");
    assert_eq!(rows[0].created_at_ms, NOW);
    assert!(rows[0].metadata.is_empty());
}

#[test]
fn bootstraps_idempotently_across_two_stores_on_one_connection() {
    let f = fixture();
    let second = WorkspaceStore::new(f.db.clone(), f.paths.clone(), f.clock.clone()).unwrap();
    assert_eq!(second.list().unwrap().len(), 1);
}

#[test]
fn creates_a_workspace_and_its_directory() {
    let f = fixture();
    let created = f.create("Client Acme");
    assert_eq!(created.id, "client-acme");
    assert_eq!(created.name, "Client Acme");
    assert!(!created.is_default);
    assert!(f.dir("client-acme").is_dir());
    assert!(f.dir("client-acme").starts_with(f.root.path()));
}

#[test]
fn lists_the_default_first_then_by_name() {
    let f = fixture();
    f.create("Zulu");
    f.create("alpha");
    assert_eq!(f.ids(), [DEFAULT_WORKSPACE_ID, "alpha", "zulu"]);
}

#[test]
fn disambiguates_a_slug_that_is_already_taken() {
    let f = fixture();
    assert_eq!(f.create("Notes").id, "notes");
    assert_eq!(f.create("notes").id, "notes-2");
    assert_eq!(f.create("notes").id, "notes-3");
}

#[test]
fn keeps_a_disambiguated_slug_inside_the_length_limit() {
    let f = fixture();
    let long = "x".repeat(40);
    assert_eq!(f.create(&long).id, long);
    let second = f.create(&long).id;
    assert!(is_workspace_id(&second));
    assert_eq!(second.len(), 40);
    assert!(second.ends_with("-2"));
}

#[test]
fn refuses_an_explicit_id_that_is_not_a_legal_slug() {
    let f = fixture();
    let error = f.create_with_id("Escape", "../etc").unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert_eq!(error.details["id"], "../etc");
    assert_eq!(
        kind_of(f.create_with_id("Shouty", "Work")),
        ErrorKind::InvalidInput
    );
}

#[test]
fn refuses_a_reserved_id() {
    let f = fixture();
    for id in ["default", "con", "nul", "com1", "lpt9"] {
        assert!(RESERVED_WORKSPACE_IDS.contains(&id));
        assert_eq!(
            kind_of(f.create_with_id("Nope", id)),
            ErrorKind::InvalidInput
        );
    }
}

#[test]
fn refuses_a_duplicate_id() {
    let f = fixture();
    f.create_with_id("Notes", "notes").unwrap();
    assert_eq!(
        kind_of(f.create_with_id("Notes again", "notes")),
        ErrorKind::Conflict
    );
}

#[test]
fn refuses_a_name_that_is_only_whitespace() {
    let f = fixture();
    assert_eq!(
        kind_of(f.store.create(CreateWorkspace {
            name: "   ".to_owned(),
            ..CreateWorkspace::default()
        })),
        ErrorKind::InvalidInput
    );
}

#[test]
fn adopts_an_existing_directory_which_is_what_makes_delete_then_recreate_work() {
    let f = fixture();
    std::fs::create_dir_all(f.dir("research")).unwrap();
    std::fs::write(f.dir("research").join("notes.md"), "kept").unwrap();

    let created = f.create_with_id("Research", "research").unwrap();
    assert_eq!(created.id, "research");
    assert!(f.dir("research").join("notes.md").is_file());
}

#[test]
fn refuses_a_slug_that_collides_with_a_file_in_the_default_workspace() {
    // A file would make every operation in that workspace fail with ENOTDIR.
    let f = fixture();
    std::fs::write(f.dir("notes"), "not a folder").unwrap();
    let error = f.create_with_id("Notes", "notes").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(f.store.get("notes").unwrap(), None);
}

#[test]
fn renames_without_moving_anything_on_disk() {
    let f = fixture();
    let created = f.create("Old");
    f.clock.advance(Duration::from_secs(1));
    let renamed = f.store.rename(&created.id, "  New  ").unwrap();
    assert_eq!(renamed.name, "New");
    assert_eq!(renamed.id, "old");
    let stored = f.store.get(&created.id).unwrap().unwrap();
    assert_eq!(stored.name, "New");
    assert_eq!(stored.updated_at_ms, NOW + 1000);
    assert!(f.dir(&created.id).is_dir());
}

#[test]
fn refuses_to_rename_something_that_is_not_there_or_to_nothing() {
    let f = fixture();
    assert_eq!(kind_of(f.store.rename("ghost", "New")), ErrorKind::NotFound);
    let created = f.create("Here");
    assert_eq!(
        kind_of(f.store.rename(&created.id, " ")),
        ErrorKind::InvalidInput
    );
}

#[test]
fn relocates_the_row_and_the_tree_together_keeping_what_is_inside() {
    let f = fixture();
    let created = f.create("Client Acme");
    std::fs::write(f.dir(&created.id).join("notes.md"), "kept").unwrap();

    let moved = f.store.relocate(&created.id, "acme24").unwrap();

    assert_eq!(moved.id, "acme24");
    // The label is untouched: the folder and the name are separate answers.
    assert_eq!(moved.name, "Client Acme");
    assert_eq!(f.store.get(&created.id).unwrap(), None);
    assert!(f.dir("acme24").join("notes.md").is_file());
    assert!(!f.dir(&created.id).exists());
}

#[test]
fn takes_the_shared_layer_with_it_since_that_is_keyed_by_workspace_too() {
    let f = fixture();
    let created = f.create("Research");
    let shared = shared_dir_for(&f.paths, &created.id).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("facts.md"), "pooled").unwrap();

    f.store.relocate(&created.id, "lab").unwrap();

    assert!(
        shared_dir_for(&f.paths, "lab")
            .unwrap()
            .join("facts.md")
            .is_file()
    );
    assert!(!shared.exists());
}

#[test]
fn relocating_is_a_no_op_when_the_folder_is_the_one_it_already_has() {
    let f = fixture();
    f.create_with_id("Notes", "notes").unwrap();
    assert_eq!(f.store.relocate("notes", "notes").unwrap().id, "notes");
    assert!(f.dir("notes").is_dir());
}

#[test]
fn refuses_to_move_the_default_whose_folder_is_the_root_the_others_live_in() {
    let f = fixture();
    assert_eq!(
        kind_of(f.store.relocate(DEFAULT_WORKSPACE_ID, "somewhere")),
        ErrorKind::Conflict
    );
}

#[test]
fn refuses_a_folder_that_is_not_a_legal_slug_or_is_reserved() {
    let f = fixture();
    let created = f.create("Notes");
    for folder in ["../etc", "Work", "con"] {
        assert_eq!(
            kind_of(f.store.relocate(&created.id, folder)),
            ErrorKind::InvalidInput
        );
    }
}

#[test]
fn refuses_a_folder_another_workspace_already_registers() {
    let f = fixture();
    f.create_with_id("Alpha", "alpha").unwrap();
    let beta = f.create_with_id("Beta", "beta").unwrap();
    assert_eq!(
        kind_of(f.store.relocate(&beta.id, "alpha")),
        ErrorKind::Conflict
    );
}

#[test]
fn refuses_a_folder_that_exists_on_disk_rather_than_renaming_over_it() {
    // `rename(2)` replaces an empty directory at the destination on POSIX, and
    // the thing it would swallow is a folder the user or the agent put there.
    let f = fixture();
    let created = f.create("Notes");
    std::fs::create_dir_all(f.dir("occupied")).unwrap();

    assert_eq!(
        kind_of(f.store.relocate(&created.id, "occupied")),
        ErrorKind::Conflict
    );
    assert_eq!(f.store.get(&created.id).unwrap().unwrap().id, created.id);
}

#[test]
fn refuses_to_move_something_that_is_not_there() {
    let f = fixture();
    assert_eq!(
        kind_of(f.store.relocate("ghost", "elsewhere")),
        ErrorKind::NotFound
    );
}

#[test]
fn leaves_the_row_alone_when_the_directory_could_not_be_moved() {
    // A row updated before a `rename(2)` that failed would name a folder
    // nobody could find.
    let f = fixture();
    let created = f.create("Notes");
    std::fs::remove_dir_all(f.dir(&created.id)).unwrap();

    let error = f.store.relocate(&created.id, "moved").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(error.details["id"], "moved");
    assert_eq!(f.store.get(&created.id).unwrap().unwrap().id, created.id);
}

#[test]
fn detaches_without_touching_the_files() {
    let f = fixture();
    let created = f.create("Research");
    std::fs::write(f.dir(&created.id).join("notes.md"), "kept").unwrap();

    f.store.delete(&created.id).unwrap();

    assert_eq!(f.store.get(&created.id).unwrap(), None);
    assert!(f.dir(&created.id).join("notes.md").is_file());
}

#[test]
fn refuses_to_delete_the_default_or_something_that_is_not_there() {
    let f = fixture();
    assert_eq!(
        kind_of(f.store.delete(DEFAULT_WORKSPACE_ID)),
        ErrorKind::Conflict
    );
    assert_eq!(kind_of(f.store.delete("ghost")), ErrorKind::NotFound);
}

#[test]
fn round_trips_metadata() {
    let f = fixture();
    let created = f
        .store
        .create(CreateWorkspace {
            name: "Tagged".to_owned(),
            id: None,
            metadata: Some(
                [("colour".to_owned(), json!("green"))]
                    .into_iter()
                    .collect(),
            ),
        })
        .unwrap();
    assert_eq!(
        f.store.get(&created.id).unwrap().unwrap().metadata["colour"],
        json!("green")
    );
}

#[test]
fn debug_output_names_the_paths_and_nothing_else() {
    let f = fixture();
    let debug = format!("{:?}", f.store);
    assert!(debug.starts_with("WorkspaceStore { paths:"));
    assert!(debug.ends_with(".. }"));
}

// sessions and workspaces

#[test]
fn a_session_defaults_to_the_default_workspace_and_keeps_the_one_it_was_created_in() {
    let f = fixture();
    let sessions = make_store_on(f.db.clone(), Arc::clone(&f.clock)).unwrap();
    f.create_with_id("Acme", "acme").unwrap();

    assert_eq!(
        sessions
            .ensure_session("web-1", CreateSession::default())
            .unwrap()
            .workspace_id,
        DEFAULT_WORKSPACE_ID
    );
    let in_acme = CreateSession {
        workspace_id: Some("acme".to_owned()),
        ..CreateSession::default()
    };
    assert_eq!(
        sessions
            .ensure_session("web-2", in_acme)
            .unwrap()
            .workspace_id,
        "acme"
    );
    // The rule that makes switching workspaces mid-turn safe: the stored row
    // cannot be talked into changing by a later request that claims otherwise.
    let in_other = CreateSession {
        workspace_id: Some("other".to_owned()),
        ..CreateSession::default()
    };
    assert_eq!(
        sessions
            .ensure_session("web-2", in_other)
            .unwrap()
            .workspace_id,
        "acme"
    );

    let by_workspace = ListSessions {
        workspace_id: Some("acme".to_owned()),
        ..ListSessions::default()
    };
    let listed: Vec<String> = sessions
        .list_sessions(&by_workspace)
        .unwrap()
        .into_iter()
        .map(|row| row.session.key)
        .collect();
    assert_eq!(listed, ["web-2"]);
    assert_eq!(
        sessions
            .list_sessions(&ListSessions::default())
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn the_tables_do_not_share_a_foreign_key() {
    // A detached workspace's sessions must keep resolving to their own files,
    // so `sessions.workspace_id` may name a workspace with no row.
    let f = fixture();
    let sessions = make_store_on(f.db.clone(), Arc::clone(&f.clock)).unwrap();
    let detached = CreateSession {
        workspace_id: Some("detached".to_owned()),
        ..CreateSession::default()
    };
    sessions.ensure_session("s", detached).unwrap();
    assert_eq!(sessions.count_by_workspace("detached").unwrap(), 1);
    assert_eq!(f.store.get("detached").unwrap(), None);
}
