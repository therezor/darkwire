//! Saving an `exec` rule from an approval prompt.
//!
//! The prompt sends the rule with its answer, and the hub hands it here before
//! it releases the call. Server side rather than a second request from the
//! browser, so the check and the write see the same settings, and so a channel
//! that answers through the hub gets the same path.

use std::sync::Arc;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{CommandPolicy, Config, ConfigPatch, ExecRule, with_exec_rule};
use darkwire_security::assert_standing_rule;
use darkwire_tools::permission_for;
use parking_lot::Mutex;

use crate::runtime::ServerRuntime;

/// Adds a rule to an agent, or says why it cannot.
///
/// Takes the agent, the command the prompt was answering, and the rule.
pub type RuleWriter = Arc<dyn Fn(&str, &CommandPolicy, &ExecRule) -> Result<()> + Send + Sync>;

/// The writer over a runtime's settings.
///
/// One save at a time. Each reads the settings, checks the rule against them
/// and writes them back, and two interleaved would drop the first rule.
pub fn rule_writer(runtime: Arc<dyn ServerRuntime>) -> RuleWriter {
    let lock = Arc::new(Mutex::new(()));
    Arc::new(move |agent_id, command, rule| {
        let serialised = lock.lock();
        let saved = append_exec_rule(runtime.as_ref(), agent_id, command, rule);
        drop(serialised);
        saved
    })
}

/// Checks that `rule` approves `command` on this agent, then saves it.
pub fn append_exec_rule(
    runtime: &dyn ServerRuntime,
    agent_id: &str,
    command: &CommandPolicy,
    rule: &ExecRule,
) -> Result<()> {
    let patch = checked_rule_patch(&runtime.config(), agent_id, command, rule)?;
    runtime.apply_settings(patch)?;
    Ok(())
}

/// The patch that adds `rule` to an agent, once it is known to approve
/// `command` there.
///
/// Apart from the write so a process without a [`ServerRuntime`], the terminal
/// chat, saves a rule through the same check.
pub fn checked_rule_patch(
    config: &Config,
    agent_id: &str,
    command: &CommandPolicy,
    rule: &ExecRule,
) -> Result<ConfigPatch> {
    let Some(entry) = config.agents.list.get(agent_id) else {
        return Err(WireError::new(
            ErrorKind::NotFound,
            format!("No agent \"{agent_id}\" to save the rule on."),
        ));
    };
    let exec = &entry.settings.exec;
    assert_standing_rule(
        &exec.rules,
        rule,
        command,
        permission_for(Some(&entry.tools), "exec"),
        exec.shell,
    )?;
    Ok(with_exec_rule(config, agent_id, rule.clone()))
}
