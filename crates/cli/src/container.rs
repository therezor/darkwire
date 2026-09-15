//! `ghostai container list` — where an agent's commands would run.
//!
//! A different question from the toolbox next door. A toolbox decides *what* an
//! agent may call; a container decides what the machine running those calls is
//! allowed to be — its image, its capabilities, who it runs as. They are two
//! files because they are two decisions: handing an agent one more operation
//! should not mean re-reading an image, and changing an image should not mean
//! re-reading every toolbox that might run in it.
//!
//! The listing names anything the definition weakened, and whether a restricted
//! egress allow-list could be enforced in it at all. Both are the difference
//! between a container that bounds an agent and one that only looks as though
//! it does.

use std::io::Write;

use ghostai_core::{GhostError, LoadConfigOptions, Result, load_config};
use ghostai_protocol::toolbox::ContainerDefinition;
use ghostai_security::{PolicyStore, assert_gateway_compatible, weakened_in};

use crate::Streams;
use crate::i18n::Env;
use crate::program::Globals;
use crate::runtime::load_options;

/// Everything about a definition that bears on whether it is safe to approve.
fn describe(container: &ContainerDefinition) -> Vec<String> {
    let mut lines = vec![
        format!("    image      {}", container.image),
        format!(
            "    sharing    {}",
            if container.shared {
                "shared across agents and sessions in a workspace"
            } else {
                "private to one agent and session"
            }
        ),
        format!("    user       {}", container.user),
        format!("    workdir    {}", container.workdir),
        format!(
            "    limits     {} MB, {} cpu, {} pids",
            container.limits.memory_mb, container.limits.cpus, container.limits.pids_max
        ),
    ];
    if !container.caps.add.is_empty() {
        lines.push(format!("    caps       +{}", container.caps.add.join(" +")));
    }
    if let Err(error) = assert_gateway_compatible(container) {
        // Not a refusal: a container with no network is perfectly usable, and
        // the operator may never ask this one for an allow-list. It is printed
        // because discovering it on a settings save, after choosing this
        // container for that exact purpose, is the worse order to learn it in.
        lines.push(format!(
            "    egress     restricted mode unavailable — {}",
            error.message
        ));
    }
    for warning in weakened_in(container) {
        lines.push(format!("    {warning}  <-- review this"));
    }
    lines
}

/// Runs one `ghostai container` invocation and answers with its exit code.
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
    let listing = store.list_containers();
    if listing.is_empty() {
        writeln!(
            streams.out,
            "No containers installed under {}",
            store.root().join("containers").display()
        )
        .map_err(GhostError::from)?;
        return Ok(0);
    }
    for entry in listing {
        writeln!(streams.out, "{}", entry.name).map_err(GhostError::from)?;
        if let Some(container) = entry.value.as_ref() {
            for line in describe(container) {
                writeln!(streams.out, "{line}").map_err(GhostError::from)?;
            }
        }
        if let Some(problem) = entry.problem.as_deref() {
            writeln!(streams.out, "    problem    {problem}").map_err(GhostError::from)?;
        }
        writeln!(streams.out).map_err(GhostError::from)?;
    }
    Ok(0)
}
