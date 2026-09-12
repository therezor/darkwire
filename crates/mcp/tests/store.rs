//! Where OAuth secrets live: the in-memory store and the vault namespace.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use ghostai_mcp::{
    MCP_CREDENTIAL_NAMESPACE, McpSecretSlot, McpSecretStore, MemorySecretStore, VaultSecretStore,
};
use ghostai_security::CredentialVault;
use ghostai_security::testkit::FixedRandom;
use parking_lot::Mutex;

#[test]
fn memory_store_clears_every_slot_for_one_server_when_asked_for_none_in_particular() {
    let store = MemorySecretStore::new();
    store.write("a", McpSecretSlot::Tokens, "1").unwrap();
    store.write("a", McpSecretSlot::Client, "2").unwrap();
    store.write("b", McpSecretSlot::Tokens, "3").unwrap();

    store.clear("a", None).unwrap();
    assert_eq!(store.read("a", McpSecretSlot::Tokens), None);
    assert_eq!(store.read("a", McpSecretSlot::Client), None);
    assert_eq!(store.read("b", McpSecretSlot::Tokens).as_deref(), Some("3"));
}

#[test]
fn memory_store_clears_exactly_one_slot_when_asked() {
    let store = MemorySecretStore::new();
    store.write("a", McpSecretSlot::Tokens, "1").unwrap();
    store.write("a", McpSecretSlot::Client, "2").unwrap();
    store.clear("a", Some(McpSecretSlot::Tokens)).unwrap();
    assert_eq!(store.read("a", McpSecretSlot::Tokens), None);
    assert_eq!(store.read("a", McpSecretSlot::Client).as_deref(), Some("2"));
}

#[test]
fn vault_store_writes_under_the_mcp_namespace_with_the_server_and_slot_as_key() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("vault.json");
    let vault =
        CredentialVault::open(&file, &[7u8; 32], Arc::new(FixedRandom::constant(9))).unwrap();
    let vault = Arc::new(Mutex::new(vault));
    let store = VaultSecretStore::new(Arc::clone(&vault));

    store
        .write("github", McpSecretSlot::Tokens, "{\"a\":1}")
        .unwrap();
    store
        .write("github", McpSecretSlot::Client, "{\"c\":1}")
        .unwrap();
    assert_eq!(
        vault.lock().get(MCP_CREDENTIAL_NAMESPACE, "github:tokens"),
        Some("{\"a\":1}")
    );
    assert_eq!(
        store.read("github", McpSecretSlot::Tokens).as_deref(),
        Some("{\"a\":1}")
    );
    assert_eq!(
        store.read("github", McpSecretSlot::Client).as_deref(),
        Some("{\"c\":1}")
    );

    store.clear("github", Some(McpSecretSlot::Client)).unwrap();
    assert_eq!(store.read("github", McpSecretSlot::Client), None);
    store.clear("github", None).unwrap();
    assert_eq!(store.read("github", McpSecretSlot::Tokens), None);
    assert!(vault.lock().keys(MCP_CREDENTIAL_NAMESPACE).is_empty());
}

#[test]
fn slots_spell_their_keys() {
    assert_eq!(McpSecretSlot::Tokens.as_str(), "tokens");
    assert_eq!(McpSecretSlot::Client.as_str(), "client");
}
