//! Approved-operation scopes enforce the granted surface at dispatch time.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
use ghostai_core::Result;
use ghostai_protocol::{ToolDefinition, ToolPermission, ToolPermissions, ToolRisk, ToolSource};
use ghostai_security::{JailOptions, PolicyStore, WorkspaceJail};
use ghostai_tools::operations::approved_operation_scope;
use ghostai_tools::{
    AnyTool, BoxFuture, CommandRunner, RunOutcome, RunRequest, Tool, ToolContext, ToolExecution,
    ToolRegistry,
};
use serde_json::json;
use std::sync::Arc;

#[derive(Default)]
struct Recording(parking_lot::Mutex<Vec<Vec<String>>>);
impl CommandRunner for Recording {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        let mut argv = vec![request.plan.file];
        argv.extend(request.plan.args);
        self.0.lock().push(argv);
        Box::pin(async {
            Ok(RunOutcome {
                stdout: "ok".into(),
                stderr: String::new(),
                truncated: false,
                code: Some(0),
                signal: None,
                timed_out: false,
                transcript_dir: None,
            })
        })
    }
}

struct Waiting;
impl CommandRunner for Waiting {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        Box::pin(async move {
            request.token.cancelled().await;
            Err(ghostai_core::GhostError::aborted("test command"))
        })
    }
}

struct WaitingTool(ToolDefinition);
impl Tool for WaitingTool {
    fn definition(&self) -> &ToolDefinition {
        &self.0
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Safe
    }
    fn execute<'a>(
        &'a self,
        _args: serde_json::Value,
        context: &'a ToolContext,
    ) -> BoxFuture<'a, ToolExecution> {
        Box::pin(async move {
            context.token.cancelled().await;
            ghostai_core::GhostError::aborted("registered test tool").into()
        })
    }
}

/// A policy root holding one toolbox and the definitions it grants.
///
/// The directory is returned with the store because it has to outlive it: the
/// store reads the manifest again on every re-authorisation check.
fn policy_root(
    toolbox: &serde_json::Value,
    definitions: &[(&str, serde_json::Value)],
) -> (tempfile::TempDir, Arc<PolicyStore>) {
    let root = tempfile::tempdir().unwrap();
    for dir in ["toolboxes", "tool-definitions", "workspace"] {
        std::fs::create_dir_all(root.path().join(dir)).unwrap();
    }
    std::fs::write(
        root.path().join("toolboxes/check.json"),
        toolbox.to_string(),
    )
    .unwrap();
    for (name, definition) in definitions {
        std::fs::write(
            root.path().join(format!("tool-definitions/{name}.json")),
            definition.to_string(),
        )
        .unwrap();
    }
    let store = Arc::new(PolicyStore::new(root.path()));
    (root, store)
}

fn jail_context(store: &PolicyStore) -> ToolContext {
    let jail =
        Arc::new(WorkspaceJail::new(JailOptions::new(store.root().join("workspace"))).unwrap());
    ToolContext::new(jail, Arc::new(ghostai_protocol::ToolsConfig::default()))
}

#[tokio::test]
async fn scoped_calls_cannot_enable_undeclared_tools_override_ceilings_or_add_argv() {
    let (_root, store) = policy_root(
        &json!({"schema":"ghostai.toolbox/1","name":"check","tools":[{"name":"status","definition":"status","permission":"ask"}]}),
        &[(
            "status",
            json!({"schema":"ghostai.tool/1","description":"Status","implementation":{"kind":"command","executable":"/usr/bin/git","argv":["status"]},"parameters":{"type":"object","properties":{},"additionalProperties":false}}),
        )],
    );
    let approved = store.approve_toolbox("check").unwrap();
    let overrides: ToolPermissions = [
        ("status".into(), ToolPermission::Allow),
        ("exec".into(), ToolPermission::Allow),
    ]
    .into_iter()
    .collect();
    let scope = approved_operation_scope(
        &approved,
        &store,
        &Arc::new(ToolRegistry::new()),
        &overrides,
        None,
    )
    .unwrap();
    assert_eq!(scope.permission_for("status"), ToolPermission::Ask);
    assert_eq!(scope.permission_for("exec"), ToolPermission::Deny);
    assert!(scope.get("exec").is_none());
    assert_eq!(scope.definitions().len(), 1);
    let runner = Arc::new(Recording::default());
    let mut context = jail_context(&store);
    context.runner = runner.clone();
    let tool = scope.get("status").unwrap();
    assert!(
        tool.execute(json!({"args":["push"]}), &context)
            .await
            .is_error
    );
    assert_eq!(runner.0.lock().len(), 0);
    let outcome = tool.execute(json!({}), &context).await;
    assert!(!outcome.is_error, "{}", outcome.content);
    assert_eq!(runner.0.lock()[0], ["/usr/bin/git", "status"]);
    context.runner = Arc::new(Waiting);
    let revoker = Arc::clone(&store);
    let ((), cancelled) = tokio::join!(
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            revoker.revoke_toolbox("check").unwrap();
        },
        tool.execute(json!({}), &context)
    );
    assert!(cancelled.is_error);
    assert!(cancelled.content.contains("approval revoked"));
    assert!(tool.execute(json!({}), &context).await.is_error);
    assert_eq!(runner.0.lock().len(), 1);
}

#[tokio::test]
async fn revocation_cancels_an_active_registered_operation() {
    let definition: ToolDefinition = serde_json::from_value(json!({
        "name":"wait_registered", "description":"Wait", "parameters":{"type":"object","properties":{},"additionalProperties":false},
        "risk":"safe", "source":"builtin"
    })).unwrap();
    let digest = ghostai_tools::operations::definition_digest(&definition).unwrap();
    let (_root, store) = policy_root(
        &json!({"schema":"ghostai.toolbox/1","name":"check","tools":[{"name":"wait","definition":"wait","permission":"allow"}]}),
        &[(
            "wait",
            json!({"schema":"ghostai.tool/1","description":"Wait","implementation":{"kind":"registered","tool":"wait_registered","digest":digest},"parameters":{"type":"object","properties":{},"additionalProperties":false}}),
        )],
    );
    let approved = store.approve_toolbox("check").unwrap();
    let registry = Arc::new(ToolRegistry::new());
    let waiting: AnyTool = Arc::new(WaitingTool(definition));
    registry
        .register_all(vec![waiting], ToolSource::Builtin)
        .unwrap();
    let scope =
        approved_operation_scope(&approved, &store, &registry, &ToolPermissions::new(), None)
            .unwrap();
    let context = jail_context(&store);
    let tool = scope.get("wait").unwrap();
    let revoker = Arc::clone(&store);
    let ((), result) = tokio::join!(
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            revoker.revoke_toolbox("check").unwrap();
        },
        tool.execute(json!({}), &context)
    );
    assert!(result.is_error);
    assert!(result.content.contains("approval revoked"));
}
