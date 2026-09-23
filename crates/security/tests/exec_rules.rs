//! Command rules: parsing, matching, precedence, the shell ceiling, and the
//! check a rule saved from a prompt has to pass.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a rule that cannot parse is a failing test either way"
)]

use darkwire_core::ErrorKind;
use darkwire_protocol::{CommandPolicy, ExecRule, ExecToolConfig, ToolPermission};
use darkwire_security::{
    ExecRules, assert_exec_rules, assert_standing_rule, exec_verdict, invocation_digest, is_shell,
    parse_exec_rule,
};
use proptest::prelude::*;

use ToolPermission::{Allow, Ask, Deny};

fn rule(action: ToolPermission, argv: &[&str]) -> ExecRule {
    ExecRule {
        action,
        argv: argv.iter().map(|&token| token.to_owned()).collect(),
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|&part| part.to_owned()).collect()
}

fn verdict(rules: &[ExecRule], call: &[&str]) -> ToolPermission {
    let parsed = ExecRules::parse(rules).unwrap();
    exec_verdict(&parsed, &argv(call), Ask, Ask).permission
}

fn command(call: &[&str]) -> CommandPolicy {
    CommandPolicy {
        argv: argv(call),
        shell: call.first().is_some_and(|program| is_shell(program)),
        rule: None,
    }
}

#[test]
fn refuses_a_rule_that_cannot_mean_anything() {
    for (argv, needle) in [
        (vec![], "empty"),
        (vec!["cargo", "*", "test"], "last token"),
        (vec![""], "empty program"),
        (vec!["/usr/bin/git"], "path"),
        (vec!["bin\\git.exe"], "path"),
        (vec!["git", "a\0b"], "NUL"),
    ] {
        let error = parse_exec_rule(&rule(Allow, &argv), 2).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Config, "{argv:?}");
        assert!(error.message.contains("rule 3"), "{}", error.message);
        assert!(error.message.contains(needle), "{}", error.message);
        assert_eq!(error.details["rule"], 2);
    }
}

#[test]
fn accepts_a_catch_all() {
    assert!(parse_exec_rule(&rule(Deny, &["*"]), 0).is_ok());
}

#[test]
fn matches_the_program_by_basename_and_the_rest_exactly() {
    let git = [rule(Allow, &["git", "status"])];
    for program in ["git", "/usr/bin/git", "git.exe", "C:\\Git\\git.exe"] {
        assert_eq!(verdict(&git, &[program, "status"]), Allow, "{program}");
    }
    assert_eq!(verdict(&git, &["git", "Status"]), Ask);
    assert_eq!(verdict(&git, &["git", "status", "-s"]), Ask);
    assert_eq!(verdict(&git, &["git"]), Ask);
    // A pattern's program is reduced the same way.
    assert_eq!(verdict(&[rule(Allow, &["git.exe"])], &["git"]), Allow);
}

#[test]
fn a_trailing_star_matches_none_or_more() {
    let rules = [rule(Allow, &["cargo", "test", "*"])];
    assert_eq!(verdict(&rules, &["cargo", "test"]), Allow);
    assert_eq!(verdict(&rules, &["cargo", "test", "-p", "x"]), Allow);
    assert_eq!(verdict(&rules, &["cargo", "build"]), Ask);
    assert_eq!(verdict(&rules, &["cargo"]), Ask);
    assert_eq!(
        verdict(&[rule(Allow, &["*"])], &["anything", "at", "all"]),
        Allow
    );
}

#[test]
fn falls_back_to_the_tools_permission_when_nothing_matches() {
    let parsed = ExecRules::parse(&[rule(Deny, &["rm", "*"])]).unwrap();
    for fallback in [Allow, Ask] {
        let verdict = exec_verdict(&parsed, &argv(&["ls"]), fallback, Ask);
        assert_eq!(verdict.permission, fallback);
        assert!(verdict.rule.is_none());
    }
}

#[test]
fn the_more_specific_rule_wins_whatever_the_order() {
    // More literal tokens.
    let rules = [
        rule(Deny, &["cargo", "test", "--release", "*"]),
        rule(Allow, &["cargo", "test", "*"]),
    ];
    assert_eq!(verdict(&rules, &["cargo", "test", "--release"]), Deny);
    assert_eq!(verdict(&rules, &["cargo", "test", "-p", "x"]), Allow);
    let reversed: Vec<ExecRule> = rules.iter().rev().cloned().collect();
    assert_eq!(verdict(&reversed, &["cargo", "test", "--release"]), Deny);

    // An allow-list: a catch-all deny under specific allows.
    let allow_list = [rule(Allow, &["git", "*"]), rule(Deny, &["*"])];
    assert_eq!(verdict(&allow_list, &["git", "log"]), Allow);
    assert_eq!(verdict(&allow_list, &["curl", "x"]), Deny);

    // An exact rule over a wildcard of the same length.
    let exact = [
        rule(Allow, &["git", "push", "*"]),
        rule(Ask, &["git", "push"]),
    ];
    assert_eq!(verdict(&exact, &["git", "push"]), Ask);

    // Then the stricter action.
    let tie = [rule(Allow, &["make", "*"]), rule(Ask, &["make", "*"])];
    assert_eq!(verdict(&tie, &["make"]), Ask);
    let tie = [rule(Deny, &["make", "*"]), rule(Ask, &["make", "*"])];
    assert_eq!(verdict(&tie, &["make"]), Deny);
}

#[test]
fn reports_the_first_of_two_identical_rules() {
    let rules = [rule(Allow, &["ls"]), rule(Allow, &["ls"])];
    let parsed = ExecRules::parse(&rules).unwrap();
    assert_eq!(parsed.decide(&argv(&["ls"])).unwrap().index, 0);
}

#[test]
fn caps_a_shell_at_the_shell_permission() {
    let parsed = ExecRules::parse(&[
        rule(Allow, &["*"]),
        rule(Allow, &["bash", "*"]),
        rule(Allow, &["bash", "ci.sh"]),
    ])
    .unwrap();
    let shell =
        |call: &[&str], fallback, ceiling| exec_verdict(&parsed, &argv(call), fallback, ceiling);

    // No wildcard lifts a shell above the ceiling.
    let capped = shell(&["bash", "other.sh"], Allow, Ask);
    assert_eq!(capped.permission, Ask);
    assert!(capped.shell);
    assert_eq!(shell(&["zsh", "x"], Allow, Ask).permission, Ask);
    // An exact rule decides a shell on its own.
    assert_eq!(shell(&["bash", "ci.sh"], Ask, Ask).permission, Allow);
    // A ceiling of allow leaves the rule's answer alone.
    assert_eq!(shell(&["bash", "other.sh"], Ask, Allow).permission, Allow);
    // And deny beats everything, the exact rule included.
    assert_eq!(shell(&["bash", "ci.sh"], Allow, Deny).permission, Deny);
    // Not a shell: the ceiling does not apply.
    let plain = shell(&["git", "status"], Ask, Deny);
    assert_eq!(plain.permission, Allow);
    assert!(!plain.shell);
    // A stricter rule is not relaxed by the ceiling.
    let strict = ExecRules::parse(&[rule(Deny, &["sh", "*"])]).unwrap();
    assert_eq!(
        exec_verdict(&strict, &argv(&["sh", "x"]), Ask, Allow).permission,
        Deny
    );
}

#[test]
fn knows_a_shell_by_its_basename() {
    for program in ["bash", "/bin/sh", "pwsh.exe", "cmd"] {
        assert!(is_shell(program), "{program}");
    }
    assert!(!is_shell("git"));
    assert!(!is_shell("bashful"));
}

#[test]
fn digests_one_exact_call() {
    let digest = invocation_digest(&argv(&["a", "b"]));
    assert_eq!(digest.len(), 64);
    assert_eq!(digest, invocation_digest(&argv(&["a", "b"])));
    assert_ne!(digest, invocation_digest(&argv(&["a b"])));
    assert_ne!(digest, invocation_digest(&argv(&["ab"])));
}

#[test]
fn saves_a_rule_that_approves_the_call_it_was_chosen_for() {
    let call = command(&["cargo", "test", "-p", "x"]);
    assert!(
        assert_standing_rule(&[], &rule(Allow, &["cargo", "test", "*"]), &call, Ask, Ask).is_ok()
    );
}

#[test]
fn refuses_a_rule_that_would_not_approve_the_call() {
    let call = command(&["cargo", "test", "--release"]);
    let refused = |rules: &[ExecRule], candidate: &ExecRule, call: &CommandPolicy| {
        let error = assert_standing_rule(rules, candidate, call, Ask, Ask).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        error.message
    };

    assert!(refused(&[], &rule(Ask, &["cargo", "*"]), &call).contains("must allow"));
    assert!(refused(&[], &rule(Allow, &["cargo", "build", "*"]), &call).contains("does not cover"));
    let shadow = [rule(Deny, &["cargo", "test", "--release"])];
    let message = refused(&shadow, &rule(Allow, &["cargo", "test", "*"]), &call);
    assert!(message.contains("cargo test --release"), "{message}");
    let shell = command(&["bash", "ci.sh"]);
    assert!(refused(&[], &rule(Allow, &["bash", "ci.sh"]), &shell).contains("shell"));

    let bad = assert_standing_rule(&[], &rule(Allow, &["cargo", "*", "x"]), &call, Ask, Ask);
    assert_eq!(bad.unwrap_err().kind, ErrorKind::Config);
}

#[test]
fn names_the_agent_when_its_rules_do_not_parse() {
    let config = ExecToolConfig {
        rules: vec![rule(Allow, &["git"]), rule(Allow, &["/bin/rm"])],
        ..ExecToolConfig::default()
    };
    let error = assert_exec_rules(&config, "coder").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.starts_with("Agent \"coder\": Command rule 2"));
    assert_eq!(error.details["agentId"], "coder");
    assert_eq!(error.details["rule"], 1);
    assert!(assert_exec_rules(&ExecToolConfig::default(), "coder").is_ok());
}

fn token() -> impl Strategy<Value = String> {
    "[a-z0-9=.-]{1,6}"
}

proptest! {
    #[test]
    fn any_prefix_and_a_star_matches_the_call(
        call in prop::collection::vec(token(), 1..6),
        cut in 1usize..6,
    ) {
        let cut = cut.min(call.len());
        let mut pattern: Vec<&str> = call[..cut].iter().map(String::as_str).collect();
        pattern.push("*");
        let parsed = ExecRules::parse(&[rule(Allow, &pattern)]).unwrap();
        prop_assert!(parsed.decide(&call).is_some());
    }
}
