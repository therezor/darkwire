//! `ghostai container` — list, approve and revoke container definitions.
//!
//! The same content-hash approval as a toolbox next door, over a different
//! question. A toolbox decides *what* an agent may call; a container decides
//! what the machine running those calls is allowed to be — its image, its
//! capabilities, who it runs as. Two approvals because they are two decisions:
//! an operator handing an agent one more operation should not have to re-review
//! an image, and an operator changing an image should not have to re-review
//! every toolbox that might run in it.
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
use crate::program::{Globals, StoreAction};
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
pub fn run(
    globals: &Globals,
    action: StoreAction,
    id: Option<&str>,
    env: &Env,
    streams: &mut Streams,
) -> Result<u8> {
    let loaded = load_config(LoadConfigOptions {
        paths: load_options(globals, None, env),
        file: None,
    })?;
    let store = PolicyStore::new(loaded.paths.policy_dir.clone());
    match act(&store, action, id, streams) {
        Ok(code) => Ok(code),
        Err(error) => {
            let _ = writeln!(streams.err, "{}", error.message);
            Ok(1)
        }
    }
}

fn act(
    store: &PolicyStore,
    action: StoreAction,
    id: Option<&str>,
    streams: &mut Streams,
) -> Result<u8> {
    if action == StoreAction::List {
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
            let state = if entry.approved {
                "approved"
            } else {
                "NOT APPROVED"
            };
            writeln!(streams.out, "{}  [{state}]", entry.name).map_err(GhostError::from)?;
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
        return Ok(0);
    }

    let Some(id) = id.filter(|value| !value.is_empty()) else {
        writeln!(
            streams.err,
            "Which container? Pass an id — see `ghostai container list`."
        )
        .map_err(GhostError::from)?;
        return Ok(2);
    };

    if action == StoreAction::Revoke {
        store.revoke_container(id)?;
        writeln!(
            streams.out,
            "Revoked {id}. The definition is still installed; it will no longer run."
        )
        .map_err(GhostError::from)?;
        return Ok(0);
    }

    let approved = store.approve_container(id)?;
    writeln!(streams.out, "Approved {id}:").map_err(GhostError::from)?;
    for line in describe(&approved.definition) {
        writeln!(streams.out, "{line}").map_err(GhostError::from)?;
    }
    writeln!(streams.out, "    definition sha256:{}", approved.sha256).map_err(GhostError::from)?;
    writeln!(streams.out).map_err(GhostError::from)?;
    writeln!(
        streams.out,
        "Editing the definition changes its hash and revokes this approval."
    )
    .map_err(GhostError::from)?;
    Ok(0)
}
