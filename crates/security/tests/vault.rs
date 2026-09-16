//! The credential vault, against `fixtures/vault/envelope.json`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use darkwire_core::{ErrorKind, Result};
use darkwire_security::testkit::FixedRandom;
use darkwire_security::{
    CredentialVault, KeyFileStore, KeyStore, OsRandom, RandomSource, VAULT_KEY_BYTES,
    resolve_vault_key,
};
use serde_json::{Value, json};

use common::{cases, kind_of, read_fixture, temp_base, write};

const KEY: [u8; 32] = [7; 32];
const OTHER_KEY: [u8; 32] = [9; 32];
const AAD: &[u8] = b"darkwire-vault-v1";

fn open_at(file: &Path, key: &[u8]) -> Result<CredentialVault> {
    CredentialVault::open(file, key, Arc::new(OsRandom))
}

fn open(file: &Path) -> CredentialVault {
    open_at(file, &KEY).unwrap()
}

fn read_text(file: &Path) -> String {
    std::fs::read_to_string(file).unwrap()
}

fn envelope(file: &Path) -> serde_json::Map<String, Value> {
    serde_json::from_str::<Value>(&read_text(file))
        .unwrap()
        .as_object()
        .unwrap()
        .clone()
}

fn decrypt(key: &[u8], text: &str) -> String {
    let envelope: Value = serde_json::from_str(text).unwrap();
    let iv = STANDARD.decode(envelope["iv"].as_str().unwrap()).unwrap();
    let tag = STANDARD.decode(envelope["tag"].as_str().unwrap()).unwrap();
    let mut data = STANDARD.decode(envelope["data"].as_str().unwrap()).unwrap();
    data.extend_from_slice(&tag);
    let cipher = Aes256Gcm::new_from_slice(key).unwrap();
    let plain = cipher
        .decrypt(
            &Nonce::try_from(iv.as_slice()).unwrap(),
            Payload {
                msg: &data,
                aad: AAD,
            },
        )
        .unwrap();
    String::from_utf8(plain).unwrap()
}

/// Encrypts arbitrary plaintext under the real key, so only the payload is wrong.
fn write_payload(file: &Path, plaintext: &str) {
    let _ = std::fs::remove_file(file);
    let mut vault = open(file);
    vault.set("placeholder", "x", "y").unwrap();
    let mut envelope = envelope(file);
    let iv = STANDARD.decode(envelope["iv"].as_str().unwrap()).unwrap();
    let cipher = Aes256Gcm::new_from_slice(&KEY).unwrap();
    let sealed = cipher
        .encrypt(
            &Nonce::try_from(iv.as_slice()).unwrap(),
            Payload {
                msg: plaintext.as_bytes(),
                aad: AAD,
            },
        )
        .unwrap();
    let (data, tag) = sealed.split_at(sealed.len() - 16);
    envelope.insert("data".to_owned(), json!(STANDARD.encode(data)));
    envelope.insert("tag".to_owned(), json!(STANDARD.encode(tag)));
    write(file, serde_json::to_string(&envelope).unwrap());
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn matches_the_envelope_fixture() {
    let fixture = read_fixture("vault/envelope.json");
    let constants = &fixture["constants"];
    assert_eq!(constants["keyBytes"], json!(VAULT_KEY_BYTES));
    assert_eq!(constants["aad"], json!(std::str::from_utf8(AAD).unwrap()));

    let (_dir, base) = temp_base();
    let mut failures = Vec::new();
    for (index, case) in cases(&fixture).into_iter().enumerate() {
        let input = &case["input"];
        let key = STANDARD.decode(input["key"].as_str().unwrap()).unwrap();
        let iv = STANDARD.decode(input["iv"].as_str().unwrap()).unwrap();
        let file = base.join(format!("case-{index}")).join("vault.json");
        let mut vault =
            CredentialVault::open(&file, &key, Arc::new(FixedRandom::pattern(&iv))).unwrap();
        for entry in input["entries"].as_array().unwrap() {
            vault
                .set(
                    entry["namespace"].as_str().unwrap(),
                    entry["key"].as_str().unwrap(),
                    entry["value"].as_str().unwrap(),
                )
                .unwrap();
        }
        for entry in input["deletes"].as_array().unwrap() {
            vault
                .delete(
                    entry["namespace"].as_str().unwrap(),
                    entry["key"].as_str().unwrap(),
                )
                .unwrap();
        }
        let text = read_text(&file);
        let actual = json!({"file": text, "plaintext": decrypt(&key, &text)});
        if actual != case["output"] {
            failures.push(format!(
                "{}\n  expected {}\n  actual   {}",
                case["name"], case["output"], actual
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    let key_file = &fixture["keyFile"];
    let key = STANDARD
        .decode(key_file["input"]["key"].as_str().unwrap())
        .unwrap();
    let path = base.join("vault.key");
    assert!(KeyFileStore::new(&path).save(&key).unwrap());
    assert_eq!(
        read_text(&path),
        key_file["output"]["contents"].as_str().unwrap()
    );
}

// Key file store

#[test]
fn the_key_file_round_trips_and_is_private() {
    let (_dir, base) = temp_base();
    let path = base.join("nested").join("vault.key");
    let store = KeyFileStore::new(&path);
    assert_eq!(store.name(), "keyfile");
    assert_eq!(store.load().unwrap(), None);
    assert!(store.save(&KEY).unwrap());
    assert_eq!(store.load().unwrap().unwrap(), KEY);
    #[cfg(unix)]
    assert_eq!(mode_of(&path), 0o600);
    // Saving over an existing file re-asserts the mode.
    #[cfg(unix)]
    {
        chmod(&path, 0o644);
        assert!(store.save(&OTHER_KEY).unwrap());
        assert_eq!(mode_of(&path), 0o600);
    }
    assert!(format!("{store:?}").contains("KeyFileStore"));
}

#[cfg(unix)]
#[test]
fn refuses_a_key_file_other_users_can_read() {
    let (_dir, base) = temp_base();
    let path = base.join("vault.key");
    let store = KeyFileStore::new(&path);
    store.save(&KEY).unwrap();
    chmod(&path, 0o644);
    let error = store.load().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("chmod 600"));
    assert_eq!(error.details["mode"], json!(0o644));
}

#[test]
fn refuses_a_key_file_that_is_not_a_32_byte_key() {
    let (_dir, base) = temp_base();
    let path = base.join("vault.key");
    write(&path, STANDARD.encode([0u8; 8]));
    #[cfg(unix)]
    chmod(&path, 0o600);
    assert_eq!(kind_of(&KeyFileStore::new(&path).load()), "config");
    write(&path, "not base64 at all!!");
    assert_eq!(kind_of(&KeyFileStore::new(&path).load()), "config");
}

#[test]
fn tolerates_trailing_whitespace_in_a_key_file() {
    let (_dir, base) = temp_base();
    let path = base.join("vault.key");
    write(&path, format!("{}\n", STANDARD.encode(KEY)));
    #[cfg(unix)]
    chmod(&path, 0o600);
    assert_eq!(KeyFileStore::new(&path).load().unwrap().unwrap(), KEY);
}

#[cfg(unix)]
#[test]
fn a_key_file_save_reports_a_directory_it_cannot_create() {
    let (_dir, base) = temp_base();
    write(&base.join("a-file"), "x");
    let store = KeyFileStore::new(base.join("a-file").join("vault.key"));
    assert!(store.save(&KEY).is_err());
}

// Resolving the key

struct Fake {
    name: &'static str,
    key: Option<Vec<u8>>,
    accepts: bool,
    saved: Mutex<Vec<Vec<u8>>>,
}

impl Fake {
    fn new(name: &'static str, key: Option<&[u8]>, accepts: bool) -> Fake {
        Fake {
            name,
            key: key.map(<[u8]>::to_vec),
            accepts,
            saved: Mutex::new(Vec::new()),
        }
    }
}

impl KeyStore for Fake {
    fn name(&self) -> String {
        self.name.to_owned()
    }

    fn load(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.key.clone())
    }

    fn save(&self, key: &[u8]) -> Result<bool> {
        self.saved.lock().unwrap().push(key.to_vec());
        Ok(self.accepts)
    }
}

#[test]
fn takes_the_first_store_that_has_a_key() {
    let first = Fake::new("first", Some(&KEY), true);
    let second = Fake::new("second", Some(&OTHER_KEY), true);
    let resolved = resolve_vault_key(&[&first, &second], &OsRandom).unwrap();
    assert_eq!(resolved.source, "first");
    assert!(!resolved.created);
    assert_eq!(resolved.key, KEY);
    assert_eq!(resolved, resolved.clone());
}

#[test]
fn falls_through_to_a_store_that_has_one() {
    let empty = Fake::new("empty", None, true);
    let keyfile = Fake::new("keyfile", Some(&KEY), true);
    let resolved = resolve_vault_key(&[&empty, &keyfile], &OsRandom).unwrap();
    assert_eq!(resolved.source, "keyfile");
    assert_eq!(resolved.key, KEY);
}

#[test]
fn generates_and_persists_a_key_on_first_run() {
    let keychain = Fake::new("keychain", None, false);
    let keyfile = Fake::new("keyfile", None, true);
    let resolved = resolve_vault_key(&[&keychain, &keyfile], &FixedRandom::constant(3)).unwrap();
    assert_eq!(resolved.source, "keyfile");
    assert!(resolved.created);
    assert_eq!(resolved.key, vec![3u8; VAULT_KEY_BYTES]);
    assert_eq!(keyfile.saved.lock().unwrap()[0], resolved.key);
}

#[test]
fn fails_rather_than_running_with_a_key_nothing_stored() {
    let nope = Fake::new("nope", None, false);
    let error = resolve_vault_key(&[&nope], &OsRandom).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert_eq!(error.details["stores"], json!(["nope"]));
}

#[test]
fn a_store_error_propagates() {
    let (_dir, base) = temp_base();
    let path = base.join("vault.key");
    write(&path, "short");
    #[cfg(unix)]
    chmod(&path, 0o600);
    let store = KeyFileStore::new(&path);
    assert_eq!(kind_of(&resolve_vault_key(&[&store], &OsRandom)), "config");
}

#[test]
fn uses_real_randomness_by_default() {
    let collect = Fake::new("x", None, true);
    resolve_vault_key(&[&collect], &OsRandom).unwrap();
    resolve_vault_key(&[&collect], &OsRandom).unwrap();
    let saved = collect.saved.lock().unwrap();
    assert_ne!(saved[0], saved[1]);
}

#[test]
fn works_end_to_end_against_a_real_key_file() {
    let (_dir, base) = temp_base();
    let store = KeyFileStore::new(base.join("vault.key"));
    let first = resolve_vault_key(&[&store], &OsRandom).unwrap();
    assert!(first.created);
    let second = resolve_vault_key(&[&store], &OsRandom).unwrap();
    assert!(!second.created);
    assert_eq!(second.key, first.key);
}

// The vault

struct Vault {
    _dir: tempfile::TempDir,
    base: PathBuf,
    file: PathBuf,
}

fn setup() -> Vault {
    let (dir, base) = temp_base();
    let file = base.join("credentials.enc");
    Vault {
        _dir: dir,
        base,
        file,
    }
}

#[test]
fn refuses_a_key_of_the_wrong_size() {
    let v = setup();
    assert_eq!(kind_of(&open_at(&v.file, &[0u8; 16])), "invalid_input");
}

#[test]
fn starts_empty_when_there_is_no_file_yet() {
    let v = setup();
    let vault = open(&v.file);
    assert!(vault.namespaces().is_empty());
    assert_eq!(vault.get("providers", "openai"), None);
    assert!(!v.file.exists());
    assert!(format!("{vault:?}").contains("CredentialVault"));
}

#[test]
fn stores_reads_back_and_survives_a_reopen() {
    let v = setup();
    let mut vault = open(&v.file);
    vault.set("providers", "openai", "sk-secret").unwrap();
    vault.set("channels", "telegram", "bot-token").unwrap();
    assert_eq!(vault.get("providers", "openai"), Some("sk-secret"));
    assert!(vault.has("providers", "openai"));
    assert!(!vault.has("providers", "groq"));

    let second = open(&v.file);
    assert_eq!(second.get("providers", "openai"), Some("sk-secret"));
    assert_eq!(second.get("channels", "telegram"), Some("bot-token"));
    assert_eq!(second.namespaces(), ["providers", "channels"]);
}

#[test]
fn writes_the_file_private_with_no_temporary_and_no_plaintext() {
    let v = setup();
    open(&v.file)
        .set("providers", "openai", "sk-super-secret-value")
        .unwrap();
    #[cfg(unix)]
    assert_eq!(mode_of(&v.file), 0o600);
    assert!(!v.base.join("credentials.enc.tmp").exists());
    let raw = read_text(&v.file);
    assert!(!raw.contains("sk-super-secret-value"));
    assert!(!raw.contains("openai"));
    let envelope = envelope(&v.file);
    assert_eq!(envelope["v"], json!(1));
    assert_eq!(envelope["alg"], json!("aes-256-gcm"));
}

#[test]
fn uses_a_fresh_iv_per_write() {
    let v = setup();
    let mut vault = open(&v.file);
    vault.set("a", "b", "c").unwrap();
    let first = read_text(&v.file);
    vault.set("a", "b", "c").unwrap();
    assert_ne!(read_text(&v.file), first);
}

#[test]
fn rejects_an_empty_namespace_or_key() {
    let v = setup();
    let mut vault = open(&v.file);
    assert_eq!(kind_of(&vault.set("", "k", "v")), "invalid_input");
    assert_eq!(kind_of(&vault.set("ns", "", "v")), "invalid_input");
}

#[test]
fn deletes_values_and_empty_namespaces() {
    let v = setup();
    let mut vault = open(&v.file);
    vault.set("providers", "openai", "a").unwrap();
    vault.set("providers", "groq", "b").unwrap();
    assert!(vault.delete("providers", "openai").unwrap());
    assert_eq!(vault.keys("providers"), ["groq"]);
    assert!(vault.delete("providers", "groq").unwrap());
    assert!(vault.namespaces().is_empty());
    assert!(open(&v.file).namespaces().is_empty());

    assert!(!vault.delete("providers", "openai").unwrap());
    vault.set("providers", "groq", "b").unwrap();
    assert!(!vault.delete("providers", "openai").unwrap());
    assert!(vault.keys("nope").is_empty());
}

#[test]
fn clears_one_namespace_or_everything() {
    let v = setup();
    let mut vault = open(&v.file);
    vault.set("providers", "openai", "a").unwrap();
    vault.set("channels", "telegram", "b").unwrap();
    assert_eq!(vault.clear(Some("providers")).unwrap(), 1);
    assert_eq!(vault.namespaces(), ["channels"]);
    assert_eq!(vault.clear(Some("providers")).unwrap(), 0);

    vault.set("providers", "openai", "a").unwrap();
    vault.set("providers", "groq", "b").unwrap();
    assert_eq!(vault.clear(None).unwrap(), 3);
    assert!(vault.namespaces().is_empty());
    assert!(open(&v.file).namespaces().is_empty());
}

#[test]
fn describes_names_without_values() {
    let v = setup();
    let mut vault = open(&v.file);
    vault.set("providers", "openai", "sk-secret").unwrap();
    vault.set("providers", "groq", "gsk-secret").unwrap();
    let described = vault.describe();
    assert_eq!(
        serde_json::to_value(&described).unwrap(),
        json!({"providers": ["openai", "groq"]})
    );
    assert!(
        !serde_json::to_string(&described)
            .unwrap()
            .contains("secret")
    );
}

#[test]
fn a_namespace_named_like_a_prototype_property_is_just_a_key() {
    let v = setup();
    let mut vault = open(&v.file);
    vault.set("__proto__", "polluted", "yes").unwrap();
    vault.set("constructor", "polluted", "yes").unwrap();
    assert_eq!(vault.get("__proto__", "polluted"), Some("yes"));
    assert_eq!(open(&v.file).get("__proto__", "polluted"), Some("yes"));
}

#[test]
fn verifies_in_constant_time() {
    let v = setup();
    let mut vault = open(&v.file);
    vault.set("auth", "token", "correct-horse").unwrap();
    assert!(vault.verify("auth", "token", "correct-horse"));
    assert!(!vault.verify("auth", "token", "correct-horsf"));
    assert!(!vault.verify("auth", "token", "correct"));
    assert!(!vault.verify("auth", "token", ""));
    assert!(!vault.verify("auth", "missing", "anything"));
}

#[test]
fn refuses_the_wrong_key_rather_than_reporting_an_empty_vault() {
    let v = setup();
    open(&v.file)
        .set("providers", "openai", "sk-secret")
        .unwrap();
    let error = open_at(&v.file, &OTHER_KEY).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("authentication failed"));
}

#[test]
fn refuses_tampered_ciphertext_tag_iv_or_encoding() {
    let v = setup();
    open(&v.file)
        .set("providers", "openai", "sk-secret")
        .unwrap();
    let original = envelope(&v.file);

    let mut data = STANDARD.decode(original["data"].as_str().unwrap()).unwrap();
    data[0] ^= 0xff;
    let mut flipped = original.clone();
    flipped.insert("data".to_owned(), json!(STANDARD.encode(&data)));
    write(&v.file, serde_json::to_string(&flipped).unwrap());
    assert!(
        open_at(&v.file, &KEY)
            .unwrap_err()
            .message
            .contains("authentication failed")
    );

    let mut zero_tag = original.clone();
    zero_tag.insert("tag".to_owned(), json!(STANDARD.encode([0u8; 16])));
    write(&v.file, serde_json::to_string(&zero_tag).unwrap());
    assert!(
        open_at(&v.file, &KEY)
            .unwrap_err()
            .message
            .contains("authentication failed")
    );

    let mut short_iv = original.clone();
    short_iv.insert("iv".to_owned(), json!(STANDARD.encode([1u8; 4])));
    write(&v.file, serde_json::to_string(&short_iv).unwrap());
    assert!(
        open_at(&v.file, &KEY)
            .unwrap_err()
            .message
            .contains("authentication failed")
    );

    let mut bad_base64 = original.clone();
    bad_base64.insert("data".to_owned(), json!("*not base64*"));
    write(&v.file, serde_json::to_string(&bad_base64).unwrap());
    assert!(
        open_at(&v.file, &KEY)
            .unwrap_err()
            .message
            .contains("authentication failed")
    );
}

#[test]
fn refuses_malformed_envelopes() {
    let v = setup();
    for contents in ["not json at all", "{\"v\":1}", "\"a string\"", "null"] {
        write(&v.file, contents);
        assert_eq!(kind_of(&open_at(&v.file, &KEY)), "config", "{contents}");
    }

    std::fs::remove_file(&v.file).unwrap();
    open(&v.file)
        .set("providers", "openai", "sk-secret")
        .unwrap();
    let original = envelope(&v.file);
    let mut future = original.clone();
    future.insert("v".to_owned(), json!(99));
    write(&v.file, serde_json::to_string(&future).unwrap());
    assert!(
        open_at(&v.file, &KEY)
            .unwrap_err()
            .message
            .contains("unsupported format v99")
    );

    let mut other = original;
    other.insert("alg".to_owned(), json!("aes-128-cbc"));
    write(&v.file, serde_json::to_string(&other).unwrap());
    assert!(
        open_at(&v.file, &KEY)
            .unwrap_err()
            .message
            .contains("unsupported format")
    );
}

#[test]
fn refuses_a_directory_where_the_vault_should_be() {
    let v = setup();
    let as_directory = v.base.join("vault-as-directory");
    std::fs::create_dir_all(&as_directory).unwrap();
    // Not "no vault yet": this must not start empty.
    let error = open_at(&as_directory, &KEY).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("cannot be opened"));
}

#[test]
fn refuses_a_payload_that_decrypts_but_is_not_a_credential_store() {
    let v = setup();
    for plaintext in [
        "plain text",
        "[1,2,3]",
        "null",
        "{\"providers\":\"oops\"}",
        "{\"providers\":[]}",
        "{\"providers\":{\"openai\":42}}",
    ] {
        write_payload(&v.file, plaintext);
        assert_eq!(kind_of(&open_at(&v.file, &KEY)), "config", "{plaintext}");
    }
}

#[cfg(unix)]
#[test]
fn reports_a_failed_write_as_storage() {
    let v = setup();
    let mut vault = open(&v.file);
    vault.set("providers", "openai", "sk-secret").unwrap();
    chmod(&v.base, 0o500);
    let outcome = vault.set("providers", "groq", "gsk");
    chmod(&v.base, 0o700);
    assert_eq!(kind_of(&outcome), "storage");
    assert!(!v.base.join("credentials.enc.tmp").exists());
}

#[test]
fn the_random_source_is_used_for_the_iv() {
    let v = setup();
    let mut vault =
        CredentialVault::open(&v.file, &KEY, Arc::new(FixedRandom::pattern(&[1, 2, 3]))).unwrap();
    vault.set("a", "b", "c").unwrap();
    let envelope = envelope(&v.file);
    let mut expected = [0u8; 12];
    FixedRandom::pattern(&[1, 2, 3]).fill(&mut expected);
    assert_eq!(envelope["iv"], json!(STANDARD.encode(expected)));
}
