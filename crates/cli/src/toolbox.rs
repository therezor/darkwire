//! `ghostai toolbox` — list, approve and revoke toolboxes.
//!
//! `approve` is the whole security model in one verb. It records the sha256 of
//! the toolbox bytes *and every operation definition they name*, as they are
//! now, and resolution later compares against that — so this is not a flag
//! being set, it is a statement about specific content. Editing any of those
//! files afterwards changes the hash and revokes the approval automatically,
//! which is why nothing here needs a `--force`.
//!
//! The listing prints what an operator has to weigh before approving, not just
//! the id: every granted operation, its permission ceiling, and the exact
//! program and argument mapping behind it. A review that shows only a name is a
//! rubber stamp with extra steps.

use std::io::Write;

use ghostai_core::{GhostError, LoadConfigOptions, Result, load_config};
use ghostai_protocol::toolbox::{OperationArgument, OperationImplementation};
use ghostai_security::{PolicyStore, ResolvedToolbox};

use crate::Streams;
use crate::i18n::Env;
use crate::program::{Globals, StoreAction};
use crate::runtime::load_options;

/// Everything about a toolbox that bears on whether it is safe to approve.
///
/// Each grant is printed with the operation behind it rather than its name
/// alone: two toolboxes can both grant `search`, and what an operator is
/// approving is the program that runs, not the word.
fn describe(resolved: &ResolvedToolbox) -> Vec<String> {
    let mut lines = Vec::new();
    for grant in &resolved.toolbox.tools {
        lines.push(format!(
            "    tool       {}  [{}]  from {}",
            grant.name,
            permission_name(grant.permission),
            grant.definition
        ));
        let Some(operation) = resolved.operations.get(&grant.name) else {
            continue;
        };
        lines.push(format!("      {}", operation.description));
        match &operation.implementation {
            OperationImplementation::Transcript => {
                lines.push("      reads this agent's own command output".to_owned());
            }
            OperationImplementation::Registered { tool, .. } => {
                lines.push(format!("      calls the installed tool {tool}"));
            }
            OperationImplementation::Command {
                executable,
                argv,
                argv_input,
            } => {
                let mut rendered = vec![executable.clone()];
                rendered.extend(argv.iter().map(|argument| match argument {
                    OperationArgument::Literal(value) => value.clone(),
                    OperationArgument::Input(input) => format!("<{}>", input.input),
                }));
                lines.push(format!("      runs {}", rendered.join(" ")));
                if let Some(input) = argv_input {
                    // Named on its own line because it is the one grant shape
                    // that lets a model choose arguments the operator never
                    // wrote, and the review should not have to infer it.
                    lines.push(format!(
                        "      plus any arguments the model puts in <{input}>"
                    ));
                }
            }
        }
    }
    lines
}

fn permission_name(permission: ghostai_protocol::ToolPermission) -> &'static str {
    match permission {
        ghostai_protocol::ToolPermission::Allow => "allow",
        ghostai_protocol::ToolPermission::Ask => "ask",
        ghostai_protocol::ToolPermission::Deny => "deny",
    }
}

/// Runs one `ghostai toolbox` invocation and answers with its exit code.
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
            // `GhostError` messages are written to be read by the person who
            // caused them — they already name the file and what to do next.
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
        let listing = store.list_toolboxes();
        if listing.is_empty() {
            writeln!(
                streams.out,
                "No toolboxes installed under {}",
                store.root().join("toolboxes").display()
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
            if let Some(resolved) = entry.value.as_ref() {
                for line in describe(resolved) {
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
            "Which toolbox? Pass an id — see `ghostai toolbox list`."
        )
        .map_err(GhostError::from)?;
        return Ok(2);
    };

    if action == StoreAction::Revoke {
        store.revoke_toolbox(id)?;
        writeln!(
            streams.out,
            "Revoked {id}. The manifest is still installed; it will no longer run."
        )
        .map_err(GhostError::from)?;
        return Ok(0);
    }

    let approved = store.approve_toolbox(id)?;
    writeln!(streams.out, "Approved {id}:").map_err(GhostError::from)?;
    for line in describe(&approved.resolved) {
        writeln!(streams.out, "{line}").map_err(GhostError::from)?;
    }
    writeln!(streams.out, "    bundle     sha256:{}", approved.sha256())
        .map_err(GhostError::from)?;
    writeln!(streams.out).map_err(GhostError::from)?;
    writeln!(
        streams.out,
        "Editing the manifest or any definition it names changes this hash and\nrevokes the approval."
    )
    .map_err(GhostError::from)?;
    Ok(0)
}
