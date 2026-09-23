//! Saving an `exec` rule from an approval prompt, against a runtime's
//! settings.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_core::ErrorKind;
use darkwire_protocol::config::{Config, parse_config};
use darkwire_protocol::{CommandPolicy, ExecRule, ToolPermission};
use darkwire_server::exec_rules::{append_exec_rule, rule_writer};
use darkwire_server::runtime::ServerRuntime;
use darkwire_server::testkit::{TestServer, TestServerOptions, start_test_server};
use serde_json::json;

fn server(config: Config) -> TestServer {
    start_test_server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    })
    .expect("a test server")
}

fn coder() -> Config {
    parse_config(json!({"agents": {"list": {"coder": {
        "label": "Coder",
        "tools": {"exec": "ask"},
        "exec": {"rules": [{"action": "deny", "argv": ["cargo", "test", "--release"]}]},
    }}}}))
    .unwrap()
}

fn rule(argv: &[&str]) -> ExecRule {
    ExecRule {
        action: ToolPermission::Allow,
        argv: argv.iter().map(|&token| token.to_owned()).collect(),
    }
}

fn command(argv: &[&str]) -> CommandPolicy {
    CommandPolicy {
        argv: argv.iter().map(|&token| token.to_owned()).collect(),
        shell: false,
        rule: None,
    }
}

fn rules_of(test: &TestServer) -> Vec<ExecRule> {
    test.runtime.config().agents.list["coder"]
        .settings
        .exec
        .rules
        .clone()
}

#[test]
fn saves_a_rule_that_approves_the_call_and_keeps_the_rest_of_the_agent() {
    let test = server(coder());
    append_exec_rule(
        test.runtime.as_ref(),
        "coder",
        &command(&["cargo", "test", "-p", "x"]),
        &rule(&["cargo", "test", "*"]),
    )
    .unwrap();

    let rules = rules_of(&test);
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[1], rule(&["cargo", "test", "*"]));
    let entry = &test.runtime.config().agents.list["coder"];
    assert_eq!(entry.label, "Coder");
    assert_eq!(entry.tools["exec"], ToolPermission::Ask);
    assert_eq!(test.runtime.patches().len(), 1);
}

#[test]
fn refuses_a_rule_a_narrower_one_would_override_and_writes_nothing() {
    let test = server(coder());
    let error = append_exec_rule(
        test.runtime.as_ref(),
        "coder",
        &command(&["cargo", "test", "--release"]),
        &rule(&["cargo", "test", "*"]),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(
        error.message.contains("cargo test --release"),
        "{}",
        error.message
    );
    assert!(test.runtime.patches().is_empty());
    assert_eq!(rules_of(&test).len(), 1);
}

#[test]
fn refuses_an_agent_that_is_not_there() {
    let test = server(coder());
    let error = append_exec_rule(
        test.runtime.as_ref(),
        "nobody",
        &command(&["ls"]),
        &rule(&["ls"]),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
    assert!(test.runtime.patches().is_empty());
}

#[test]
fn the_writer_saves_through_the_runtime() {
    let test = server(coder());
    let writer = rule_writer(Arc::clone(&test.runtime) as Arc<dyn ServerRuntime>);
    writer(
        "coder",
        &command(&["git", "status"]),
        &rule(&["git", "status"]),
    )
    .unwrap();
    writer("coder", &command(&["git", "log"]), &rule(&["git", "log"])).unwrap();
    assert_eq!(rules_of(&test).len(), 3);
}
