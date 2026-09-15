//! The tool registry: what the model is offered, and the one path by which a
//! tool is called.
//!
//! Instantiated per install and shared by every agent, and everything
//! registered carries the source that registered it. That tag is what makes
//! `unregister_by_source(Extension)` exact: uninstalling an extension removes
//! its tools and nothing else.
//!
//! Three properties matter more than they look:
//!
//!  - **`definitions()` is memoised and sorted by name.** Tool definitions sit
//!    in the prompt prefix that providers cache. An MCP server reconnecting and
//!    re-registering its tools in a different order would rewrite that prefix
//!    and throw the cache away for no semantic change, so the order is the name
//!    order rather than the insertion order. The memo is invalidated on every
//!    mutation, which is the only way it can be wrong.
//!
//!  - **`execute` never fails.** A failed tool call is a legal history entry —
//!    the model needs to see the error to recover from it — so every failure
//!    comes back as a [`ToolExecution`] with `is_error` set and a `kind` from
//!    the core taxonomy. A caller that has to handle a `Result` per call
//!    eventually forgets to, and the turn dies instead of the call.
//!
//!  - **Truncate first, fence second.** Every result is cut to
//!    `config.max_output_chars` and then, when the context carries this turn's
//!    nonce, wrapped in the tool-output delimiter. The other order cuts the
//!    closing delimiter off the envelope, and a tool result the model cannot
//!    see the end of is one it reads as continuing into the conversation.

use std::sync::Arc;
use std::time::Duration;

use ghostai_core::history::truncate_head_tail;
use ghostai_core::{Clock, ErrorKind, GhostError, Result, SystemClock};
use ghostai_protocol::{ToolDefinition, ToolPermission, ToolPermissions, ToolSource};
use ghostai_security::{WrapToolOutputOptions, wrap_tool_output};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Map, Value};

use crate::scope::{is_enabled, permission_for};
use crate::tool::{AnyTool, BoxFuture, ToolContext, ToolExecution};

/// How a registry is built.
#[derive(Clone, Default)]
pub struct ToolRegistryOptions {
    /// Wall-clock cap on a single tool call. `0` disables it.
    ///
    /// Comes from `agent.toolTimeoutMs`. Measured on tokio's timer, which tests
    /// pause instead of waiting.
    pub timeout_ms: u64,
    /// Measures each call. Defaults to the host clock.
    pub clock: Option<Arc<dyn Clock>>,
}

/// A call as the provider adapters produce it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolInvocation {
    /// The tool's name.
    pub name: String,
    /// Raw JSON as the model emitted it. Empty or absent means no arguments.
    pub arguments_json: Option<String>,
}

impl ToolInvocation {
    /// A call with no arguments.
    pub fn named(name: impl Into<String>) -> ToolInvocation {
        ToolInvocation {
            name: name.into(),
            arguments_json: None,
        }
    }

    /// A call with raw JSON arguments.
    pub fn with_json(name: impl Into<String>, arguments_json: impl Into<String>) -> ToolInvocation {
        ToolInvocation {
            name: name.into(),
            arguments_json: Some(arguments_json.into()),
        }
    }
}

/// What the loop needs from a tool collection: what to advertise, what a name
/// means, and how to call it.
///
/// [`ToolRegistry`] implements it, and so does every restricted view of one.
/// The loop takes this rather than the registry so that an agent with a tool
/// subset is not a special case anywhere in the turn.
pub trait ToolScope: Send + Sync {
    /// The definitions to send to the provider. Sorted, memoised.
    fn definitions(&self) -> Arc<[ToolDefinition]>;
    /// `None` for a name this scope cannot see, whoever else registered it.
    fn get(&self, name: &str) -> Option<AnyTool>;
    /// What this scope permits for `name` — the whole of the gate's input.
    ///
    /// On the scope rather than looked up from config by the caller, because
    /// the scope is the only thing that knows where a name came from: a toolbox
    /// program and a built-in of the same name resolve to different tools, and
    /// a caller reading one map would answer for the wrong one.
    fn permission_for(&self, name: &str) -> ToolPermission;
    /// Validates, runs, bounds and reports one call. Never fails.
    fn execute<'a>(
        &'a self,
        call: &'a ToolInvocation,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, ToolExecution>;
}

/// Identifies a listener for [`ToolRegistry::unsubscribe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(u64);

type Listener = Arc<dyn Fn() + Send + Sync>;

struct Registration {
    tool: AnyTool,
    source: ToolSource,
}

struct State {
    tools: IndexMap<String, Registration>,
    timeout_ms: u64,
    cached: Option<Arc<[ToolDefinition]>>,
    revision: u64,
    listeners: Vec<(ListenerId, Listener)>,
    next_listener: u64,
}

/// The source-tagged collection of tools. See the module docs.
pub struct ToolRegistry {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        f.debug_struct("ToolRegistry")
            .field("tools", &state.tools.keys().collect::<Vec<_>>())
            .field("timeout_ms", &state.timeout_ms)
            .field("revision", &state.revision)
            .finish_non_exhaustive()
    }
}

impl Default for ToolRegistry {
    fn default() -> ToolRegistry {
        ToolRegistry::new()
    }
}

impl ToolRegistry {
    /// An empty registry with no timeout, on the host clock.
    pub fn new() -> ToolRegistry {
        ToolRegistry::with_options(ToolRegistryOptions::default())
    }

    /// An empty registry.
    pub fn with_options(options: ToolRegistryOptions) -> ToolRegistry {
        ToolRegistry {
            state: Mutex::new(State {
                tools: IndexMap::new(),
                timeout_ms: options.timeout_ms,
                cached: None,
                revision: 0,
                listeners: Vec::new(),
                next_listener: 1,
            }),
            clock: options.clock.unwrap_or_else(|| Arc::new(SystemClock)),
        }
    }

    /// How many tools are registered.
    pub fn size(&self) -> usize {
        self.state.lock().tools.len()
    }

    /// Bumped on every mutation. A scope memoising a filtered definition list
    /// keys on it, so a tool registered after the scope was built still shows
    /// up.
    pub fn revision(&self) -> u64 {
        self.state.lock().revision
    }

    /// Watches for a change to what is registered.
    ///
    /// The seam a transport reaches an MCP reconnection or an extension load
    /// through, without either of them knowing a transport exists. A server
    /// connecting calls `register`; `invalidate` is the one funnel every
    /// mutation already passes through; the WebSocket's `tools.changed` frame
    /// falls out.
    ///
    /// **A listener is told that something changed, not what.** The
    /// definitions are one memoised call away and a diff nobody asked for would
    /// be a second thing to keep correct. Callers should coalesce: registering
    /// a server's forty tools is forty mutations and should be one frame.
    pub fn subscribe(&self, listener: impl Fn() + Send + Sync + 'static) -> ListenerId {
        let mut state = self.state.lock();
        let id = ListenerId(state.next_listener);
        state.next_listener += 1;
        state.listeners.push((id, Arc::new(listener)));
        id
    }

    /// Stops a listener. Returns whether it was subscribed.
    pub fn unsubscribe(&self, id: ListenerId) -> bool {
        let mut state = self.state.lock();
        let before = state.listeners.len();
        state.listeners.retain(|(listener, _)| *listener != id);
        state.listeners.len() != before
    }

    /// Every mutation goes through here, so no path can bump one and not the
    /// other. The listeners are called with the lock released, so one may read
    /// the registry back.
    fn invalidate(state: &mut State) -> Vec<Listener> {
        state.cached = None;
        state.revision += 1;
        state
            .listeners
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect()
    }

    fn notify(listeners: Vec<Listener>) {
        for listener in listeners {
            listener();
        }
    }

    /// The per-call wall-clock cap. `0` is none.
    pub fn timeout_ms(&self) -> u64 {
        self.state.lock().timeout_ms
    }

    /// The one mutable setting on a registry.
    ///
    /// An agent's `toolTimeoutMs` is editable in the settings panel, and the
    /// alternative — building a new registry when it changes — would throw away
    /// every MCP and extension registration on it, which is far more than the
    /// operator asked to change. A call already in flight keeps the timeout it
    /// started under; the timer is armed at entry and never re-read.
    pub fn set_timeout_ms(&self, ms: u64) {
        self.state.lock().timeout_ms = ms;
    }

    /// Registers a tool under its own name.
    ///
    /// A duplicate is a `conflict` rather than a silent overwrite: two sources
    /// claiming one name means the model's calls would go to whichever
    /// registered last, and which one that is depends on extension load order.
    /// MCP tools are flattened to `mcp_{server}_{tool}` upstream of here for
    /// the same reason.
    pub fn register(&self, tool: AnyTool, source: ToolSource) -> Result<()> {
        let listeners = {
            let mut state = self.state.lock();
            Self::insert(&mut state, tool, source)?;
            Self::invalidate(&mut state)
        };
        Self::notify(listeners);
        Ok(())
    }

    fn insert(state: &mut State, tool: AnyTool, source: ToolSource) -> Result<()> {
        let name = tool.definition().name.clone();
        if let Some(existing) = state.tools.get(&name) {
            let existing_source = serde_json::to_value(existing.source)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            return Err(GhostError::new(
                ErrorKind::Conflict,
                format!("Tool {name} is already registered by {existing_source}"),
            )
            .with_detail("tool", name.as_str())
            .with_detail(
                "source",
                serde_json::to_value(source).unwrap_or(Value::Null),
            )
            .with_detail("existingSource", existing_source));
        }
        state.tools.insert(name, Registration { tool, source });
        Ok(())
    }

    /// Registers every tool from one source, rolling back if any name collides.
    ///
    /// A half-installed extension is worse than one that failed to install.
    pub fn register_all(
        &self,
        tools: impl IntoIterator<Item = AnyTool>,
        source: ToolSource,
    ) -> Result<()> {
        let listeners = {
            let mut state = self.state.lock();
            let mut added: Vec<String> = Vec::new();
            for tool in tools {
                let name = tool.definition().name.clone();
                if let Err(error) = Self::insert(&mut state, tool, source) {
                    for name in &added {
                        state.tools.shift_remove(name);
                    }
                    return Err(error);
                }
                added.push(name);
            }
            Self::invalidate(&mut state)
        };
        Self::notify(listeners);
        Ok(())
    }

    /// Removes one tool. Returns whether it was there.
    pub fn unregister(&self, name: &str) -> bool {
        let listeners = {
            let mut state = self.state.lock();
            if state.tools.shift_remove(name).is_none() {
                return false;
            }
            Self::invalidate(&mut state)
        };
        Self::notify(listeners);
        true
    }

    /// Removes everything one source registered. Returns how many went.
    ///
    /// This is the whole of extension teardown for tools, and it is exact by
    /// construction — no name matching, no restart.
    pub fn unregister_by_source(&self, source: ToolSource) -> usize {
        let (removed, listeners) = {
            let mut state = self.state.lock();
            let before = state.tools.len();
            state
                .tools
                .retain(|_, registration| registration.source != source);
            let removed = before - state.tools.len();
            if removed == 0 {
                return 0;
            }
            (removed, Self::invalidate(&mut state))
        };
        Self::notify(listeners);
        removed
    }

    /// Removes everything.
    pub fn clear(&self) {
        let listeners = {
            let mut state = self.state.lock();
            if state.tools.is_empty() {
                return;
            }
            state.tools.clear();
            Self::invalidate(&mut state)
        };
        Self::notify(listeners);
    }

    /// Whether a tool of that name is registered.
    pub fn has(&self, name: &str) -> bool {
        self.state.lock().tools.contains_key(name)
    }

    /// The tool of that name.
    pub fn get(&self, name: &str) -> Option<AnyTool> {
        self.state
            .lock()
            .tools
            .get(name)
            .map(|registration| Arc::clone(&registration.tool))
    }

    /// Who registered `name`.
    pub fn source_of(&self, name: &str) -> Option<ToolSource> {
        self.state
            .lock()
            .tools
            .get(name)
            .map(|registration| registration.source)
    }

    /// Every registered name, sorted.
    pub fn names(&self) -> Vec<String> {
        let state = self.state.lock();
        Self::sorted_names(&state, None)
    }

    fn sorted_names(state: &State, perms: Option<&ToolPermissions>) -> Vec<String> {
        let mut names: Vec<String> = state
            .tools
            .keys()
            .filter(|name| is_enabled(perms, name))
            .cloned()
            .collect();
        names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        names
    }

    /// A view of this registry restricted to what one agent may call.
    ///
    /// Always a wrapper. There is no unrestricted agent under the permission
    /// model — an empty map is an agent with no tools, not an agent with all of
    /// them — so there is nothing to fast-path.
    pub fn select(self: &Arc<ToolRegistry>, permissions: ToolPermissions) -> Arc<dyn ToolScope> {
        Arc::new(RegistryScope {
            registry: Arc::clone(self),
            permissions,
            cached: Mutex::new(None),
        })
    }

    /// The definitions to send to the provider. Sorted, memoised.
    pub fn definitions(&self) -> Arc<[ToolDefinition]> {
        let mut state = self.state.lock();
        if let Some(cached) = &state.cached {
            return Arc::clone(cached);
        }
        // Code-unit order, not a locale collation: the sort feeds a cached
        // prompt prefix, and a locale-dependent order would make that prefix
        // differ between the developer's machine and the container it ships in.
        let mut definitions: Vec<ToolDefinition> = state
            .tools
            .values()
            .map(|registration| {
                let mut definition = registration.tool.definition().clone();
                definition.source = registration.source;
                definition
            })
            .collect();
        definitions.sort_by(|a, b| a.name.encode_utf16().cmp(b.name.encode_utf16()));
        let cached: Arc<[ToolDefinition]> = Arc::from(definitions);
        state.cached = Some(Arc::clone(&cached));
        cached
    }

    /// Validates, runs, bounds and reports one call. Never fails.
    ///
    /// `permissions` is the calling scope's map; `None` is the bare registry.
    /// The timeout is raced against the handler rather than merely signalled
    /// to it, because a handler that ignores its token would otherwise hang the
    /// turn forever. Dropping the future is what unwinds the work in flight —
    /// which is why every built-in also takes the token and `exec` hands it to
    /// the child process, so the process goes away too.
    pub async fn execute_scoped(
        &self,
        call: &ToolInvocation,
        ctx: &ToolContext,
        permissions: Option<&ToolPermissions>,
    ) -> ToolExecution {
        let started = self.clock.monotonic();
        let (tool, timeout_ms) = {
            let state = self.state.lock();
            let tool = state
                .tools
                .get(&call.name)
                .filter(|_| is_enabled(permissions, &call.name))
                .map(|registration| Arc::clone(&registration.tool));
            let Some(tool) = tool else {
                // A tool the scope hides is indistinguishable from one that
                // does not exist, deliberately: the model was never offered
                // it, and an error that admitted it exists but is off-limits
                // would invite the model to argue about it. The available
                // list is the scope's, so the suggestion is actionable.
                let available = Self::sorted_names(&state, permissions).join(", ");
                drop(state);
                return self.finish(
                    call,
                    ToolExecution::error(
                        ErrorKind::NotFound,
                        format!("No tool named {}. Available: {available}", call.name),
                    ),
                    started,
                    ctx,
                );
            };
            (tool, state.timeout_ms)
        };

        let raw = match parse_arguments(call.arguments_json.as_deref()) {
            Ok(raw) => raw,
            Err(error) => return self.finish(call, error.into(), started, ctx),
        };

        // Checked before the handler is entered rather than left to the race
        // below. A turn cancelled while a `write_file` call was queued must not
        // perform the write and only then notice, and a handler cannot be
        // trusted to check first when the registry can guarantee it.
        if ctx.token.is_cancelled() {
            return self.finish(
                call,
                ToolExecution::error(ErrorKind::Aborted, format!("Tool {} aborted", call.name)),
                started,
                ctx,
            );
        }

        let child = ctx.token.child_token();
        let scoped = ctx.clone().with_token(child.clone());
        let running = tool.execute(raw, &scoped);
        let deadline = async {
            if timeout_ms == 0 {
                std::future::pending::<()>().await;
            } else {
                tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
            }
        };

        let execution = tokio::select! {
            execution = running => {
                // A handler that watches the token and reports its own failure
                // races the cancellation branch below: when the token fires,
                // both are ready in the same poll and `select!` picks either.
                // The abort is the truer classification of that failure, so it
                // is applied here rather than left to chance. A handler that
                // *succeeded* keeps its result — work already done is not worth
                // discarding because a stop landed in the same instant.
                if execution.is_error && ctx.token.is_cancelled() {
                    ToolExecution::error(ErrorKind::Aborted, format!("Tool {} aborted", call.name))
                } else {
                    execution
                }
            }
            () = ctx.token.cancelled() => {
                ToolExecution::error(ErrorKind::Aborted, format!("Tool {} aborted", call.name))
            }
            () = deadline => {
                child.cancel();
                ToolExecution::error(
                    ErrorKind::Timeout,
                    format!("Tool {} timed out after {timeout_ms} ms", call.name),
                )
            }
        };
        self.finish(call, execution, started, ctx)
    }

    /// Stamps, measures, truncates and fences one outcome.
    fn finish(
        &self,
        call: &ToolInvocation,
        mut execution: ToolExecution,
        started: Duration,
        ctx: &ToolContext,
    ) -> ToolExecution {
        let elapsed = self.clock.monotonic().saturating_sub(started);
        execution.name.clone_from(&call.name);
        execution.duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);

        let budget = usize::try_from(ctx.config.max_output_chars).unwrap_or(usize::MAX);
        let capped = truncate_head_tail(&execution.content, budget);
        execution.truncated = capped.truncated;
        execution.content = capped.text;

        if let Some(nonce) = &ctx.nonce {
            match wrap_tool_output(
                &execution.content,
                &WrapToolOutputOptions::new(&call.name, nonce),
            ) {
                Ok(wrapped) => execution.envelope = Some(wrapped),
                Err(error) => {
                    // A nonce this turn cannot fence with is a defect upstream,
                    // not something the model can act on; the call still
                    // reports rather than the turn dying.
                    execution = ToolExecution::error(ErrorKind::Internal, error.message)
                        .with_details(error.details);
                    execution.name.clone_from(&call.name);
                    execution.duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
                }
            }
        }

        if execution.is_aborted() {
            // An abort is the user pressing Stop. Logged at debug, not warn, or
            // every cancelled turn fills the log with things nobody needs to
            // act on.
            tracing::debug!(tool = %call.name, duration_ms = execution.duration_ms, "tool aborted");
        } else if let Some(kind) = execution.kind {
            tracing::warn!(
                tool = %call.name,
                duration_ms = execution.duration_ms,
                kind = %kind,
                "tool failed"
            );
        } else {
            tracing::debug!(
                tool = %call.name,
                duration_ms = execution.duration_ms,
                truncated = execution.truncated,
                is_error = execution.is_error,
                "tool executed"
            );
        }
        execution
    }
}

impl ToolScope for ToolRegistry {
    fn definitions(&self) -> Arc<[ToolDefinition]> {
        ToolRegistry::definitions(self)
    }

    fn get(&self, name: &str) -> Option<AnyTool> {
        ToolRegistry::get(self, name)
    }

    /// `allow`, always: an unscoped registry is the CLI's and the tests' view,
    /// and it is not reachable from a turn.
    fn permission_for(&self, _: &str) -> ToolPermission {
        ToolPermission::Allow
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolInvocation,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, ToolExecution> {
        Box::pin(self.execute_scoped(call, ctx, None))
    }
}

/// The model's `arguments_json` as a value.
///
/// An empty or whitespace-only string is what providers emit for a no-argument
/// call, and it is not malformed JSON — it is the absence of arguments, which
/// the schema's own `required` list is the right thing to judge.
fn parse_arguments(arguments_json: Option<&str>) -> Result<Value> {
    let Some(text) = arguments_json.filter(|text| !text.trim().is_empty()) else {
        return Ok(Value::Object(Map::new()));
    };
    serde_json::from_str(text).map_err(|error| {
        GhostError::new(
            ErrorKind::InvalidInput,
            format!("Tool arguments are not valid JSON: {text}"),
        )
        .with_source(error)
    })
}

/// A restricted view of one registry.
///
/// Holds the registry rather than a snapshot of it: an extension registering a
/// tool after boot has to become visible to every agent whose permissions
/// admit it, and a view built once at agent-resolution time would never see
/// it. The memo is keyed on the registry's revision for exactly that reason.
struct RegistryScope {
    registry: Arc<ToolRegistry>,
    permissions: ToolPermissions,
    cached: Mutex<Option<(u64, Arc<[ToolDefinition]>)>>,
}

impl ToolScope for RegistryScope {
    fn definitions(&self) -> Arc<[ToolDefinition]> {
        let revision = self.registry.revision();
        let mut cached = self.cached.lock();
        if let Some((at, definitions)) = &*cached
            && *at == revision
        {
            return Arc::clone(definitions);
        }
        let filtered: Vec<ToolDefinition> = self
            .registry
            .definitions()
            .iter()
            .filter(|definition| is_enabled(Some(&self.permissions), &definition.name))
            .cloned()
            .collect();
        let definitions: Arc<[ToolDefinition]> = Arc::from(filtered);
        *cached = Some((revision, Arc::clone(&definitions)));
        definitions
    }

    fn get(&self, name: &str) -> Option<AnyTool> {
        if !is_enabled(Some(&self.permissions), name) {
            return None;
        }
        self.registry.get(name)
    }

    fn permission_for(&self, name: &str) -> ToolPermission {
        permission_for(Some(&self.permissions), name)
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolInvocation,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, ToolExecution> {
        Box::pin(
            self.registry
                .execute_scoped(call, ctx, Some(&self.permissions)),
        )
    }
}
