//! Finding one provider instance's API key.
//!
//! Two sources, one order, and a rule about when not to open the vault at all.
//! Kept in its own module because precedence between two credential stores is
//! the kind of thing worth asserting directly, rather than inferring from a
//! request header three layers away.

use std::collections::HashMap;
use std::sync::Arc;

use darkwire_core::{Result, WirePaths};
use darkwire_providers::ProviderInstance;
use darkwire_security::{
    CredentialVault, KeyFileStore, KeyStore, KeychainOptions, KeychainStore, OsRandom,
    resolve_vault_key,
};
use parking_lot::Mutex;

/// The vault namespace provider API keys live under.
pub const PROVIDER_CREDENTIAL_NAMESPACE: &str = "providers";

/// What a caller says about the vault.
///
/// Three states rather than two, and the third is the one a boolean cannot
/// hold: **an explicit "no vault" is not the same as not having said.** A test
/// that must not touch a keychain says [`VaultChoice::None`]; a caller that has
/// already opened one hands it over; everything else gets the default, which is
/// to open the vault only if one is already on disk.
#[derive(Clone, Default)]
pub enum VaultChoice {
    /// Open `<root>/vault.json` on demand, and only if it already exists.
    #[default]
    Default,
    /// Never open a vault. Credentials come from the environment alone.
    None,
    /// Use this one.
    Given(Arc<Mutex<CredentialVault>>),
}

impl std::fmt::Debug for VaultChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            VaultChoice::Default => "VaultChoice::Default",
            VaultChoice::None => "VaultChoice::None",
            VaultChoice::Given(_) => "VaultChoice::Given",
        })
    }
}

/// Opens the vault, creating its key on first use.
///
/// Exported so a caller that already knows it needs credentials — the settings
/// route writing a key the operator just typed — does not have to reproduce the
/// store order to get the same file.
pub fn open_vault(paths: &WirePaths) -> Result<CredentialVault> {
    let random = Arc::new(OsRandom);
    let keychain = KeychainStore::new(KeychainOptions::default());
    let key_file = KeyFileStore::new(paths.key_file.clone());
    let stores: [&dyn KeyStore; 2] = [&keychain, &key_file];
    let resolved = resolve_vault_key(&stores, random.as_ref())?;
    CredentialVault::open(&paths.vault_file, &resolved.key, random)
}

/// The credential for one instance: vault first, environment second.
///
/// **Keyed by instance id, not provider id.** Two Ollama servers are two
/// instances and can hold different tokens. Nothing had to be migrated for this:
/// a pre-instance entry's key *was* its provider id, so the id it takes on is
/// the string its credential was already stored under.
///
/// Returns `None` rather than failing when there is none — a local model server
/// usually needs no key, and `create_provider` is what refuses a remote endpoint
/// that does. Vault failures are not swallowed: a vault that will not open means
/// the wrong key or a modified file, and quietly continuing without it would
/// reach the provider as an unexplained 401.
///
/// The vault is opened only when one already exists on disk, and that condition
/// is doing real work rather than saving a file read. Resolving the vault key
/// writes one to the OS keychain the first time it runs, so opening the vault on
/// every `darkwire chat` against a local Ollama would be a keychain entry created
/// for an install that never stores a credential.
///
/// That check replaces a narrower one — "skip the vault entirely for a local
/// provider with no `env_key`" — which had the side effect of making a token
/// typed for a local instance unreadable. A LAN model server behind an auth
/// proxy is a real configuration, and the API-base check already permits a key
/// over plain HTTP to a private address, so the lookup was the only thing
/// standing in the way.
///
/// The vault wins over the environment: a spec's `env_key` is documented as the
/// variable consulted when the vault holds no key, and an exported variable in
/// some shell must not silently override the credential the operator stored.
pub fn find_credential<S: std::hash::BuildHasher>(
    instance: &ProviderInstance,
    paths: &WirePaths,
    env: &HashMap<String, String, S>,
    vault: &VaultChoice,
) -> Result<Option<String>> {
    let fallback = instance
        .spec
        .env_key
        .as_deref()
        .and_then(|key| env.get(key))
        .filter(|value| !value.is_empty())
        .cloned();

    let stored = match vault {
        VaultChoice::None => None,
        VaultChoice::Given(vault) => vault
            .lock()
            .get(PROVIDER_CREDENTIAL_NAMESPACE, &instance.id)
            .map(str::to_owned),
        VaultChoice::Default if !paths.vault_file.exists() => None,
        VaultChoice::Default => open_vault(paths)?
            .get(PROVIDER_CREDENTIAL_NAMESPACE, &instance.id)
            .map(str::to_owned),
    };

    Ok(stored.filter(|value| !value.is_empty()).or(fallback))
}
