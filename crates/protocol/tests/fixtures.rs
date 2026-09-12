//! The parity oracle, read back.
//!
//! `fixtures/protocol/<function>.json`, `fixtures/ws/frames/<type>.json` and
//! `fixtures/uuid/v7.json` are written by the TypeScript suite
//! (`packages/protocol/test/fixtures.test.ts`) from the implementation that is
//! the source of truth. This runs every case through the Rust port and asserts
//! the same answer, so the two languages cannot drift on a function whose
//! whole job is that they agree. Every file must be consumed: a fixture with no
//! Rust counterpart is a function that was not ported, and a Rust counterpart
//! with no file is a case nothing pins.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use ghostai_protocol::{
    AgentPreset, AgentSettingsChange, ClientMessage, Config, DEFAULT_LIVE_STATE_TEMPLATE,
    DEFAULT_MEMORY_TEMPLATE, DEFAULT_PLATFORM_HOST_TEMPLATE, DEFAULT_PLATFORM_TOOLBOX_TEMPLATE,
    DEFAULT_SKILLS_TEMPLATE, DEFAULT_SYSTEM_PROMPT_TEMPLATE, DEFAULT_TOOL_POLICY_TEMPLATE,
    DEFAULT_TOOLBOX_TEMPLATE, DEFAULT_WRAP_UP_TEMPLATE, LIVE_PROMPT_PLACEHOLDERS,
    MEMORY_PROMPT_PLACEHOLDERS, PLATFORM_PROMPT_PLACEHOLDERS, PROMPT_PLACEHOLDERS,
    RAW_PROMPT_PLACEHOLDERS, SKILLS_PROMPT_PLACEHOLDERS, ServerMessage, SubagentRunRef,
    TOOL_POLICY_PLACEHOLDERS, TOOLBOX_PROMPT_PLACEHOLDERS, ToolDefinition, ToolPromptOverrides,
    TurnTiming, UNSEQUENCED_SERVER_EVENTS, Usage, UuidRandom, agent_settings_patch,
    apply_tool_prompts, default_subagent_prompt, derive_agent_id, derive_workspace_id,
    effective_tool_policy, is_loopback_host, is_slug_id, names_delimiter, new_uuid,
    preset_to_agent_entry, render_prompt_template, render_wrap_up, slugify, subagent_runs_of,
    subagent_tool_name, tokens_per_second, tool_policy_uses_nonce, unknown_placeholders,
    with_subagent_run,
};
use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use common::{canonical_numbers, diff};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    files
}

fn arg<T: DeserializeOwned>(input: &Value, name: &str) -> T {
    let value = input
        .get(name)
        .unwrap_or_else(|| panic!("input has no `{name}`"));
    serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("input `{name}`: {e}"))
}

fn optional_arg<T: DeserializeOwned>(input: &Value, name: &str) -> Option<T> {
    input
        .get(name)
        .map(|value| serde_json::from_value(value.clone()).unwrap())
}

fn to_value<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap()
}

fn option_value<T: serde::Serialize>(value: Option<T>) -> Value {
    value.map_or(Value::Null, to_value)
}

fn strs(known: &[String]) -> Vec<&str> {
    known.iter().map(String::as_str).collect()
}

/// The constants a `default:` or `vocabulary:` case pins byte for byte.
fn named_constant(name: &str) -> Option<Value> {
    Some(match name {
        "DEFAULT_SYSTEM_PROMPT_TEMPLATE" => json!(DEFAULT_SYSTEM_PROMPT_TEMPLATE),
        "DEFAULT_LIVE_STATE_TEMPLATE" => json!(DEFAULT_LIVE_STATE_TEMPLATE),
        "DEFAULT_WRAP_UP_TEMPLATE" => json!(DEFAULT_WRAP_UP_TEMPLATE),
        "DEFAULT_PLATFORM_HOST_TEMPLATE" => json!(DEFAULT_PLATFORM_HOST_TEMPLATE),
        "DEFAULT_PLATFORM_TOOLBOX_TEMPLATE" => json!(DEFAULT_PLATFORM_TOOLBOX_TEMPLATE),
        "DEFAULT_TOOLBOX_TEMPLATE" => json!(DEFAULT_TOOLBOX_TEMPLATE),
        "DEFAULT_TOOL_POLICY_TEMPLATE" => json!(DEFAULT_TOOL_POLICY_TEMPLATE),
        "DEFAULT_SKILLS_TEMPLATE" => json!(DEFAULT_SKILLS_TEMPLATE),
        "DEFAULT_MEMORY_TEMPLATE" => json!(DEFAULT_MEMORY_TEMPLATE),
        "PROMPT_PLACEHOLDERS" => json!(PROMPT_PLACEHOLDERS),
        "LIVE_PROMPT_PLACEHOLDERS" => json!(LIVE_PROMPT_PLACEHOLDERS),
        "PLATFORM_PROMPT_PLACEHOLDERS" => json!(PLATFORM_PROMPT_PLACEHOLDERS),
        "TOOLBOX_PROMPT_PLACEHOLDERS" => json!(TOOLBOX_PROMPT_PLACEHOLDERS),
        "TOOL_POLICY_PLACEHOLDERS" => json!(TOOL_POLICY_PLACEHOLDERS),
        "MEMORY_PROMPT_PLACEHOLDERS" => json!(MEMORY_PROMPT_PLACEHOLDERS),
        "SKILLS_PROMPT_PLACEHOLDERS" => json!(SKILLS_PROMPT_PLACEHOLDERS),
        "RAW_PROMPT_PLACEHOLDERS" => json!(RAW_PROMPT_PLACEHOLDERS),
        _ => return None,
    })
}

/// A case whose name pins a constant asserts the input *is* that constant, so
/// the fixture proves the Rust text matches and not only that the function
/// agrees on whatever text it was handed.
fn check_pinned_constant(case_name: &str, input: &Value) {
    if let Some(constant) = case_name.strip_prefix("default: ") {
        assert_eq!(
            input["template"],
            named_constant(constant).unwrap(),
            "{constant} differs"
        );
    }
    if let Some(constant) = case_name.strip_prefix("vocabulary: ") {
        assert_eq!(
            input["known"],
            named_constant(constant).unwrap(),
            "{constant} differs"
        );
    }
}

/// Every dual-implemented function, by the name the fixture files use.
const FUNCTIONS: &[&str] = &[
    "renderPromptTemplate",
    "unknownPlaceholders",
    "renderWrapUp",
    "effectiveToolPolicy",
    "toolPolicyUsesNonce",
    "namesDelimiter",
    "tokensPerSecond",
    "turnRate",
    "isLoopbackHost",
    "agentSettingsPatch",
    "slugify",
    "deriveAgentId",
    "deriveWorkspaceId",
    "isSlugId",
    "subagentToolName",
    "subagentRunsOf",
    "withSubagentRun",
    "defaultSubagentPrompt",
    "presetToAgentEntry",
    "applyToolPrompts",
    "isSequencedServerMessage",
];

fn run(function: &str, input: &Value) -> Value {
    match function {
        "renderPromptTemplate" => {
            let values: IndexMap<String, String> = arg(input, "values");
            json!(render_prompt_template(
                &arg::<String>(input, "template"),
                &values
            ))
        }
        "unknownPlaceholders" => {
            let known: Vec<String> = optional_arg(input, "known").unwrap_or_else(|| {
                PROMPT_PLACEHOLDERS
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect()
            });
            json!(unknown_placeholders(
                &arg::<String>(input, "template"),
                &strs(&known)
            ))
        }
        "renderWrapUp" => {
            json!(render_wrap_up(
                &arg::<String>(input, "template"),
                arg(input, "iterationsLeft")
            ))
        }
        "effectiveToolPolicy" => {
            let template: Option<String> = optional_arg(input, "template");
            json!(effective_tool_policy(template.as_deref()))
        }
        "toolPolicyUsesNonce" => {
            let template: Option<String> = optional_arg(input, "template");
            json!(tool_policy_uses_nonce(template.as_deref()))
        }
        "namesDelimiter" => json!(names_delimiter(&arg::<String>(input, "template"))),
        "tokensPerSecond" => {
            let usage: Usage = arg(input, "usage");
            option_value(tokens_per_second(&usage, arg(input, "elapsedMs")))
        }
        "turnRate" => {
            let usage: Usage = arg(input, "usage");
            let timing: TurnTiming = arg(input, "timing");
            option_value(ghostai_protocol::turn_rate(&usage, &timing))
        }
        "isLoopbackHost" => json!(is_loopback_host(&arg::<String>(input, "host"))),
        "agentSettingsPatch" => {
            let config: Config = arg(input, "config");
            let changes: AgentSettingsChange = arg(input, "changes");
            to_value(agent_settings_patch(
                &config,
                &arg::<String>(input, "agentId"),
                &changes,
            ))
        }
        "slugify" => {
            let reserved: Vec<String> = arg(input, "reserved");
            json!(slugify(
                &arg::<String>(input, "name"),
                &strs(&reserved),
                &arg::<String>(input, "fallback")
            ))
        }
        "deriveAgentId" => json!(derive_agent_id(&arg::<String>(input, "label"))),
        "deriveWorkspaceId" => json!(derive_workspace_id(&arg::<String>(input, "name"))),
        "isSlugId" => json!(is_slug_id(&arg::<String>(input, "value"))),
        "subagentToolName" => json!(subagent_tool_name(&arg::<String>(input, "agentId"))),
        "subagentRunsOf" => to_value(subagent_runs_of(&arg(input, "metadata"))),
        "withSubagentRun" => {
            let run: SubagentRunRef = arg(input, "run");
            to_value(with_subagent_run(
                &arg(input, "metadata"),
                &arg::<String>(input, "callId"),
                &run,
            ))
        }
        "defaultSubagentPrompt" => json!(default_subagent_prompt(&arg::<String>(input, "label"))),
        "presetToAgentEntry" => {
            let preset: AgentPreset = arg(input, "preset");
            to_value(preset_to_agent_entry(&preset))
        }
        "applyToolPrompts" => {
            let definitions: Vec<ToolDefinition> = arg(input, "definitions");
            let overrides: ToolPromptOverrides = arg(input, "overrides");
            to_value(apply_tool_prompts(&definitions, &overrides))
        }
        "isSequencedServerMessage" => {
            let message: ServerMessage = arg(input, "message");
            json!(message.seq().is_some())
        }
        other => panic!("no Rust port is registered for `{other}`"),
    }
}

#[test]
fn every_function_fixture_agrees() {
    let files = json_files(&fixtures_dir().join("protocol"));
    let mut seen = BTreeSet::new();
    let mut report = String::new();
    for path in &files {
        let fixture = read_json(path);
        let function = fixture["function"].as_str().unwrap();
        assert_eq!(
            format!("{function}.json"),
            path.file_name().unwrap().to_string_lossy(),
            "file name and function disagree"
        );
        seen.insert(function.to_owned());
        for case in fixture["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            check_pinned_constant(name, &case["input"]);
            let expected = canonical_numbers(case["output"].clone());
            let actual = canonical_numbers(run(function, &case["input"]));
            if expected != actual {
                let _ = writeln!(report, "{function} / {name}:");
                diff("", &expected, &actual, &mut report);
            }
        }
    }
    assert!(
        report.is_empty(),
        "the Rust port disagrees with the fixtures:\n{report}"
    );

    let expected: BTreeSet<String> = FUNCTIONS.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(
        seen, expected,
        "every ported function has a fixture and every fixture a port"
    );
}

#[test]
fn every_ws_frame_round_trips() {
    let files = json_files(&fixtures_dir().join("ws/frames"));
    let mut client = BTreeSet::new();
    let mut server = BTreeSet::new();
    for path in &files {
        let fixture = read_json(path);
        let frame = &fixture["frame"];
        let kind = frame["type"].as_str().unwrap().to_owned();
        assert_eq!(
            format!("{kind}.json"),
            path.file_name().unwrap().to_string_lossy()
        );
        match fixture["direction"].as_str().unwrap() {
            "client" => {
                let parsed: ClientMessage =
                    serde_json::from_value(frame.clone()).unwrap_or_else(|e| panic!("{kind}: {e}"));
                assert_eq!(parsed.tag(), kind);
                assert_eq!(
                    canonical_numbers(to_value(&parsed)),
                    canonical_numbers(frame.clone()),
                    "{kind} did not round-trip"
                );
                client.insert(kind);
            }
            "server" => {
                let parsed: ServerMessage =
                    serde_json::from_value(frame.clone()).unwrap_or_else(|e| panic!("{kind}: {e}"));
                assert_eq!(parsed.tag(), kind);
                assert_eq!(
                    canonical_numbers(to_value(&parsed)),
                    canonical_numbers(frame.clone()),
                    "{kind} did not round-trip"
                );
                assert_eq!(
                    parsed.seq().is_none(),
                    UNSEQUENCED_SERVER_EVENTS.contains(&kind.as_str()),
                    "{kind}: seq() disagrees with UNSEQUENCED_SERVER_EVENTS"
                );
                assert_eq!(parsed.seq(), frame.get("seq").and_then(Value::as_u64));
                server.insert(kind);
            }
            other => panic!("unknown direction {other}"),
        }
    }
    let expect = |values: &[&str]| {
        values
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(client, expect(ClientMessage::VALUES));
    assert_eq!(server, expect(ServerMessage::VALUES));
}

#[test]
fn every_uuid_case_agrees() {
    let fixture = read_json(&fixtures_dir().join("uuid/v7.json"));
    let cases = fixture["cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let hex = case["random"].as_str().unwrap();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let random: UuidRandom = bytes.try_into().unwrap();
        assert_eq!(
            new_uuid(case["nowMs"].as_u64().unwrap(), &random),
            case["uuid"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
}
