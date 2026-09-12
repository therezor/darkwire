//! The `ghostai` command line, as a library.
//!
//! The binary is a shim: it builds a tokio runtime, calls [`run`], and turns
//! the answer into an exit code. Everything else is here, because a test in
//! `tests/` is a separate crate that cannot reach into a `main.rs` — and
//! because coverage measures `src/`, so code that only the binary could run is
//! code nothing measures.
//!
//! The layering below this crate is enforced by Cargo rather than by review: a
//! crate that does not list another in `[dependencies]` cannot `use` it. This
//! one lists all of them, which is the definition of a composition root.
#![forbid(unsafe_code)]

pub mod agent;
pub mod ask;
pub mod catalogue;
pub mod chat;
pub mod commands;
pub mod extension;
pub mod header;
pub mod i18n;
pub mod init;
pub mod log_line;
pub mod menu;
pub mod messages;
pub mod models;
pub mod pickers;
pub mod preset;
pub mod presets;
pub mod program;
pub mod render;
pub mod runtime;
pub mod serve;
pub mod server_runtime;
pub mod skill_install;
pub mod telegram;
// Compiled only into a `test-hooks` build, and armed only by the environment on
// top of that. See the module for the two-switch rule.
#[cfg(feature = "test-hooks")]
pub mod test_hooks;
pub mod toolbox;

use std::io::Write;

use ghostai_core::GhostError;

use crate::i18n::{Env, Translations, describe_error};
use crate::program::{Invocation, Parsed, Subcommand};

pub use crate::program::VERSION;

/// The exit code a SIGINT during a one-shot turn produces.
///
/// 128 + SIGINT, which is what a shell reports for a process killed by the
/// signal — so a script that branches on "the user pressed Ctrl-C" reads the
/// same number from this program as from `cat`.
pub const INTERRUPTED: u8 = 130;

/// Where a command writes.
///
/// Injected rather than reached for, so a test drives a whole run and reads
/// what it printed without a terminal, a pipe or a captured global.
pub struct Streams {
    /// The answer.
    pub out: Box<dyn Write + Send>,
    /// Everything that is not the answer.
    pub err: Box<dyn Write + Send>,
}

impl Streams {
    /// The process's own streams.
    #[must_use]
    pub fn process() -> Streams {
        Streams {
            out: Box::new(std::io::stdout()),
            err: Box::new(std::io::stderr()),
        }
    }
}

impl std::fmt::Debug for Streams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Streams")
    }
}

/// Runs one command line and answers with the process exit code.
///
/// Never exits the process: the caller sets the code and lets the streams
/// drain on their own, which is the difference between a piped answer arriving
/// in full and being truncated at whatever the pipe had flushed.
pub async fn run<I, S>(argv: I, env: &Env, streams: &mut Streams) -> u8
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString> + Clone,
{
    // From the environment only. `--help` and a bad flag are both answered
    // before any subcommand has loaded a config, so `config.ui.locale` does not
    // exist yet — see the seam documented in `i18n.rs`.
    let translations = Translations::for_env(env, None);

    match program::parse(argv, env, &translations) {
        Parsed::Printed(text, code) => {
            let _ = streams.out.write_all(text.as_bytes());
            let _ = streams.out.flush();
            code
        }
        Parsed::Refused(text, code) => {
            let _ = streams.err.write_all(text.as_bytes());
            let _ = streams.err.flush();
            code
        }
        Parsed::Run(invocation) => match dispatch(*invocation, env, streams).await {
            Ok(code) => code,
            Err(error) => {
                let _ = writeln!(streams.err, "✖ {}", failure_text(&error, env));
                let _ = streams.err.flush();
                1
            }
        },
    }
}

/// The sentence, or the sentence plus what the error carried.
///
/// Under `GHOSTAI_DEBUG` the structured detail is what was asked for; otherwise
/// the sentence the error carries is the whole of what a person needs. There is
/// no stack to print — the errors here are values, not unwinds — so the debug
/// form shows the kind and the details map instead, which is the same
/// information a log line would have held.
fn failure_text(error: &GhostError, env: &Env) -> String {
    if !env.debug() {
        return describe_error(error);
    }
    let details = serde_json::to_string(&error.details).unwrap_or_else(|_| "{}".to_owned());
    format!(
        "{} [kind={} retryable={} details={details}]",
        error.message, error.kind, error.retryable
    )
}

async fn dispatch(
    invocation: Invocation,
    env: &Env,
    streams: &mut Streams,
) -> Result<u8, GhostError> {
    let Invocation { globals, command } = invocation;
    match command {
        Subcommand::Chat(args) => chat::run(&globals, *args, env, streams).await,
        Subcommand::Init => init::run(&globals, env, streams).await,
        Subcommand::Serve(args) => serve::run(&globals, *args, env, streams).await,
        Subcommand::Toolbox(action, id) => {
            toolbox::run(&globals, action, id.as_deref(), env, streams)
        }
        Subcommand::Extension(action, id) => {
            extension::run(&globals, action, id.as_deref(), env, streams)
        }
        Subcommand::Agent(command) => agent::run(&globals, &command, env, streams),
        Subcommand::Preset(catalogue, action) => {
            preset::run(&globals, &catalogue, action, env, streams).await
        }
    }
}
