//! The registry: registration, notification, the memoised definition list and
//! an `execute` that never fails.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::ToolSource;
use darkwire_tools::testkit::TestWorkspace;
use darkwire_tools::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolInvocation, ToolOutput, ToolRegistry,
    ToolScope, ToolSpec, TypedTool,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    #[schemars(description = "What to echo.")]
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

struct Echo;

impl ToolHandler for Echo {
    type Args = EchoArgs;

    fn execute<'a>(
        &'a self,
        args: EchoArgs,
        _: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move { Ok(ToolOutput::text(args.text)) })
    }
}

struct Failing;

impl ToolHandler for Failing {
    type Args = NoArgs;

    fn execute<'a>(&'a self, _: NoArgs, _: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move { Err(WireError::new(ErrorKind::Tool, "boom")) })
    }
}

struct Flagged;

impl ToolHandler for Flagged {
    type Args = NoArgs;

    fn execute<'a>(&'a self, _: NoArgs, _: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move { Ok(ToolOutput::error("Error: not found").with_detail("code", 7)) })
    }
}

/// A handler that never returns on its own.
///
/// `deaf` is the case the race exists for: a tool that ignores its token would
/// hang the turn forever if the registry only *signalled* the timeout instead
/// of racing it.
struct Blocking {
    deaf: bool,
}

impl ToolHandler for Blocking {
    type Args = NoArgs;

    fn execute<'a>(&'a self, _: NoArgs, ctx: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            if self.deaf {
                std::future::pending::<()>().await;
            }
            ctx.token.cancelled().await;
            Err(WireError::new(ErrorKind::Tool, "interrupted"))
        })
    }
}

struct Observer {
    entered: Arc<AtomicBool>,
}

impl ToolHandler for Observer {
    type Args = NoArgs;

    fn execute<'a>(&'a self, _: NoArgs, _: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput>> {
        self.entered.store(true, Ordering::SeqCst);
        Box::pin(async move { Ok(ToolOutput::text("ran")) })
    }
}

fn tool<H: ToolHandler>(name: &str, handler: H) -> AnyTool {
    Arc::new(TypedTool::new(ToolSpec::new(name, format!("The {name} tool.")), handler).unwrap())
}

fn echo() -> AnyTool {
    tool("echo", Echo)
}

fn failing() -> AnyTool {
    tool("failing", Failing)
}

fn flagged() -> AnyTool {
    tool("flagged", Flagged)
}

fn names(registry: &ToolRegistry) -> Vec<String> {
    registry
        .definitions()
        .iter()
        .map(|definition| definition.name.clone())
        .collect()
}

#[test]
fn registers_and_looks_up_by_name() {
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    assert!(registry.has("echo"));
    assert_eq!(registry.get("echo").unwrap().definition().name, "echo");
    assert_eq!(registry.source_of("echo"), Some(ToolSource::Builtin));
    assert_eq!(registry.size(), 1);
    assert!(format!("{registry:?}").contains("echo"));
}

#[test]
fn refuses_a_duplicate_name_rather_than_overwriting() {
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    let error = registry
        .register(echo(), ToolSource::Extension)
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(error.message.contains("already registered by builtin"));
    assert_eq!(registry.source_of("echo"), Some(ToolSource::Builtin));
}

#[test]
fn rolls_register_all_back_when_a_name_in_the_batch_collides() {
    let registry = ToolRegistry::new();
    registry.register(failing(), ToolSource::Builtin).unwrap();
    let error = registry
        .register_all([echo(), failing()], ToolSource::Extension)
        .err()
        .unwrap();
    assert!(error.message.contains("already registered"));
    // A half-installed extension is worse than one that failed to install.
    assert!(!registry.has("echo"));
    assert_eq!(registry.source_of("failing"), Some(ToolSource::Builtin));
}

#[test]
fn removes_exactly_what_one_source_registered() {
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    registry.register(failing(), ToolSource::Extension).unwrap();
    registry.register(flagged(), ToolSource::Extension).unwrap();
    assert_eq!(registry.unregister_by_source(ToolSource::Extension), 2);
    assert_eq!(registry.names(), vec!["echo"]);
    assert_eq!(registry.unregister_by_source(ToolSource::Mcp), 0);
}

#[test]
fn reports_whether_a_single_unregister_did_anything() {
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    assert!(registry.unregister("echo"));
    assert!(!registry.unregister("echo"));
}

#[test]
fn clears_everything() {
    let registry = ToolRegistry::default();
    registry.clear();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    registry.clear();
    assert_eq!(registry.size(), 0);
    assert!(registry.definitions().is_empty());
}

#[test]
fn subscribe_fires_on_every_mutation_and_not_on_a_no_op() {
    let registry = ToolRegistry::new();
    let fired = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fired);
    registry.subscribe(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    registry.register(echo(), ToolSource::Builtin).unwrap();
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    registry.register(failing(), ToolSource::Mcp).unwrap();
    assert_eq!(fired.load(Ordering::SeqCst), 2);
    registry.unregister_by_source(ToolSource::Mcp);
    assert_eq!(fired.load(Ordering::SeqCst), 3);
    // Nothing was registered by an extension, so nothing changed.
    registry.unregister_by_source(ToolSource::Extension);
    assert_eq!(fired.load(Ordering::SeqCst), 3);
    registry.clear();
    assert_eq!(fired.load(Ordering::SeqCst), 4);
    // Already empty.
    registry.clear();
    assert_eq!(fired.load(Ordering::SeqCst), 4);
}

#[test]
fn subscribe_stops_after_unsubscribe() {
    let registry = ToolRegistry::new();
    let fired = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fired);
    let id = registry.subscribe(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    registry.register(echo(), ToolSource::Builtin).unwrap();
    assert!(registry.unsubscribe(id));
    assert!(!registry.unsubscribe(id));
    registry.register(failing(), ToolSource::Builtin).unwrap();
    assert_eq!(fired.load(Ordering::SeqCst), 1);
}

#[test]
fn a_listener_may_read_the_registry_back() {
    // Called with the lock released, so a transport can ask for the new
    // definitions from inside its own notification.
    let registry = Arc::new(ToolRegistry::new());
    let seen = Arc::new(AtomicUsize::new(0));
    let inner = Arc::clone(&registry);
    let counter = Arc::clone(&seen);
    registry.subscribe(move || {
        counter.store(inner.definitions().len(), Ordering::SeqCst);
    });
    registry.register(echo(), ToolSource::Builtin).unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[test]
fn definitions_sort_by_name_so_the_cached_prompt_prefix_is_stable() {
    let registry = ToolRegistry::new();
    registry.register(flagged(), ToolSource::Builtin).unwrap();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    registry.register(failing(), ToolSource::Builtin).unwrap();
    assert_eq!(names(&registry), vec!["echo", "failing", "flagged"]);
}

#[test]
fn definitions_memoise_until_the_registry_changes() {
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    let first = registry.definitions();
    assert!(Arc::ptr_eq(&registry.definitions(), &first));

    registry.register(failing(), ToolSource::Mcp).unwrap();
    let second = registry.definitions();
    assert!(!Arc::ptr_eq(&second, &first));
    assert_eq!(second.len(), 2);

    registry.unregister_by_source(ToolSource::Mcp);
    assert!(!Arc::ptr_eq(&registry.definitions(), &second));
}

#[test]
fn definitions_keep_the_memo_on_a_no_op_mutation() {
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    let first = registry.definitions();
    assert!(!registry.unregister("absent"));
    assert_eq!(registry.unregister_by_source(ToolSource::Extension), 0);
    assert!(Arc::ptr_eq(&registry.definitions(), &first));
}

#[test]
fn definitions_carry_the_registration_source_of_each_tool() {
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Mcp).unwrap();
    assert_eq!(registry.definitions()[0].source, ToolSource::Mcp);
    assert_eq!(registry.revision(), 1);
}

#[tokio::test]
async fn execute_runs_a_tool_and_reports_the_result() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    let execution = registry
        .execute_scoped(
            &ToolInvocation::with_json("echo", r#"{"text":"hello"}"#),
            ws.context(),
            None,
        )
        .await;
    assert_eq!(execution.name, "echo");
    assert_eq!(execution.content, "hello");
    assert!(!execution.is_error);
    assert!(!execution.truncated);
    assert_eq!(execution.kind, None);
}

#[tokio::test]
async fn execute_treats_absent_or_empty_arguments_as_an_empty_object() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry.register(flagged(), ToolSource::Builtin).unwrap();
    for call in [
        ToolInvocation::named("flagged"),
        ToolInvocation::with_json("flagged", ""),
        ToolInvocation::with_json("flagged", "   "),
    ] {
        let execution = registry.execute_scoped(&call, ws.context(), None).await;
        assert_eq!(execution.content, "Error: not found");
    }
}

#[tokio::test]
async fn execute_reports_a_flagged_failure_without_treating_the_text_as_the_signal() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry.register(flagged(), ToolSource::Builtin).unwrap();
    let execution = registry
        .execute_scoped(&ToolInvocation::named("flagged"), ws.context(), None)
        .await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, None);
    assert_eq!(
        execution.details,
        json!({"code": 7}).as_object().cloned().unwrap()
    );
}

#[tokio::test]
async fn execute_never_fails_for_an_unknown_tool() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    let execution = registry
        .execute_scoped(&ToolInvocation::named("nope"), ws.context(), None)
        .await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::NotFound));
    assert!(execution.content.contains("echo"));
}

#[tokio::test]
async fn execute_never_fails_on_malformed_argument_json() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    let execution = registry
        .execute_scoped(
            &ToolInvocation::with_json("echo", "{oops"),
            ws.context(),
            None,
        )
        .await;
    assert_eq!(execution.kind, Some(ErrorKind::InvalidInput));
}

#[tokio::test]
async fn execute_never_fails_on_schema_invalid_arguments() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    let execution = registry
        .execute_scoped(
            &ToolInvocation::with_json("echo", r#"{"text":1}"#),
            ws.context(),
            None,
        )
        .await;
    assert_eq!(execution.kind, Some(ErrorKind::InvalidInput));
    assert_eq!(execution.details.get("tool"), Some(&json!("echo")));
}

#[tokio::test]
async fn execute_never_fails_when_the_handler_does() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry.register(failing(), ToolSource::Builtin).unwrap();
    let execution = registry
        .execute_scoped(&ToolInvocation::named("failing"), ws.context(), None)
        .await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::Tool));
    assert_eq!(execution.content, "boom");
}

#[tokio::test]
async fn execute_leaves_a_long_result_whole_for_the_dispatcher_to_cut() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry.register(echo(), ToolSource::Builtin).unwrap();
    let long = "x".repeat(5_000);
    let execution = registry
        .execute_scoped(
            &ToolInvocation::with_json("echo", json!({"text": long}).to_string()),
            &ws.with(|config| config.max_output_chars = 100),
            None,
        )
        .await;
    assert!(!execution.truncated);
    assert_eq!(execution.content, long);
}

#[tokio::test]
async fn execute_refuses_to_enter_a_handler_once_the_turn_is_aborted() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    let entered = Arc::new(AtomicBool::new(false));
    registry
        .register(
            tool(
                "observer",
                Observer {
                    entered: Arc::clone(&entered),
                },
            ),
            ToolSource::Builtin,
        )
        .unwrap();
    ws.token().cancel();
    let execution = registry
        .execute_scoped(&ToolInvocation::named("observer"), ws.context(), None)
        .await;
    assert!(!entered.load(Ordering::SeqCst));
    assert_eq!(execution.kind, Some(ErrorKind::Aborted));
}

#[tokio::test]
async fn execute_propagates_the_turn_token_into_the_handler() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry
        .register(
            tool("waiting", Blocking { deaf: false }),
            ToolSource::Builtin,
        )
        .unwrap();
    let token = ws.token().clone();
    let call = ToolInvocation::named("waiting");
    let running = registry.execute_scoped(&call, ws.context(), None);
    let cancel = async move {
        tokio::task::yield_now().await;
        token.cancel();
    };
    let (execution, ()) = tokio::join!(running, cancel);
    assert_eq!(execution.kind, Some(ErrorKind::Aborted));
}

#[tokio::test(start_paused = true)]
async fn execute_times_a_deaf_handler_out() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry
        .register(tool("deaf", Blocking { deaf: true }), ToolSource::Builtin)
        .unwrap();
    let execution = registry
        .execute_scoped(
            &ToolInvocation::named("deaf"),
            &ws.with(|config| config.tool_timeout_ms = 30_000),
            None,
        )
        .await;
    assert_eq!(execution.kind, Some(ErrorKind::Timeout));
    assert!(execution.content.contains("30000"));
}

#[tokio::test(start_paused = true)]
async fn execute_uses_the_calling_agents_own_timeout() {
    // One registry serves every agent, so the cap has to come with the call.
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry
        .register(tool("deaf", Blocking { deaf: true }), ToolSource::Builtin)
        .unwrap();
    let call = ToolInvocation::named("deaf");
    let short = registry
        .execute_scoped(
            &call,
            &ws.with(|config| config.tool_timeout_ms = 1_000),
            None,
        )
        .await;
    let long = registry
        .execute_scoped(
            &call,
            &ws.with(|config| config.tool_timeout_ms = 9_000),
            None,
        )
        .await;
    assert!(short.content.contains("1000"), "{}", short.content);
    assert!(long.content.contains("9000"), "{}", long.content);
}

#[tokio::test(start_paused = true)]
async fn execute_reports_an_abort_as_aborted_even_when_a_timeout_is_configured() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry
        .register(tool("deaf", Blocking { deaf: true }), ToolSource::Builtin)
        .unwrap();
    let ctx = ws.with(|config| config.tool_timeout_ms = 30_000);
    let token = ctx.token.clone();
    let call = ToolInvocation::named("deaf");
    let running = registry.execute_scoped(&call, &ctx, None);
    let cancel = async move {
        tokio::task::yield_now().await;
        token.cancel();
    };
    let (execution, ()) = tokio::join!(running, cancel);
    assert_eq!(execution.kind, Some(ErrorKind::Aborted));
}

#[tokio::test]
async fn the_registry_is_itself_a_scope_that_allows_everything() {
    let ws = TestWorkspace::new();
    let registry: Arc<dyn ToolScope> = Arc::new({
        let registry = ToolRegistry::new();
        registry.register(echo(), ToolSource::Builtin).unwrap();
        registry
    });
    assert_eq!(
        registry.permission_for("anything"),
        darkwire_protocol::ToolPermission::Allow
    );
    assert_eq!(registry.definitions().len(), 1);
    assert!(registry.get("echo").is_some());
    let execution = registry
        .execute(
            &ToolInvocation::with_json("echo", r#"{"text":"via scope"}"#),
            ws.context(),
        )
        .await;
    assert_eq!(execution.content, "via scope");
}
