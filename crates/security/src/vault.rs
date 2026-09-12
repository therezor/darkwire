//! The credential vault.
//!
//! Provider API keys, OAuth refresh tokens and channel bot tokens are the most
//! valuable things GhostAI holds, and the config file is the wrong place for them:
//! it gets pasted into issues, copied between machines, and read back and
//! rewritten by the settings UI. They live here instead — AES-256-GCM, one file,
//! `0600`.
//!
//! GCM rather than CBC because the vault has to detect *tampering*, not just keep
//! secrets: an attacker who can write the file but not read it could otherwise
//! flip bits in a stored base URL and redirect every request that uses the key
//! next to it. A failed authentication tag is a hard error, never a fallback to
//! "treat the file as empty", which would silently discard every credential the
//! moment something went wrong.
//!
//! The key comes from the OS keychain when one is reachable and from a `0600`
//! keyfile when it is not. The fallback is not a lesser mode grudgingly
//! tolerated — it is what makes the vault work in a container, over SSH, and on a
//! headless host, which is where a self-hosted agent usually runs. Both paths are
//! tried in order and the first that answers wins, so a machine that gains a
//! keychain later keeps working without migration.
//!
//! The cipher hands back ciphertext with the 16-byte tag appended; the envelope
//! stores the tag in its own field, so the two are split on write and rejoined on
//! read. Contents are an insertion-ordered map at both levels, because the bytes
//! on disk are the serialisation of that map and a reorder would be a rewrite.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ghostai_core::{ErrorKind, GhostError, Result, ensure_dir};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::random::RandomSource;

/// AES-256.
pub const VAULT_KEY_BYTES: usize = 32;
/// 96 bits, the size GCM is specified for. Longer IVs are hashed and gain nothing.
const IV_BYTES: usize = 12;
/// GCM's full tag.
const TAG_BYTES: usize = 16;
const VAULT_VERSION: u64 = 1;
const VAULT_ALGORITHM: &str = "aes-256-gcm";

/// Bound into the authentication tag, so a file from an older format or another
/// application cannot be replayed into this one even with the right key.
const VAULT_AAD: &[u8] = b"ghostai-vault-v1";

// Key stores

/// Somewhere a master key can live.
pub trait KeyStore: Send + Sync {
    /// Identifies the store in logs and in [`resolve_vault_key`]'s result.
    fn name(&self) -> String;
    /// `None` when this store has no key — including when it is unavailable.
    fn load(&self) -> Result<Option<Vec<u8>>>;
    /// `false` when the store could not accept the key, so the next one is tried.
    fn save(&self, key: &[u8]) -> Result<bool>;
}

/// The fallback: the key as base64 in a `0600` file.
///
/// A key file that anyone else on the host can read is not a key file, so one
/// with group or other permissions is refused rather than used. POSIX only —
/// Windows reports a mode that has nothing to do with its ACLs, and the file
/// inherits the user profile's protection there.
#[derive(Debug, Clone)]
pub struct KeyFileStore {
    file: PathBuf,
}

impl KeyFileStore {
    /// A store at `file`.
    pub fn new(file: impl Into<PathBuf>) -> KeyFileStore {
        KeyFileStore { file: file.into() }
    }

    #[cfg(unix)]
    fn assert_private(&self) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&self.file)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Vault key file is readable by other users (mode {mode:o}): {}. Run chmod 600 on it, or delete it to generate a new key — every stored credential is lost with the old one.",
                    self.file.display()
                ),
            )
            .with_detail("file", self.file.to_string_lossy().into_owned())
            .with_detail("mode", mode));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn assert_private(&self) -> Result<()> {
        Ok(())
    }
}

impl KeyStore for KeyFileStore {
    fn name(&self) -> String {
        "keyfile".to_owned()
    }

    fn load(&self) -> Result<Option<Vec<u8>>> {
        let Ok(raw) = std::fs::read_to_string(&self.file) else {
            return Ok(None);
        };
        self.assert_private()?;
        let key = STANDARD.decode(raw.trim()).unwrap_or_default();
        if key.len() != VAULT_KEY_BYTES {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Vault key file is not a {VAULT_KEY_BYTES}-byte key: {}",
                    self.file.display()
                ),
            )
            .with_detail("file", self.file.to_string_lossy().into_owned()));
        }
        Ok(Some(key))
    }

    fn save(&self, key: &[u8]) -> Result<bool> {
        if let Some(parent) = self.file.parent() {
            ensure_dir(parent)?;
        }
        write_private(&self.file, STANDARD.encode(key).as_bytes())?;
        Ok(true)
    }
}

/// Writes `contents` to `file`, created `0600`, and re-asserts the mode when the
/// file already existed.
fn write_private(file: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut handle = options.open(file)?;
    handle.write_all(contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The master key, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVaultKey {
    /// 32 bytes.
    pub key: Vec<u8>,
    /// The name of the store the key came from, or was written to.
    pub source: String,
    /// Whether this call generated it.
    pub created: bool,
}

/// Loads the master key, generating and persisting one on first run.
///
/// If no store accepts the new key it fails rather than proceeding in memory: a
/// vault encrypted under a key that was never written is a vault whose contents
/// are lost at the next restart, and discovering that later is worse than not
/// starting now.
pub fn resolve_vault_key(
    stores: &[&dyn KeyStore],
    random: &dyn RandomSource,
) -> Result<ResolvedVaultKey> {
    for store in stores {
        if let Some(key) = store.load()? {
            return Ok(ResolvedVaultKey {
                key,
                source: store.name(),
                created: false,
            });
        }
    }

    let mut key = vec![0u8; VAULT_KEY_BYTES];
    random.fill(&mut key);
    for store in stores {
        if store.save(&key)? {
            return Ok(ResolvedVaultKey {
                key,
                source: store.name(),
                created: true,
            });
        }
    }
    Err(
        GhostError::new(ErrorKind::Config, "No key store accepted the new vault key").with_detail(
            "stores",
            serde_json::Value::Array(
                stores
                    .iter()
                    .map(|store| serde_json::Value::from(store.name()))
                    .collect(),
            ),
        ),
    )
}

// The vault

/// The on-disk shape. Field order is the byte order on disk.
#[derive(Debug, Serialize, Deserialize)]
struct VaultEnvelope {
    v: serde_json::Number,
    alg: String,
    iv: String,
    tag: String,
    data: String,
}

/// Namespace → key → value, in insertion order.
type VaultContents = IndexMap<String, IndexMap<String, String>>;

fn corrupt(file: &Path, reason: &str) -> GhostError {
    GhostError::new(
        ErrorKind::Config,
        format!(
            "Vault at {} could not be read ({reason}). This means the wrong key or a modified file; it is never an empty vault. Restore the file, or delete it to start over and lose every stored credential.",
            file.display()
        ),
    )
    .with_detail("file", file.to_string_lossy().into_owned())
}

/// The credential store.
pub struct CredentialVault {
    file: PathBuf,
    key: Vec<u8>,
    random: Arc<dyn RandomSource>,
    contents: VaultContents,
}

impl std::fmt::Debug for CredentialVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialVault")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

impl CredentialVault {
    /// Opens the vault at `file` under `key`, reading what is there.
    ///
    /// Never writes: an empty map on disk only ever comes from deleting the last
    /// key, so a first run leaves no file behind.
    pub fn open(file: &Path, key: &[u8], random: Arc<dyn RandomSource>) -> Result<CredentialVault> {
        if key.len() != VAULT_KEY_BYTES {
            return Err(GhostError::new(
                ErrorKind::InvalidInput,
                format!(
                    "Vault key must be {VAULT_KEY_BYTES} bytes, got {}",
                    key.len()
                ),
            ));
        }
        let mut vault = CredentialVault {
            file: file.to_path_buf(),
            key: key.to_vec(),
            random,
            contents: IndexMap::new(),
        };
        vault.contents = vault.read()?;
        Ok(vault)
    }

    /// `None` when either the namespace or the key is absent.
    pub fn get(&self, namespace: &str, key: &str) -> Option<&str> {
        self.contents
            .get(namespace)
            .and_then(|bucket| bucket.get(key))
            .map(String::as_str)
    }

    /// Whether a value is stored.
    pub fn has(&self, namespace: &str, key: &str) -> bool {
        self.get(namespace, key).is_some()
    }

    /// Writes through to disk. A credential that only reached memory is not stored.
    pub fn set(&mut self, namespace: &str, key: &str, value: &str) -> Result<()> {
        if namespace.is_empty() || key.is_empty() {
            return Err(GhostError::new(
                ErrorKind::InvalidInput,
                "Vault namespace and key must be non-empty",
            ));
        }
        self.contents
            .entry(namespace.to_owned())
            .or_default()
            .insert(key.to_owned(), value.to_owned());
        self.write()
    }

    /// Removes one value. `false` when nothing matched.
    pub fn delete(&mut self, namespace: &str, key: &str) -> Result<bool> {
        let Some(bucket) = self.contents.get_mut(namespace) else {
            return Ok(false);
        };
        if bucket.shift_remove(key).is_none() {
            return Ok(false);
        }
        // An empty namespace is removed rather than left behind, so `namespaces`
        // does not report a provider as configured after its key was deleted.
        if bucket.is_empty() {
            self.contents.shift_remove(namespace);
        }
        self.write()?;
        Ok(true)
    }

    /// The key names in a namespace.
    pub fn keys(&self, namespace: &str) -> Vec<String> {
        self.contents
            .get(namespace)
            .map(|bucket| bucket.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Every namespace with at least one value.
    pub fn namespaces(&self) -> Vec<String> {
        self.contents.keys().cloned().collect()
    }

    /// Removes one namespace, or everything. Returns how many values were dropped.
    pub fn clear(&mut self, namespace: Option<&str>) -> Result<usize> {
        let removed = match namespace {
            None => {
                let removed = self.contents.values().map(IndexMap::len).sum();
                self.contents = IndexMap::new();
                removed
            }
            Some(namespace) => match self.contents.shift_remove(namespace) {
                Some(bucket) => bucket.len(),
                None => return Ok(0),
            },
        };
        self.write()?;
        Ok(removed)
    }

    /// Namespaces and key *names* only.
    ///
    /// The settings UI needs to show that a provider has a key configured without
    /// the key crossing the network, and this is the shape that answers it. There
    /// is deliberately no method that returns every value at once.
    pub fn describe(&self) -> IndexMap<String, Vec<String>> {
        self.contents
            .iter()
            .map(|(namespace, bucket)| (namespace.clone(), bucket.keys().cloned().collect()))
            .collect()
    }

    /// Whether a supplied secret matches the stored one, in constant time.
    ///
    /// A plain comparison on a token leaks its prefix through timing. Callers
    /// comparing a bearer token or a webhook signature use this instead. The
    /// length is compared first — a token's length is not the secret, its content
    /// is, so answering early there is acceptable and unavoidable.
    pub fn verify(&self, namespace: &str, key: &str, candidate: &str) -> bool {
        let Some(stored) = self.get(namespace, key) else {
            return false;
        };
        stored.len() == candidate.len() && bool::from(stored.as_bytes().ct_eq(candidate.as_bytes()))
    }

    fn read(&self) -> Result<VaultContents> {
        let raw = match std::fs::read_to_string(&self.file) {
            Ok(raw) => raw,
            // Only "no vault yet" starts empty. A permission error must not be
            // mistaken for a first run, because the next `set` would overwrite a
            // file full of credentials with one holding a single entry.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(IndexMap::new());
            }
            Err(error) => {
                return Err(
                    corrupt(&self.file, &format!("cannot be opened: {:?}", error.kind()))
                        .with_source(error),
                );
            }
        };

        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| corrupt(&self.file, "not valid JSON").with_source(error))?;
        let envelope: VaultEnvelope = serde_json::from_value(value)
            .map_err(|_| corrupt(&self.file, "missing envelope fields"))?;
        if envelope.v.as_u64() != Some(VAULT_VERSION) || envelope.alg != VAULT_ALGORITHM {
            return Err(corrupt(
                &self.file,
                &format!("unsupported format v{}/{}", envelope.v, envelope.alg),
            ));
        }

        let plaintext = self
            .decrypt(&envelope)
            .ok_or_else(|| corrupt(&self.file, "authentication failed"))?;
        let parsed: serde_json::Value = serde_json::from_slice(&plaintext).map_err(|error| {
            corrupt(&self.file, "decrypted payload is not JSON").with_source(error)
        })?;
        let serde_json::Value::Object(namespaces) = parsed else {
            return Err(corrupt(&self.file, "decrypted payload is not an object"));
        };

        let mut contents = VaultContents::new();
        for (namespace, bucket) in namespaces {
            let serde_json::Value::Object(bucket) = bucket else {
                return Err(corrupt(
                    &self.file,
                    &format!("namespace \"{namespace}\" is not an object"),
                ));
            };
            let mut values = IndexMap::new();
            for (key, value) in bucket {
                // A non-string value would come back from `get` as text and reach
                // an Authorization header as its debug rendering.
                let serde_json::Value::String(value) = value else {
                    return Err(corrupt(
                        &self.file,
                        &format!("value at {namespace}.{key} is not a string"),
                    ));
                };
                values.insert(key, value);
            }
            contents.insert(namespace, values);
        }
        Ok(contents)
    }

    /// The plaintext, or `None` for anything the cipher refuses — a bad tag, a
    /// wrong IV length, an undecodable field.
    fn decrypt(&self, envelope: &VaultEnvelope) -> Option<Vec<u8>> {
        let iv = STANDARD.decode(&envelope.iv).ok()?;
        let tag = STANDARD.decode(&envelope.tag).ok()?;
        let mut data = STANDARD.decode(&envelope.data).ok()?;
        data.extend_from_slice(&tag);
        let cipher = Aes256Gcm::new_from_slice(&self.key).ok()?;
        let nonce = Nonce::try_from(iv.as_slice()).ok()?;
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &data,
                    aad: VAULT_AAD,
                },
            )
            .ok()
    }

    fn write(&self) -> Result<()> {
        let plaintext = serde_json::to_string(&self.contents)
            .map_err(|error| GhostError::new(ErrorKind::Internal, error.to_string()))?;
        let mut iv = [0u8; IV_BYTES];
        self.random.fill(&mut iv);
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|_| GhostError::new(ErrorKind::Internal, "Vault key is not 32 bytes"))?;
        let sealed = cipher
            .encrypt(
                &Nonce::from(iv),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| GhostError::new(ErrorKind::Internal, "Vault encryption failed"))?;
        let split = sealed.len().saturating_sub(TAG_BYTES);
        let (data, tag) = sealed.split_at(split);

        let envelope = VaultEnvelope {
            v: serde_json::Number::from(VAULT_VERSION),
            alg: VAULT_ALGORITHM.to_owned(),
            iv: STANDARD.encode(iv),
            tag: STANDARD.encode(tag),
            data: STANDARD.encode(data),
        };
        let text = serde_json::to_string(&envelope)
            .map_err(|error| GhostError::new(ErrorKind::Internal, error.to_string()))?;

        let cannot_write = |error: std::io::Error| {
            GhostError::new(
                ErrorKind::Storage,
                format!("Cannot write the vault at {}", self.file.display()),
            )
            .with_detail("file", self.file.to_string_lossy().into_owned())
            .with_source(error)
        };
        if let Some(parent) = self.file.parent() {
            ensure_dir(parent).map_err(|error| {
                GhostError::new(
                    ErrorKind::Storage,
                    format!("Cannot write the vault at {}", self.file.display()),
                )
                .with_detail("file", self.file.to_string_lossy().into_owned())
                .with_source(error)
            })?;
        }
        // Write-then-rename, so an interrupted write cannot leave a half-encrypted
        // file where every credential used to be. The temporary file is created
        // `0600` and rename preserves it.
        let temporary = PathBuf::from(format!("{}.tmp", self.file.display()));
        let outcome = write_private(&temporary, text.as_bytes())
            .and_then(|()| std::fs::rename(&temporary, &self.file));
        if let Err(error) = outcome {
            // Nothing useful to do if this fails: the original file is still intact
            // either way.
            let _ = std::fs::remove_file(&temporary);
            return Err(cannot_write(error));
        }
        Ok(())
    }
}
