//! A toolbox's granted operations, as the tools an agent actually calls.
//!
//! [`approved_operation_scope`] builds a **complete** scope rather than an
//! overlay on the built-ins, and that is the design rather than an
//! optimisation. An agent with a toolbox can call its grants and nothing else:
//! no `exec` to reach a program the toolbox did not grant, no `read_file`
//! outside what an operation was approved to read. A toolbox that wants a
//! built-in back grants it explicitly, as a `registered` operation pinned to
//! that tool's own definition digest, so "this agent may read files" is a line
//! in a reviewed manifest instead of a default nobody chose.
//!
//! Authorisation is re-checked **during** a call, not only before it. A turn
//! can run for minutes, and an operator who revokes a toolbox mid-run means it
//! now, not when the process happens to exit. A 250 ms ticker re-resolves the
//! approval beside the running command and cancels the moment the hash it was
//! authorised under stops matching.

use ghostai_core::{ErrorKind, GhostError, Result};
use ghostai_protocol::toolbox::{OperationImplementation, ToolOperation};
use ghostai_protocol::{ToolDefinition, ToolPermissions, ToolRisk, ToolSource};
use ghostai_security::{
    ApprovedToolbox, ExecGuardOptions, PolicyStore, command_argv, guard_exec, manifest_hash,
    narrow_permission, toolbox::invalid, validate_input,
};
use serde_json::Value;
use std::sync::Arc;

use crate::{
    AnyTool, BoxFuture, RunRequest, Tool, ToolContext, ToolExecution, ToolRegistry, ToolScope,
};

/// How long an in-flight operation may go unchecked against its approval.
const REAUTHORIZE_INTERVAL_MS: u64 = 250;

/// The sandbox service, injected without giving tools an engine handle.
///
/// A tool that could talk to the container engine directly would be a tool that
/// could choose its own image and mounts. This trait is the whole of what one
/// may ask for: a named operation in a named toolbox, at an approval hash the
/// service verifies for itself.
pub trait OperationExecutor: Send + Sync {
    /// Execute a named approved operation. Inputs stay structured across the
    /// boundary — nothing is ever flattened into a command string.
    fn execute<'a>(
        &'a self,
        toolbox: &'a str,
        approval: &'a str,
        operation: &'a str,
        args: Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, ToolExecution>;
}

struct ApprovedOperation {
    definition: ToolDefinition,
    operation: ToolOperation,
    toolbox: String,
    hash: String,
    store: Arc<PolicyStore>,
    registry: Arc<ToolRegistry>,
    remote: Option<Arc<dyn OperationExecutor>>,
}

impl ApprovedOperation {
    /// Whether the toolbox still resolves to the bytes this tool was built
    /// from.
    fn still_authorized(&self) -> bool {
        self.store
            .require_toolbox(&self.toolbox)
            .is_ok_and(|approved| approved.sha256() == self.hash)
    }

    fn revoked() -> ToolExecution {
        GhostError::new(ErrorKind::Tool, "Toolbox approval revoked during execution").into()
    }
}

impl Tool for ApprovedOperation {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn risk(&self) -> ToolRisk {
        self.definition.risk
    }

    fn execute<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolExecution> {
        Box::pin(async move {
            if !self.still_authorized() {
                return invalid("Toolbox changed; reload the agent before executing").into();
            }
            if let Err(error) = validate_input(&self.operation, &args) {
                return error.into();
            }
            if ctx.token.is_cancelled() {
                return GhostError::aborted("tool operation").into();
            }
            match &self.operation.implementation {
                OperationImplementation::Registered { tool, digest } => {
                    self.run_registered(tool, digest, args, ctx).await
                }
                OperationImplementation::Command { .. } | OperationImplementation::Transcript => {
                    if let Some(remote) = &self.remote {
                        return remote
                            .execute(&self.toolbox, &self.hash, &self.definition.name, args, ctx)
                            .await;
                    }
                    self.run_locally(args, ctx).await
                }
            }
        })
    }
}

impl ApprovedOperation {
    /// A built-in, MCP or extension tool the toolbox granted by name.
    ///
    /// The digest is re-checked here as well as at approval, because an
    /// extension can be replaced or an MCP server can re-advertise a tool of
    /// the same name with a different schema while the process is running. A
    /// grant is for the definition that was reviewed, not for the name.
    async fn run_registered(
        &self,
        tool: &str,
        digest: &str,
        args: Value,
        ctx: &ToolContext,
    ) -> ToolExecution {
        let Some(registered) = self.registry.get(tool) else {
            return invalid("Registered operation is unavailable").into();
        };
        let current = self
            .registry
            .definitions()
            .iter()
            .find(|definition| definition.name == tool)
            .cloned();
        if current
            .as_ref()
            .and_then(|definition| definition_digest(definition).ok())
            .as_deref()
            != Some(digest)
        {
            return invalid("Registered tool identity changed; review and approve its definition")
                .into();
        }
        let token = ctx.token.child_token();
        let mut nested = ctx.clone();
        nested.token = token.clone();
        let run = registered.execute(args, &nested);
        self.watch(run, token).await
    }

    /// A command operation with no container: a guarded child process on the
    /// host, inside the workspace jail.
    async fn run_locally(&self, args: Value, ctx: &ToolContext) -> ToolExecution {
        let command = match command_argv(&self.operation, &args, &ctx.jail) {
            Ok(command) => command,
            Err(error) => return error.into(),
        };
        let plan = match guard_exec(
            &command,
            &ExecGuardOptions {
                jail: &ctx.jail,
                config: Some(&ctx.config.exec),
                env: &ctx.env,
                sandboxed: ctx.sandboxed,
            },
        ) {
            Ok(plan) => plan,
            Err(error) => return error.into(),
        };
        let token = ctx.token.child_token();
        let run = ctx.runner.run(RunRequest {
            timeout_ms: plan.timeout_ms,
            plan: plan.clone(),
            token: token.clone(),
            clock: Arc::clone(&ctx.clock),
            tee: None,
        });
        self.watch(
            Box::pin(async move {
                match run.await {
                    Ok(outcome) => {
                        crate::builtin::exec::render_run(&command, &plan, &outcome).into()
                    }
                    Err(error) => error.into(),
                }
            }),
            token,
        )
        .await
    }

    /// Runs `work` while re-checking the approval beside it, cancelling the
    /// moment it stops matching.
    async fn watch(
        &self,
        work: impl Future<Output = ToolExecution>,
        token: tokio_util::sync::CancellationToken,
    ) -> ToolExecution {
        tokio::pin!(work);
        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(REAUTHORIZE_INTERVAL_MS));
        loop {
            tokio::select! {
                result = &mut work => return result,
                _ = interval.tick() => {
                    if !self.still_authorized() {
                        token.cancel();
                        // Awaited rather than dropped: the work owns a child
                        // process, and returning first would leave it running
                        // with nothing holding a handle to stop it.
                        let _ = (&mut work).await;
                        return Self::revoked();
                    }
                }
            }
        }
    }
}

/// Stable approval identity of a registered tool's schema and source.
pub fn definition_digest(definition: &ToolDefinition) -> Result<String> {
    serde_json::to_vec(definition)
        .map(|bytes| manifest_hash(&bytes))
        .map_err(|error| invalid(error.to_string()))
}

/// A complete scope: no ambient registry tools leak through this boundary.
///
/// `overrides` is the agent's own `toolbox.tools` map, which may only tighten
/// each grant's ceiling. `*` stands for every grant it does not name, so
/// `{"*": "deny", "git_status": "allow"}` is one line rather than a denial per
/// grant — and "allow" there cannot raise a grant the manifest marked `ask`.
pub fn approved_operation_scope(
    approved: &ApprovedToolbox,
    store: &Arc<PolicyStore>,
    registry: &Arc<ToolRegistry>,
    overrides: &ToolPermissions,
    remote: Option<&Arc<dyn OperationExecutor>>,
) -> Result<Arc<dyn ToolScope>> {
    let scoped = Arc::new(ToolRegistry::new());
    let mut permissions = ToolPermissions::new();
    for grant in &approved.resolved.toolbox.tools {
        let operation = approved
            .resolved
            .operations
            .get(&grant.name)
            .ok_or_else(|| invalid("Unresolved operation"))?
            .clone();
        let requested = overrides
            .get(&grant.name)
            .or_else(|| overrides.get(ghostai_protocol::TOOLBOX_DEFAULT_KEY));
        let permission = requested.map_or(grant.permission, |requested| {
            narrow_permission(grant.permission, *requested)
        });
        permissions.insert(grant.name.clone(), permission);
        let risk = match &operation.implementation {
            OperationImplementation::Transcript => ToolRisk::Safe,
            OperationImplementation::Command { .. } => ToolRisk::Exec,
            OperationImplementation::Registered { tool, digest } => {
                let definition = registry
                    .definitions()
                    .iter()
                    .find(|definition| &definition.name == tool)
                    .cloned()
                    .ok_or_else(|| invalid(format!("Tool {tool} is not installed")))?;
                if definition_digest(&definition)? != *digest {
                    return Err(invalid(format!(
                        "Tool {tool} identity does not match the approved definition"
                    )));
                }
                definition.risk
            }
        };
        let tool: AnyTool = Arc::new(ApprovedOperation {
            definition: ToolDefinition {
                name: grant.name.clone(),
                description: operation.description.clone(),
                parameters: operation
                    .parameters
                    .as_object()
                    .cloned()
                    .ok_or_else(|| invalid("Invalid schema"))?
                    .into_iter()
                    .collect(),
                risk,
                source: ToolSource::Builtin,
                annotations: None,
            },
            operation,
            toolbox: approved.resolved.toolbox.name.clone(),
            hash: approved.sha256().to_owned(),
            store: Arc::clone(store),
            registry: Arc::clone(registry),
            remote: remote.map(Arc::clone),
        });
        scoped.register_all(vec![tool], ToolSource::Builtin)?;
    }
    Ok(scoped.select(permissions))
}
