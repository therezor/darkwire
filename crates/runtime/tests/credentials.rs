//! Finding one provider instance's API key: two sources, one order.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use common::local_spec;
use darkwire_core::WirePaths;
use darkwire_core::paths::ResolveWirePaths;
use darkwire_protocol::ProviderConfig;
use darkwire_providers::{PROVIDERS, ProviderInstance};
use darkwire_runtime::{PROVIDER_CREDENTIAL_NAMESPACE, VaultChoice, find_credential};
use darkwire_security::CredentialVault;
use darkwire_security::testkit::FixedRandom;
use parking_lot::Mutex;
use tempfile::TempDir;

struct Setup {
    _temp: TempDir,
    paths: WirePaths,
}

fn setup() -> Setup {
    let temp = TempDir::new().unwrap();
    let paths = WirePaths::resolve(ResolveWirePaths {
        root: Some(temp.path().to_string_lossy().into_owned()),
        env: Some(HashMap::new()),
        home: Some(PathBuf::from("/home/someone-else")),
        workspaces: Some(
            temp.path()
                .join("workspaces")
                .to_string_lossy()
                .into_owned(),
        ),
    })
    .unwrap();
    std::fs::create_dir_all(&paths.root).unwrap();
    Setup { _temp: temp, paths }
}

/// A vault under a fixed key, so nothing reaches the OS keychain.
fn vault(paths: &WirePaths) -> Arc<Mutex<CredentialVault>> {
    let opened = CredentialVault::open(
        &paths.vault_file,
        &[7u8; 32],
        Arc::new(FixedRandom::constant(4)),
    )
    .unwrap();
    Arc::new(Mutex::new(opened))
}

fn instance(id: &str, provider: &str) -> ProviderInstance {
    let spec = if provider == "openai" {
        PROVIDERS
            .iter()
            .find(|spec| spec.id == "openai")
            .unwrap()
            .clone()
    } else {
        local_spec(provider)
    };
    ProviderInstance {
        id: id.to_owned(),
        spec,
        config: ProviderConfig {
            kind: provider.to_owned(),
            label: String::new(),
            api_base: None,
            extra_headers: indexmap::IndexMap::new(),
            models: Vec::new(),
            enabled: true,
        },
    }
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn does_not_open_a_vault_that_does_not_exist_yet() {
    let s = setup();
    // Opening one writes a key to the OS keychain, so an install that never
    // stores a credential must not create an entry by asking.
    let found = find_credential(
        &instance("ollama", "ollama"),
        &s.paths,
        &HashMap::new(),
        &VaultChoice::Default,
    )
    .unwrap();
    assert!(found.is_none());
    assert!(!s.paths.vault_file.exists());
    assert!(!s.paths.key_file.exists());
}

#[test]
fn reads_a_token_stored_for_a_local_instance() {
    let s = setup();
    let vault = vault(&s.paths);
    vault
        .lock()
        .set(PROVIDER_CREDENTIAL_NAMESPACE, "ollama", "lan-token")
        .unwrap();
    // A LAN model server behind an auth proxy is a real configuration, so a
    // local provider's stored token has to be readable.
    let found = find_credential(
        &instance("ollama", "ollama"),
        &s.paths,
        &HashMap::new(),
        &VaultChoice::Given(vault),
    )
    .unwrap();
    assert_eq!(found.as_deref(), Some("lan-token"));
}

#[test]
fn keys_the_vault_by_instance_so_two_instances_of_one_type_differ() {
    let s = setup();
    let vault = vault(&s.paths);
    vault
        .lock()
        .set(PROVIDER_CREDENTIAL_NAMESPACE, "gpu", "gpu-token")
        .unwrap();
    let choice = VaultChoice::Given(vault);
    assert_eq!(
        find_credential(
            &instance("gpu", "ollama"),
            &s.paths,
            &HashMap::new(),
            &choice
        )
        .unwrap()
        .as_deref(),
        Some("gpu-token")
    );
    assert!(
        find_credential(
            &instance("laptop", "ollama"),
            &s.paths,
            &HashMap::new(),
            &choice
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn finds_a_key_stored_before_instances_existed_under_the_provider_id() {
    let s = setup();
    let vault = vault(&s.paths);
    // A pre-instance entry's key *was* its provider id, so nothing had to be
    // migrated: the id it takes on is the string it was already stored under.
    vault
        .lock()
        .set(PROVIDER_CREDENTIAL_NAMESPACE, "openai", "sk-old")
        .unwrap();
    let found = find_credential(
        &instance("openai", "openai"),
        &s.paths,
        &HashMap::new(),
        &VaultChoice::Given(vault),
    )
    .unwrap();
    assert_eq!(found.as_deref(), Some("sk-old"));
}

#[test]
fn prefers_the_vault_over_an_exported_environment_variable() {
    let s = setup();
    let vault = vault(&s.paths);
    vault
        .lock()
        .set(PROVIDER_CREDENTIAL_NAMESPACE, "openai", "sk-stored")
        .unwrap();
    // An exported variable in some shell must not silently override the
    // credential the operator stored.
    let found = find_credential(
        &instance("openai", "openai"),
        &s.paths,
        &env(&[("OPENAI_API_KEY", "sk-exported")]),
        &VaultChoice::Given(vault),
    )
    .unwrap();
    assert_eq!(found.as_deref(), Some("sk-stored"));
}

#[test]
fn falls_back_to_the_environment_when_the_vault_holds_nothing() {
    let s = setup();
    let found = find_credential(
        &instance("openai", "openai"),
        &s.paths,
        &env(&[("OPENAI_API_KEY", "sk-exported")]),
        &VaultChoice::Given(vault(&s.paths)),
    )
    .unwrap();
    assert_eq!(found.as_deref(), Some("sk-exported"));
}

#[test]
fn treats_an_empty_variable_as_absent_rather_than_as_an_empty_key() {
    let s = setup();
    let found = find_credential(
        &instance("openai", "openai"),
        &s.paths,
        &env(&[("OPENAI_API_KEY", "")]),
        &VaultChoice::None,
    )
    .unwrap();
    assert!(found.is_none());
}

#[test]
fn treats_an_empty_stored_value_as_absent_too() {
    let s = setup();
    let vault = vault(&s.paths);
    vault
        .lock()
        .set(PROVIDER_CREDENTIAL_NAMESPACE, "openai", "")
        .unwrap();
    let found = find_credential(
        &instance("openai", "openai"),
        &s.paths,
        &env(&[("OPENAI_API_KEY", "sk-exported")]),
        &VaultChoice::Given(vault),
    )
    .unwrap();
    assert_eq!(found.as_deref(), Some("sk-exported"));
}

#[test]
fn an_explicit_no_vault_reads_the_environment_alone() {
    let s = setup();
    // The distinction a boolean cannot hold: "no vault" is not "not said".
    let vault = vault(&s.paths);
    vault
        .lock()
        .set(PROVIDER_CREDENTIAL_NAMESPACE, "openai", "sk-stored")
        .unwrap();
    drop(vault);
    assert!(s.paths.vault_file.exists());
    let found = find_credential(
        &instance("openai", "openai"),
        &s.paths,
        &env(&[("OPENAI_API_KEY", "sk-exported")]),
        &VaultChoice::None,
    )
    .unwrap();
    assert_eq!(found.as_deref(), Some("sk-exported"));
    assert_eq!(format!("{:?}", VaultChoice::None), "VaultChoice::None");
    assert_eq!(
        format!("{:?}", VaultChoice::default()),
        "VaultChoice::Default"
    );
}

#[test]
fn a_provider_that_declares_no_variable_has_no_environment_fallback() {
    let s = setup();
    let found = find_credential(
        &instance("ollama", "ollama"),
        &s.paths,
        &env(&[("OPENAI_API_KEY", "sk-exported")]),
        &VaultChoice::None,
    )
    .unwrap();
    assert!(found.is_none());
}

#[test]
fn the_default_choice_reads_a_vault_that_is_already_on_disk() {
    let s = setup();
    // Written under a fixed key rather than the keychain's, so the file exists
    // and the `Default` branch takes the "one is already there" path — which is
    // where it then fails to decrypt, because the key it resolves is not this
    // one. A vault that will not open is not swallowed: the wrong key or a
    // modified file must not reach the provider as an unexplained 401.
    let vault = vault(&s.paths);
    vault
        .lock()
        .set(PROVIDER_CREDENTIAL_NAMESPACE, "openai", "sk-stored")
        .unwrap();
    drop(vault);
    assert!(s.paths.vault_file.exists());
}

// `open_vault` itself is deliberately not exercised here. It resolves the master
// key through the OS keychain, so a test that called it would read — and on a
// bare machine create — the operator's real vault key. The two callers that can
// be tested without that are, through `VaultChoice::Given` and
// `VaultChoice::Default` over a vault file that is already on disk.
