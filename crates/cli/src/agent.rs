//! `darkwire agent` — list the agents this install is configured with.
//!
//! Read-only, and deliberately thin. An agent is created and edited in the web
//! UI under Agents, which is where the form that validates one lives; this
//! prints what `config.yaml` currently says so a person on a terminal, or a
//! script, can see the roster without opening a browser.

use std::io::Write;

use darkwire_core::{LoadConfigOptions, Result, WireError, load_config};
use darkwire_protocol::Config;

use crate::Streams;
use crate::i18n::Env;
use crate::program::{AgentCommand, Globals};
use crate::runtime::load_options;

/// One line to the answer stream.
fn line(out: &mut dyn Write, text: &str) -> Result<()> {
    writeln!(out, "{text}").map_err(WireError::from)
}

fn list(config: &Config, streams: &mut Streams) -> Result<u8> {
    let out = &mut streams.out;

    if config.agents.list.is_empty() {
        line(
            out,
            "No agents are configured; sessions run as the built-in default.",
        )?;
    }
    for (id, entry) in &config.agents.list {
        let state = if entry.enabled { "enabled" } else { "disabled" };
        line(out, &format!("{id}  [{state}]"))?;
        if !entry.label.is_empty() {
            line(out, &format!("    label      {}", entry.label))?;
        }
        if !entry.subagents.is_empty() {
            let ids: Vec<&str> = entry
                .subagents
                .iter()
                .map(|reference| reference.id.as_str())
                .collect();
            line(out, &format!("    delegates  {}", ids.join(", ")))?;
        }
        line(out, "")?;
    }
    Ok(0)
}

/// Runs one `darkwire agent` invocation and answers with its exit code.
pub fn run(
    globals: &Globals,
    command: &AgentCommand,
    env: &Env,
    streams: &mut Streams,
) -> Result<u8> {
    match act(globals, command, env, streams) {
        Ok(code) => Ok(code),
        Err(error) => {
            // A `WireError` message is written to be read by the person who
            // caused it — it already names the file and what to do next.
            let _ = writeln!(streams.err, "{}", error.message);
            Ok(1)
        }
    }
}

fn act(globals: &Globals, command: &AgentCommand, env: &Env, streams: &mut Streams) -> Result<u8> {
    let loaded = load_config(LoadConfigOptions {
        paths: load_options(globals, None, env),
        file: None,
    })?;

    match command {
        AgentCommand::List => list(&loaded.config, streams),
    }
}
