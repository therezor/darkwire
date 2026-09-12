//! The OS keychain stores, driven through an injected command runner.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ghostai_security::{
    CommandResult, CommandRunner, KeyStore, KeychainOptions, KeychainStore, Platform,
    SystemCommandRunner, VAULT_KEY_BYTES,
};

const KEY: [u8; VAULT_KEY_BYTES] = [7; VAULT_KEY_BYTES];

fn ok(stdout: &str) -> CommandResult {
    CommandResult {
        status: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
    }
}

fn fail() -> CommandResult {
    CommandResult {
        status: Some(1),
        stdout: String::new(),
        stderr: "nope".to_owned(),
    }
}

fn unavailable() -> CommandResult {
    CommandResult {
        status: None,
        stdout: String::new(),
        stderr: String::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    file: String,
    args: Vec<String>,
    input: Option<String>,
}

/// Replays scripted results and records every call. The last result repeats.
struct Scripted {
    results: Mutex<Vec<CommandResult>>,
    calls: Mutex<Vec<Call>>,
}

impl Scripted {
    fn new(results: Vec<CommandResult>) -> Arc<Scripted> {
        Arc::new(Scripted {
            results: Mutex::new(results),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

impl CommandRunner for Scripted {
    fn run(&self, file: &str, args: &[&str], input: Option<&str>) -> CommandResult {
        self.calls.lock().unwrap().push(Call {
            file: file.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            input: input.map(str::to_owned),
        });
        let mut results = self.results.lock().unwrap();
        if results.len() > 1 {
            results.remove(0)
        } else {
            results[0].clone()
        }
    }
}

fn store(platform: Platform, runner: Arc<Scripted>) -> KeychainStore {
    KeychainStore::new(KeychainOptions {
        platform,
        runner,
        ..KeychainOptions::default()
    })
}

#[test]
fn reads_a_key_from_the_macos_keychain() {
    let runner = Scripted::new(vec![ok(&format!("{}\n", STANDARD.encode(KEY)))]);
    let store = store(Platform::Darwin, Arc::clone(&runner));
    assert_eq!(store.name(), "keychain:darwin");
    assert_eq!(store.load().unwrap().unwrap(), KEY);
    assert_eq!(
        runner.calls()[0],
        Call {
            file: "security".to_owned(),
            args: [
                "find-generic-password",
                "-s",
                "ghostai-vault",
                "-a",
                "master-key",
                "-w"
            ]
            .iter()
            .map(|a| (*a).to_owned())
            .collect(),
            input: None,
        }
    );
}

#[test]
fn writes_to_the_macos_keychain_with_the_secret_on_stdin_twice() {
    let encoded = STANDARD.encode(KEY);
    // The write, then the read-back that proves it took.
    let runner = Scripted::new(vec![ok(""), ok(&encoded)]);
    assert!(
        store(Platform::Darwin, Arc::clone(&runner))
            .save(&KEY)
            .unwrap()
    );
    let call = &runner.calls()[0];
    // argv is readable via `ps` for the life of the process.
    assert!(!call.args.contains(&encoded));
    assert!(call.args.contains(&"-U".to_owned()));
    // Twice: the tool prompts for the password and then to confirm it. Sent once,
    // the second prompt reads EOF, an empty password is stored and the exit code
    // is still 0.
    assert_eq!(
        call.input.as_deref(),
        Some(format!("{encoded}\n{encoded}\n").as_str())
    );
    assert_eq!(runner.calls().len(), 2);
}

#[test]
fn refuses_to_claim_a_macos_write_it_cannot_read_back() {
    let runner = Scripted::new(vec![ok(""), ok("")]);
    assert!(!store(Platform::Darwin, runner).save(&KEY).unwrap());
    let other = Scripted::new(vec![ok(""), ok(&STANDARD.encode([1u8; 32]))]);
    assert!(!store(Platform::Darwin, other).save(&KEY).unwrap());
}

#[test]
fn reads_and_writes_the_linux_secret_service() {
    let encoded = STANDARD.encode(KEY);
    let runner = Scripted::new(vec![ok(&encoded)]);
    let store = store(Platform::Linux, Arc::clone(&runner));
    assert_eq!(store.name(), "keychain:linux");
    assert_eq!(store.load().unwrap().unwrap(), KEY);
    assert_eq!(runner.calls()[0].file, "secret-tool");
    assert!(store.save(&KEY).unwrap());
    let call = &runner.calls()[1];
    assert!(!call.args.contains(&encoded));
    assert_eq!(call.input.as_deref(), Some(encoded.as_str()));
    assert!(call.args.contains(&"store".to_owned()));
}

#[test]
fn reports_no_key_when_lookup_fails_or_the_tool_is_missing() {
    for platform in [Platform::Darwin, Platform::Linux] {
        let failing = store(platform.clone(), Scripted::new(vec![fail()]));
        assert_eq!(failing.load().unwrap(), None);
        assert!(!failing.save(&KEY).unwrap());
        let missing = store(platform, Scripted::new(vec![unavailable()]));
        assert_eq!(missing.load().unwrap(), None);
        assert!(!missing.save(&KEY).unwrap());
    }
}

#[test]
fn treats_a_truncated_entry_as_absent() {
    let store = store(Platform::Darwin, Scripted::new(vec![ok("dHJ1bmNhdGVk")]));
    assert_eq!(store.load().unwrap(), None);
    let garbage = self::store(Platform::Linux, Scripted::new(vec![ok("*not base64*")]));
    assert_eq!(garbage.load().unwrap(), None);
}

#[test]
fn reports_unavailable_on_platforms_with_no_tool() {
    let store = store(
        Platform::Other("windows".to_owned()),
        Scripted::new(vec![ok("")]),
    );
    assert_eq!(store.name(), "keychain:unavailable(windows)");
    assert_eq!(store.load().unwrap(), None);
    assert!(!store.save(&KEY).unwrap());
}

#[test]
fn defaults_to_this_platform_and_the_real_runner() {
    // Constructed only — loading here would prompt the developer's own keychain.
    let options = KeychainOptions::default();
    assert_eq!(options.platform, Platform::current());
    assert!(format!("{options:?}").contains("ghostai-vault"));
    let store = KeychainStore::new(options);
    assert!(store.name().starts_with("keychain:"));
    assert!(format!("{store:?}").contains("KeychainStore"));
    match std::env::consts::OS {
        "macos" => assert_eq!(Platform::current(), Platform::Darwin),
        "linux" => assert_eq!(Platform::current(), Platform::Linux),
        other => assert_eq!(Platform::current(), Platform::Other(other.to_owned())),
    }
}

#[test]
fn honours_a_custom_service_and_account() {
    let runner = Scripted::new(vec![ok("")]);
    let store = KeychainStore::new(KeychainOptions {
        platform: Platform::Linux,
        runner: runner.clone(),
        service: "svc".to_owned(),
        account: "acct".to_owned(),
    });
    let _ = store.load().unwrap();
    assert_eq!(
        runner.calls()[0].args,
        ["lookup", "service", "svc", "account", "acct"]
    );
}

#[cfg(unix)]
#[test]
fn the_system_runner_captures_output_status_and_stdin() {
    let echoed = SystemCommandRunner.run("cat", &[], Some("echoed"));
    assert_eq!(echoed.status, Some(0));
    assert_eq!(echoed.stdout, "echoed");

    let no_input = SystemCommandRunner.run("true", &[], None);
    assert_eq!(no_input.status, Some(0));

    let failed = SystemCommandRunner.run("false", &[], None);
    assert_eq!(failed.status, Some(1));

    let missing = SystemCommandRunner.run("ghostai-no-such-binary", &[], Some("x"));
    assert_eq!(missing, unavailable());
    assert!(format!("{SystemCommandRunner:?}").contains("SystemCommandRunner"));
}
