//! The permission map and the restricted view of a registry it drives.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_core::{ErrorKind, Result};
use darkwire_protocol::{ToolPermission, ToolPermissions, ToolSource};
use darkwire_tools::testkit::TestWorkspace;
use darkwire_tools::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolInvocation, ToolOutput, ToolRegistry,
    ToolScope, ToolSpec, TypedTool, is_enabled, permission_for,
};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

struct Named(String);

impl ToolHandler for Named {
    type Args = NoArgs;

    fn execute<'a>(&'a self, _: NoArgs, _: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move { Ok(ToolOutput::text(self.0.clone())) })
    }
}

fn tool(name: &str) -> AnyTool {
    Arc::new(
        TypedTool::new(
            ToolSpec::new(name, format!("The {name} tool.")),
            Named(name.to_owned()),
        )
        .unwrap(),
    )
}

fn perms(entries: &[(&str, ToolPermission)]) -> ToolPermissions {
    entries
        .iter()
        .map(|(name, permission)| ((*name).to_owned(), *permission))
        .collect()
}

/// Everything the fixture registry holds, at `allow`.
fn all() -> ToolPermissions {
    perms(&[
        ("read_file", ToolPermission::Allow),
        ("write_file", ToolPermission::Allow),
        ("exec", ToolPermission::Allow),
    ])
}

fn registry() -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_all(
            [tool("read_file"), tool("write_file"), tool("exec")],
            ToolSource::Builtin,
        )
        .unwrap();
    registry
}

fn names(scope: &dyn ToolScope) -> Vec<String> {
    scope
        .definitions()
        .iter()
        .map(|definition| definition.name.clone())
        .collect()
}

#[test]
fn permission_for_reads_back_what_the_map_says() {
    let map = perms(&[
        ("read_file", ToolPermission::Allow),
        ("exec", ToolPermission::Ask),
        ("write_file", ToolPermission::Deny),
    ]);
    assert_eq!(
        permission_for(Some(&map), "read_file"),
        ToolPermission::Allow
    );
    assert_eq!(permission_for(Some(&map), "exec"), ToolPermission::Ask);
    assert_eq!(
        permission_for(Some(&map), "write_file"),
        ToolPermission::Deny
    );
}

#[test]
fn permission_for_denies_a_tool_the_map_does_not_mention() {
    // The whole model: enabling is explicit, so silence is not consent.
    assert_eq!(
        permission_for(Some(&perms(&[])), "exec"),
        ToolPermission::Deny
    );
    assert_eq!(
        permission_for(
            Some(&perms(&[("read_file", ToolPermission::Allow)])),
            "exec"
        ),
        ToolPermission::Deny
    );
}

#[test]
fn permission_for_allows_everything_when_there_is_no_map_at_all() {
    assert_eq!(permission_for(None, "exec"), ToolPermission::Allow);
}

#[test]
fn is_enabled_treats_only_deny_and_absent_as_off() {
    assert!(is_enabled(
        Some(&perms(&[("exec", ToolPermission::Allow)])),
        "exec"
    ));
    assert!(is_enabled(
        Some(&perms(&[("exec", ToolPermission::Ask)])),
        "exec"
    ));
    assert!(!is_enabled(
        Some(&perms(&[("exec", ToolPermission::Deny)])),
        "exec"
    ));
    assert!(!is_enabled(Some(&perms(&[])), "exec"));
}

#[test]
fn select_offers_nothing_for_an_empty_map() {
    let registry = registry();
    assert!(registry.select(perms(&[])).definitions().is_empty());
    assert_eq!(registry.definitions().len(), 3);
}

#[test]
fn select_hides_a_denied_tool_from_the_definitions() {
    let registry = registry();
    let mut map = all();
    map.insert("exec".to_owned(), ToolPermission::Deny);
    let scope = registry.select(map);
    assert_eq!(names(scope.as_ref()), vec!["read_file", "write_file"]);
    assert_eq!(registry.definitions().len(), 3);
}

#[test]
fn select_offers_a_tool_set_to_ask() {
    let registry = registry();
    let scope = registry.select(perms(&[
        ("read_file", ToolPermission::Allow),
        ("exec", ToolPermission::Ask),
    ]));
    assert_eq!(names(scope.as_ref()), vec!["exec", "read_file"]);
    assert_eq!(scope.permission_for("exec"), ToolPermission::Ask);
    assert_eq!(scope.permission_for("write_file"), ToolPermission::Deny);
}

#[test]
fn select_keeps_definitions_sorted() {
    let registry = registry();
    let listed = names(registry.select(all()).as_ref());
    let mut sorted = listed.clone();
    sorted.sort();
    assert_eq!(listed, sorted);
}

#[test]
fn select_reports_a_hidden_tool_as_absent_rather_than_forbidden() {
    let registry = registry();
    let mut map = all();
    map.insert("exec".to_owned(), ToolPermission::Deny);
    let scope = registry.select(map);
    assert!(scope.get("exec").is_none());
    assert_eq!(
        scope.get("read_file").unwrap().definition().name,
        "read_file"
    );
    // The registry still has it — this is a view, not a removal.
    assert!(registry.get("exec").is_some());
}

#[tokio::test]
async fn select_refuses_to_execute_a_hidden_tool_without_admitting_it_exists() {
    let ws = TestWorkspace::new();
    let registry = registry();
    let mut map = all();
    map.insert("exec".to_owned(), ToolPermission::Deny);
    let scope = registry.select(map);
    let result = scope
        .execute(&ToolInvocation::named("exec"), ws.context())
        .await;
    assert!(result.is_error);
    assert_eq!(result.kind, Some(ErrorKind::NotFound));
    assert!(result.content.contains("read_file, write_file"));
    assert!(!result.content.to_lowercase().contains("denied"));
}

#[tokio::test]
async fn select_still_executes_a_tool_the_map_admits() {
    let ws = TestWorkspace::new();
    let registry = registry();
    let scope = registry.select(perms(&[("read_file", ToolPermission::Allow)]));
    let result = scope
        .execute(&ToolInvocation::named("read_file"), ws.context())
        .await;
    assert!(!result.is_error);
    assert_eq!(result.content, "read_file");
}

#[tokio::test]
async fn select_sees_a_tool_registered_after_the_scope_was_built() {
    // An extension loading at runtime must become visible to every agent whose
    // permissions admit it; a scope that snapshotted the list never would.
    let ws = TestWorkspace::new();
    let registry = registry();
    let mut map = all();
    map.insert("exec".to_owned(), ToolPermission::Deny);
    map.insert("list_dir".to_owned(), ToolPermission::Allow);
    let scope = registry.select(map);
    assert_eq!(scope.definitions().len(), 2);

    registry
        .register(tool("list_dir"), ToolSource::Extension)
        .unwrap();
    assert_eq!(
        names(scope.as_ref()),
        vec!["list_dir", "read_file", "write_file"]
    );
    let result = scope
        .execute(&ToolInvocation::named("list_dir"), ws.context())
        .await;
    assert!(!result.is_error);
}

#[test]
fn select_does_not_admit_a_late_registered_tool_the_agent_never_enabled() {
    let registry = registry();
    let mut map = all();
    map.insert("exec".to_owned(), ToolPermission::Deny);
    let scope = registry.select(map);
    registry
        .register(tool("list_dir"), ToolSource::Extension)
        .unwrap();
    assert_eq!(names(scope.as_ref()), vec!["read_file", "write_file"]);
}

#[test]
fn select_drops_a_tool_the_registry_unregisters() {
    let registry = registry();
    let mut map = all();
    map.insert("exec".to_owned(), ToolPermission::Deny);
    let scope = registry.select(map);
    assert_eq!(scope.definitions().len(), 2);
    registry.unregister("write_file");
    assert_eq!(names(scope.as_ref()), vec!["read_file"]);
}

#[test]
fn select_reuses_the_memo_while_nothing_has_changed_and_rebuilds_when_it_does() {
    let registry = registry();
    let mut map = all();
    map.insert("list_dir".to_owned(), ToolPermission::Allow);
    let scope = registry.select(map);
    let first = scope.definitions();
    assert!(Arc::ptr_eq(&scope.definitions(), &first));
    registry
        .register(tool("list_dir"), ToolSource::Builtin)
        .unwrap();
    let second = scope.definitions();
    assert!(!Arc::ptr_eq(&second, &first));
    assert!(Arc::ptr_eq(&scope.definitions(), &second));
}

#[test]
fn select_gives_two_agents_independent_views_of_one_registry() {
    let registry = registry();
    let reviewer = registry.select(perms(&[("read_file", ToolPermission::Allow)]));
    let mut map = all();
    map.insert("exec".to_owned(), ToolPermission::Deny);
    let writer = registry.select(map);
    assert_eq!(reviewer.definitions().len(), 1);
    assert_eq!(writer.definitions().len(), 2);
    assert_eq!(registry.definitions().len(), 3);
}
