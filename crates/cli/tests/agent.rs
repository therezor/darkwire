//! `darkwire agent list`.
//!
//! The command is read-only, so what is asserted is the reading: an install
//! with agents prints them with their state, and an install with none says so
//! rather than printing an empty block. Creating an agent is the web UI's job
//! and is covered there.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "clippy's allow-*-in-tests covers `#[test]` bodies only, and the \
              fixtures here are ordinary functions; a fixture that cannot load is \
              a failing test either way, and `?` in one hides which line gave up"
)]

use std::path::Path;

use darkwire::Streams;
use darkwire::i18n::Env;
use darkwire::program::{AgentCommand, Globals};
use tempfile::TempDir;

/// A sink that keeps what was written to it, so a test can read it back.
#[derive(Clone, Default)]
struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl Sink {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What one run produced.
struct Run {
    code: u8,
    output: String,
    errors: String,
}

/// One temporary install.
struct Home {
    dir: TempDir,
}

impl Home {
    fn new() -> Home {
        Home {
            dir: TempDir::new().expect("a temporary home"),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn globals(&self) -> Globals {
        Globals {
            home: Some(self.path().to_string_lossy().into_owned()),
            color: Some(false),
            ..Globals::default()
        }
    }

    /// A settings tree with the agents this test wants in it.
    fn config(&self, yaml: &str) {
        std::fs::write(self.path().join("config.yaml"), yaml).expect("a config file");
    }

    fn list(&self) -> Run {
        let out = Sink::default();
        let err = Sink::default();
        let mut streams = Streams {
            out: Box::new(out.clone()),
            err: Box::new(err.clone()),
        };
        let code = darkwire::agent::run(
            &self.globals(),
            &AgentCommand::List,
            &Env::default(),
            &mut streams,
        )
        .expect("the command answers with an exit code rather than failing");
        Run {
            code,
            output: out.text(),
            errors: err.text(),
        }
    }
}

#[test]
fn shows_every_configured_agent_with_its_state() {
    let home = Home::new();
    home.config(
        "agents:\n  list:\n    scribe:\n      label: Scribe\n      enabled: true\n    \
         researcher:\n      enabled: false\n",
    );

    let run = home.list();

    assert_eq!(run.code, 0, "{}", run.errors);
    assert!(run.output.contains("scribe  [enabled]"), "{}", run.output);
    assert!(run.output.contains("label      Scribe"), "{}", run.output);
    assert!(
        run.output.contains("researcher  [disabled]"),
        "{}",
        run.output
    );
}

#[test]
fn names_the_agents_one_agent_delegates_to() {
    let home = Home::new();
    home.config(
        "agents:\n  list:\n    lead:\n      subagents:\n        - id: scribe\n          \
         permission: allow\n    scribe: {}\n",
    );

    let run = home.list();

    assert!(run.output.contains("delegates  scribe"), "{}", run.output);
}

#[test]
fn lists_the_built_in_default_on_a_fresh_install() {
    // A fresh install always holds the built-in default, so the listing is
    // never empty. What it must not do is print nothing at all.
    let home = Home::new();

    let run = home.list();

    assert_eq!(run.code, 0, "{}", run.errors);
    assert!(run.output.contains("default  [enabled]"), "{}", run.output);
}
