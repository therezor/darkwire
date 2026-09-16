//! The one module that knows both a tool-owning subsystem and the registry.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::sync::Arc;

use common::tool;
use darkwire_protocol::ToolSource;
use darkwire_runtime::registry_tool_sink;
use darkwire_tools::ToolRegistry;

fn registry() -> Arc<ToolRegistry> {
    Arc::new(ToolRegistry::new())
}

#[test]
fn registers_a_servers_tools_under_the_mcp_source() {
    let registry = registry();
    let sink = registry_tool_sink(Arc::clone(&registry), ToolSource::Mcp);
    assert!(
        sink.replace("files", vec![tool("mcp_files_read")])
            .is_empty()
    );
    assert_eq!(registry.source_of("mcp_files_read"), Some(ToolSource::Mcp));
}

#[test]
fn removes_only_the_names_the_server_it_is_replacing_had() {
    let registry = registry();
    let sink = registry_tool_sink(Arc::clone(&registry), ToolSource::Mcp);
    sink.replace("files", vec![tool("a"), tool("b")]);
    sink.replace("github", vec![tool("c")]);

    // A reconnect with a shorter list loses what it no longer has, and takes
    // nothing of its neighbour's.
    sink.replace("files", vec![tool("a")]);
    assert!(registry.has("a"));
    assert!(!registry.has("b"));
    assert!(registry.has("c"));
}

#[test]
fn unregisters_everything_for_a_server_that_went_away() {
    let registry = registry();
    let sink = registry_tool_sink(Arc::clone(&registry), ToolSource::Mcp);
    sink.replace("files", vec![tool("a"), tool("b")]);
    sink.replace("files", Vec::new());
    assert_eq!(registry.size(), 0);
}

#[test]
fn is_idempotent_so_a_repeated_publish_does_not_double_register() {
    let registry = registry();
    let sink = registry_tool_sink(Arc::clone(&registry), ToolSource::Mcp);
    for _ in 0..3 {
        assert!(sink.replace("files", vec![tool("a")]).is_empty());
    }
    assert_eq!(registry.size(), 1);
}

#[test]
fn reports_a_clash_with_another_source_and_keeps_the_rest() {
    let registry = registry();
    registry
        .register(tool("taken"), ToolSource::Builtin)
        .unwrap();
    let sink = registry_tool_sink(Arc::clone(&registry), ToolSource::Extension);

    // One clash must not cost an owner its other tools, nor take down the
    // reconcile that was registering them.
    let rejected = sink.replace("ext", vec![tool("taken"), tool("mine")]);
    assert_eq!(rejected, vec!["taken".to_owned()]);
    assert!(registry.has("mine"));
    assert_eq!(registry.source_of("taken"), Some(ToolSource::Builtin));
}

#[test]
fn does_not_later_unregister_a_name_it_never_owned() {
    let registry = registry();
    registry
        .register(tool("taken"), ToolSource::Builtin)
        .unwrap();
    let sink = registry_tool_sink(Arc::clone(&registry), ToolSource::Extension);
    sink.replace("ext", vec![tool("taken")]);
    // Nothing was accepted, so the owner holds nothing — and the next replace
    // must not reach for the name the built-in still has.
    sink.replace("ext", vec![tool("mine")]);
    assert_eq!(registry.source_of("taken"), Some(ToolSource::Builtin));
    assert!(registry.has("mine"));
}
