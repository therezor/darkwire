//! The turn: what it sends, what it stores, and how it ends.
//!
//! Every timing test runs on a paused tokio clock, and the harness's clock
//! reads that same timer — so a wall-clock cap and a heartbeat cadence can be
//! asserted in one test without two notions of "now".

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "a fixture that cannot be built is a failing test either way, and a \
              turn's assertions belong beside the turn that produced them"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::harness::{Behaviour, FakeTool, Harness, Setup, answer, events_of};
use darkwire_agent::testkit::{ScriptedTurn, raw_tool_call, tool_call};
use darkwire_agent::{AgentLoopOptions, LoopAgent, TurnInput};
use darkwire_core::ErrorKind;
use darkwire_protocol::{
    AgentSettings, ChatMessage, PromptMode, StopReason, ToolPermission, ToolRisk, Usage,
};
use futures::StreamExt as _;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn usage(prompt: u64, completion: u64) -> Usage {
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        cached_tokens: None,
        reasoning_tokens: None,
    }
}

// The ordinary turn

#[tokio::test]
async fn it_streams_an_answer_and_persists_the_exchange() {
    let harness = Harness::build(Setup::with(vec![ScriptedTurn {
        deltas: vec!["Hello".to_owned(), " there".to_owned()],
        usage: Some(usage(10, 4)),
        ..ScriptedTurn::default()
    }]));

    let (events, result) = harness.say("web:1", "hi").await;
    let result = result.expect("a completed turn");

    assert_eq!(answer(&events), "Hello there");
    assert_eq!(result.text, "Hello there");
    assert_eq!(result.stop_reason, StopReason::Complete);
    assert_eq!(result.iterations, 1);
    assert_eq!(result.usage, usage(10, 4));

    let stored = harness.stored("web:1");
    assert_eq!(stored.len(), 2);
    assert!(matches!(stored[0], ChatMessage::User(_)));
    assert!(matches!(stored[1], ChatMessage::Assistant(_)));
}

#[tokio::test]
async fn it_opens_and_closes_the_turn_exactly_once() {
    let harness = Harness::simple();
    let (events, _) = harness.say("web:1", "hi").await;

    let starts = events_of(&events, "turn.start");
    let ends = events_of(&events, "turn.end");
    assert_eq!(starts.len(), 1);
    assert_eq!(ends.len(), 1);
    // Reported on the start as well as on the end, because a turn that fails
    // never reaches its end and a failed turn with no seq is one nothing can
    // re-run.
    assert_eq!(starts[0]["firstSeq"], json!(1));
    assert_eq!(ends[0]["firstSeq"], json!(1));
    assert_eq!(ends[0]["lastSeq"], json!(2));
    assert_eq!(ends[0]["stopReason"], json!("complete"));
    // The first event, before anything fallible.
    assert_eq!(events[0].tag(), "turn.start");
    assert_eq!(events.last().unwrap().tag(), "turn.end");
}

#[tokio::test]
async fn it_reports_reasoning_apart_from_the_answer() {
    let harness = Harness::build(Setup::with(vec![ScriptedTurn {
        deltas: vec!["The answer.".to_owned()],
        reasoning: vec!["Thinking".to_owned()],
        ..ScriptedTurn::default()
    }]));

    let (events, _) = harness.say("web:1", "hi").await;

    assert_eq!(answer(&events), "The answer.");
    let thinking = events_of(&events, "reasoning.delta");
    assert_eq!(thinking.len(), 1);
    assert_eq!(thinking[0]["text"], json!("Thinking"));
}

#[tokio::test]
async fn an_empty_delta_is_never_sent() {
    let harness = Harness::build(Setup::with(vec![ScriptedTurn {
        deltas: vec![String::new(), "x".to_owned()],
        reasoning: vec![String::new()],
        ..ScriptedTurn::default()
    }]));

    let (events, _) = harness.say("web:1", "hi").await;

    assert_eq!(events_of(&events, "assistant.delta").len(), 1);
    assert_eq!(events_of(&events, "reasoning.delta").len(), 0);
}

#[tokio::test]
async fn a_turn_that_reasoned_and_said_nothing_still_completes() {
    let harness = Harness::build(Setup::with(vec![ScriptedTurn {
        reasoning: vec!["mm".to_owned()],
        ..ScriptedTurn::default()
    }]));

    let (_, result) = harness.say("web:1", "hi").await;
    let result = result.expect("a completed turn");

    assert_eq!(result.stop_reason, StopReason::Complete);
    assert_eq!(result.text, "");
}

#[tokio::test]
async fn it_sends_no_temperature_or_effort_unless_configured() {
    let harness = Harness::simple();
    let _ = harness.say("web:1", "hi").await;

    let request = &harness.provider.requests()[0];
    assert_eq!(request.temperature, None);
    assert_eq!(request.reasoning_effort, None);
    // Keyed on the session so every request in a conversation lands on the same
    // cache shard.
    assert_eq!(request.cache_key.as_deref(), Some("web:1"));
    assert_eq!(request.model, "test-model");
}

#[tokio::test]
async fn it_sends_a_temperature_that_was_configured() {
    let harness = Harness::build(Setup {
        config: AgentSettings {
            model: "test-model".to_owned(),
            temperature: Some(0.2),
            ..AgentSettings::default()
        },
        ..Setup::default()
    });
    let _ = harness.say("web:1", "hi").await;

    assert_eq!(harness.provider.requests()[0].temperature, Some(0.2));
}

#[tokio::test]
async fn it_refuses_to_construct_without_a_model() {
    let harness = Harness::simple();
    let error = darkwire_agent::AgentLoop::new(AgentLoopOptions::new(
        harness.provider.clone(),
        harness
            .registry
            .select(darkwire_protocol::ToolPermissions::new()),
        Arc::clone(&harness.store),
        Arc::new(darkwire_security::single_jail(Arc::clone(&harness.jail))),
    ))
    .expect_err("no model configured");

    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("No model configured"));
}

// Tools

#[tokio::test]
async fn it_runs_the_tools_the_model_asked_for_then_answers() {
    let read = FakeTool::reading("read_file", "file contents");
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({"path": "a.md"}))]),
            ScriptedTurn::text("It says hello."),
        ],
        tools: vec![read.clone()],
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "read it").await;
    let result = result.expect("a completed turn");

    assert_eq!(result.iterations, 2);
    assert_eq!(result.text, "It says hello.");
    assert_eq!(read.calls(), vec![json!({"path": "a.md"})]);

    let calls = events_of(&events, "tool.call");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["name"], json!("read_file"));
    assert_eq!(calls[0]["args"], json!({"path": "a.md"}));
    assert_eq!(calls[0]["risk"], json!("safe"));

    let results = events_of(&events, "tool.result");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["ok"], json!(true));
    // The tool's own output, not the envelope: showing a human a defence
    // mechanism as though it were part of the answer is the thing this avoids.
    assert_eq!(results[0]["content"], json!("file contents"));
}

#[tokio::test]
async fn it_wraps_the_stored_result_but_not_the_one_it_reports() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![FakeTool::reading("read_file", "secret contents")],
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "read").await;

    let reported = events_of(&events, "tool.result");
    assert_eq!(reported[0]["content"], json!("secret contents"));

    let stored = harness.stored("web:1");
    let ChatMessage::Tool(tool) = &stored[2] else {
        panic!("the third message is the tool result")
    };
    assert!(tool.content.contains("secret contents"));
    assert!(tool.content.contains("tool_output_"), "{}", tool.content);
    assert!(tool.content.starts_with('<'));
}

#[tokio::test]
async fn it_truncates_a_long_result_before_wrapping_it() {
    // The other order cuts the closing delimiter off the envelope, and a result
    // the model cannot see the end of is one it reads as continuing into the
    // conversation.
    let long = "x".repeat(500);
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![FakeTool::reading("read_file", &long)],
        max_tool_result_chars: 100,
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "read").await;

    let reported = events_of(&events, "tool.result");
    assert_eq!(reported[0]["truncated"], json!(true));
    assert!(
        reported[0]["content"]
            .as_str()
            .unwrap()
            .contains("characters truncated")
    );

    let stored = harness.stored("web:1");
    let ChatMessage::Tool(tool) = &stored[2] else {
        panic!("a tool message")
    };
    assert!(tool.truncated);
    // The envelope closes, whatever the budget did to the content.
    assert!(tool.content.trim_end().ends_with('>'));
}

#[tokio::test]
async fn a_failed_call_is_a_result_the_model_can_recover_from() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]),
            ScriptedTurn::text("I could not read it."),
        ],
        tools: vec![FakeTool::new(
            "read_file",
            ToolRisk::Safe,
            Behaviour::Fail("no such file".to_owned()),
        )],
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "read").await;

    assert_eq!(result.expect("a turn").stop_reason, StopReason::Complete);
    let results = events_of(&events, "tool.result");
    assert_eq!(results[0]["ok"], json!(false));
    // A failed tool is not a failed turn.
    assert!(events_of(&events, "error").is_empty());
}

#[tokio::test]
async fn a_call_the_scope_cannot_resolve_is_answered_rather_than_dropped() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "invented", &json!({}))]),
            ScriptedTurn::text("Sorry."),
        ],
        tools: vec![FakeTool::reading("read_file", "x")],
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "go").await;

    // Every tool call gets a `tool` message: an unanswered one is a provider
    // 400 on the next request.
    assert_eq!(events_of(&events, "tool.result").len(), 1);
    let stored = harness.stored("web:1");
    assert!(matches!(stored[2], ChatMessage::Tool(_)));
}

#[tokio::test]
async fn it_leaves_history_legal_for_the_next_request() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![
                tool_call("c1", "read_file", &json!({})),
                tool_call("c2", "write_file", &json!({})),
            ]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![
            FakeTool::reading("read_file", "a"),
            FakeTool::writing("write_file", "b"),
        ],
        ..Setup::default()
    });

    let _ = harness.say("web:1", "go").await;

    let stored = harness.stored("web:1");
    let ChatMessage::Assistant(assistant) = &stored[1] else {
        panic!("the assistant turn")
    };
    assert_eq!(assistant.tool_calls.len(), 2);
    // One `tool` message per call, in the order the model asked, immediately
    // after the turn that made them.
    let answered: Vec<&str> = stored[2..4]
        .iter()
        .map(|message| match message {
            ChatMessage::Tool(tool) => tool.tool_call_id.as_str(),
            _ => panic!("a tool message"),
        })
        .collect();
    assert_eq!(answered, vec!["c1", "c2"]);
}

#[tokio::test]
async fn the_nonce_and_the_definitions_are_computed_once_per_turn() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]),
            ScriptedTurn::calls(vec![tool_call("c2", "read_file", &json!({}))]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![FakeTool::reading("read_file", "x")],
        ..Setup::default()
    });

    let _ = harness.say("web:1", "go").await;
    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 3);

    // Both sit in the part of the prompt providers cache: regenerating them
    // mid-turn would rewrite the prefix and throw the cache away for no
    // semantic change.
    let tools: Vec<_> = requests.iter().map(|r| r.tools.clone()).collect();
    assert_eq!(tools[0], tools[1]);
    assert_eq!(tools[1], tools[2]);

    let stored = harness.stored("web:1");
    let tags: Vec<String> = stored
        .iter()
        .filter_map(|message| match message {
            ChatMessage::Tool(tool) => Some(tool.content.clone()),
            _ => None,
        })
        .map(|content| content.lines().next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0], tags[1], "both results carry the same delimiter");
}

#[tokio::test]
async fn the_static_half_stays_byte_identical_across_iterations() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]),
            ScriptedTurn::calls(vec![tool_call("c2", "read_file", &json!({}))]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![FakeTool::reading("read_file", "x")],
        ..Setup::default()
    });

    let _ = harness.say("web:1", "go").await;

    let requests = harness.provider.requests();
    let systems: Vec<String> = requests
        .iter()
        .map(|request| match &request.messages[0] {
            ChatMessage::System(system) => system.content.clone(),
            _ => panic!("the first message is the system prompt"),
        })
        .collect();
    assert_eq!(systems[0], systems[1]);
    assert_eq!(systems[1], systems[2]);
    // And the volatile half goes last, after the history, so the conversation
    // stays inside the cached prefix.
    for request in &requests {
        let last = request.messages.last().unwrap();
        let ChatMessage::User(user) = last else {
            panic!("the last message is the reminder")
        };
        let text = darkwire_core::text_of(last);
        assert!(text.starts_with("<system-reminder>"), "{text}");
        assert_eq!(user.content.len(), 1);
    }
}

#[tokio::test]
async fn the_runtime_half_is_sent_and_never_stored() {
    let harness = Harness::simple();
    let _ = harness.say("web:1", "hi").await;

    let request = &harness.provider.requests()[0];
    let reminder = darkwire_core::text_of(request.messages.last().unwrap());
    assert!(reminder.contains("Current time"));

    for message in harness.stored("web:1") {
        assert!(!darkwire_core::text_of(&message).contains("system-reminder"));
    }
}

#[tokio::test]
async fn every_message_before_the_trailing_turn_is_identical_across_iterations() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![FakeTool::reading("read_file", "x")],
        ..Setup::default()
    });

    let _ = harness.say("web:1", "go").await;

    let requests = harness.provider.requests();
    // History is append-only, so the second request's prefix is the first
    // request's — everything a provider already cached.
    let first = &requests[0].messages;
    let second = &requests[1].messages;
    assert_eq!(first[..first.len() - 1], second[..first.len() - 1]);
    assert!(second.len() > first.len());
}

// Tools switched off

#[tokio::test]
async fn an_agent_with_tools_off_advertises_nothing_and_runs_nothing() {
    let read = FakeTool::reading("read_file", "x");
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]),
            ScriptedTurn::text("I cannot."),
        ],
        tools: vec![read.clone()],
        config: AgentSettings {
            model: "test-model".to_owned(),
            tools_enabled: false,
            ..AgentSettings::default()
        },
        ..Setup::default()
    });

    assert!(harness.agent_loop.permitted_definitions().is_empty());

    let (events, result) = harness.say("web:1", "read").await;

    assert!(harness.provider.requests()[0].tools.is_empty());
    assert!(read.calls().is_empty(), "nothing ran");
    assert_eq!(result.expect("a turn").stop_reason, StopReason::Complete);

    // A refusal rather than a silent drop, because every tool call must be
    // answered. And a notice, because nothing was *denied* — the model invented
    // a call it was never offered.
    let notices = events_of(&events, "notice");
    assert_eq!(notices[0]["kind"], json!("tools_disabled"));
    let results = events_of(&events, "tool.result");
    assert_eq!(results[0]["ok"], json!(false));
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains("did not run")
    );
}

#[tokio::test]
async fn switching_tools_off_leaves_the_agent_permissions_alone() {
    let harness = Harness::build(Setup {
        tools: vec![FakeTool::reading("read_file", "x")],
        config: AgentSettings {
            model: "test-model".to_owned(),
            tools_enabled: false,
            ..AgentSettings::default()
        },
        ..Setup::default()
    });
    let _ = harness.say("web:1", "hi").await;

    // Off is not the same as denying every tool: the map is untouched and still
    // says `allow`, and the tool is simply not offered to this model.
    assert_eq!(
        harness.agent_loop.permitted_definitions().len(),
        0,
        "nothing advertised"
    );
}

// Caps

#[tokio::test]
async fn it_stops_at_the_iteration_cap_and_says_so() {
    let harness = Harness::build(Setup {
        // Running past the end repeats the last turn: a model that never stops
        // calling tools.
        turns: vec![ScriptedTurn::calls(vec![tool_call(
            "c1",
            "read_file",
            &json!({}),
        )])],
        tools: vec![FakeTool::reading("read_file", "x")],
        config: AgentSettings {
            model: "test-model".to_owned(),
            max_tool_iterations: 3,
            ..AgentSettings::default()
        },
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "loop").await;
    let result = result.expect("a turn");

    assert_eq!(result.stop_reason, StopReason::MaxIterations);
    assert_eq!(result.iterations, 3);
    assert!(result.text.contains("I stopped after 3 tool iterations"));
    // Unlike an error, this is persisted: the next turn's history has to
    // explain why the task stopped half-done.
    let stored = harness.stored("web:1");
    let last = darkwire_core::text_of(stored.last().unwrap());
    assert!(last.contains("I stopped after 3 tool iterations"));
    assert!(answer(&events).contains("I stopped after 3"));
}

#[tokio::test(start_paused = true)]
async fn it_stops_at_the_wall_cap_checked_before_a_request_rather_than_after() {
    let harness = Harness::build(Setup {
        turns: vec![
            // The first request takes longer than the cap, so the check at the
            // top of the *second* iteration is the one that fires.
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]).after(5_000),
            ScriptedTurn::text("never asked"),
        ],
        tools: vec![FakeTool::reading("read_file", "x")],
        config: AgentSettings {
            model: "test-model".to_owned(),
            loop_wall_timeout_ms: 2_000,
            ..AgentSettings::default()
        },
        ..Setup::default()
    });

    let (_, result) = harness.say("web:1", "slow").await;
    let result = result.expect("a turn");

    assert_eq!(result.stop_reason, StopReason::WallTimeout);
    assert_eq!(result.iterations, 1, "the cap stopped the second request");
    assert_eq!(harness.provider.request_count(), 1);
    assert!(result.text.contains("I ran out of time for this turn"));
    assert!(result.text.contains("against a 2s cap"));
}

// Cancellation

#[tokio::test]
async fn a_turn_stopped_before_it_starts_never_calls_the_provider() {
    let harness = Harness::simple();
    let token = CancellationToken::new();
    token.cancel();

    let turn = harness
        .agent_loop
        .run(TurnInput::new("web:1", "hi"), &token);
    let (_, result) = turn.collect().await;

    assert_eq!(result.expect("a turn").stop_reason, StopReason::Aborted);
    assert_eq!(harness.provider.request_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn stopping_a_turn_mid_stream_ends_it_without_an_error() {
    let harness = Harness::build(Setup::with(vec![
        ScriptedTurn::text("never arrives").after(10_000),
    ]));

    let turn = harness
        .agent_loop
        .run(TurnInput::new("web:1", "hi"), &CancellationToken::new());
    turn.stop();
    let (events, result) = turn.collect().await;

    // An abort is a stop, never an error: nothing failed, somebody asked.
    assert_eq!(result.expect("a turn").stop_reason, StopReason::Aborted);
    assert!(events_of(&events, "error").is_empty());
    assert_eq!(
        events_of(&events, "turn.end")[0]["stopReason"],
        json!("aborted")
    );
}

#[tokio::test(start_paused = true)]
async fn a_stopped_turn_still_answers_every_tool_call() {
    let harness = Harness::build(Setup {
        turns: vec![ScriptedTurn::calls(vec![
            tool_call("c1", "slow", &json!({})),
            tool_call("c2", "slow", &json!({})),
        ])],
        tools: vec![FakeTool::new("slow", ToolRisk::Write, Behaviour::Hang)],
        ..Setup::default()
    });

    let turn = harness
        .agent_loop
        .run(TurnInput::new("web:1", "go"), &CancellationToken::new());
    let token = turn.token().clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });
    let (events, result) = turn.collect().await;

    assert_eq!(result.expect("a turn").stop_reason, StopReason::Aborted);
    // Stopping mid-tool without writing one would make the *next* turn fail on
    // history the user cannot see.
    assert_eq!(events_of(&events, "tool.result").len(), 2);
    let stored = harness.stored("web:1");
    let tool_messages = stored
        .iter()
        .filter(|message| matches!(message, ChatMessage::Tool(_)))
        .count();
    assert_eq!(tool_messages, 2);
}

#[tokio::test(start_paused = true)]
async fn abandoning_the_stream_unwinds_the_turn() {
    // The property `TurnGuard` exists for: dropping the receiver cancels the
    // turn's token, exactly as an explicit stop does.
    let harness = Harness::build(Setup::with(vec![
        ScriptedTurn::text("never arrives").after(60_000),
    ]));

    let turn = harness
        .agent_loop
        .run(TurnInput::new("web:1", "hi"), &CancellationToken::new());
    let token = turn.token().clone();
    assert!(!token.is_cancelled());

    drop(turn);
    token.cancelled().await;
    assert!(token.is_cancelled());

    // And nothing was recorded for a turn nobody finished: the stats row and
    // the closing event agree, and neither is a turn that ended.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        harness
            .store
            .turn_stats("web:1", None)
            .expect("stats")
            .is_empty()
    );
}

#[tokio::test(start_paused = true)]
async fn a_dropped_turn_leaves_no_steering_behind() {
    let harness = Harness::build(Setup::with(vec![ScriptedTurn::text("slow").after(60_000)]));
    harness.steering.push("web:1", "wait", 0);

    let turn = harness
        .agent_loop
        .run(TurnInput::new("web:1", "hi"), &CancellationToken::new());
    drop(turn);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Whatever ended the turn, nothing queued for it may leak into the next.
    assert!(!harness.steering.has_pending("web:1"));
}

#[tokio::test]
async fn a_turn_stopped_after_it_started_still_reports_its_end() {
    // The other half of the property above: a caller still holding the receiver
    // is told the turn ended, because it is there to be told.
    let harness = Harness::build(Setup::with(vec![ScriptedTurn::text("hi")]));
    let mut turn = harness
        .agent_loop
        .run(TurnInput::new("web:1", "hi"), &CancellationToken::new());

    let mut tags = Vec::new();
    while let Some(event) = turn.next().await {
        tags.push(event.tag().to_owned());
    }
    assert!(tags.contains(&"turn.end".to_owned()));
}

// Failures

#[tokio::test]
async fn a_provider_failure_never_reaches_history() {
    let harness = Harness::build(Setup::with(vec![ScriptedTurn::failing(
        ErrorKind::Provider,
        "the provider returned 400",
    )]));

    let (events, result) = harness.say("web:1", "hi").await;
    let result = result.expect("a turn");

    assert_eq!(result.stop_reason, StopReason::Error);
    let errors = events_of(&events, "error");
    assert_eq!(errors[0]["code"], json!("provider_error"));
    assert_eq!(errors[0]["message"], json!("the provider returned 400"));

    // A 400 written into the transcript is replayed on every later request in
    // that session, so one malformed turn becomes a permanently poisoned one.
    let stored = harness.stored("web:1");
    assert_eq!(stored.len(), 1, "only the user's own message");
    // It is recorded where it cannot poison anything: the stats row.
    let stats = harness.store.turn_stats("web:1", None).expect("stats");
    assert_eq!(stats[0].error.as_deref(), Some("the provider returned 400"));
}

#[tokio::test]
async fn it_maps_an_error_kind_onto_the_wire_code() {
    for (kind, code) in [
        (ErrorKind::InvalidInput, "bad_request"),
        (ErrorKind::NotFound, "not_found"),
        (ErrorKind::Conflict, "bad_request"),
        (ErrorKind::PermissionDenied, "unauthorized"),
        (ErrorKind::JailEscape, "unauthorized"),
        (ErrorKind::Network, "provider_error"),
        (ErrorKind::Timeout, "provider_error"),
        (ErrorKind::RateLimited, "rate_limited"),
        (ErrorKind::Config, "config_invalid"),
        (ErrorKind::Tool, "tool_error"),
        // Anything unmapped lands on `internal` rather than on whichever
        // branch happened to be last.
        (ErrorKind::Storage, "internal"),
        (ErrorKind::Extension, "internal"),
    ] {
        let harness = Harness::build(Setup::with(vec![ScriptedTurn::failing(kind, "boom")]));
        let (events, _) = harness.say("web:1", "hi").await;
        assert_eq!(
            events_of(&events, "error")[0]["code"],
            json!(code),
            "{kind:?}"
        );
    }
}

#[tokio::test]
async fn a_stream_that_ends_without_a_result_fails_the_turn() {
    let harness = Harness::build(Setup::with(vec![ScriptedTurn {
        deltas: vec!["half an ans".to_owned()],
        omit_done: true,
        ..ScriptedTurn::default()
    }]));

    let (events, result) = harness.say("web:1", "hi").await;

    // Treating that as an empty answer would silently end the turn on a
    // transport bug.
    assert_eq!(result.expect("a turn").stop_reason, StopReason::Error);
    let errors = events_of(&events, "error");
    assert_eq!(errors[0]["retryable"], json!(true));
    assert!(
        errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("without a result")
    );
    assert_eq!(harness.stored("web:1").len(), 1);
}

// Steering

#[tokio::test]
async fn a_correction_that_arrives_during_the_final_answer_continues_the_turn() {
    let harness = Harness::build(Setup::with(vec![
        ScriptedTurn::text("first answer"),
        ScriptedTurn::text("second answer"),
    ]));
    // Queued before the turn starts, so the first iteration drains it and the
    // second finds nothing pending. The property under test is the one after
    // that: a queue that fills while the first answer is composed keeps going.
    harness.steering.push("web:1", "no, the other one", 0);

    let (_, result) = harness.say("web:1", "go").await;
    let result = result.expect("a turn");

    let stored = harness.stored("web:1");
    // The correction is in history, prefixed, before the model's answer.
    let steered = darkwire_core::text_of(&stored[1]);
    assert!(steered.starts_with("[Steering"));
    assert!(steered.contains("no, the other one"));
    assert_eq!(result.text, "first answer");
}

#[tokio::test(start_paused = true)]
async fn a_correction_queued_mid_answer_is_answered_rather_than_discarded() {
    let harness = Harness::build(Setup::with(vec![
        ScriptedTurn::text("first").after(1_000),
        ScriptedTurn::text("second"),
    ]));

    let turn = harness
        .agent_loop
        .run(TurnInput::new("web:1", "go"), &CancellationToken::new());
    // The first iteration has drained an empty queue and is inside its request
    // by now, so this lands while the model is composing its final answer —
    // which is the moment the property is about.
    tokio::time::sleep(Duration::from_millis(500)).await;
    harness.steering.push("web:1", "actually, do this", 0);
    let (_, result) = turn.collect().await;
    let result = result.expect("a turn");

    // Ending there discards the correction, and from the outside a discarded
    // correction is indistinguishable from an ignored one.
    assert_eq!(result.iterations, 2);
    assert_eq!(result.text, "second");
    assert_eq!(result.stop_reason, StopReason::Complete);
}

#[tokio::test]
async fn a_turn_clears_its_session_queue_when_it_ends() {
    let harness = Harness::simple();
    harness.steering.push("web:1", "one", 0);
    let _ = harness.say("web:1", "hi").await;
    assert!(!harness.steering.has_pending("web:1"));
}

// A tool call written as text

#[tokio::test]
async fn it_corrects_a_model_that_wrote_a_call_as_text_and_takes_the_retry() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::text("<tool_call>\n{\"name\": \"read_file\"}\n</tool_call>"),
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]),
            ScriptedTurn::text("It says hello."),
        ],
        tools: vec![FakeTool::reading("read_file", "hello")],
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "read it").await;
    let result = result.expect("a turn");

    assert_eq!(result.text, "It says hello.");
    let notices = events_of(&events, "notice");
    assert_eq!(notices[0]["kind"], json!("degraded"));
    assert!(
        notices[0]["message"]
            .as_str()
            .unwrap()
            .contains("wrote a call to `read_file` as text")
    );

    // The correction rides in the runtime half, so it costs no cached prefix
    // and leaves nothing behind in history.
    let second = darkwire_core::text_of(harness.provider.requests()[1].messages.last().unwrap());
    assert!(second.contains("## Correction"));
    assert!(second.contains("Call `read_file` now, properly."));
    for message in harness.stored("web:1") {
        assert!(!darkwire_core::text_of(&message).contains("## Correction"));
    }
}

#[tokio::test]
async fn it_corrects_once_then_lets_the_answer_stand() {
    let written = "<tool_call>\n{\"name\": \"read_file\"}\n</tool_call>";
    let harness = Harness::build(Setup {
        turns: vec![ScriptedTurn::text(written)],
        tools: vec![FakeTool::reading("read_file", "hello")],
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "read it").await;
    let result = result.expect("a turn");

    // A model that gets it wrong twice is not going to be talked round, and a
    // loop of corrections would burn the iteration budget saying the same
    // thing.
    assert_eq!(result.iterations, 2);
    assert_eq!(result.stop_reason, StopReason::Complete);
    assert_eq!(events_of(&events, "notice").len(), 1);
}

#[tokio::test]
async fn an_answer_that_merely_mentions_a_tool_is_left_alone() {
    let harness = Harness::build(Setup {
        turns: vec![ScriptedTurn::text(
            "You would call read_file with a \"name\" like this.",
        )],
        tools: vec![FakeTool::reading("read_file", "hello")],
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "how?").await;

    assert_eq!(result.expect("a turn").iterations, 1);
    assert!(events_of(&events, "notice").is_empty());
}

// Stats and timings

#[tokio::test]
async fn it_adds_up_usage_across_every_request_in_the_turn() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn {
                tool_calls: vec![tool_call("c1", "read_file", &json!({}))],
                usage: Some(Usage {
                    cached_tokens: Some(2),
                    ..usage(10, 5)
                }),
                ..ScriptedTurn::default()
            },
            ScriptedTurn {
                deltas: vec!["done".to_owned()],
                usage: Some(Usage {
                    reasoning_tokens: Some(3),
                    ..usage(20, 7)
                }),
                ..ScriptedTurn::default()
            },
        ],
        tools: vec![FakeTool::reading("read_file", "x")],
        ..Setup::default()
    });

    let (_, result) = harness.say("web:1", "go").await;
    let total = result.expect("a turn").usage;

    assert_eq!(total.prompt_tokens, 30);
    assert_eq!(total.completion_tokens, 12);
    assert_eq!(total.total_tokens, 42);
    // An optional figure one request reported and the other did not is kept
    // rather than dropped.
    assert_eq!(total.cached_tokens, Some(2));
    assert_eq!(total.reasoning_tokens, Some(3));
}

#[tokio::test]
async fn it_pairs_generation_time_with_the_tokens_produced_inside_it() {
    let harness = Harness::build(Setup {
        turns: vec![
            // Ollama's shape for a bare tool call: charged for its tokens,
            // measured at zero. Those tokens must sit out with the window.
            ScriptedTurn {
                tool_calls: vec![tool_call("c1", "read_file", &json!({}))],
                usage: Some(usage(10, 225)),
                generation_ms: Some(0.0),
                ..ScriptedTurn::default()
            },
            ScriptedTurn {
                deltas: vec!["done".to_owned()],
                usage: Some(usage(10, 40)),
                generation_ms: Some(2_000.4),
                first_token_ms: Some(120.0),
                ..ScriptedTurn::default()
            },
        ],
        tools: vec![FakeTool::reading("read_file", "x")],
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "go").await;
    let end = &events_of(&events, "turn.end")[0];

    // Rounded to whole milliseconds, which the wire requires.
    assert_eq!(end["generationMs"], json!(2_000));
    // Only the tokens of the request that measured a window.
    assert_eq!(end["generationTokens"], json!(40));
    assert_eq!(end["usage"]["completionTokens"], json!(265));
    assert!(end["firstTokenMs"].as_u64().unwrap() >= 120);
}

#[tokio::test]
async fn it_omits_both_halves_of_a_rate_nothing_could_measure() {
    // Absence is what separates "not measured" from "measured as zero", and a
    // rate needs that distinction to know when to fall back.
    let harness = Harness::simple();
    let (events, _) = harness.say("web:1", "hi").await;
    let end = &events_of(&events, "turn.end")[0];

    assert_eq!(end.get("generationMs"), None);
    assert_eq!(end.get("generationTokens"), None);
    assert_eq!(end.get("firstTokenMs"), None);
}

#[tokio::test]
async fn it_records_what_the_turn_cost_and_reports_the_same_figures() {
    let harness = Harness::build(Setup::with(vec![ScriptedTurn {
        deltas: vec!["done".to_owned()],
        usage: Some(usage(11, 3)),
        generation_ms: Some(500.0),
        ..ScriptedTurn::default()
    }]));

    let (events, result) = harness.say("web:1", "hi").await;
    let result = result.expect("a turn");

    let stats = harness.store.turn_stats("web:1", None).expect("stats");
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].turn_id, result.turn_id);
    assert_eq!(stats[0].usage, usage(11, 3));
    assert_eq!(stats[0].stop_reason, StopReason::Complete);
    assert_eq!(stats[0].model, "test-model");
    assert_eq!(stats[0].workspace_id, "default");
    assert_eq!(stats[0].error, None);

    // Built once rather than twice, so the stored row and the event cannot come
    // to disagree about the same turn.
    let end = &events_of(&events, "turn.end")[0];
    assert_eq!(end["generationMs"].as_i64(), stats[0].generation_ms);
}

// The session row

#[tokio::test]
async fn it_names_an_unnamed_conversation_after_its_first_message() {
    let harness = Harness::simple();
    let _ = harness.say("web:1", "Fix the login bug").await;

    let session = harness.store.get_session("web:1").unwrap().unwrap();
    assert_eq!(session.title, "Fix the login bug");

    // Guarded on the *stored* title, so this can only ever fire once and a
    // manual rename is never clobbered.
    let _ = harness.say("web:1", "and the signup one").await;
    let session = harness.store.get_session("web:1").unwrap().unwrap();
    assert_eq!(session.title, "Fix the login bug");
}

#[tokio::test]
async fn a_message_with_nothing_to_name_it_after_leaves_the_title_empty() {
    let harness = Harness::simple();
    let _ = harness.say("web:1", "   ").await;

    let session = harness.store.get_session("web:1").unwrap().unwrap();
    assert_eq!(session.title, "");
}

#[tokio::test]
async fn it_records_the_channel_as_the_session_origin() {
    let harness = Harness::simple();
    let _ = harness
        .run(TurnInput {
            channel: Some("telegram".to_owned()),
            ..TurnInput::new("tg:1", "hi")
        })
        .await;

    assert_eq!(
        harness.store.get_session("tg:1").unwrap().unwrap().origin,
        "telegram"
    );
}

#[tokio::test]
async fn it_takes_the_turn_id_from_a_caller_that_already_published_one() {
    let harness = Harness::simple();
    let (events, result) = harness
        .run(TurnInput {
            turn_id: Some("published".to_owned()),
            ..TurnInput::new("web:1", "hi")
        })
        .await;

    assert_eq!(result.expect("a turn").turn_id, "published");
    assert_eq!(
        events_of(&events, "turn.start")[0]["turnId"],
        json!("published")
    );
}

#[tokio::test]
async fn the_stored_workspace_wins_over_the_one_a_frame_claims() {
    let harness = Harness::simple();
    // Creates the session in `alpha`.
    let _ = harness
        .run(TurnInput {
            workspace_id: Some("alpha".to_owned()),
            ..TurnInput::new("web:1", "one")
        })
        .await;
    // A later frame claiming another workspace cannot move it: that is what
    // stops a crafted frame pointing an existing session's tools elsewhere.
    let _ = harness
        .run(TurnInput {
            workspace_id: Some("beta".to_owned()),
            ..TurnInput::new("web:1", "two")
        })
        .await;

    assert_eq!(
        harness
            .store
            .get_session("web:1")
            .unwrap()
            .unwrap()
            .workspace_id,
        "alpha"
    );
}

// The prompt a turn would send

#[tokio::test]
async fn the_preview_is_the_prompt_a_turn_would_carry() {
    let harness = Harness::build(Setup {
        tools: vec![FakeTool::reading("read_file", "x")],
        ..Setup::default()
    });
    let _ = harness.say("web:1", "hi").await;

    let preview = harness
        .agent_loop
        .preview_prompt(&darkwire_agent::PromptPreviewInput {
            session_key: "web:1".to_owned(),
            ..Default::default()
        })
        .await
        .expect("a preview");

    let sent = match &harness.provider.requests()[0].messages[0] {
        ChatMessage::System(system) => system.content.clone(),
        _ => panic!("a system message"),
    };
    assert_eq!(preview.static_prompt, sent);
    // Both halves, separately: they are two messages at two ends of the request
    // and they are billed differently.
    assert!(preview.runtime_block.contains("Current time"));
    assert!(!preview.static_prompt.contains("Current time"));
}

#[tokio::test]
async fn a_preview_of_a_session_that_does_not_exist_uses_the_default_workspace() {
    let harness = Harness::simple();
    let preview = harness
        .agent_loop
        .preview_prompt(&darkwire_agent::PromptPreviewInput {
            session_key: "web:never".to_owned(),
            channel: Some("cli".to_owned()),
            ..Default::default()
        })
        .await
        .expect("a preview");

    assert!(preview.static_prompt.contains("`default` workspace"));
}

#[tokio::test]
async fn a_raw_agent_sends_one_blob_and_no_trailing_turn() {
    let harness = Harness::build(Setup {
        agent: Some(LoopAgent {
            id: "raw".to_owned(),
            prompt: darkwire_agent::PromptAgent {
                label: "Raw".to_owned(),
                prompt_mode: Some(PromptMode::Raw),
                system_prompt: "Only this. {{time}}".to_owned(),
                ..Default::default()
            },
            ..LoopAgent::default()
        }),
        ..Setup::default()
    });

    let _ = harness.say("web:1", "hi").await;
    let request = &harness.provider.requests()[0];

    let ChatMessage::System(system) = &request.messages[0] else {
        panic!("a system message")
    };
    assert!(system.content.starts_with("Only this. Tuesday"));
    // The last message is the user's own, not a reminder: raw mode places
    // everything itself.
    assert!(matches!(
        request.messages.last(),
        Some(ChatMessage::User(_))
    ));
    let last = darkwire_core::text_of(request.messages.last().unwrap());
    assert_eq!(last, "hi");
}

#[tokio::test]
async fn a_contributor_static_section_runs_once_per_turn_not_once_per_iteration() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting(AtomicUsize);
    impl darkwire_agent::prompt::ContextContributor for Counting {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn static_section<'a>(
            &'a self,
            _context: &'a darkwire_agent::prompt::StaticPromptContext,
        ) -> darkwire_providers::BoxFuture<'a, Option<String>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(Some("# Section".to_owned())))
        }
    }

    let counting = Arc::new(Counting(AtomicUsize::new(0)));
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]),
            ScriptedTurn::calls(vec![tool_call("c2", "read_file", &json!({}))]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![FakeTool::reading("read_file", "x")],
        contributors: vec![counting.clone()],
        ..Setup::default()
    });

    let _ = harness.say("web:1", "go").await;

    // A static section may do I/O, so it runs once per turn and never per
    // iteration — the whole reason the two halves are built separately.
    assert_eq!(counting.0.load(Ordering::SeqCst), 1);
    assert_eq!(harness.provider.request_count(), 3);
}

// The context report

#[tokio::test]
async fn it_reports_the_context_whenever_the_history_grows() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({}))]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![FakeTool::reading("read_file", "x")],
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "go").await;
    let usage = events_of(&events, "context.usage");

    // Emitted where the history grew, not at the end: "will this fit" is asked
    // while a twenty-tool turn is appending most of a window.
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0]["sessionKey"], json!("web:1"));
    assert!(usage[0]["estimatedTokens"].as_u64().unwrap() > 0);
    let breakdown = usage[0]["breakdown"].as_object().unwrap();
    let sections: Vec<&str> = breakdown.keys().map(String::as_str).collect();
    assert_eq!(
        sections,
        vec!["systemPrompt", "tools", "messages", "runtimeBlock"]
    );
}

// Arguments

#[tokio::test]
async fn it_reports_the_arguments_a_model_sent_malformed_or_absent() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![
                raw_tool_call("c1", "read_file", "{not json"),
                raw_tool_call("c2", "read_file", ""),
            ]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![FakeTool::reading("read_file", "x")],
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "go").await;
    let calls = events_of(&events, "tool.call");

    // Parsing is best-effort on purpose: malformed JSON from a model is common
    // enough that it must not break the event stream.
    assert_eq!(calls[0]["args"], json!("{not json"));
    assert_eq!(calls[1]["args"], json!({}));
}

#[tokio::test]
async fn a_definition_the_operator_reworded_is_what_the_model_is_sent() {
    let mut overrides = darkwire_protocol::ToolPromptOverrides::new();
    overrides.insert(
        "read_file".to_owned(),
        darkwire_protocol::ToolPromptOverride {
            description: "Open a file in this project.".to_owned(),
            fields: indexmap::IndexMap::new(),
        },
    );

    let harness = Harness::build(Setup {
        tools: vec![FakeTool::reading("read_file", "x")],
        agent: Some(LoopAgent {
            id: "coder".to_owned(),
            tool_prompts: Some(overrides),
            ..LoopAgent::default()
        }),
        ..Setup::default()
    });

    let definitions = harness.agent_loop.permitted_definitions();
    assert_eq!(definitions[0].description, "Open a file in this project.");

    let _ = harness.say("web:1", "hi").await;
    assert_eq!(
        harness.provider.requests()[0].tools[0].description,
        "Open a file in this project."
    );
}

#[tokio::test]
async fn a_denied_tool_is_never_advertised() {
    let mut permissions = darkwire_protocol::ToolPermissions::new();
    permissions.insert("read_file".to_owned(), ToolPermission::Allow);
    permissions.insert("write_file".to_owned(), ToolPermission::Deny);

    let harness = Harness::build(Setup {
        tools: vec![
            FakeTool::reading("read_file", "x"),
            FakeTool::writing("write_file", "y"),
        ],
        permissions: Some(permissions),
        ..Setup::default()
    });

    // `deny` and absent are identical: the tool is not in the definitions the
    // model is sent.
    let definitions = harness.agent_loop.permitted_definitions();
    let names: Vec<&str> = definitions.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["read_file"]);
}

#[tokio::test]
async fn the_loop_reports_what_a_turn_would_reach() {
    let harness = Harness::simple();
    assert_eq!(harness.agent_loop.model(), "test-model");
    assert_eq!(harness.agent_loop.provider(), "scripted");
    assert_eq!(harness.agent_loop.agent_id(), "default");
    assert!(harness.agent_loop.steering().is_empty());

    harness.agent_loop.steer("web:1", "hurry up");
    assert!(harness.agent_loop.steering().has_pending("web:1"));
    // And the debug form names the agent rather than dumping every seam.
    let shown = format!("{:?}", harness.agent_loop);
    assert!(shown.contains("test-model"), "{shown}");
}

#[tokio::test]
async fn an_event_stream_is_consumable_as_a_stream() {
    let harness = Harness::simple();
    let turn = harness
        .agent_loop
        .run(TurnInput::new("web:1", "hi"), &CancellationToken::new());

    let tags: Vec<String> = turn.map(|event| event.tag().to_owned()).collect().await;

    assert_eq!(tags.first().map(String::as_str), Some("turn.start"));
    assert_eq!(tags.last().map(String::as_str), Some("turn.end"));
}

#[tokio::test]
async fn finishing_a_turn_drains_whatever_is_left() {
    let harness = Harness::build(Setup::with(vec![ScriptedTurn {
        // More than a buffer's worth would deadlock a caller that awaited the
        // outcome without reading; `finish` drains for exactly that reason.
        deltas: (0..300).map(|n| n.to_string()).collect(),
        ..ScriptedTurn::default()
    }]));

    let turn = harness
        .agent_loop
        .run(TurnInput::new("web:1", "hi"), &CancellationToken::new());
    let result = turn.finish().await.expect("a turn");

    assert_eq!(result.stop_reason, StopReason::Complete);
}

/// Lazy tool discovery: the short list, the door, and what a session pulls in.
mod lazy_discovery {
    use darkwire_protocol::{AgentSettings, ToolPermissions};
    use darkwire_tools::tool_search_tool;

    use super::*;

    fn lazy(pins: &[&str]) -> AgentSettings {
        AgentSettings {
            lazy_discovery: true,
            pinned_tools: pins.iter().map(|pin| (*pin).to_owned()).collect(),
            ..Setup::default().config
        }
    }

    fn names(definitions: &[darkwire_protocol::ToolDefinition]) -> Vec<&str> {
        definitions.iter().map(|tool| tool.name.as_str()).collect()
    }

    fn four_tools() -> Vec<darkwire_tools::AnyTool> {
        vec![
            FakeTool::reading("read_file", "x"),
            FakeTool::writing("write_file", "y"),
            FakeTool::reading("memory", "m"),
            tool_search_tool(),
        ]
    }

    fn setup(turns: Vec<ScriptedTurn>, pins: &[&str]) -> Setup {
        Setup {
            turns,
            tools: four_tools(),
            config: lazy(pins),
            ..Setup::default()
        }
    }

    #[tokio::test]
    async fn off_sends_the_permitted_list_unchanged() {
        let harness = Harness::build(Setup {
            tools: four_tools(),
            ..Setup::default()
        });
        assert_eq!(
            harness.agent_loop.tool_definitions("web:1"),
            harness.agent_loop.permitted_definitions()
        );
    }

    #[tokio::test]
    async fn on_sends_the_door_then_the_pins_in_registry_order() {
        let harness = Harness::build(setup(vec![ScriptedTurn::text("ok")], &[]));
        assert_eq!(
            names(&harness.agent_loop.tool_definitions("web:1")),
            vec!["tool_search"]
        );

        let harness = Harness::build(setup(
            vec![ScriptedTurn::text("ok")],
            &["write_file", "memory", "not_registered"],
        ));
        assert_eq!(
            names(&harness.agent_loop.tool_definitions("web:1")),
            vec!["tool_search", "memory", "write_file"]
        );

        let _ = harness.say("web:1", "hi").await;
        assert_eq!(
            names(&harness.provider.requests()[0].tools),
            vec!["tool_search", "memory", "write_file"]
        );
    }

    #[tokio::test]
    async fn a_pin_the_agent_denies_is_not_sent() {
        let mut permissions = ToolPermissions::new();
        permissions.insert("tool_search".to_owned(), ToolPermission::Allow);
        permissions.insert("read_file".to_owned(), ToolPermission::Allow);
        permissions.insert("write_file".to_owned(), ToolPermission::Deny);
        let harness = Harness::build(Setup {
            permissions: Some(permissions),
            ..setup(vec![ScriptedTurn::text("ok")], &["write_file"])
        });
        assert_eq!(
            names(&harness.agent_loop.tool_definitions("web:1")),
            vec!["tool_search"]
        );
    }

    #[tokio::test]
    async fn an_agent_with_nothing_to_hide_gets_the_whole_list_without_the_door_mattering() {
        let harness = Harness::build(setup(
            vec![ScriptedTurn::text("ok")],
            &["read_file", "write_file", "memory"],
        ));
        assert_eq!(
            harness.agent_loop.tool_definitions("web:1"),
            harness.agent_loop.permitted_definitions()
        );
    }

    #[tokio::test]
    async fn the_door_needs_no_entry_in_the_permission_map() {
        // An agent from before the feature: a map that never heard of
        // `tool_search`. It still gets the short list, because the door is not
        // the map's to grant or refuse.
        let mut permissions = ToolPermissions::new();
        permissions.insert("read_file".to_owned(), ToolPermission::Allow);
        permissions.insert("write_file".to_owned(), ToolPermission::Allow);
        let harness = Harness::build(Setup {
            permissions: Some(permissions),
            ..setup(vec![ScriptedTurn::text("ok")], &[])
        });
        assert_eq!(
            names(&harness.agent_loop.tool_definitions("web:1")),
            vec!["tool_search"]
        );
    }

    #[tokio::test]
    async fn the_prompt_explains_the_short_list_only_when_there_is_one() {
        let harness = Harness::build(setup(vec![ScriptedTurn::text("ok")], &[]));
        let _ = harness.say("web:1", "hi").await;
        let system = darkwire_core::text_of(&harness.provider.requests()[0].messages[0]);
        assert!(system.contains("## Finding tools"), "{system}");

        let harness = Harness::build(Setup {
            tools: four_tools(),
            ..Setup::default()
        });
        let _ = harness.say("web:1", "hi").await;
        let system = darkwire_core::text_of(&harness.provider.requests()[0].messages[0]);
        assert!(!system.contains("## Finding tools"), "{system}");
    }

    #[tokio::test]
    async fn a_search_finds_only_what_the_agent_may_call_and_marks_the_visible() {
        let mut permissions = ToolPermissions::new();
        permissions.insert("tool_search".to_owned(), ToolPermission::Allow);
        permissions.insert("read_file".to_owned(), ToolPermission::Allow);
        permissions.insert("memory".to_owned(), ToolPermission::Allow);
        permissions.insert("write_file".to_owned(), ToolPermission::Deny);
        let harness = Harness::build(Setup {
            permissions: Some(permissions),
            ..setup(
                vec![
                    ScriptedTurn::calls(vec![tool_call(
                        "c1",
                        "tool_search",
                        &json!({"query": "file memory"}),
                    )]),
                    ScriptedTurn::text("ok"),
                ],
                &["memory"],
            )
        });
        let (events, _) = harness.say("web:1", "find").await;
        let results = events_of(&events, "tool.result");
        let content = results[0]["content"].as_str().unwrap();
        assert!(content.contains("- read_file:"), "{content}");
        assert!(
            content.contains("- memory (already in your tool list)"),
            "{content}"
        );
        assert!(!content.contains("write_file"), "{content}");
        assert!(!content.contains("tool_search:"), "{content}");
    }

    #[tokio::test]
    async fn an_activation_reaches_the_very_next_request_and_lasts_the_session() {
        let read = FakeTool::reading("read_file", "x");
        let harness = Harness::build(Setup {
            tools: vec![
                read.clone(),
                FakeTool::writing("write_file", "y"),
                tool_search_tool(),
            ],
            config: lazy(&[]),
            turns: vec![
                ScriptedTurn::calls(vec![tool_call(
                    "c1",
                    "tool_search",
                    &json!({"activate": ["read_file", "ghost"]}),
                )]),
                ScriptedTurn::calls(vec![tool_call("c2", "read_file", &json!({"path": "a"}))]),
                ScriptedTurn::text("done"),
                ScriptedTurn::text("still here"),
            ],
            ..Setup::default()
        });

        let (events, result) = harness.say("web:1", "go").await;
        assert_eq!(result.unwrap().iterations, 3);
        let requests = harness.provider.requests();
        assert_eq!(names(&requests[0].tools), vec!["tool_search"]);
        // The request right after the activating batch already carries it.
        assert_eq!(names(&requests[1].tools), vec!["tool_search", "read_file"]);
        assert_eq!(names(&requests[2].tools), vec!["tool_search", "read_file"]);
        assert_eq!(read.calls().len(), 1);

        // No schema in the transcript, and the miss is answered.
        let results = events_of(&events, "tool.result");
        let content = results[0]["content"].as_str().unwrap();
        assert!(content.contains("Activated: read_file."), "{content}");
        assert!(content.contains("Unknown tool \"ghost\""), "{content}");
        assert!(!content.contains("properties"), "{content}");

        // The next turn on the same session starts with it; another session
        // does not.
        let _ = harness.say("web:1", "again").await;
        assert_eq!(
            names(&harness.provider.requests()[3].tools),
            vec!["tool_search", "read_file"]
        );
        assert_eq!(
            names(&harness.agent_loop.tool_definitions("web:2")),
            vec!["tool_search"]
        );
    }

    #[tokio::test]
    async fn activations_are_advertised_in_the_order_asked_never_re_sorted() {
        let harness = Harness::build(setup(
            vec![
                ScriptedTurn::calls(vec![tool_call(
                    "c1",
                    "tool_search",
                    &json!({"activate": ["write_file", "read_file"]}),
                )]),
                ScriptedTurn::text("done"),
            ],
            &["memory"],
        ));
        let _ = harness.say("web:1", "go").await;
        assert_eq!(
            names(&harness.provider.requests()[1].tools),
            vec!["tool_search", "memory", "write_file", "read_file"]
        );
    }

    #[tokio::test]
    async fn a_hidden_tool_called_by_name_runs_and_is_activated_by_that_call() {
        let read = FakeTool::reading("read_file", "x");
        let harness = Harness::build(Setup {
            tools: vec![
                read.clone(),
                FakeTool::writing("write_file", "y"),
                tool_search_tool(),
            ],
            config: lazy(&[]),
            turns: vec![
                ScriptedTurn::calls(vec![tool_call("c1", "read_file", &json!({"path": "a"}))]),
                ScriptedTurn::text("done"),
            ],
            ..Setup::default()
        });
        let (events, _) = harness.say("web:1", "go").await;
        assert_eq!(read.calls().len(), 1);
        assert_eq!(events_of(&events, "tool.result")[0]["ok"], json!(true));
        assert_eq!(
            names(&harness.provider.requests()[1].tools),
            vec!["tool_search", "read_file"]
        );
    }

    #[tokio::test]
    async fn an_override_for_tool_search_reaches_the_request() {
        let mut overrides = darkwire_protocol::ToolPromptOverrides::new();
        overrides.insert(
            "tool_search".to_owned(),
            darkwire_protocol::ToolPromptOverride {
                description: "Look things up.".to_owned(),
                fields: indexmap::IndexMap::new(),
            },
        );
        let harness = Harness::build(Setup {
            agent: Some(LoopAgent {
                tool_prompts: Some(overrides),
                ..LoopAgent::default()
            }),
            ..setup(vec![ScriptedTurn::text("ok")], &[])
        });
        let _ = harness.say("web:1", "hi").await;
        let tools = &harness.provider.requests()[0].tools;
        assert_eq!(tools[0].name, "tool_search");
        assert_eq!(tools[0].description, "Look things up.");
    }

    #[tokio::test]
    async fn a_call_written_as_text_to_a_hidden_tool_is_still_corrected() {
        let harness = Harness::build(setup(
            vec![
                ScriptedTurn::text(r#"{"name": "read_file", "arguments": {"path": "a"}}"#),
                ScriptedTurn::text("Sorry."),
            ],
            &[],
        ));
        let (_, result) = harness.say("web:1", "go").await;
        assert_eq!(result.unwrap().iterations, 2);
    }
}
