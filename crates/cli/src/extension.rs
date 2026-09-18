//! `darkwire extension` — list, approve and revoke extensions.
//!
//! The same exception `darkwire container` is, for the same reason: approving code
//! the agent will run is the one operator action that cannot be delegated to
//! the agent, and an install driven from a terminal needs a way to perform it
//! without opening a browser.
//!
//! `approve` is the whole security model in one verb, and here it is one step
//! stronger than the container's. A container manifest pins an immutable image, so
//! hashing the manifest hashes the code; an extension manifest names a
//! *command*, so this records a digest over every byte under the install
//! directory. Editing any file — including the one the manifest points at —
//! moves the digest and revokes the approval, which is why nothing here needs a
//! `--force`.
//!
//! It writes only the approval row. Nothing here loads anything — a running
//! `darkwire serve` reloads through `POST /api/extensions/:id/approve`, and this
//! command is for the install that is not running yet or is being prepared.

use std::io::Write;
use std::sync::Arc;

use darkwire_core::{Database, LoadConfigOptions, Result, SystemClock, WireError, load_config};
use darkwire_protocol::ExtensionContribution;
use darkwire_security::{ExtensionResolution, ExtensionResolutionState, ExtensionStore};

use crate::Streams;
use crate::i18n::Env;
use crate::program::{Globals, StoreAction};
use crate::runtime::load_options;

/// Everything about an extension that bears on whether it is safe to approve.
fn describe(resolution: &ExtensionResolution) -> Vec<String> {
    let Some(manifest) = resolution.manifest.as_ref() else {
        return Vec::new();
    };

    let mut lines = vec![format!("    from       {}", resolution.dir.display())];
    if !manifest.version.is_empty() {
        lines.push(format!("    version    {}", manifest.version));
    }
    if !manifest.description.is_empty() {
        lines.push(format!("    about      {}", manifest.description));
    }
    // The argv, not a module path: a `darkwire.extension/2` extension is a child
    // process, and what an operator is approving is the program that runs.
    lines.push(format!("    command    {}", manifest.command.join(" ")));
    // The line that matters most, so it is never abbreviated away: an extension
    // declaring nothing can still run arbitrary code, and one declaring `tools`
    // is asking for something an operator has to grant per agent afterwards.
    let contributes = if manifest.contributes.is_empty() {
        "nothing declared".to_owned()
    } else {
        manifest
            .contributes
            .iter()
            .map(|kind| contribution(*kind).to_owned())
            .collect::<Vec<_>>()
            .join(", ")
    };
    lines.push(format!("    adds       {contributes}"));
    lines
}

/// The wire spelling of one contribution kind.
fn contribution(kind: ExtensionContribution) -> &'static str {
    match kind {
        ExtensionContribution::Tools => "tools",
        ExtensionContribution::Channels => "channels",
        ExtensionContribution::Providers => "providers",
        ExtensionContribution::Context => "context",
        ExtensionContribution::Commands => "commands",
    }
}

/// The state, shouted for everything but an approval.
///
/// The asymmetry is the point: `approved` is the ordinary outcome and every
/// other state is one an operator has to do something about.
fn label(resolution: &ExtensionResolution) -> String {
    match resolution.state {
        ExtensionResolutionState::Approved => "approved".to_owned(),
        ExtensionResolutionState::Unapproved => "UNAPPROVED".to_owned(),
        ExtensionResolutionState::Drifted => "DRIFTED".to_owned(),
        ExtensionResolutionState::Failed => "FAILED".to_owned(),
    }
}

/// Runs one `darkwire extension` invocation and answers with its exit code.
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
    let store = ExtensionStore::new(
        database,
        loaded.paths.extensions_dir.clone(),
        Arc::new(SystemClock),
    )?;

    match act(&store, action, id, &loaded.paths.extensions_dir, streams) {
        Ok(code) => Ok(code),
        Err(error) => {
            let _ = writeln!(streams.err, "{}", error.message);
            Ok(1)
        }
    }
}

fn act(
    store: &ExtensionStore,
    action: StoreAction,
    id: Option<&str>,
    dir: &std::path::Path,
    streams: &mut Streams,
) -> Result<u8> {
    if action == StoreAction::List {
        let ids = store.installed_ids();
        if ids.is_empty() {
            writeln!(
                streams.out,
                "No extensions installed under {}",
                dir.display()
            )
            .map_err(WireError::from)?;
            return Ok(0);
        }
        for id in ids {
            let resolution = store.resolve(&id)?;
            writeln!(streams.out, "{id}  [{}]", label(&resolution)).map_err(WireError::from)?;
            for line in describe(&resolution) {
                writeln!(streams.out, "{line}").map_err(WireError::from)?;
            }
            // The whole sentence, not a summary of it. Each of the refusals
            // already names the command that fixes it.
            if let Some(problem) = resolution.problem.as_deref() {
                for line in problem.lines() {
                    writeln!(streams.out, "    {line}").map_err(WireError::from)?;
                }
            }
            writeln!(streams.out).map_err(WireError::from)?;
        }
        return Ok(0);
    }

    let Some(id) = id.filter(|value| !value.is_empty()) else {
        writeln!(
            streams.err,
            "Which extension? Pass an id. See `darkwire extension list`."
        )
        .map_err(WireError::from)?;
        return Ok(2);
    };

    if action == StoreAction::Revoke {
        store.revoke(id)?;
        writeln!(
            streams.out,
            "Revoked {id}. The files are still installed; it will no longer load."
        )
        .map_err(WireError::from)?;
        return Ok(0);
    }

    let approved = store.approve(id)?;
    writeln!(streams.out, "Approved {id}:").map_err(WireError::from)?;
    for line in describe(&approved) {
        writeln!(streams.out, "{line}").map_err(WireError::from)?;
    }
    writeln!(streams.out, "    digest     sha256:{}", approved.digest).map_err(WireError::from)?;
    writeln!(streams.out).map_err(WireError::from)?;
    writeln!(
        streams.out,
        "Editing any file under that directory changes the digest and revokes"
    )
    .map_err(WireError::from)?;
    writeln!(
        streams.out,
        "this approval. An extension runs as a child process with the same"
    )
    .map_err(WireError::from)?;
    writeln!(streams.out, "access the account running the server has.").map_err(WireError::from)?;
    Ok(0)
}
