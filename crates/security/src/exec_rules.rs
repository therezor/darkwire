//! Command rules: which `exec` calls run unattended, which ask, which are
//! refused.
//!
//! A rule is a typed argv pattern with an action, and it applies wherever the
//! agent runs. Each token matches one argument exactly, and a final `*` matches
//! any number of remaining arguments. The first token matches the program's basename,
//! through the same [`binary_name`] the guard uses, so `git` covers
//! `/usr/bin/git` and `git.exe`.
//!
//! Precedence is by specificity, not by list order, so a rule saved from an
//! approval prompt can never silently outrank a narrower one the operator
//! wrote by hand. The winner is the rule with, in turn:
//!
//!  1. more literal tokens;
//!  2. no trailing `*`;
//!  3. the stricter action, `deny` over `ask` over `allow`.
//!
//! That makes `deny *` plus a few `allow` rows an allow-list, and keeps
//! `deny cargo test --release` in force beside `allow cargo test *`.
//!
//! Rules decide whether a call may run unattended. They are evaluated where a
//! call is authorised, before it runs, and never in the guard: the guard
//! decides whether a command can run at all, and a rule that widened it would
//! make the jail a setting.

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{CommandPolicy, ExecRule, ExecToolConfig, ToolPermission};
use serde_json::Value;

use crate::environment::sha256_hex;
use crate::exec_guard::{SHELL_BINARIES, binary_name};

/// The token that matches any number of remaining arguments.
pub const REST: &str = "*";

/// A rule, checked and ready to match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRule {
    /// Where it sits in the agent's list.
    pub index: usize,
    /// The rule as written.
    pub source: ExecRule,
    literals: Vec<String>,
    rest: bool,
}

impl ParsedRule {
    /// The action a matching call gets.
    pub fn action(&self) -> ToolPermission {
        self.source.action
    }

    /// How many tokens must match exactly.
    pub fn literals(&self) -> usize {
        self.literals.len()
    }

    /// Whether the rule names the whole command, with no trailing `*`.
    pub fn is_exact(&self) -> bool {
        !self.rest
    }

    /// Whether the rule covers this call.
    pub fn matches(&self, argv: &[String]) -> bool {
        let long_enough = if self.rest {
            argv.len() >= self.literals.len()
        } else {
            argv.len() == self.literals.len()
        };
        long_enough
            && self
                .literals
                .iter()
                .zip(argv)
                .enumerate()
                .all(|(position, (literal, argument))| {
                    if position == 0 {
                        *literal == binary_name(argument)
                    } else {
                        literal == argument
                    }
                })
    }

    fn rank(&self) -> (usize, bool, u8) {
        (self.literals(), self.is_exact(), strictness(self.action()))
    }
}

fn strictness(permission: ToolPermission) -> u8 {
    match permission {
        ToolPermission::Allow => 0,
        ToolPermission::Ask => 1,
        ToolPermission::Deny => 2,
    }
}

fn stricter(a: ToolPermission, b: ToolPermission) -> ToolPermission {
    if strictness(a) >= strictness(b) { a } else { b }
}

fn refusal(index: usize, message: impl Into<String>) -> WireError {
    WireError::new(ErrorKind::Config, message).with_detail("rule", index)
}

/// Checks one rule. `index` is its place in the list, for the error.
pub fn parse_exec_rule(rule: &ExecRule, index: usize) -> Result<ParsedRule> {
    let position = index + 1;
    let Some((last, init)) = rule.argv.split_last() else {
        return Err(refusal(
            index,
            format!("Command rule {position} is empty. Name a program."),
        ));
    };
    if init.iter().any(|token| token == REST) {
        return Err(refusal(
            index,
            format!("Command rule {position}: \"*\" may only be the last token."),
        ));
    }
    if rule.argv.iter().any(|token| token.contains('\0')) {
        return Err(refusal(
            index,
            format!("Command rule {position} contains a NUL byte."),
        ));
    }
    let program = &rule.argv[0];
    if program.is_empty() {
        return Err(refusal(
            index,
            format!("Command rule {position} names an empty program."),
        ));
    }
    if program != REST && (program.contains('/') || program.contains('\\')) {
        return Err(refusal(
            index,
            format!(
                "Command rule {position} names a path ({program}). Write the program's name: a rule matches it wherever it is installed."
            ),
        ));
    }

    let rest = last == REST;
    let mut literals: Vec<String> = if rest {
        init.to_vec()
    } else {
        rule.argv.clone()
    };
    if let Some(first) = literals.first_mut() {
        *first = binary_name(first);
    }
    Ok(ParsedRule {
        index,
        source: rule.clone(),
        literals,
        rest,
    })
}

/// An agent's rules, parsed once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecRules {
    rules: Vec<ParsedRule>,
}

impl ExecRules {
    /// Fails on the first rule that does not parse.
    pub fn parse(rules: &[ExecRule]) -> Result<ExecRules> {
        let rules = rules
            .iter()
            .enumerate()
            .map(|(index, rule)| parse_exec_rule(rule, index))
            .collect::<Result<Vec<_>>>()?;
        Ok(ExecRules { rules })
    }

    /// The rule that decides this call, if any covers it.
    pub fn decide(&self, argv: &[String]) -> Option<&ParsedRule> {
        self.rules
            .iter()
            .filter(|rule| rule.matches(argv))
            // `max_by_key` keeps the last of equals; `rev` makes it the first,
            // so of two identical rules the one listed first is reported.
            .rev()
            .max_by_key(|rule| rule.rank())
    }
}

/// What the rules made of one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecVerdict<'a> {
    /// What the call gets.
    pub permission: ToolPermission,
    /// The rule that decided, if one did.
    pub rule: Option<&'a ParsedRule>,
    /// The program is a shell.
    pub shell: bool,
}

/// Whether a program is a shell, by the guard's own list.
pub fn is_shell(argv0: &str) -> bool {
    SHELL_BINARIES.contains(&binary_name(argv0).as_str())
}

/// The permission one call gets.
///
/// `fallback` is the tool's own permission, for a call no rule covers. `shell`
/// is the agent's ceiling for shells: `deny` refuses one outright, an exact
/// rule decides one on its own, and anything broader is capped at `shell`.
/// A wildcard cannot approve a shell, because `bash *` covers every program
/// there is.
pub fn exec_verdict<'a>(
    rules: &'a ExecRules,
    argv: &[String],
    fallback: ToolPermission,
    shell: ToolPermission,
) -> ExecVerdict<'a> {
    let rule = rules.decide(argv);
    let is_shell_call = argv.first().is_some_and(|program| is_shell(program));
    let base = rule.map_or(fallback, ParsedRule::action);
    let permission = if !is_shell_call {
        base
    } else if shell == ToolPermission::Deny {
        ToolPermission::Deny
    } else if rule.is_some_and(ParsedRule::is_exact) {
        base
    } else {
        stricter(base, shell)
    };
    ExecVerdict {
        permission,
        rule,
        shell: is_shell_call,
    }
}

/// A stable key for one exact call.
///
/// Hashed as a JSON array, so `["a b"]` and `["a", "b"]` cannot collide.
pub fn invocation_digest(argv: &[String]) -> String {
    let parts: Vec<Value> = argv
        .iter()
        .map(|argument| Value::from(argument.as_str()))
        .collect();
    sha256_hex(Value::Array(parts).to_string().as_bytes())
}

fn standing(message: impl Into<String>) -> WireError {
    WireError::new(ErrorKind::InvalidInput, message)
}

/// Refuses a rule saved from a prompt unless it approves the call it was
/// saved for.
///
/// The prompt is answering one call. A rule that does not cover it, or that a
/// more specific rule would still override, would save something the operator
/// did not see work, and leave the call waiting for an answer it already got.
pub fn assert_standing_rule(
    rules: &[ExecRule],
    rule: &ExecRule,
    command: &CommandPolicy,
    fallback: ToolPermission,
    shell: ToolPermission,
) -> Result<()> {
    if rule.action != ToolPermission::Allow {
        return Err(standing(
            "A rule saved from a prompt must allow the command.",
        ));
    }
    if command.shell {
        return Err(standing(
            "A shell cannot be allowed from a prompt: a rule for it would cover every program. Approve it once or for this session.",
        ));
    }
    let parsed = parse_exec_rule(rule, rules.len())?;
    if !parsed.matches(&command.argv) {
        return Err(standing(format!(
            "The rule \"{}\" does not cover this command.",
            rule.argv.join(" ")
        )));
    }
    let mut all = rules.to_vec();
    all.push(rule.clone());
    let parsed_all = ExecRules::parse(&all)?;
    let verdict = exec_verdict(&parsed_all, &command.argv, fallback, shell);
    if verdict.permission != ToolPermission::Allow {
        let winner = verdict
            .rule
            .map_or_else(String::new, |winner| winner.source.argv.join(" "));
        return Err(standing(format!(
            "The rule \"{winner}\" is more specific and would still apply to this command. Change it in the agent's settings."
        )));
    }
    Ok(())
}

/// Refuses an agent whose rules do not parse, naming the agent.
///
/// Raised where the agent is resolved, so a bad rule is a config error at
/// save time rather than a refusal the first time the model runs a command.
pub fn assert_exec_rules(config: &ExecToolConfig, agent_id: &str) -> Result<()> {
    ExecRules::parse(&config.rules).map_err(|error| {
        let mut refused = WireError::new(
            ErrorKind::Config,
            format!("Agent \"{agent_id}\": {}", error.message),
        )
        .with_detail("agentId", agent_id);
        if let Some(rule) = error.details.get("rule") {
            refused = refused.with_detail("rule", rule.clone());
        }
        refused
    })?;
    Ok(())
}
