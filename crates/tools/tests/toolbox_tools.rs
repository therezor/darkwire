//! A toolbox's entries as callables, their permissions, and the overlay scope.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use ghostai_core::{ErrorKind, Result};
use ghostai_protocol::{ToolPermission, ToolPermissions, ToolRisk, Toolbox};
use ghostai_tools::testkit::TestWorkspace;
use ghostai_tools::{
    BoxFuture, BuiltinOptions, CommandRunner, RunOutcome, RunRequest, ToolInvocation, ToolRegistry,
    ToolScope, register_builtins, toolbox_permissions, toolbox_tool, toolbox_tools,
    visible_toolbox_entries, with_toolbox_tools,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

const DIGEST: &str = "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn toolbox_of(overrides: Value) -> Toolbox {
    let mut base = json!({
        "schema": "ghostai.toolbox/1",
        "name": "web-research",
        "image": DIGEST,
        "tools": [
            {
                "name": "ddgr",
                "use": "Search the web.",
                "args": "--json is required; the plain form is a pager and hangs.",
                "example": ["--json", "-n", "3", "sqlite wal mode"],
                "requiresArgs": true,
            },
            {"name": "fetch", "use": "Read a web page as text.", "requiresArgs": true},
        ],
    });
    if let (Value::Object(base), Value::Object(overrides)) = (&mut base, overrides) {
        for (key, value) in overrides {
            base.insert(key, value);
        }
    }
    serde_json::from_value(base).unwrap()
}

fn exposed() -> Toolbox {
    toolbox_of(json!({"expose": "tools"}))
}

fn perms(entries: &[(&str, ToolPermission)]) -> ToolPermissions {
    entries
        .iter()
        .map(|(name, permission)| ((*name).to_owned(), *permission))
        .collect()
}

fn names_of(tools: &[Arc<dyn ghostai_tools::Tool>]) -> Vec<String> {
    tools
        .iter()
        .map(|tool| tool.definition().name.clone())
        .collect()
}

#[test]
fn exposes_nothing_by_default_because_prose_is_the_cheap_answer() {
    assert!(toolbox_tools(&toolbox_of(json!({}))).unwrap().is_empty());
}

#[test]
fn materialises_one_callable_per_declared_entry_when_asked() {
    assert_eq!(
        names_of(&toolbox_tools(&exposed()).unwrap()),
        vec!["ddgr", "fetch"]
    );
}

#[test]
fn carries_the_declared_use_into_the_description_the_model_reads() {
    let tools = toolbox_tools(&exposed()).unwrap();
    assert_eq!(tools[0].definition().description, "Search the web.");
    assert!(!tools[0].definition().description.contains("web-research"));
    let plain = toolbox_tools(&toolbox_of(
        json!({"expose": "tools", "tools": [{"name": "uptime"}]}),
    ))
    .unwrap();
    assert_eq!(plain[0].definition().description, "Run `uptime`.");
}

#[test]
fn puts_the_argument_guidance_and_a_copyable_example_on_the_args_field() {
    let tools = toolbox_tools(&exposed()).unwrap();
    let described = tools[0].definition().parameters["properties"]["args"]["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(described.contains("--json is required"));
    assert!(described.contains(r#"["--json","-n","3","sqlite wal mode"]"#));
    assert!(described.contains("do not repeat it"));
    let fetch = tools[1].definition().parameters["properties"]["args"]["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(fetch.starts_with("Arguments as separate strings."));
}

#[tokio::test]
async fn refuses_an_empty_call_for_a_program_that_needs_an_argument() {
    let ws = TestWorkspace::new();
    let tools = toolbox_tools(&exposed()).unwrap();
    let refused = tools[1].execute(json!({"args": []}), ws.context()).await;
    assert_eq!(refused.kind, Some(ErrorKind::InvalidInput));
    // Required rather than defaulted: "always send it, sometimes empty" is a
    // simpler contract for a model than "omit it unless you need it".
    let missing = tools[0].execute(json!({}), ws.context()).await;
    assert_eq!(missing.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn still_accepts_an_empty_array_for_a_program_that_takes_none() {
    let ws = TestWorkspace::new();
    let tools = toolbox_tools(&toolbox_of(json!({
        "expose": "tools",
        "tools": [{"name": "uptime", "use": "Show load."}],
    })))
    .unwrap();
    // Validation passes and the guard is reached — which refuses `uptime` for
    // being absent from this test's PATH or runs it; either way the call was
    // not an invalid-input refusal.
    let execution = tools[0].execute(json!({"args": []}), ws.context()).await;
    assert_ne!(execution.kind, Some(ErrorKind::InvalidInput));
}

#[test]
fn runs_at_the_exec_risk_band_because_it_is_exec() {
    assert_eq!(toolbox_tools(&exposed()).unwrap()[0].risk(), ToolRisk::Exec);
}

#[test]
fn drops_an_entry_no_provider_would_accept_as_a_function_name() {
    let tools = toolbox_tools(&toolbox_of(json!({
        "expose": "tools",
        "tools": [{"name": "foo bar"}, {"name": "ok"}],
    })))
    .unwrap();
    assert_eq!(names_of(&tools), vec!["ok"]);
}

#[test]
fn keeps_advertising_an_array_because_that_is_what_a_capable_model_should_send() {
    let tools = toolbox_tools(&exposed()).unwrap();
    assert_eq!(
        tools[0].definition().parameters["properties"]["args"]["type"],
        json!("array")
    );
    assert_eq!(
        tools[0].definition().parameters["properties"]["args"]["minItems"],
        json!(1)
    );
}

#[test]
fn permissions_are_empty_unless_the_entries_are_real_callables() {
    assert!(toolbox_permissions(&toolbox_of(json!({})), &perms(&[])).is_empty());
}

#[test]
fn permissions_report_the_manifest_default_per_program() {
    assert_eq!(
        toolbox_permissions(&exposed(), &perms(&[])),
        perms(&[
            ("ddgr", ToolPermission::Ask),
            ("fetch", ToolPermission::Ask)
        ])
    );
}

#[test]
fn permissions_carry_a_declared_permission_through() {
    let toolbox = toolbox_of(json!({
        "expose": "tools",
        "tools": [
            {"name": "search", "use": "Search.", "permission": "allow"},
            {"name": "nmap", "use": "Scan.", "permission": "deny"},
        ],
    }));
    assert_eq!(
        toolbox_permissions(&toolbox, &perms(&[])),
        perms(&[
            ("search", ToolPermission::Allow),
            ("nmap", ToolPermission::Deny)
        ])
    );
}

#[test]
fn permissions_skip_an_entry_no_provider_would_accept_exactly_as_the_callables_do() {
    let toolbox = toolbox_of(json!({
        "expose": "tools",
        "tools": [{"name": "ok", "use": "Fine."}, {"name": "not ok"}],
    }));
    let permissions = toolbox_permissions(&toolbox, &perms(&[]));
    let keys: Vec<&String> = permissions.keys().collect();
    assert_eq!(keys, vec!["ok"]);
    assert_eq!(names_of(&toolbox_tools(&toolbox).unwrap()), vec!["ok"]);
}

#[test]
fn permissions_let_an_agents_override_win_over_the_manifest() {
    assert_eq!(
        toolbox_permissions(&exposed(), &perms(&[("ddgr", ToolPermission::Allow)])),
        perms(&[
            ("ddgr", ToolPermission::Allow),
            ("fetch", ToolPermission::Ask)
        ])
    );
}

#[test]
fn permissions_take_star_as_the_default_for_every_entry_left_unnamed() {
    assert_eq!(
        toolbox_permissions(
            &exposed(),
            &perms(&[
                ("*", ToolPermission::Deny),
                ("fetch", ToolPermission::Allow)
            ])
        ),
        perms(&[
            ("ddgr", ToolPermission::Deny),
            ("fetch", ToolPermission::Allow)
        ])
    );
}

#[test]
fn permissions_may_widen_as_well_as_narrow() {
    let toolbox = toolbox_of(json!({
        "expose": "tools",
        "tools": [{"name": "nmap", "use": "Scan.", "permission": "deny"}],
    }));
    assert_eq!(
        toolbox_permissions(&toolbox, &perms(&[("nmap", ToolPermission::Allow)])),
        perms(&[("nmap", ToolPermission::Allow)])
    );
}

#[test]
fn visible_entries_is_every_entry_when_nothing_is_overridden() {
    let toolbox = toolbox_of(json!({}));
    let visible: Vec<&str> = visible_toolbox_entries(&toolbox, &perms(&[]))
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(visible, vec!["ddgr", "fetch"]);
}

#[test]
fn visible_entries_drops_what_the_agent_denied() {
    let toolbox = exposed();
    let visible: Vec<&str> = visible_toolbox_entries(
        &toolbox,
        &perms(&[
            ("*", ToolPermission::Deny),
            ("fetch", ToolPermission::Allow),
        ]),
    )
    .iter()
    .map(|entry| entry.name.as_str())
    .collect();
    assert_eq!(visible, vec!["fetch"]);
}

#[test]
fn visible_entries_applies_to_a_prompt_only_box() {
    let toolbox = toolbox_of(json!({}));
    assert!(toolbox_permissions(&toolbox, &perms(&[("*", ToolPermission::Deny)])).is_empty());
    let visible: Vec<&str> =
        visible_toolbox_entries(&toolbox, &perms(&[("ddgr", ToolPermission::Deny)]))
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
    assert_eq!(visible, vec!["fetch"]);
}

fn base() -> Arc<dyn ToolScope> {
    let registry = ToolRegistry::new();
    register_builtins(&registry, None, BuiltinOptions::default()).unwrap();
    Arc::new(registry)
}

fn overlay(permissions: ToolPermissions) -> Arc<dyn ToolScope> {
    with_toolbox_tools(
        base(),
        toolbox_tools(&exposed()).unwrap(),
        permissions,
        None,
    )
    .unwrap()
}

fn names(scope: &dyn ToolScope) -> Vec<String> {
    scope
        .definitions()
        .iter()
        .map(|definition| definition.name.clone())
        .collect()
}

#[test]
fn lays_the_toolbox_over_the_built_ins_sorted_as_one_list() {
    let scope = overlay(toolbox_permissions(&exposed(), &perms(&[])));
    let listed = names(scope.as_ref());
    assert!(listed.contains(&"ddgr".to_owned()));
    assert!(listed.contains(&"read_file".to_owned()));
    let mut sorted = listed.clone();
    sorted.sort();
    assert_eq!(listed, sorted);
    // Memoised against the base list, so asking twice does not re-sort.
    assert!(Arc::ptr_eq(&scope.definitions(), &scope.definitions()));
}

#[test]
fn returns_the_base_untouched_when_nothing_is_exposed() {
    let registry = base();
    let scope = with_toolbox_tools(Arc::clone(&registry), Vec::new(), perms(&[]), None).unwrap();
    assert!(Arc::ptr_eq(&scope.definitions(), &registry.definitions()));
}

#[test]
fn never_sends_the_model_a_program_the_agent_was_not_given() {
    let scope = overlay(toolbox_permissions(
        &exposed(),
        &perms(&[("*", ToolPermission::Deny), ("ddgr", ToolPermission::Allow)]),
    ));
    let listed = names(scope.as_ref());
    assert!(listed.contains(&"ddgr".to_owned()));
    assert!(!listed.contains(&"fetch".to_owned()));
    assert!(listed.contains(&"read_file".to_owned()));
    assert!(scope.get("fetch").is_none());
}

#[test]
fn resolves_a_toolbox_name_to_the_toolbox_tool_not_the_registry() {
    let scope = overlay(toolbox_permissions(&exposed(), &perms(&[])));
    assert_eq!(scope.get("ddgr").unwrap().definition().name, "ddgr");
    assert_eq!(
        scope.get("read_file").unwrap().definition().name,
        "read_file"
    );
    assert!(scope.get("nothing").is_none());
}

#[test]
fn reports_the_overlay_tools_own_permission_and_defers_to_the_base_otherwise() {
    let mut permissions = toolbox_permissions(&exposed(), &perms(&[]));
    permissions.insert("ddgr".to_owned(), ToolPermission::Allow);
    let scope = overlay(permissions);
    assert_eq!(scope.permission_for("ddgr"), ToolPermission::Allow);
    assert_eq!(scope.permission_for("fetch"), ToolPermission::Ask);
    assert_eq!(scope.permission_for("read_file"), ToolPermission::Allow);
}

#[test]
fn hides_an_overlay_tool_the_agent_switched_off() {
    let mut permissions = toolbox_permissions(&exposed(), &perms(&[]));
    permissions.insert("ddgr".to_owned(), ToolPermission::Deny);
    let scope = overlay(permissions);
    assert!(!names(scope.as_ref()).contains(&"ddgr".to_owned()));
    assert!(scope.get("ddgr").is_none());
}

struct Recording {
    seen: Mutex<Vec<Vec<String>>>,
}

impl CommandRunner for Recording {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        Box::pin(async move {
            let mut argv = vec![request.plan.file.clone()];
            argv.extend(request.plan.args.iter().cloned());
            self.seen.lock().push(argv);
            Ok(RunOutcome {
                stdout: "[]".to_owned(),
                code: Some(0),
                ..RunOutcome::default()
            })
        })
    }
}

#[tokio::test]
async fn runs_a_toolbox_tool_through_the_context_runner_as_exec_would() {
    // The whole point: this is a *spelling* of exec, so the guard, the runner
    // and the container all apply unchanged.
    let ws = TestWorkspace::new();
    let recording = Arc::new(Recording {
        seen: Mutex::new(Vec::new()),
    });
    let mut ctx = ws.context().clone();
    ctx.runner = Arc::clone(&recording) as Arc<dyn CommandRunner>;
    ctx.sandboxed = true;
    let scope = overlay(toolbox_permissions(&exposed(), &perms(&[])));

    let result = scope
        .execute(
            &ToolInvocation::with_json("ddgr", json!({"args": ["--json", "q"]}).to_string()),
            &ctx,
        )
        .await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(recording.seen.lock()[0], vec!["ddgr", "--json", "q"]);
    // Both the base and the overlay answer through the same scope.
    let base_result = scope
        .execute(&ToolInvocation::named("list_dir"), &ctx)
        .await;
    assert!(!base_result.is_error, "{}", base_result.content);
}

#[tokio::test]
async fn accepts_the_half_serialised_array_a_model_actually_produced() {
    let ws = TestWorkspace::new();
    let toolbox = toolbox_of(json!({
        "name": "research",
        "expose": "tools",
        "tools": [{"name": "search", "use": "Search the web.", "requiresArgs": true}],
    }));
    let tool = toolbox_tool(&toolbox.tools[0]).unwrap().unwrap();
    let recording = Arc::new(Recording {
        seen: Mutex::new(Vec::new()),
    });
    let mut ctx = ws.context().clone();
    ctx.runner = Arc::clone(&recording) as Arc<dyn CommandRunner>;
    ctx.sandboxed = true;

    let execution = tool
        .execute(
            json!({"args": r#"[0] SUFFOLK wildfire 2024 fires reports UK US news updates"]"#}),
            &ctx,
        )
        .await;
    assert!(!execution.is_error, "{}", execution.content);
    assert_eq!(recording.seen.lock()[0][1], "SUFFOLK");

    // Coercion is not permissiveness: `requiresArgs` is checked *after* it.
    let empty = tool.execute(json!({"args": ""}), &ctx).await;
    assert_eq!(empty.kind, Some(ErrorKind::InvalidInput));
    let none = tool.execute(json!({"args": []}), &ctx).await;
    assert_eq!(none.kind, Some(ErrorKind::InvalidInput));
}

#[test]
fn toolbox_tool_is_none_for_a_name_no_provider_accepts() {
    let toolbox = toolbox_of(json!({"tools": [{"name": "foo bar"}]}));
    assert!(toolbox_tool(&toolbox.tools[0]).unwrap().is_none());
}
