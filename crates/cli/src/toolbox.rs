//! `ghostai toolbox` — list, approve and revoke toolboxes.
//!
//! A fourth command on a surface the roadmap said would stay at three. The
//! exception is deliberate: approving a toolbox is the one operator action that
//! cannot be delegated to the agent, and an install driven from a terminal
//! needs a way to perform it without opening a browser.
//!
//! `approve` is the whole security model in one verb. It records the sha256 of
//! the manifest bytes *as they are now*, and resolution later compares against
//! that — so this is not a flag being set, it is a statement about specific
//! content. Editing the manifest afterwards changes the hash and revokes the
//! approval automatically, which is why nothing here needs a `--force`.
//!
//! The listing prints what an operator has to weigh before approving, not just
//! the id: the image, the network ceiling, the capabilities added back, and any
//! hardening the profile switched off. A review that shows only a name is a
//! rubber stamp with extra steps.

use std::io::Write;
use std::sync::Arc;

use ghostai_core::{Database, GhostError, LoadConfigOptions, Result, SystemClock, load_config};
use ghostai_protocol::{Toolbox, ToolboxNetworkMode};
use ghostai_security::{ToolboxStore, weakened_in};

use crate::Streams;
use crate::i18n::Env;
use crate::program::{Globals, StoreAction};
use crate::runtime::load_options;

/// The wire spelling of a network ceiling.
///
/// Matched rather than `Debug`-printed: this is what an operator reads before
/// approving, and it has to be the same word the manifest and the settings
/// panel use.
fn network_mode(mode: ToolboxNetworkMode) -> &'static str {
    match mode {
        ToolboxNetworkMode::None => "none",
        ToolboxNetworkMode::Allowlist => "allowlist",
        ToolboxNetworkMode::Open => "open",
    }
}

/// Everything about a profile that bears on whether it is safe to approve.
fn describe(profile: &Toolbox) -> Vec<String> {
    let mut lines = vec![
        format!("    image      {}", profile.image),
        format!("    network    {}", network_mode(profile.network.max_mode)),
        format!(
            "    limits     {} MB, {} cpu",
            profile.limits.memory_mb, profile.limits.cpus
        ),
    ];
    // Only when non-default: a review that lists every field it did *not* need
    // to worry about is a review nobody reads to the end of.
    if !profile.caps.add.is_empty() {
        lines.push(format!("    caps       +{}", profile.caps.add.join(" +")));
    }
    for warning in weakened_in(profile) {
        lines.push(format!("    {warning}  <-- review this"));
    }
    lines
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
    let database = Database::open(&loaded.paths.db_file)?;
    let store = ToolboxStore::new(
        database,
        loaded.paths.toolboxes_dir.clone(),
        Arc::new(SystemClock),
    )?;

    match act(&store, action, id, &loaded.paths.toolboxes_dir, streams) {
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
    store: &ToolboxStore,
    action: StoreAction,
    id: Option<&str>,
    dir: &std::path::Path,
    streams: &mut Streams,
) -> Result<u8> {
    if action == StoreAction::List {
        let listing = store.list();
        if listing.is_empty() {
            writeln!(
                streams.out,
                "No toolboxes installed under {}",
                dir.display()
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
            if let Some(profile) = entry.toolbox.as_ref() {
                for line in describe(profile) {
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
        store.revoke(id)?;
        writeln!(
            streams.out,
            "Revoked {id}. The manifest is still installed; it will no longer run."
        )
        .map_err(GhostError::from)?;
        return Ok(0);
    }

    let approved = store.approve(id)?;
    writeln!(streams.out, "Approved {id}:").map_err(GhostError::from)?;
    for line in describe(&approved.toolbox) {
        writeln!(streams.out, "{line}").map_err(GhostError::from)?;
    }
    writeln!(
        streams.out,
        "    manifest   sha256:{}",
        approved.manifest_sha256
    )
    .map_err(GhostError::from)?;
    writeln!(streams.out).map_err(GhostError::from)?;
    writeln!(
        streams.out,
        "Editing the manifest changes its hash and revokes this approval."
    )
    .map_err(GhostError::from)?;
    Ok(0)
}
