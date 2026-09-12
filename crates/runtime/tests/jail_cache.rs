//! One jail per workspace, kept between turns.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ghostai_core::paths::ResolveGhostPaths;
use ghostai_core::{ErrorKind, GhostPaths};
use ghostai_protocol::DEFAULT_WORKSPACE_ID;
use ghostai_runtime::jail_cache::JailFactory;
use ghostai_runtime::{JailCache, MAX_CACHED_JAILS};
use ghostai_security::{JailOptions, JailResolver, WorkspaceJail};
use tempfile::TempDir;

struct Setup {
    _temp: TempDir,
    root: PathBuf,
    paths: GhostPaths,
    built: Arc<AtomicUsize>,
}

fn setup() -> Setup {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    let paths = GhostPaths::resolve(ResolveGhostPaths {
        root: Some(root.to_string_lossy().into_owned()),
        env: Some(std::collections::HashMap::new()),
        home: Some(PathBuf::from("/home/someone-else")),
        workspace: None,
    })
    .unwrap();
    Setup {
        _temp: temp,
        root,
        paths,
        built: Arc::new(AtomicUsize::new(0)),
    }
}

impl Setup {
    /// A factory that counts constructions rather than avoiding the disk: the
    /// jail is what canonicalises, and a double would not be one.
    fn factory(&self) -> JailFactory {
        let built = Arc::clone(&self.built);
        Arc::new(move |root: &Path| {
            built.fetch_add(1, Ordering::SeqCst);
            WorkspaceJail::new(JailOptions::new(root))
        })
    }

    fn cache(&self, max: usize) -> JailCache {
        JailCache::with_factory(self.paths.clone(), max, self.factory()).unwrap()
    }
}

#[test]
fn builds_the_default_eagerly_so_an_unusable_workspace_fails_at_construction() {
    let s = setup();
    let cache = s.cache(MAX_CACHED_JAILS);
    assert_eq!(s.built.load(Ordering::SeqCst), 1);
    assert_eq!(cache.size(), 1);
    // Canonical, so the comparison is against the canonical root too: a macOS
    // temp directory is reached through a symlink.
    let canonical = std::fs::canonicalize(&s.root).unwrap();
    assert!(cache.default_jail().root().starts_with(&canonical));
}

#[test]
fn fails_from_the_constructor_when_the_workspace_root_cannot_be_used() {
    let s = setup();
    // A file where the workspace root should be: the jail cannot create a
    // directory over it, and the failure belongs to construction rather than to
    // the first tool call of the next turn.
    std::fs::create_dir_all(&s.root).unwrap();
    std::fs::write(&s.paths.workspace, "not a directory").unwrap();
    let error = common::err(JailCache::new(s.paths.clone()));
    assert_eq!(error.kind, ErrorKind::Config);
}

#[test]
fn maps_the_default_to_the_workspace_root_and_a_named_workspace_beneath_it() {
    let s = setup();
    let cache = s.cache(MAX_CACHED_JAILS);
    let named = cache.for_workspace_checked("research").unwrap();
    assert!(named.root().starts_with(cache.default_jail().root()));
    assert!(named.root().ends_with("research"));
}

#[test]
fn creates_a_named_workspace_directory_on_first_use() {
    let s = setup();
    let cache = s.cache(MAX_CACHED_JAILS);
    assert!(!s.paths.workspace.join("research").exists());
    cache.for_workspace_checked("research").unwrap();
    assert!(s.paths.workspace.join("research").is_dir());
}

#[test]
fn returns_the_same_instance_on_a_hit_rather_than_re_canonicalising() {
    let s = setup();
    let cache = s.cache(MAX_CACHED_JAILS);
    let first = cache.for_workspace_checked("research").unwrap();
    let second = cache.for_workspace_checked("research").unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(s.built.load(Ordering::SeqCst), 2);
}

#[test]
fn refuses_an_id_that_is_not_a_legal_slug_without_creating_anything() {
    let s = setup();
    let cache = s.cache(MAX_CACHED_JAILS);
    for bad in ["../escape", "Upper", "", "a/b"] {
        let error = common::err(cache.for_workspace_checked(bad));
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{bad}");
    }
    assert_eq!(cache.size(), 1);
    // The resolver cannot report a refusal, so it degrades to the default rather
    // than inventing a directory: every id that reaches a turn was validated
    // when its session row was written.
    let fallback = cache.for_workspace("../escape");
    assert!(Arc::ptr_eq(&fallback, &cache.default_jail()));
}

#[test]
fn evicts_the_least_recently_used_once_past_the_bound() {
    let s = setup();
    let cache = s.cache(2);
    // The default already holds one of the two slots.
    cache.for_workspace_checked("a").unwrap();
    cache.for_workspace_checked("b").unwrap();
    assert_eq!(cache.size(), 2);
    // `a` was the oldest non-default, so it was dropped.
    let before = s.built.load(Ordering::SeqCst);
    cache.for_workspace_checked("a").unwrap();
    assert_eq!(s.built.load(Ordering::SeqCst), before + 1);
}

#[test]
fn never_evicts_the_default_which_is_held_for_the_life_of_the_cache() {
    let s = setup();
    let cache = s.cache(1);
    cache.for_workspace_checked("a").unwrap();
    cache.for_workspace_checked("b").unwrap();
    let before = s.built.load(Ordering::SeqCst);
    // The default's entry survives, so the lookup does not rebuild a jail the
    // object already has.
    cache.for_workspace_checked(DEFAULT_WORKSPACE_ID).unwrap();
    assert_eq!(s.built.load(Ordering::SeqCst), before);
}

#[test]
fn keeps_a_re_used_workspace_at_the_young_end_of_the_map() {
    let s = setup();
    let cache = s.cache(3);
    cache.for_workspace_checked("a").unwrap();
    cache.for_workspace_checked("b").unwrap();
    cache.for_workspace_checked("a").unwrap();
    cache.for_workspace_checked("c").unwrap();
    let before = s.built.load(Ordering::SeqCst);
    cache.for_workspace_checked("a").unwrap();
    assert_eq!(s.built.load(Ordering::SeqCst), before, "a was still warm");
}

#[test]
fn rebuilds_one_workspace_after_its_folder_has_moved_out_from_under_it() {
    let s = setup();
    let cache = s.cache(MAX_CACHED_JAILS);
    cache.for_workspace_checked("research").unwrap();
    let before = s.built.load(Ordering::SeqCst);
    cache.evict("research");
    cache.for_workspace_checked("research").unwrap();
    assert_eq!(s.built.load(Ordering::SeqCst), before + 1);
}

#[test]
fn ignores_an_eviction_of_the_default_which_is_held_regardless() {
    let s = setup();
    let cache = s.cache(MAX_CACHED_JAILS);
    cache.evict(DEFAULT_WORKSPACE_ID);
    let before = s.built.load(Ordering::SeqCst);
    cache.for_workspace_checked(DEFAULT_WORKSPACE_ID).unwrap();
    assert_eq!(s.built.load(Ordering::SeqCst), before);
}

#[test]
fn rebuilds_everything_after_a_clear() {
    let s = setup();
    let cache = s.cache(MAX_CACHED_JAILS);
    cache.for_workspace_checked("a").unwrap();
    cache.clear();
    assert_eq!(cache.size(), 0);
    let before = s.built.load(Ordering::SeqCst);
    cache.for_workspace_checked("a").unwrap();
    // The default is still held outright, so it never has to be rebuilt.
    assert!(Arc::ptr_eq(&cache.default_jail(), &cache.default_jail()));
    assert_eq!(s.built.load(Ordering::SeqCst), before + 1);
}

#[test]
fn keeps_two_workspaces_disjoint_and_neither_containing_the_other() {
    let s = setup();
    let cache = s.cache(MAX_CACHED_JAILS);
    let one = cache.for_workspace_checked("one").unwrap();
    let other = cache.for_workspace_checked("other").unwrap();
    assert!(!one.root().starts_with(other.root()));
    assert!(!other.root().starts_with(one.root()));
    // And a traversal cannot address a sibling: the jail normalises the input
    // into its own root before it looks at the filesystem, so the path lands
    // inside `one` rather than reaching across.
    let escaped = one.resolve("../other/secret.txt").unwrap();
    assert!(escaped.starts_with(one.root()));
    assert!(!escaped.starts_with(other.root()));
}

#[test]
fn puts_every_named_workspace_inside_the_default_which_is_the_chosen_layout() {
    let s = setup();
    let cache = s.cache(MAX_CACHED_JAILS);
    let named = cache.for_workspace_checked("one").unwrap();
    assert!(named.root().starts_with(cache.default_jail().root()));
    assert!(format!("{cache:?}").contains("JailCache"));
}

#[test]
fn the_default_bound_is_sized_for_a_working_session() {
    assert_eq!(MAX_CACHED_JAILS, 8);
    let s = setup();
    let cache = JailCache::new(s.paths.clone()).unwrap();
    for index in 0..MAX_CACHED_JAILS + 2 {
        cache.for_workspace_checked(&format!("w{index}")).unwrap();
    }
    assert_eq!(cache.size(), MAX_CACHED_JAILS);
}
