//! What the terminal does to a runtime that the server's own port does not.
//!
//! The composition itself is tested where it lives. What is here is the seam:
//! how a global flag becomes a path resolution, and the one write a chat prompt
//! makes to `config.yaml`.

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

use std::sync::Arc;

use ghostai::i18n::Env;
use ghostai::program::Globals;
use ghostai::runtime::{create_chat_runtime, env_map, load_options, save_settings, settings_of};
use ghostai_core::paths::ResolveGhostPaths;
use ghostai_core::{LoadConfigOptions, load_config, parse_config};
use ghostai_protocol::config::ConfigPatch;
use ghostai_runtime::{RuntimeOptions, VaultChoice};

fn home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// A runtime over a temporary home that never opens a vault.
///
/// The vault is refused rather than left to its default, because resolving a
/// key mints a keychain entry the first time it runs — and a test suite must
/// not put anything in a developer's keychain.
fn runtime(dir: &tempfile::TempDir, options: RuntimeOptions) -> Arc<ghostai_runtime::GhostRuntime> {
    create_chat_runtime(RuntimeOptions {
        home: Some(dir.path().display().to_string()),
        vault: VaultChoice::None,
        env: Some(std::collections::HashMap::new()),
        ..options
    })
    .unwrap()
}

#[test]
fn load_options_carry_the_home_flag() {
    let globals = Globals {
        home: Some("/srv/ghost".to_owned()),
        ..Globals::default()
    };
    let resolved: ResolveGhostPaths = load_options(&globals, None, &Env::empty());
    assert_eq!(resolved.root.as_deref(), Some("/srv/ghost"));
    assert_eq!(resolved.workspace, None);
}

#[test]
fn load_options_keep_the_two_workspace_ideas_apart() {
    // `--workspace` is a *directory* and moves the whole tree; a workspace id
    // is a registry row whose directory is derived. Only the first reaches the
    // path resolution.
    let resolved = load_options(&Globals::default(), Some("/tmp/tree"), &Env::empty());
    assert_eq!(resolved.workspace.as_deref(), Some("/tmp/tree"));
}

#[test]
fn the_environment_map_carries_the_variables_the_core_reads() {
    let env: Env = [
        ("GHOSTAI_HOME", "/srv/ghost"),
        ("HOME", "/home/someone"),
        ("SOMETHING_ELSE", "ignored"),
    ]
    .into_iter()
    .collect();
    let map = env_map(&env);
    assert_eq!(
        map.get("GHOSTAI_HOME").map(String::as_str),
        Some("/srv/ghost")
    );
    assert_eq!(map.get("HOME").map(String::as_str), Some("/home/someone"));
    // Named rather than copied wholesale, so "which variable moved this" stays
    // answerable.
    assert_eq!(map.get("SOMETHING_ELSE"), None);
}

#[test]
fn load_options_carry_a_provider_key_variable_the_table_names() {
    // Resolution consults an exported key when no config names a provider, so
    // the variable has to reach it — and which variables those are is the
    // provider table's decision rather than this file's.
    let key = ghostai_providers::PROVIDERS
        .iter()
        .find_map(|spec| spec.env_key.clone())
        .expect("some provider declares a key variable");
    let env: Env = [(key.as_str(), "sk-test")].into_iter().collect();
    let resolved = load_options(&Globals::default(), None, &env);
    assert_eq!(
        resolved.env.and_then(|map| map.get(&key).cloned()),
        Some("sk-test".to_owned())
    );
}

#[test]
fn uses_the_provider_and_model_named_on_the_command_line() {
    let dir = home();
    let built = runtime(
        &dir,
        RuntimeOptions {
            provider: Some("ollama".to_owned()),
            model: Some("qwen3:8b".to_owned()),
            ..RuntimeOptions::default()
        },
    );

    assert_eq!(built.spec().map(|spec| spec.id), Some("ollama".to_owned()));
    assert_eq!(built.model(), "qwen3:8b");
    assert_eq!(built.require_loop().unwrap().model(), "qwen3:8b");
    assert!(!built.has_credential());
}

#[test]
fn refuses_the_turn_and_not_the_runtime_when_nothing_names_a_provider() {
    // Resolution answers nothing rather than picking one, because a request
    // landing at an endpoint nobody chose fails as a 401 from somewhere
    // unexpected. The runtime still builds, because the server shares it and
    // has to come up on a bare machine.
    let dir = home();
    let built = runtime(&dir, RuntimeOptions::default());

    assert!(!built.configured());
    let error = built.require_loop().unwrap_err();
    assert!(
        error.message.contains("No provider could be resolved"),
        "{}",
        error.message
    );
    assert!(error.message.contains("ghostai init"), "{}", error.message);
}

#[test]
fn save_settings_writes_the_merged_tree_to_the_config_file() {
    let dir = home();
    let built = runtime(
        &dir,
        RuntimeOptions {
            provider: Some("ollama".to_owned()),
            model: Some("qwen3:8b".to_owned()),
            ..RuntimeOptions::default()
        },
    );

    let patch: ConfigPatch =
        serde_json::from_value(serde_json::json!({"ui": {"timezone": "Europe/Berlin"}})).unwrap();
    let merged = save_settings(&built, &patch).unwrap();
    assert_eq!(merged.ui.timezone, "Europe/Berlin");

    // The file, not just this process: the whole point of the call.
    let reread = parse_config(
        &std::fs::read_to_string(built.file()).unwrap(),
        built.file(),
    )
    .unwrap();
    assert_eq!(reread.ui.timezone, "Europe/Berlin");
}

#[test]
fn save_settings_reports_a_file_it_could_not_write_as_a_storage_failure() {
    // The operator's file, not the operator's typing. The change is already
    // live for this run, which is the honest outcome to report.
    let dir = home();
    let built = runtime(&dir, RuntimeOptions::default());

    // A directory where the file should be: the write fails and the rebuild
    // has already landed.
    std::fs::remove_file(built.file()).ok();
    std::fs::create_dir_all(built.file()).unwrap();

    let patch: ConfigPatch =
        serde_json::from_value(serde_json::json!({"ui": {"timezone": "Europe/Berlin"}})).unwrap();
    let error = save_settings(&built, &patch).unwrap_err();
    assert_eq!(error.kind, ghostai_core::ErrorKind::Storage);
    assert!(
        error.message.contains("live for this run"),
        "{}",
        error.message
    );
}

#[test]
fn settings_of_answers_the_schema_default_for_an_id_naming_nothing() {
    // Nothing is inherited: an id naming no agent gets the same answer an entry
    // that named none of these fields would have got.
    let dir = home();
    let loaded = load_config(LoadConfigOptions {
        paths: ResolveGhostPaths {
            root: Some(dir.path().display().to_string()),
            env: Some(std::collections::HashMap::new()),
            ..ResolveGhostPaths::default()
        },
        file: None,
    })
    .unwrap();

    let missing = settings_of(&loaded.config, Some("no-such-agent"));
    let default = settings_of(&loaded.config, None);
    assert_eq!(missing.context_window_tokens, default.context_window_tokens);
}

#[test]
fn settings_of_reads_an_empty_id_as_the_default_agent() {
    let dir = home();
    let loaded = load_config(LoadConfigOptions {
        paths: ResolveGhostPaths {
            root: Some(dir.path().display().to_string()),
            env: Some(std::collections::HashMap::new()),
            ..ResolveGhostPaths::default()
        },
        file: None,
    })
    .unwrap();
    assert_eq!(
        settings_of(&loaded.config, Some("")),
        settings_of(&loaded.config, None)
    );
}
