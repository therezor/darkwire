//! Where an MCP server's secrets live.
//!
//! `mcp_servers` is a legal `CredentialVault` namespace beside `providers`, and
//! this is the module that uses it. The split it enforces:
//!
//! - **`headers` stays in `config.yaml`, in the clear.** It is operator-typed
//!   configuration, the panel showing it is the panel it was typed into, and
//!   pretending otherwise would be security theatre over a file the operator
//!   can open.
//! - **Anything OAuth minted goes in the vault.** A refresh token is not
//!   operator-typed; it is a long-lived credential this process obtained, and
//!   the vault exists precisely so that one is encrypted at rest.
//!
//! The trait is here rather than the vault being taken directly so a test can
//! hold one in memory without a keychain, and so [`crate::oauth`] cannot reach
//! any namespace but its own.

use std::collections::HashMap;
use std::sync::Arc;

use ghostai_core::Result;
use ghostai_security::CredentialVault;
use parking_lot::Mutex;

/// The vault namespace every MCP credential lives under.
pub const MCP_CREDENTIAL_NAMESPACE: &str = "mcp_servers";

/// What OAuth persists per server. One key each, under the server's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum McpSecretSlot {
    /// The token response: access token, refresh token, expiry.
    Tokens,
    /// The dynamically registered client.
    Client,
}

impl McpSecretSlot {
    const ALL: [McpSecretSlot; 2] = [McpSecretSlot::Tokens, McpSecretSlot::Client];

    /// The key suffix.
    pub fn as_str(self) -> &'static str {
        match self {
            McpSecretSlot::Tokens => "tokens",
            McpSecretSlot::Client => "client",
        }
    }
}

/// `<serverId>:<slot>`.
fn key_for(server_id: &str, slot: McpSecretSlot) -> String {
    format!("{server_id}:{}", slot.as_str())
}

/// Per-server secret storage, one slot at a time.
pub trait McpSecretStore: Send + Sync {
    /// The stored value, if any.
    fn read(&self, server_id: &str, slot: McpSecretSlot) -> Option<String>;
    /// Stores `value`, replacing what was there.
    fn write(&self, server_id: &str, slot: McpSecretSlot, value: &str) -> Result<()>;
    /// Removes one slot, or every slot for the server when `slot` is `None`.
    fn clear(&self, server_id: &str, slot: Option<McpSecretSlot>) -> Result<()>;
}

/// The slots `clear` touches for a given request.
fn slots(slot: Option<McpSecretSlot>) -> Vec<McpSecretSlot> {
    slot.map_or_else(|| McpSecretSlot::ALL.to_vec(), |one| vec![one])
}

/// The credential vault, scoped to the MCP namespace.
#[derive(Debug, Clone)]
pub struct VaultSecretStore {
    vault: Arc<Mutex<CredentialVault>>,
}

impl VaultSecretStore {
    /// A store over a shared vault.
    pub fn new(vault: Arc<Mutex<CredentialVault>>) -> VaultSecretStore {
        VaultSecretStore { vault }
    }
}

impl McpSecretStore for VaultSecretStore {
    fn read(&self, server_id: &str, slot: McpSecretSlot) -> Option<String> {
        self.vault
            .lock()
            .get(MCP_CREDENTIAL_NAMESPACE, &key_for(server_id, slot))
            .map(str::to_owned)
    }

    fn write(&self, server_id: &str, slot: McpSecretSlot, value: &str) -> Result<()> {
        self.vault
            .lock()
            .set(MCP_CREDENTIAL_NAMESPACE, &key_for(server_id, slot), value)
    }

    fn clear(&self, server_id: &str, slot: Option<McpSecretSlot>) -> Result<()> {
        let mut vault = self.vault.lock();
        for candidate in slots(slot) {
            vault.delete(MCP_CREDENTIAL_NAMESPACE, &key_for(server_id, candidate))?;
        }
        Ok(())
    }
}

/// A store with nowhere to persist.
///
/// What an install with `vault: false` gets, and what every test uses. Tokens
/// survive the process and no longer, which for a build that switched the
/// vault off is the honest behaviour: the alternative is writing a refresh
/// token to somewhere it was explicitly not asked to go.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    contents: Mutex<HashMap<String, String>>,
}

impl MemorySecretStore {
    /// An empty store.
    pub fn new() -> MemorySecretStore {
        MemorySecretStore::default()
    }
}

impl McpSecretStore for MemorySecretStore {
    fn read(&self, server_id: &str, slot: McpSecretSlot) -> Option<String> {
        self.contents.lock().get(&key_for(server_id, slot)).cloned()
    }

    fn write(&self, server_id: &str, slot: McpSecretSlot, value: &str) -> Result<()> {
        self.contents
            .lock()
            .insert(key_for(server_id, slot), value.to_owned());
        Ok(())
    }

    fn clear(&self, server_id: &str, slot: Option<McpSecretSlot>) -> Result<()> {
        let mut contents = self.contents.lock();
        for candidate in slots(slot) {
            contents.remove(&key_for(server_id, candidate));
        }
        Ok(())
    }
}
