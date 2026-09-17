//! A whole turn, assembled from doubles.
//!
//! Everything a loop needs is either a testkit double — a scripted provider, a
//! clock reading tokio's timer, a fixed nonce source — or a real collaborator
//! on a temporary workspace: a real SQLite store, a real jail, a real tool
//! registry. That mix is deliberate. The invariants worth testing here are
//! about what reaches storage and what the model is sent, and a mocked store
//! would let a test pass while the append it was asserting never happened.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use darkwire_agent::approval::{ApprovalDecision, ApprovalGate, ApprovalRequest};
use darkwire_agent::prompt::{ContextContributor, Host, Platform};
use darkwire_agent::testkit::{
    CountingIds, FixedRandom, ScriptedProvider, ScriptedTurn, TokioClock,
};
use darkwire_agent::{
    AgentEvent, AgentLoop, AgentLoopOptions, LoopAgent, LoopResolver, SteeringQueue,
    SubagentBinding, TurnInput, TurnResult,
};
use darkwire_core::{Database, Result, SessionStore};
use darkwire_protocol::json::Object;
use darkwire_protocol::{
    AgentEnvironment, AgentSettings, ToolDefinition, ToolPermission, ToolPermissions, ToolRisk,
};
use darkwire_providers::BoxFuture;
use darkwire_security::{JailOptions, JailResolver, WorkspaceJail, single_jail};
use darkwire_tools::{
    AnyTool, BoxFuture as ToolFuture, EnvironmentResolver, Placed, PlacementRequest, Tool,
    ToolContext, ToolExecution, ToolInvocation, ToolRegistry, ToolScope,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// An environment resolver that records what it was asked and answers the host.
///
/// Every turn resolves a placement, so this is how a test sees the environment
/// a turn settled on without a container runtime anywhere near it. What it
/// answers is beside the point; what it was *asked* is the assertion.
#[derive(Debug, Default)]
pub struct RecordingEnvironments {
    asked: Mutex<Vec<PlacementRequest>>,
}

impl RecordingEnvironments {
    pub fn new() -> Arc<RecordingEnvironments> {
        Arc::new(RecordingEnvironments::default())
    }

    /// The environment name each turn resolved with, in order.
    pub fn names(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.environment.clone())
            .collect()
    }

    /// The agent id each turn resolved under, in order.
    pub fn agents(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.agent_id.clone())
            .collect()
    }
}

impl EnvironmentResolver for RecordingEnvironments {
    fn for_turn(&self, request: &PlacementRequest) -> Placed {
        self.asked.lock().unwrap().push(request.clone());
        Placed::host()
    }
}

/// A tool whose behaviour is a closure, so a test states what a call does.
pub struct FakeTool {
    definition: ToolDefinition,
    risk: ToolRisk,
    behaviour: Behaviour,
    calls: Arc<Mutex<Vec<Value>>>,
}

/// What a fake tool does when called.
pub enum Behaviour {
    /// Answers immediately.
    Answer(String),
    /// Fails immediately.
    Fail(String),
    /// Sleeps on tokio's timer, then answers. The seam for a heartbeat test.
    Slow(u64, String),
    /// Never answers until the turn's token fires.
    Hang,
}

impl FakeTool {
    /// A tool called `name` at `risk` that does `behaviour`.
    pub fn new(name: &str, risk: ToolRisk, behaviour: Behaviour) -> Arc<FakeTool> {
        Arc::new(FakeTool {
            definition: ToolDefinition {
                name: name.to_owned(),
                description: format!("The {name} tool."),
                parameters: object_schema(),
                risk,
                source: darkwire_protocol::ToolSource::Builtin,
                annotations: None,
            },
            risk,
            behaviour,
            calls: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// A read-only tool answering `text`.
    pub fn reading(name: &str, text: &str) -> Arc<FakeTool> {
        FakeTool::new(name, ToolRisk::Safe, Behaviour::Answer(text.to_owned()))
    }

    /// A writing tool answering `text`.
    pub fn writing(name: &str, text: &str) -> Arc<FakeTool> {
        FakeTool::new(name, ToolRisk::Write, Behaviour::Answer(text.to_owned()))
    }

    /// The arguments every call arrived with, in order.
    pub fn calls(&self) -> Vec<Value> {
        self.calls.lock().unwrap().clone()
    }
}

fn object_schema() -> Object {
    let mut schema = Object::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), json!({}));
    schema.insert("additionalProperties".to_owned(), json!(true));
    schema
}

impl Tool for FakeTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn risk(&self) -> ToolRisk {
        self.risk
    }

    fn execute<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> ToolFuture<'a, ToolExecution> {
        self.calls.lock().unwrap().push(args);
        Box::pin(async move {
            match &self.behaviour {
                Behaviour::Answer(text) => ToolExecution::ok(text.clone()),
                Behaviour::Fail(text) => {
                    ToolExecution::error(darkwire_core::ErrorKind::Tool, text.clone())
                }
                Behaviour::Slow(ms, text) => {
                    tokio::time::sleep(std::time::Duration::from_millis(*ms)).await;
                    ToolExecution::ok(text.clone())
                }
                Behaviour::Hang => {
                    ctx.token.cancelled().await;
                    ToolExecution::error(darkwire_core::ErrorKind::Aborted, "Tool aborted")
                }
            }
        })
    }
}

/// A gate whose answer is a value.
pub struct ScriptedGate {
    answers: Mutex<Vec<Answer>>,
    seen: Mutex<Vec<ApprovalRequest>>,
    /// What the gate says it already knows, before anyone is asked.
    remembered: Mutex<Option<ApprovalDecision>>,
}

/// What a gate does when asked.
#[derive(Debug, Clone)]
pub enum Answer {
    /// Approves.
    Allow,
    /// Refuses.
    Refuse,
    /// Fails, which a gate must never be allowed to read as "go ahead".
    Fail,
    /// Never answers, so the loop's own deadline decides.
    Silent,
}

impl ScriptedGate {
    /// A gate answering each request from `answers`, repeating the last.
    pub fn new(answers: Vec<Answer>) -> Arc<ScriptedGate> {
        Arc::new(ScriptedGate {
            answers: Mutex::new(answers),
            seen: Mutex::new(Vec::new()),
            remembered: Mutex::new(None),
        })
    }

    /// A gate that already holds an answer, the way one does after "this
    /// session".
    pub fn remembering(decision: ApprovalDecision) -> Arc<ScriptedGate> {
        let gate = ScriptedGate::new(vec![Answer::Silent]);
        *gate.remembered.lock().unwrap() = Some(decision);
        gate
    }

    /// Every request the gate was shown, in order.
    pub fn seen(&self) -> Vec<ApprovalRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl ApprovalGate for ScriptedGate {
    fn ask<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<ApprovalDecision>> {
        let answer = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(request.clone());
            let answers = self.answers.lock().unwrap();
            answers
                .get(seen.len() - 1)
                .or_else(|| answers.last())
                .cloned()
                .unwrap_or(Answer::Allow)
        };
        Box::pin(async move {
            match answer {
                Answer::Allow => Ok(ApprovalDecision::allow()),
                Answer::Refuse => Ok(ApprovalDecision::refuse()),
                Answer::Fail => Err(darkwire_core::WireError::new(
                    darkwire_core::ErrorKind::Internal,
                    "the gate fell over",
                )),
                Answer::Silent => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            }
        })
    }

    fn remembered(&self, _request: &ApprovalRequest) -> Option<ApprovalDecision> {
        self.remembered.lock().unwrap().clone()
    }
}

/// Resolves a subagent's loop from a map built after the parent.
#[derive(Default)]
pub struct MapResolver {
    loops: Mutex<HashMap<String, AgentLoop>>,
}

impl MapResolver {
    /// An empty resolver, to be filled once the child loops exist.
    pub fn new() -> Arc<MapResolver> {
        Arc::new(MapResolver::default())
    }

    /// Binds `agent_id` to `child`.
    pub fn insert(&self, agent_id: &str, child: AgentLoop) {
        self.loops
            .lock()
            .unwrap()
            .insert(agent_id.to_owned(), child);
    }
}

impl LoopResolver for MapResolver {
    fn loop_for(&self, agent_id: &str) -> Option<AgentLoop> {
        self.loops.lock().unwrap().get(agent_id).cloned()
    }
}

/// A whole turn's worth of collaborators, held so a test can assert on them.
pub struct Harness {
    /// The workspace every path resolves inside.
    pub workspace: TempDir,
    /// The conversation store, real SQLite in memory.
    pub store: Arc<SessionStore>,
    /// The model.
    pub provider: Arc<ScriptedProvider>,
    /// The registry the loop's scope wraps.
    pub registry: Arc<ToolRegistry>,
    /// Wall-clock and monotonic time, both from tokio's timer.
    pub clock: Arc<TokioClock>,
    /// Turn and session ids.
    pub ids: Arc<CountingIds>,
    /// The queue a steering test pushes into.
    pub steering: Arc<SteeringQueue>,
    /// The jail every tool is handed.
    pub jail: Arc<WorkspaceJail>,
    /// The loop under test.
    pub agent_loop: AgentLoop,
}

/// How a harness differs from the default.
pub struct Setup {
    /// What the model does, request by request.
    pub turns: Vec<ScriptedTurn>,
    /// The tools the registry holds.
    pub tools: Vec<AnyTool>,
    /// What the agent may do with each. Absent means every tool is allowed.
    pub permissions: Option<ToolPermissions>,
    /// The agent's settings.
    pub config: AgentSettings,
    /// Who to ask before an `ask` tool runs.
    pub approvals: Option<Arc<dyn ApprovalGate>>,
    /// The agents this one may delegate to.
    pub subagents: Vec<SubagentBinding>,
    /// Where a delegation's loop comes from.
    pub resolve_loop: Option<Arc<dyn LoopResolver>>,
    /// Which agent this loop is.
    pub agent: Option<LoopAgent>,
    /// Sections the loop knows nothing about.
    pub contributors: Vec<Arc<dyn ContextContributor>>,
    /// `0` disables the heartbeat.
    pub heartbeat_ms: u64,
    /// Head+tail budget for a tool result entering history.
    pub max_tool_result_chars: usize,
    /// Where this agent's own configuration says its commands run.
    pub environment: AgentEnvironment,
    /// Answers the placement of every turn, and records what it was asked.
    pub environments: Option<Arc<dyn EnvironmentResolver>>,
}

impl Default for Setup {
    fn default() -> Setup {
        Setup {
            turns: vec![ScriptedTurn::text("done")],
            tools: Vec::new(),
            permissions: None,
            config: AgentSettings {
                model: "test-model".to_owned(),
                ..AgentSettings::default()
            },
            approvals: None,
            subagents: Vec::new(),
            environment: AgentEnvironment::default(),
            environments: None,
            resolve_loop: None,
            agent: None,
            contributors: Vec::new(),
            heartbeat_ms: 0,
            max_tool_result_chars: 8_000,
        }
    }
}

impl Setup {
    /// A setup whose model answers with `turns`.
    pub fn with(turns: Vec<ScriptedTurn>) -> Setup {
        Setup {
            turns,
            ..Setup::default()
        }
    }
}

impl Harness {
    /// Builds everything a turn needs. Panics on a setup a turn could not run.
    pub fn build(setup: Setup) -> Harness {
        let workspace = TempDir::new().expect("a temp workspace");
        let jail =
            Arc::new(WorkspaceJail::new(JailOptions::new(workspace.path())).expect("a jail on it"));
        let clock = TokioClock::new();
        let ids = CountingIds::new("id");
        let store = Arc::new(
            SessionStore::new(
                Database::in_memory().expect("an in-memory database"),
                clock.clone(),
                {
                    let ids = Arc::clone(&ids);
                    Box::new(move || ids.next_id())
                },
            )
            .expect("a session store"),
        );

        let registry = Arc::new(ToolRegistry::with_options(
            darkwire_tools::ToolRegistryOptions {
                clock: Some(clock.clone()),
                ..darkwire_tools::ToolRegistryOptions::default()
            },
        ));
        for tool in &setup.tools {
            registry
                .register(Arc::clone(tool), darkwire_protocol::ToolSource::Builtin)
                .expect("a unique tool name");
        }

        let permissions = setup.permissions.unwrap_or_else(|| {
            setup
                .tools
                .iter()
                .map(|tool| (tool.definition().name.clone(), ToolPermission::Allow))
                .collect()
        });
        let scope: Arc<dyn ToolScope> = registry.select(permissions);

        let provider = ScriptedProvider::new(setup.turns);
        let steering = Arc::new(SteeringQueue::new());
        let subagents = darkwire_agent::subagent_map(setup.subagents).expect("unique tool names");

        let agent_loop = AgentLoop::new(AgentLoopOptions {
            config: setup.config,
            environment: setup.environment,
            environments: setup.environments,
            approvals: setup.approvals,
            subagents,
            resolve_loop: setup.resolve_loop,
            agent: setup.agent,
            contributors: setup.contributors,
            steering: Arc::clone(&steering),
            clock: clock.clone(),
            random: Arc::new(FixedRandom::default()),
            new_id: {
                let ids = Arc::clone(&ids);
                Arc::new(move || ids.next_id())
            },
            host: Host {
                platform: Platform::Linux,
                runtime_label: "Linux x64, DarkWire test".to_owned(),
            },
            time_zone: Some(Arc::new(|| "UTC".to_owned())),
            tool_heartbeat_ms: setup.heartbeat_ms,
            max_tool_result_chars: setup.max_tool_result_chars,
            ..AgentLoopOptions::new(
                provider.clone(),
                scope,
                Arc::clone(&store),
                Arc::new(single_jail(Arc::clone(&jail))) as Arc<dyn JailResolver>,
            )
        })
        .expect("a loop with a model");

        Harness {
            workspace,
            store,
            provider,
            registry,
            clock,
            ids,
            steering,
            jail,
            agent_loop,
        }
    }

    /// The simplest possible harness: one scripted answer, no tools.
    pub fn simple() -> Harness {
        Harness::build(Setup::default())
    }

    /// Runs one turn to completion and returns everything it emitted.
    pub async fn run(&self, input: TurnInput) -> (Vec<AgentEvent>, Result<TurnResult>) {
        self.agent_loop
            .run(input, &CancellationToken::new())
            .collect()
            .await
    }

    /// Runs a plain turn on `session_key` saying `content`.
    pub async fn say(
        &self,
        session_key: &str,
        content: &str,
    ) -> (Vec<AgentEvent>, Result<TurnResult>) {
        self.run(TurnInput::new(session_key, content)).await
    }

    /// Every message stored for a session, in order.
    pub fn stored(&self, session_key: &str) -> Vec<darkwire_protocol::ChatMessage> {
        self.store
            .messages(
                session_key,
                &darkwire_core::session_store::ReadMessages::default(),
            )
            .expect("stored messages")
            .into_iter()
            .map(|record| record.message)
            .collect()
    }
}

/// The invocation a dispatcher would build for a call.
pub fn invocation(name: &str, args: &Value) -> ToolInvocation {
    ToolInvocation::with_json(name, args.to_string())
}

/// Every event of one type, as JSON, so a test asserts on a field rather than
/// on a variant.
pub fn events_of(events: &[AgentEvent], tag: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|event| event.tag() == tag)
        .map(|event| serde_json::to_value(event).expect("an event serialises"))
        .collect()
}

/// The text of every assistant delta, concatenated.
pub fn answer(events: &[AgentEvent]) -> String {
    events_of(events, "assistant.delta")
        .iter()
        .filter_map(|event| event.get("text").and_then(Value::as_str))
        .collect()
}
