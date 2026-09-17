//! `darkwire environment list`: where an agent's commands would run.
//!
//! An environment definition decides what the machine running `exec` is allowed
//! to be: its image, its capabilities, who it runs as. It does not decide *what*
//! an agent may call. The tool permissions in `config.yaml` do that, and the
//! command itself is whatever the model asked `exec` to run.
//!
//! The listing names anything the definition weakened, and whether a restricted
//! egress allow-list could be enforced in it at all. Both are the difference
//! between an environment that bounds an agent and one that only looks as though
//! it does.

use std::io::Write;

use darkwire_core::{LoadConfigOptions, Result, WireError, load_config};
use darkwire_protocol::environment::EnvironmentDefinition;
use darkwire_security::{PolicyStore, assert_gateway_compatible, weakened_in};

use crate::Streams;
use crate::i18n::Env;
use crate::program::Globals;
use crate::runtime::load_options;

/// Everything about a definition that bears on whether it is safe to approve.
fn describe(environment: &EnvironmentDefinition) -> Vec<String> {
    let mut lines = vec![
        format!("    image      {}", environment.image),
        format!("    user       {}", environment.user),
        format!("    workdir    {}", environment.workdir),
        format!(
            "    limits     {} MB, {} cpu, {} pids",
            environment.limits.memory_mb, environment.limits.cpus, environment.limits.pids_max
        ),
    ];
    if !environment.caps.add.is_empty() {
        lines.push(format!(
            "    caps       +{}",
            environment.caps.add.join(" +")
        ));
    }
    // What every agent running here is told. Printed because "read a definition
    // before an agent uses it" is what this command is for, and the prose is
    // the only part of one the model ever sees.
    if let Some(prompt) = environment.prompt.as_deref().map(str::trim)
        && !prompt.is_empty()
    {
        let mut said = prompt.lines();
        if let Some(first) = said.next() {
            lines.push(format!("    says       {first}"));
        }
        for line in said {
            lines.push(format!("               {line}"));
        }
    }
    if let Err(error) = assert_gateway_compatible(environment) {
        // Not a refusal: an environment with no network is perfectly usable, and
        // the operator may never ask this one for an allow-list. It is printed
        // because discovering it on a settings save, after choosing this
        // environment for that exact purpose, is the worse order to learn it in.
        lines.push(format!(
            "    egress     restricted mode unavailable: {}",
            error.message
        ));
    }
    for warning in weakened_in(environment) {
        lines.push(format!("    {warning}  <-- review this"));
    }
    lines
}

/// Runs one `darkwire environment` invocation and answers with its exit code.
pub fn run(globals: &Globals, env: &Env, streams: &mut Streams) -> Result<u8> {
    let loaded = load_config(LoadConfigOptions {
        paths: load_options(globals, None, env),
        file: None,
    })?;
    let store = PolicyStore::new(loaded.paths.policy_dir.clone());
    match act(&store, streams) {
        Ok(code) => Ok(code),
        Err(error) => {
            let _ = writeln!(streams.err, "{}", error.message);
            Ok(1)
        }
    }
}

fn act(store: &PolicyStore, streams: &mut Streams) -> Result<u8> {
    let listing = store.list_environments();
    if listing.is_empty() {
        writeln!(
            streams.out,
            "No environments installed under {}",
            store.root().join("environments").display()
        )
        .map_err(WireError::from)?;
        // The one case where an empty list is not the truth it looks like. A
        // definition written against `darkwire.container/1` lived in
        // `containers/`, so an install that has not migrated reports nothing
        // installed while the files are still sitting there, and the operator
        // has no reason to suspect a rename. Naming the directory turns a
        // silent empty list into the instruction it should have been.
        let old = store.root().join("containers");
        if old.exists() {
            writeln!(
                streams.out,
                "\n{} still exists. Environments moved there from `containers/` and the\n  \
                 schema tag is now `darkwire.environment/1`; see docs/environments.md.",
                old.display()
            )
            .map_err(WireError::from)?;
        }
        return Ok(0);
    }
    for entry in listing {
        writeln!(streams.out, "{}", entry.name).map_err(WireError::from)?;
        if let Some(environment) = entry.value.as_ref() {
            for line in describe(environment) {
                writeln!(streams.out, "{line}").map_err(WireError::from)?;
            }
        }
        if let Some(problem) = entry.problem.as_deref() {
            writeln!(streams.out, "    problem    {problem}").map_err(WireError::from)?;
        }
        writeln!(streams.out).map_err(WireError::from)?;
    }
    Ok(0)
}
