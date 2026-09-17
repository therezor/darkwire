//! One argv in, one exit code out.
//!
//! The whole binary as a function: what `main.rs` does on top of this is build
//! a tokio runtime and hand the number to the operating system, so everything
//! a run decides is decided here and can be asserted without spawning a
//! process.
//!
//! Exit codes are the contract a script reads, which is why they are the
//! subject of most of these cases rather than the text beside them.

// `allow-unwrap-in-tests` covers a `#[test]` body; the fixture helpers beside
// them are ordinary functions, and a helper that returned `Result` would make
// every case carry a `?` for a temporary directory that cannot fail in a way
// the case could act on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture helpers in an integration test"
)]

use std::sync::{Arc, Mutex};

use darkwire::i18n::Env;
use darkwire::{Streams, run};

/// A stream a case reads back afterwards.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<u8>>>);

impl Recorder {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One run, with both streams captured.
struct Ran {
    code: u8,
    out: String,
    err: String,
}

async fn ran(argv: &[&str], env: &Env) -> Ran {
    let out = Recorder::default();
    let err = Recorder::default();
    let mut streams = Streams {
        out: Box::new(out.clone()),
        err: Box::new(err.clone()),
    };
    let mut line: Vec<String> = vec!["darkwire".to_owned()];
    line.extend(argv.iter().map(|word| (*word).to_owned()));
    let code = run(line, env, &mut streams).await;
    Ran {
        code,
        out: out.text(),
        err: err.text(),
    }
}

#[tokio::test]
async fn version_is_the_bare_number_on_stdout() {
    // A script reads this, so it is the number and nothing else — no name, no
    // `v`, no trailing prose.
    let ran = ran(&["--version"], &Env::empty()).await;
    assert_eq!(ran.code, 0);
    assert_eq!(ran.out.trim(), darkwire::VERSION);
    assert!(ran.err.is_empty(), "{}", ran.err);
}

#[tokio::test]
async fn help_goes_to_stdout_and_succeeds() {
    // `darkwire --help | less` is the reason: help that a caller asked for is
    // the answer, not a diagnostic.
    let ran = ran(&["--help"], &Env::empty()).await;
    assert_eq!(ran.code, 0);
    assert!(ran.out.contains("chat"), "{}", ran.out);
    assert!(ran.out.contains("serve"), "{}", ran.out);
    assert!(ran.err.is_empty(), "{}", ran.err);
}

#[tokio::test]
async fn a_flag_nobody_defined_is_refused_on_stderr() {
    // Two exits apart from the previous case: the text is a diagnostic and the
    // code is non-zero, so `darkwire --typo > out` leaves `out` empty rather
    // than holding a usage page a script would then try to parse.
    let ran = ran(&["--no-such-flag"], &Env::empty()).await;
    assert_ne!(ran.code, 0);
    assert!(ran.out.is_empty(), "{}", ran.out);
    assert!(!ran.err.is_empty());
}

#[tokio::test]
async fn a_bare_word_is_a_message_rather_than_a_command_nobody_defined() {
    // `chat` is the default subcommand, so `darkwire what time is it` is a
    // question and not a typo. The refusal that comes back is about the
    // install having no provider, which is what proves the word was read as a
    // message rather than rejected as a command.
    let home = tempfile::tempdir().unwrap();
    // `--home` moves DarkWire's state and `-w` the workspaces; a run that named
    // only the first would build its default workspace in the real home.
    let ran = ran(
        &[
            "--home",
            &home.path().display().to_string(),
            "-w",
            &home.path().join("workspaces").display().to_string(),
            "nosuchcommand",
        ],
        &Env::empty(),
    )
    .await;

    assert_eq!(ran.code, 1);
    assert!(ran.err.contains("darkwire init"), "{}", ran.err);
}

#[tokio::test]
async fn darkwire_debug_adds_the_structured_detail_to_a_failure() {
    // There is no stack to print — the errors here are values, not unwinds —
    // so the debug form shows the kind and the details map instead, which is
    // the same information a log line would have held.
    let home = tempfile::tempdir().unwrap();
    // A one-shot turn on an install with no provider: the one refusal that
    // travels all the way back out of `run` as a value rather than being
    // printed by the command that raised it.
    let home_path = home.path().display().to_string();
    let workspaces = home.path().join("workspaces").display().to_string();
    let argv = ["--home", &home_path, "-w", &workspaces, "hello"];

    let plain = ran(&argv, &Env::empty()).await;
    let debug = ran(&argv, &[("DARKWIRE_DEBUG", "1")].into_iter().collect()).await;

    assert_ne!(plain.code, 0);
    assert_eq!(debug.code, plain.code);
    assert!(!plain.err.contains("kind="), "{}", plain.err);
    assert!(debug.err.contains("kind="), "{}", debug.err);
    assert!(debug.err.contains("retryable="), "{}", debug.err);
}

#[tokio::test]
async fn a_help_request_for_one_command_answers_about_that_command() {
    let ran = ran(&["help", "serve"], &Env::empty()).await;
    assert_eq!(ran.code, 0);
    assert!(ran.out.contains("--ready-file"), "{}", ran.out);
    assert!(ran.out.contains("--port"), "{}", ran.out);
}

#[tokio::test]
async fn the_interrupt_code_is_the_conventional_one() {
    // 128 + SIGINT, so a shell script branching on "the user pressed Ctrl-C"
    // reads the same number from this program as from `cat`.
    assert_eq!(darkwire::INTERRUPTED, 130);
}
