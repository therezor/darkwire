//! One [`WorkspaceJail`] per workspace, kept between turns.
//!
//! Constructing a jail is not free: it creates the root and canonicalises it
//! through the filesystem, which is two syscalls on a path that every single
//! tool call needs. A turn that reads six files would pay for twelve of them,
//! and a server with several workspaces in use would pay again on every switch.
//!
//! Three decisions worth stating:
//!
//!  - **`for_workspace` never consults the registry.** It maps an id to a
//!    directory and nothing else. That is deliberate in both directions: a
//!    workspace that was *detached* still has sessions, and they must keep
//!    resolving to their own files rather than silently landing in someone
//!    else's — while an id that is not a legal slug fails here rather than
//!    becoming a directory. Whether a workspace is one the user can still see is
//!    a question for the routes, which check the registry before they get this
//!    far.
//!
//!  - **The default is built eagerly, in the constructor.** The runtime's build
//!    computes everything able to fail before it mutates anything, so that an
//!    unusable workspace leaves the runtime serving turns on the settings that
//!    worked a moment ago. A lazily-built default would move that failure to the
//!    first tool call of the next turn, which is exactly where it must not be.
//!
//!  - **Eviction is a plain removal.** A jail owns a canonical path and no
//!    handles, so dropping the reference is the whole of closing one.

use std::sync::Arc;

use ghostai_core::{GhostPaths, Result, workspace_dir_for};
use ghostai_protocol::DEFAULT_WORKSPACE_ID;
use ghostai_security::{JailOptions, JailResolver, WorkspaceJail};
use indexmap::IndexMap;
use parking_lot::Mutex;

/// Beyond this many live jails the least-recently-used is dropped.
///
/// Sized for what a person does: a handful of workspaces open across a working
/// session, and a miss costs one directory creation plus one canonicalisation.
pub const MAX_CACHED_JAILS: usize = 8;

/// Builds a jail for a root. Injected by tests, which count constructions rather
/// than touch a disk.
pub type JailFactory = Arc<dyn Fn(&std::path::Path) -> Result<WorkspaceJail> + Send + Sync>;

/// Live jails, keyed by workspace id.
pub struct JailCache {
    paths: GhostPaths,
    max: usize,
    create: JailFactory,
    /// Insertion-ordered, which is what makes the first key the LRU victim.
    jails: Mutex<IndexMap<String, Arc<WorkspaceJail>>>,
    default_jail: Arc<WorkspaceJail>,
}

impl std::fmt::Debug for JailCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JailCache")
            .field("size", &self.jails.lock().len())
            .field("max", &self.max)
            .finish_non_exhaustive()
    }
}

impl JailCache {
    /// A cache over `paths`, with the default workspace's jail already built.
    pub fn new(paths: GhostPaths) -> Result<JailCache> {
        JailCache::with_factory(
            paths,
            MAX_CACHED_JAILS,
            Arc::new(|root| WorkspaceJail::new(JailOptions::new(root))),
        )
    }

    /// A cache with its own bound and jail factory.
    pub fn with_factory(paths: GhostPaths, max: usize, create: JailFactory) -> Result<JailCache> {
        // The default is built here rather than lazily, so an unusable
        // workspace is a construction failure — which is what keeps a
        // reconfigure all-or-nothing.
        let root = workspace_dir_for(&paths, DEFAULT_WORKSPACE_ID)?;
        let default_jail = Arc::new(create(&root)?);
        let mut jails = IndexMap::new();
        jails.insert(DEFAULT_WORKSPACE_ID.to_owned(), Arc::clone(&default_jail));
        Ok(JailCache {
            paths,
            max,
            create,
            jails: Mutex::new(jails),
            default_jail,
        })
    }

    /// How many jails are live.
    pub fn size(&self) -> usize {
        self.jails.lock().len()
    }

    /// The jail for one workspace, built on first use.
    ///
    /// Fails for anything that is not a legal slug, which is the second of the
    /// two places that check — the first being the route.
    pub fn for_workspace_checked(&self, workspace_id: &str) -> Result<Arc<WorkspaceJail>> {
        {
            let mut jails = self.jails.lock();
            if let Some(hit) = jails.shift_remove(workspace_id) {
                // Re-inserted so the working set stays at the young end.
                jails.insert(workspace_id.to_owned(), Arc::clone(&hit));
                return Ok(hit);
            }
        }

        // Outside the map on purpose: a failure here must not be remembered.
        let root = workspace_dir_for(&self.paths, workspace_id)?;
        let jail = Arc::new((self.create)(&root)?);

        let mut jails = self.jails.lock();
        jails.insert(workspace_id.to_owned(), Arc::clone(&jail));
        if jails.len() > self.max {
            // Never the default: it is held by `default_jail` regardless, and
            // dropping its entry would make the next lookup rebuild a jail this
            // object already has.
            let victim = jails
                .keys()
                .find(|key| key.as_str() != DEFAULT_WORKSPACE_ID)
                .cloned();
            if let Some(victim) = victim {
                jails.shift_remove(&victim);
            }
        }
        Ok(jail)
    }

    /// Drops one workspace's jail, for when its folder has moved out from under
    /// it.
    ///
    /// A jail canonicalises its root once, at construction, so an entry for a
    /// folder that has since been renamed away holds a path that no longer
    /// exists. Left in place it is mostly inert — nothing names the old id after
    /// a relocation — but it stops being inert the moment a *new* workspace is
    /// created on the freed folder name: the lookup would hand it a jail
    /// resolved against the directory that used to be there.
    ///
    /// Never the default, whose entry is also held outright; there is nothing
    /// that can move it, so nothing asks.
    pub fn evict(&self, workspace_id: &str) {
        if workspace_id == DEFAULT_WORKSPACE_ID {
            return;
        }
        self.jails.lock().shift_remove(workspace_id);
    }

    /// Drops every cached jail. The default is rebuilt on the next lookup.
    pub fn clear(&self) {
        self.jails.lock().clear();
    }
}

impl JailResolver for JailCache {
    /// The jail for one workspace, degrading to the default.
    ///
    /// The trait cannot fail, and the only failure this lookup has is an id that
    /// is not a legal slug — which cannot reach a turn, because a session's
    /// workspace id was validated when the row was written. A caller that wants
    /// the refusal asks [`JailCache::for_workspace_checked`].
    fn for_workspace(&self, workspace_id: &str) -> Arc<WorkspaceJail> {
        self.for_workspace_checked(workspace_id)
            .unwrap_or_else(|_| Arc::clone(&self.default_jail))
    }

    fn default_jail(&self) -> Arc<WorkspaceJail> {
        Arc::clone(&self.default_jail)
    }
}
