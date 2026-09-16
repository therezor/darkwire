//! The OS keychain, via the tool each platform ships.
//!
//! Secrets are handed over on stdin, never in argv — argv is world-readable
//! through `ps` for as long as the process lives, and "the key was visible for
//! 40 ms" is still a key that leaked. Windows has no built-in equivalent that can
//! be driven this way, so it reports unavailable and the keyfile takes over.
//!
//! The command runner is injected so keychain handling is testable. A test must
//! never reach the developer's real keychain: it would prompt, and on CI it
//! would either fail or — worse — succeed and leave a key behind.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use darkwire_core::Result;

use crate::vault::{KeyStore, VAULT_KEY_BYTES};

const KEYCHAIN_SERVICE: &str = "darkwire-vault";
const KEYCHAIN_ACCOUNT: &str = "master-key";

/// What a command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    /// The exit status; `None` when the binary could not be run at all, which
    /// reads as "unavailable".
    pub status: Option<i32>,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// Runs a program with arguments and optional stdin, and captures the result.
pub trait CommandRunner: Send + Sync {
    /// Runs `file` with `args`, feeding `input` on stdin when given.
    fn run(&self, file: &str, args: &[&str], input: Option<&str>) -> CommandResult;
}

/// The real thing: a child process, never a shell.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, file: &str, args: &[&str], input: Option<&str>) -> CommandResult {
        let unavailable = CommandResult {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
        };
        let mut command = Command::new(file);
        command
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let Ok(mut child) = command.spawn() else {
            return unavailable;
        };
        if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
            // A child that exits before reading its stdin closes the pipe; the
            // write error says nothing the exit status does not.
            let _ = stdin.write_all(text.as_bytes());
        }
        let Ok(output) = child.wait_with_output() else {
            return unavailable;
        };
        CommandResult {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

/// Which keychain tool to drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platform {
    /// `security`.
    Darwin,
    /// `secret-tool`.
    Linux,
    /// No usable tool; the keyfile takes over.
    Other(String),
}

impl Platform {
    /// The platform this build runs on.
    pub fn current() -> Platform {
        match std::env::consts::OS {
            "macos" => Platform::Darwin,
            "linux" => Platform::Linux,
            other => Platform::Other(other.to_owned()),
        }
    }
}

/// How to reach the keychain.
#[derive(Clone)]
pub struct KeychainOptions {
    /// Which tool to drive. Defaults to this platform's.
    pub platform: Platform,
    /// Runs the tool. Defaults to a real child process.
    pub runner: Arc<dyn CommandRunner>,
    /// The keychain service name.
    pub service: String,
    /// The keychain account name.
    pub account: String,
}

impl Default for KeychainOptions {
    fn default() -> Self {
        KeychainOptions {
            platform: Platform::current(),
            runner: Arc::new(SystemCommandRunner),
            service: KEYCHAIN_SERVICE.to_owned(),
            account: KEYCHAIN_ACCOUNT.to_owned(),
        }
    }
}

impl std::fmt::Debug for KeychainOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeychainOptions")
            .field("platform", &self.platform)
            .field("service", &self.service)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// The OS keychain as a [`KeyStore`].
#[derive(Debug, Clone)]
pub struct KeychainStore {
    options: KeychainOptions,
}

impl KeychainStore {
    /// A store driving the platform's tool through `options.runner`.
    pub fn new(options: KeychainOptions) -> KeychainStore {
        KeychainStore { options }
    }

    /// A truncated or re-encoded entry is treated as absent rather than as a
    /// failure: regenerating a key is recoverable, refusing to start is not.
    fn decode(stdout: &str) -> Option<Vec<u8>> {
        let key = STANDARD.decode(stdout.trim()).ok()?;
        (key.len() == VAULT_KEY_BYTES).then_some(key)
    }

    fn run(&self, file: &str, args: &[&str], input: Option<&str>) -> CommandResult {
        self.options.runner.run(file, args, input)
    }

    fn load_darwin(&self) -> Option<Vec<u8>> {
        let result = self.run(
            "security",
            &[
                "find-generic-password",
                "-s",
                &self.options.service,
                "-a",
                &self.options.account,
                "-w",
            ],
            None,
        );
        (result.status == Some(0))
            .then(|| Self::decode(&result.stdout))
            .flatten()
    }

    fn save_darwin(&self, key: &[u8]) -> bool {
        let encoded = STANDARD.encode(key);
        // **Twice**, and this is the whole subtlety of this store.
        //
        // `security ... -w` with no value in argv prompts for the password and
        // then prompts again to confirm it. Sending the key once satisfies the
        // first prompt and gives EOF to the second, so the two "do not match" —
        // at which point `security` stores an *empty* password and still exits
        // 0. Every vault written on a Mac was then encrypted with a key that
        // could never be loaded back, and the failure surfaced later and
        // somewhere else, as a vault that would not decrypt.
        let result = self.run(
            "security",
            &[
                "add-generic-password",
                "-U",
                "-s",
                &self.options.service,
                "-a",
                &self.options.account,
                "-w",
            ],
            Some(&format!("{encoded}\n{encoded}\n")),
        );
        if result.status != Some(0) {
            return false;
        }
        // An exit code is not evidence here — it was 0 for the failure above.
        // Reading it back is, and a store that reports success it cannot
        // demonstrate is worse than one that declines and lets the keyfile take
        // over.
        self.load_darwin().is_some_and(|stored| stored == key)
    }

    fn load_linux(&self) -> Option<Vec<u8>> {
        let result = self.run(
            "secret-tool",
            &[
                "lookup",
                "service",
                &self.options.service,
                "account",
                &self.options.account,
            ],
            None,
        );
        (result.status == Some(0))
            .then(|| Self::decode(&result.stdout))
            .flatten()
    }

    fn save_linux(&self, key: &[u8]) -> bool {
        let result = self.run(
            "secret-tool",
            &[
                "store",
                "--label=DarkWire vault key",
                "service",
                &self.options.service,
                "account",
                &self.options.account,
            ],
            Some(&STANDARD.encode(key)),
        );
        result.status == Some(0)
    }
}

impl KeyStore for KeychainStore {
    fn name(&self) -> String {
        match &self.options.platform {
            Platform::Darwin => "keychain:darwin".to_owned(),
            Platform::Linux => "keychain:linux".to_owned(),
            Platform::Other(name) => format!("keychain:unavailable({name})"),
        }
    }

    fn load(&self) -> Result<Option<Vec<u8>>> {
        Ok(match &self.options.platform {
            Platform::Darwin => self.load_darwin(),
            Platform::Linux => self.load_linux(),
            Platform::Other(_) => None,
        })
    }

    fn save(&self, key: &[u8]) -> Result<bool> {
        Ok(match &self.options.platform {
            Platform::Darwin => self.save_darwin(key),
            Platform::Linux => self.save_linux(key),
            Platform::Other(_) => false,
        })
    }
}
