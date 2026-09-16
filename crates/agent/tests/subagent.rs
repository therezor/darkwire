//! Delegation: the pure rules, and a real turn on a real loop.
//!
//! The refusals are answered with a tool *result* rather than a failure, for
//! the reason a denial is: a model that is told its delegation was refused can
//! answer without it, and a turn that dies instead leaves the operator with an
//! error where an answer was possible.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot be built is a failing test either way"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::harness::{FakeTool, Harness, MapResolver, RecordingEnvironments, Setup, events_of};
use ghostai_agent::SubagentBinding;
use ghostai_agent::subagent::{
    DelegationRefusal, MAX_SUBAGENT_DEPTH, parse_task, refuse_delegation, refused_execution,
    subagent_definition, subagent_map, subagent_result,
};
use ghostai_agent::testkit::{ScriptedTurn, raw_tool_call, tool_call};
use ghostai_core::ErrorKind;
use ghostai_protocol::{
    AgentEnvironment, AgentSettings, EnvironmentNetwork, NetworkMode, SUBAGENT_METADATA_KEY,
    SUBAGENT_ORIGIN, StopReason, ToolPermission, default_subagent_prompt, subagent_tool_name,
};
use serde_json::json;

fn binding(agent_id: &str) -> SubagentBinding {
    SubagentBinding {
        tool_name: subagent_tool_name(agent_id),
        agent_id: agent_id.to_owned(),
        label: "Researcher".to_owned(),
        prompt: String::new(),
        permission: ToolPermission::Allow,
        inherit_environment: true,
    }
}

// The pure half

#[test]
fn a_subagent_is_advertised_as_one_tool_taking_one_task() {
    let definition = subagent_definition(&binding("researcher"));

    assert_eq!(definition.name, "ask_researcher");
    // Not `exec`, and not a new band: delegating does nothing to the machine,
    // and the subagent's own calls carry their own bands.
    assert_eq!(definition.risk, ghostai_protocol::ToolRisk::Safe);
    assert_eq!(definition.parameters["required"], json!(["task"]));
    assert_eq!(definition.parameters["additionalProperties"], json!(false));
    // The operator wrote nothing, so the fallback names the agent and says the
    // one thing a model cannot infer.
    assert_eq!(
        definition.description,
        default_subagent_prompt("Researcher")
    );
}

#[test]
fn an_operators_sentence_is_used_as_the_description_rather_than_appended() {
    // A tool description is the entire basis on which a model chooses to call
    // something, and a preamble in front of theirs would only dilute it.
    let own = "Use this when you need facts you do not have.";
    let definition = subagent_definition(&SubagentBinding {
        prompt: format!("  {own}  "),
        ..binding("researcher")
    });

    assert_eq!(definition.description, own);
}

#[test]
fn a_cycle_is_caught_however_indirectly_it_was_configured() {
    // A delegates to B, B is configured to delegate to A — which a config-time
    // self-reference check cannot see, because neither entry is wrong alone.
    let chain = vec!["coder".to_owned(), "researcher".to_owned()];
    assert_eq!(
        refuse_delegation(&chain, "coder"),
        Some(DelegationRefusal::Cycle)
    );
    assert_eq!(refuse_delegation(&chain, "summariser"), None);
    assert_eq!(refuse_delegation(&[], "anyone"), None);
}

#[test]
fn delegation_stops_at_the_depth_cap() {
    let deep: Vec<String> = (0..MAX_SUBAGENT_DEPTH)
        .map(|n| format!("agent-{n}"))
        .collect();
    assert_eq!(
        refuse_delegation(&deep, "another"),
        Some(DelegationRefusal::TooDeep)
    );
    assert_eq!(refuse_delegation(&deep[..deep.len() - 1], "another"), None);
}

#[test]
fn each_refusal_tells_the_model_something_it_can_act_on() {
    let chain = vec!["coder".to_owned()];
    let unconfigured = refused_execution(
        DelegationRefusal::Unconfigured,
        &binding("researcher"),
        &chain,
    );
    assert_eq!(unconfigured.kind, Some(ErrorKind::Config));
    assert!(unconfigured.content.contains("no provider or model"));
    assert_eq!(unconfigured.name, "ask_researcher");

    let cycle = refused_execution(DelegationRefusal::Cycle, &binding("coder"), &chain);
    assert_eq!(cycle.kind, Some(ErrorKind::PermissionDenied));
    assert!(cycle.content.contains("coder → coder"), "{}", cycle.content);

    let deep = refused_execution(DelegationRefusal::TooDeep, &binding("researcher"), &chain);
    assert!(deep.content.contains("3 levels deep"));
    // All three are answered with a result rather than a failure.
    assert!(deep.is_error);
    assert!(deep.content.contains("Do not call it again"));
}

#[test]
fn a_task_must_be_a_non_empty_string() {
    assert_eq!(
        parse_task(&json!({"task": "find out"})),
        Some("find out".to_owned())
    );
    assert_eq!(parse_task(&json!({"task": "   "})), None);
    assert_eq!(parse_task(&json!({"task": 7})), None);
    assert_eq!(parse_task(&json!({})), None);
    assert_eq!(parse_task(&json!("a string")), None);
}

#[test]
fn a_finished_run_becomes_the_answer_and_nothing_else() {
    let binding = binding("researcher");

    let answered = subagent_result(&binding, "  It is 42.  ", StopReason::Complete, 120);
    assert_eq!(answered.content, "It is 42.");
    assert!(!answered.is_error);
    assert_eq!(answered.duration_ms, 120);
    assert_eq!(answered.name, "ask_researcher");

    // "Nothing" and "cut off with nothing" are different results: the first
    // tells a model to stop looking.
    let nothing = subagent_result(&binding, "", StopReason::Complete, 1);
    assert!(
        nothing
            .content
            .contains("finished without writing an answer")
    );

    let cut_off = subagent_result(&binding, "", StopReason::WallTimeout, 1);
    assert!(cut_off.content.contains("stopped early (wall_timeout)"));
    assert!(cut_off.content.contains("this is not a finding"));

    // A cap reached with something found is still an answer, with a line saying
    // it was cut short.
    let partial = subagent_result(&binding, "Half of it.", StopReason::MaxIterations, 1);
    assert!(partial.content.starts_with("Half of it."));
    assert!(partial.content.contains("stopped early: max_iterations"));
    assert!(!partial.is_error);

    // Only a failure is a failed call.
    let failed = subagent_result(&binding, "", StopReason::Error, 1);
    assert!(failed.is_error);
    assert_eq!(failed.kind, Some(ErrorKind::Tool));
}

#[test]
fn the_stop_reason_a_model_reads_is_the_one_the_wire_carries() {
    // Restated as prose rather than serialised, so this is what stops the two
    // spellings coming apart.
    for reason in [
        StopReason::Complete,
        StopReason::Aborted,
        StopReason::MaxIterations,
        StopReason::WallTimeout,
    ] {
        let wire = serde_json::to_value(reason).unwrap();
        let wire = wire.as_str().unwrap();
        if reason == StopReason::Complete {
            continue;
        }
        let result = subagent_result(&binding("r"), "found", reason, 0);
        assert!(
            result.content.contains(wire),
            "{reason:?}: {}",
            result.content
        );
    }
}

#[test]
fn two_subagents_resolving_to_one_name_is_refused_at_build_time() {
    let error = subagent_map(vec![binding("researcher"), binding("researcher")])
        .expect_err("a duplicate tool name");

    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("ask_researcher"));

    let map =
        subagent_map(vec![binding("researcher"), binding("summariser")]).expect("unique names");
    assert_eq!(map.len(), 2);
}

// A real delegation

/// A parent whose one subagent is a second loop on the same store.
fn delegating(
    child_turns: Vec<ScriptedTurn>,
    parent_turns: Vec<ScriptedTurn>,
) -> (Harness, Arc<MapResolver>) {
    let resolver = MapResolver::new();
    let parent = Harness::build(Setup {
        turns: parent_turns,
        subagents: vec![binding("researcher")],
        resolve_loop: Some(resolver.clone()),
        ..Setup::default()
    });

    let child = Harness::build(Setup {
        turns: child_turns,
        ..Setup::default()
    });
    resolver.insert("researcher", child.agent_loop.clone());
    (parent, resolver)
}

#[tokio::test]
async fn it_advertises_one_tool_per_subagent_after_the_registrys_own() {
    // `writer` before `reviewer`, which is not alphabetical: the block keeps the
    // order the operator configured, so a reader of the tool list sees the roster
    // they wrote rather than a re-sorted one.
    let harness = Harness::build(Setup {
        tools: vec![FakeTool::reading("read_file", "x")],
        subagents: vec![binding("writer"), binding("reviewer")],
        ..Setup::default()
    });

    let names: Vec<String> = harness
        .agent_loop
        .tool_definitions()
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert_eq!(names, vec!["read_file", "ask_writer", "ask_reviewer"]);
}

#[tokio::test]
async fn a_registered_tool_of_the_same_name_hides_the_subagent_entirely() {
    let harness = Harness::build(Setup {
        tools: vec![FakeTool::reading("ask_researcher", "the registry's own")],
        subagents: vec![binding("researcher")],
        ..Setup::default()
    });

    // Not advertised *and* not reachable, rather than invisible to the model
    // and callable by a lucky guess.
    assert_eq!(harness.agent_loop.tool_definitions().len(), 1);
}

#[tokio::test]
async fn it_runs_the_subagent_and_returns_its_answer_as_the_tool_result() {
    let (parent, _resolver) = delegating(
        vec![ScriptedTurn::text("The answer is 42.")],
        vec![
            ScriptedTurn::calls(vec![tool_call(
                "c1",
                "ask_researcher",
                &json!({"task": "find the answer"}),
            )]),
            ScriptedTurn::text("It is 42."),
        ],
    );

    let (events, result) = parent.say("web:1", "delegate").await;

    assert_eq!(result.expect("a turn").text, "It is 42.");
    let results = events_of(&events, "tool.result");
    // The child's final text and nothing else: the whole point is that the
    // detour does not land in the caller's context window.
    assert_eq!(results[0]["content"], json!("The answer is 42."));
    assert_eq!(results[0]["ok"], json!(true));
}

#[tokio::test]
async fn the_childs_events_stream_addressed_to_the_delegating_call() {
    let (parent, _resolver) = delegating(
        vec![ScriptedTurn::text("Found it.")],
        vec![
            ScriptedTurn::calls(vec![tool_call(
                "c1",
                "ask_researcher",
                &json!({"task": "look"}),
            )]),
            ScriptedTurn::text("done"),
        ],
    );

    let (events, result) = parent.say("web:1", "delegate").await;
    let turn_id = result.expect("a turn").turn_id;

    let wrapped = events_of(&events, "subagent.event");
    assert!(!wrapped.is_empty());
    for event in &wrapped {
        // The root turn, not the subagent's own: the inner event carries that,
        // and this is what a transcript uses to find the turn a person reads.
        assert_eq!(event["turnId"], json!(turn_id));
        assert_eq!(event["parentSessionKey"], json!("web:1"));
        assert_eq!(event["parentCallId"], json!("c1"));
        assert_eq!(event["agentId"], json!("researcher"));
        assert_eq!(event["label"], json!("Researcher"));
        assert_eq!(event["depth"], json!(1));
    }
    let inner: Vec<&str> = wrapped
        .iter()
        .map(|event| event["event"]["type"].as_str().unwrap())
        .collect();
    assert_eq!(inner.first(), Some(&"turn.start"));
    assert_eq!(inner.last(), Some(&"turn.end"));
    assert!(inner.contains(&"assistant.delta"));
}

#[tokio::test]
async fn a_subagent_never_reports_context_for_the_conversation_on_screen() {
    let (parent, _resolver) = delegating(
        vec![ScriptedTurn::text("Found it.")],
        vec![
            ScriptedTurn::calls(vec![tool_call(
                "c1",
                "ask_researcher",
                &json!({"task": "look"}),
            )]),
            ScriptedTurn::text("done"),
        ],
    );

    let (events, _) = parent.say("web:1", "delegate").await;

    // A child measures its *own* session, and reporting it would move the
    // operator's bar to a figure describing a conversation they are not
    // reading.
    for event in events_of(&events, "subagent.event") {
        assert_ne!(event["event"]["type"], json!("context.usage"));
    }
    let root = events_of(&events, "context.usage");
    assert_eq!(root.len(), 1);
    assert_eq!(root[0]["sessionKey"], json!("web:1"));
}

/// Where a delegated turn runs, which is the caller's decision and nothing
/// else's.
///
/// The three cases below are the whole rule. It used to be implied by the
/// target naming no environment of its own, which meant "the host" at the top
/// of a chain and "inherit" below it: one spelling for two answers, and no way
/// to ask for the host under a containerised caller at all.
mod where_a_delegation_runs {
    use super::*;

    fn in_environment(name: &str) -> AgentEnvironment {
        AgentEnvironment {
            name: name.to_owned(),
            network: EnvironmentNetwork {
                mode: NetworkMode::Open,
                ..EnvironmentNetwork::default()
            },
        }
    }

    /// Parent in `caller-env`, child configured as `child` says, delegated to
    /// with `inherit`. Returns the environment each turn resolved with, parent
    /// first.
    async fn placements(
        inherit: bool,
        parent_environment: AgentEnvironment,
        child_environment: AgentEnvironment,
    ) -> (Vec<String>, Vec<String>) {
        let resolver = MapResolver::new();
        let seen = RecordingEnvironments::new();

        let parent = Harness::build(Setup {
            turns: vec![
                ScriptedTurn::calls(vec![tool_call(
                    "c1",
                    "ask_researcher",
                    &json!({"task": "look"}),
                )]),
                ScriptedTurn::text("done"),
            ],
            subagents: vec![SubagentBinding {
                inherit_environment: inherit,
                ..binding("researcher")
            }],
            resolve_loop: Some(resolver.clone()),
            environment: parent_environment,
            environments: Some(seen.clone()),
            ..Setup::default()
        });

        let child = Harness::build(Setup {
            turns: vec![ScriptedTurn::text("Found it.")],
            environment: child_environment,
            environments: Some(seen.clone()),
            ..Setup::default()
        });
        resolver.insert("researcher", child.agent_loop.clone());

        let (_, result) = parent.say("web:1", "delegate").await;
        result.expect("the delegating turn runs");
        (seen.names(), seen.agents())
    }

    #[tokio::test]
    async fn on_takes_the_callers_place_over_the_targets_own() {
        // The one behaviour change: a subagent that names its own environment
        // used to win outright. The switch decides now, so a roster an operator
        // set to inherit inherits whatever the target happens to name.
        let (names, _) = placements(
            true,
            in_environment("caller-env"),
            in_environment("its-own"),
        )
        .await;

        assert_eq!(names, vec!["caller-env", "caller-env"]);
    }

    #[tokio::test]
    async fn off_leaves_the_target_in_the_environment_it_names() {
        let (names, agents) = placements(
            false,
            in_environment("caller-env"),
            in_environment("its-own"),
        )
        .await;

        assert_eq!(names, vec!["caller-env", "its-own"]);
        // The child resolves under its own agent id, so a private definition
        // gives it its own container rather than the caller's.
        assert_eq!(agents.len(), 2);
    }

    #[tokio::test]
    async fn off_means_the_host_when_the_target_names_nothing() {
        // The case that had no spelling at all: run on this machine even though
        // the caller is in a container.
        let (names, _) = placements(
            false,
            in_environment("caller-env"),
            AgentEnvironment::default(),
        )
        .await;

        assert_eq!(names, vec!["caller-env", ""]);
    }

    #[tokio::test]
    async fn on_hands_down_the_host_when_that_is_where_the_caller_is() {
        // Inheriting from a host caller must reach the child as *the host*, not
        // as "nobody decided" — which would let it fall back to its own
        // environment and quietly contradict the switch.
        let (names, _) =
            placements(true, AgentEnvironment::default(), in_environment("its-own")).await;

        assert_eq!(names, vec!["", ""]);
    }
}

#[tokio::test]
async fn the_child_gets_its_own_session_in_the_callers_workspace() {
    let (parent, _resolver) = delegating(
        vec![ScriptedTurn::text("Found it.")],
        vec![
            ScriptedTurn::calls(vec![tool_call(
                "c1",
                "ask_researcher",
                &json!({"task": "look"}),
            )]),
            ScriptedTurn::text("done"),
        ],
    );

    let (events, _) = parent
        .run(ghostai_agent::TurnInput {
            workspace_id: Some("client-acme".to_owned()),
            ..ghostai_agent::TurnInput::new("web:1", "delegate")
        })
        .await;

    let child_key = events_of(&events, "subagent.event")[0]["sessionKey"]
        .as_str()
        .unwrap()
        .to_owned();
    let child = parent.store.get_session(&child_key).unwrap().unwrap();

    // Its own session, because context isolation is the feature; the caller's
    // workspace, because a researcher that could not read the files being
    // discussed would be useless.
    assert_eq!(child.origin, SUBAGENT_ORIGIN);
    assert_eq!(child.workspace_id, "client-acme");
    assert_eq!(child.agent_id.as_deref(), Some("researcher"));

    let lineage = &child.metadata[SUBAGENT_METADATA_KEY];
    assert_eq!(lineage["parentSessionKey"], json!("web:1"));
    assert_eq!(lineage["parentCallId"], json!("c1"));
    assert_eq!(lineage["depth"], json!(1));

    // And the parent points back, so a reloaded transcript can find the run.
    let parent_row = parent.store.get_session("web:1").unwrap().unwrap();
    let run = &parent_row.metadata["subagentRuns"]["c1"];
    assert_eq!(run["sessionKey"], json!(child_key));
    assert_eq!(run["label"], json!("Researcher"));
}

#[tokio::test]
async fn a_delegation_with_no_task_is_refused_with_a_result() {
    let (parent, _resolver) = delegating(
        vec![ScriptedTurn::text("never runs")],
        vec![
            ScriptedTurn::calls(vec![raw_tool_call("c1", "ask_researcher", "{}")]),
            ScriptedTurn::text("done"),
        ],
    );

    let (events, result) = parent.say("web:1", "delegate").await;

    assert_eq!(result.expect("a turn").stop_reason, StopReason::Complete);
    let results = events_of(&events, "tool.result");
    assert_eq!(results[0]["ok"], json!(false));
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains("must be a non-empty string")
    );
}

#[tokio::test]
async fn a_subagent_with_nothing_to_run_on_is_refused_rather_than_failing_the_turn() {
    // No resolver at all, so the binding advertises an agent that cannot run.
    let parent = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call(
                "c1",
                "ask_researcher",
                &json!({"task": "look"}),
            )]),
            ScriptedTurn::text("I did it myself."),
        ],
        subagents: vec![binding("researcher")],
        ..Setup::default()
    });

    let (events, result) = parent.say("web:1", "delegate").await;

    assert_eq!(result.expect("a turn").text, "I did it myself.");
    let results = events_of(&events, "tool.result");
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains("no provider or model configured")
    );
}

#[tokio::test(start_paused = true)]
async fn a_delegation_cap_cuts_the_child_short_and_leaves_the_turn_running() {
    let resolver = MapResolver::new();
    let parent = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call(
                "c1",
                "ask_researcher",
                &json!({"task": "take your time"}),
            )]),
            ScriptedTurn::text("It did not finish, so here is what I have."),
        ],
        subagents: vec![binding("researcher")],
        resolve_loop: Some(resolver.clone()),
        config: AgentSettings {
            model: "test-model".to_owned(),
            // The delegator caps its delegate: an agent cannot grant itself
            // more time by being called.
            subagent_timeout_ms: 1_000,
            ..AgentSettings::default()
        },
        ..Setup::default()
    });
    let child = Harness::build(Setup::with(vec![
        ScriptedTurn::text("never arrives").after(60_000),
    ]));
    resolver.insert("researcher", child.agent_loop.clone());

    let (events, result) = parent.say("web:1", "delegate").await;
    let result = result.expect("a turn");

    // The caller gets a tool result saying the subagent was cut short and
    // carries on, which is what the cap is for.
    assert_eq!(result.stop_reason, StopReason::Complete);
    assert_eq!(result.text, "It did not finish, so here is what I have.");
    let results = events_of(&events, "tool.result");
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains("stopped early"),
        "{}",
        results[0]["content"]
    );
}

#[tokio::test]
async fn a_grandchilds_event_is_forwarded_rather_than_wrapped_twice() {
    // Depth beyond one level works by forwarding, which is what keeps the wire
    // payload non-recursive: only the turn id is rewritten on the way up.
    let resolver = MapResolver::new();
    let parent = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call(
                "c1",
                "ask_researcher",
                &json!({"task": "look"}),
            )]),
            ScriptedTurn::text("done"),
        ],
        subagents: vec![binding("researcher")],
        resolve_loop: Some(resolver.clone()),
        ..Setup::default()
    });

    let grandchild = Harness::build(Setup::with(vec![ScriptedTurn::text("deep answer")]));
    let inner_resolver = MapResolver::new();
    inner_resolver.insert("summariser", grandchild.agent_loop.clone());
    let child = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call(
                "c2",
                "ask_summariser",
                &json!({"task": "again"}),
            )]),
            ScriptedTurn::text("child answer"),
        ],
        subagents: vec![binding("summariser")],
        resolve_loop: Some(inner_resolver),
        ..Setup::default()
    });
    resolver.insert("researcher", child.agent_loop.clone());

    let (events, result) = parent.say("web:1", "delegate").await;
    let turn_id = result.expect("a turn").turn_id;

    let wrapped = events_of(&events, "subagent.event");
    // Every frame, at any depth, is one wrapper naming the root turn — never a
    // wrapper inside a wrapper.
    for event in &wrapped {
        assert_eq!(event["turnId"], json!(turn_id));
        assert_ne!(event["event"]["type"], json!("subagent.event"));
    }
    let depths: Vec<u64> = wrapped
        .iter()
        .map(|event| event["depth"].as_u64().unwrap())
        .collect();
    assert!(depths.contains(&1));
    assert!(depths.contains(&2), "the grandchild's own depth survives");
    let agents: Vec<&str> = wrapped
        .iter()
        .map(|event| event["agentId"].as_str().unwrap())
        .collect();
    assert!(agents.contains(&"summariser"));
}

#[tokio::test]
async fn a_cycle_is_refused_as_a_tool_result_rather_than_a_failed_turn() {
    let resolver = MapResolver::new();
    let parent = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call(
                "c1",
                "ask_researcher",
                &json!({"task": "look"}),
            )]),
            ScriptedTurn::text("done"),
        ],
        subagents: vec![binding("researcher")],
        resolve_loop: Some(resolver.clone()),
        agent: Some(ghostai_agent::LoopAgent {
            id: "researcher".to_owned(),
            ..ghostai_agent::LoopAgent::default()
        }),
        ..Setup::default()
    });
    let child = Harness::build(Setup::with(vec![ScriptedTurn::text("never runs")]));
    resolver.insert("researcher", child.agent_loop.clone());

    let (events, result) = parent
        .run(ghostai_agent::TurnInput {
            // Already running above this call.
            chain: vec!["researcher".to_owned()],
            ..ghostai_agent::TurnInput::new("web:1", "delegate")
        })
        .await;

    assert_eq!(result.expect("a turn").stop_reason, StopReason::Complete);
    let results = events_of(&events, "tool.result");
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains("already running above this call")
    );
}

#[tokio::test(start_paused = true)]
async fn a_delegation_is_a_whole_turn_and_never_runs_beside_another() {
    // Two of them at once is background agents, which this deliberately is not.
    let resolver = MapResolver::new();
    let parent = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![
                tool_call("c1", "ask_researcher", &json!({"task": "one"})),
                tool_call("c2", "ask_researcher", &json!({"task": "two"})),
            ]),
            ScriptedTurn::text("done"),
        ],
        subagents: vec![binding("researcher")],
        resolve_loop: Some(resolver.clone()),
        ..Setup::default()
    });
    let child = Harness::build(Setup::with(vec![ScriptedTurn::text("answer").after(1_000)]));
    resolver.insert("researcher", child.agent_loop.clone());

    let started = tokio::time::Instant::now();
    let (events, _) = parent.say("web:1", "delegate twice").await;

    // One after the other, so the wall time is the sum rather than the max.
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert_eq!(events_of(&events, "tool.result").len(), 2);
}
