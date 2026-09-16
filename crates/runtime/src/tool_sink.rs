//! The one module that knows both a tool-owning subsystem and [`ToolRegistry`].
//!
//! `darkwire-tools` declares [`ToolSink`] and never learns what fills it;
//! `darkwire-mcp` and `darkwire-extension-host` each hold one and never learn what
//! is behind it. This is the composition root's job, and it is thirty lines
//! because the interfaces were designed to meet.
//!
//! The bookkeeping is the reason it exists at all.
//! `unregister_by_source(Mcp)` is the wrong grain for one server reconnecting —
//! it would take every *other* server's tools with it — so the names each owner
//! last contributed are remembered here and removed by name. An extension
//! reloading needs exactly the same thing, which is why `source` is an argument
//! rather than a constant: one applier, two owners, and the `ToolSource` tag
//! still exact for the case where a whole subsystem goes away at once.
//!
//! **It never fails.** A name that collides with a built-in, a container program
//! or another owner comes back as a rejected name rather than an error:
//! `ToolRegistry::register` treats a duplicate as a conflict, and one clash must
//! not cost an owner its other thirty-nine tools, nor take down the reconcile
//! that was registering them.

use std::sync::Arc;

use darkwire_protocol::ToolSource;
use darkwire_tools::{AnyTool, ToolRegistry, ToolSink};
use parking_lot::Mutex;

/// A sink that writes one owner's tools into `registry` under `source`.
///
/// No logger, deliberately: the rejected names are *returned*, and the owning
/// manager is where they are written out — beside the server or extension they
/// belong to. Logging them here as well would say the same thing twice from two
/// places.
pub struct RegistrySink {
    registry: Arc<ToolRegistry>,
    source: ToolSource,
    /// Owner id to the names it currently holds in the registry.
    owned: Mutex<Vec<(String, Vec<String>)>>,
}

impl std::fmt::Debug for RegistrySink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrySink")
            .field("source", &self.source)
            .field("owners", &self.owned.lock().len())
            .finish_non_exhaustive()
    }
}

/// A [`ToolSink`] over `registry`, tagging everything it accepts with `source`.
pub fn registry_tool_sink(registry: Arc<ToolRegistry>, source: ToolSource) -> Arc<dyn ToolSink> {
    Arc::new(RegistrySink {
        registry,
        source,
        owned: Mutex::new(Vec::new()),
    })
}

impl ToolSink for RegistrySink {
    fn replace(&self, owner_id: &str, tools: Vec<AnyTool>) -> Vec<String> {
        let mut owned = self.owned.lock();
        let slot = owned.iter().position(|(id, _)| id == owner_id);
        if let Some(index) = slot {
            for name in &owned[index].1 {
                self.registry.unregister(name);
            }
        }

        let mut accepted: Vec<String> = Vec::new();
        let mut rejected: Vec<String> = Vec::new();
        for tool in tools {
            let name = tool.definition().name.clone();
            match self.registry.register(tool, self.source) {
                Ok(()) => accepted.push(name),
                // A conflict is the expected outcome of two owners wanting one
                // name. A refusal for any other reason — a name the registry
                // will not advertise — is reported the same way rather than
                // raised, because this call may not fail its caller and the
                // owning manager has one place to show a rejected name.
                Err(_) => rejected.push(name),
            }
        }

        match (slot, accepted.is_empty()) {
            (Some(index), true) => {
                owned.remove(index);
            }
            (Some(index), false) => owned[index].1 = accepted,
            (None, true) => {}
            (None, false) => owned.push((owner_id.to_owned(), accepted)),
        }
        rejected
    }
}
