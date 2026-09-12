//! Every configured MCP server, reconciled against the settings tree.
//!
//! The manager is handed a map of `McpServerConfig` and diffs it. That is the
//! whole of its coupling: it does not know there is a config *file*, it does
//! not know a save happened, and it has never heard of HTTP or a WebSocket.
//! The composition root hands it the map and supplies a sink; everything else
//! — a connecting server's tools appearing in the agent editor, a
//! `tools.changed` frame reaching an open tab — falls out of the registry
//! mutating.
//!
//! **`reconcile` is synchronous and cannot fail.** The composition root calls
//! it from the region past which nothing fails, and an operator saving one
//! server's URL must not lose the save because another server is unreachable.
//! Every dial happens on a background task; every failure lands on a status
//! row.
//!
//! The diff has four outcomes, and the third is the one worth the machinery:
//!
//! | change                                | action                                    |
//! | ------------------------------------- | ----------------------------------------- |
//! | gone from config, or `enabled: false` | close, unregister, drop                   |
//! | new, or its transport moved           | close the old, construct and start        |
//! | only `enabledTools`/`toolTimeoutMs`   | re-bridge from the descriptors in memory  |
//! | nothing                               | left entirely alone                       |

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::future::BoxFuture;
use ghostai_core::{Clock, Result};
use ghostai_protocol::{McpOAuthConfig, McpServerConfig, McpServerState, McpServerStatus};
use ghostai_security::{CredentialVault, RandomSource};
use indexmap::IndexMap;
use parking_lot::Mutex;

use crate::callback::{CallbackListener, CallbackListenerOptions};
use crate::connection::{
    AuthorizationAttempt, AuthorizationBroker, BackoffOptions, McpConnection, McpConnectionOptions,
    StatusChangedFn,
};
use crate::oauth::{EndpointGuard, OAuthFlow, OAuthFlowOptions};
use crate::session::McpConnector;
use crate::spec::{McpConnectionSpec, resolve_spec, transport_fingerprint};
use crate::store::{McpSecretStore, MemorySecretStore, VaultSecretStore};

/// Where a server's tools go.
///
/// `ToolSink` in `ghostai-tools`, under this crate's own name for it: "the sink
/// a server's tools go to" is how every call site here reads. Implemented in
/// `ghostai-runtime`, the only place that knows both this and the registry.
pub use ghostai_tools::ToolSink as McpToolSink;

/// Everything the manager needs.
pub struct McpManagerOptions {
    /// Where tools go.
    pub sink: Arc<dyn McpToolSink>,
    /// Defaults to the real SDK connector at the composition root; a test
    /// supplies a fake.
    pub connect: Arc<dyn McpConnector>,
    /// Stamps `last_connected_at_ms` and judges token expiry.
    pub clock: Arc<dyn Clock>,
    /// Backoff jitter, OAuth `state`, PKCE verifiers.
    pub random: Arc<dyn RandomSource>,
    /// `None` keeps OAuth tokens in memory for the life of the process.
    pub vault: Option<Arc<Mutex<CredentialVault>>>,
    /// The backoff shape.
    pub backoff: BackoffOptions,
    /// Fired after any change to a server's status or its registered tools.
    ///
    /// Not how a tool-list change reaches a transport — that happens through
    /// the registry's own subscription, because the extension host needs the
    /// same seam and the registry is what they have in common. This is for the
    /// status list, which nothing else can observe.
    pub on_status_changed: Option<StatusChangedFn>,
    /// Overrides the loopback OAuth callback port. `Some(0)` asks for any free
    /// one.
    pub callback_port: Option<u16>,
    /// Speaks to authorization servers.
    pub http: reqwest::Client,
    /// The policy discovered OAuth endpoints must pass. `None` trusts
    /// discovery, which is only right for a test.
    pub endpoint_guard: Option<Arc<EndpointGuard>>,
}

struct Entry {
    connection: McpConnection,
    fingerprint: String,
}

struct Inner {
    options: McpManagerOptions,
    connections: Mutex<IndexMap<String, Entry>>,
    /// Servers that could not even be resolved into a spec.
    refused: Mutex<IndexMap<String, McpServerStatus>>,
    secrets: Arc<dyn McpSecretStore>,
    callback: Mutex<Option<CallbackListener>>,
    closed: AtomicBool,
}

/// The MCP servers of one process.
#[derive(Clone)]
pub struct McpManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for McpManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpManager")
            .field("servers", &self.inner.connections.lock().len())
            .finish_non_exhaustive()
    }
}

fn disabled_row(server_id: &str, config: &McpServerConfig) -> McpServerStatus {
    McpServerStatus {
        id: server_id.to_owned(),
        transport: config.kind,
        state: McpServerState::Disabled,
        enabled: false,
        tools: Vec::new(),
        filtered_tools: Vec::new(),
        server_name: String::new(),
        server_version: String::new(),
        last_error: None,
        last_connected_at_ms: None,
        authorization_url: None,
        warnings: Vec::new(),
    }
}

impl McpManager {
    /// A manager holding no servers.
    pub fn new(options: McpManagerOptions) -> McpManager {
        let secrets: Arc<dyn McpSecretStore> = match &options.vault {
            Some(vault) => Arc::new(VaultSecretStore::new(Arc::clone(vault))),
            None => Arc::new(MemorySecretStore::new()),
        };
        McpManager {
            inner: Arc::new(Inner {
                options,
                connections: Mutex::new(IndexMap::new()),
                refused: Mutex::new(IndexMap::new()),
                secrets,
                callback: Mutex::new(None),
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// How many servers a turn could actually reach right now.
    pub fn connected_count(&self) -> usize {
        self.inner
            .connections
            .lock()
            .values()
            .filter(|entry| entry.connection.state() == McpServerState::Ready)
            .count()
    }

    /// Every server, connected or not, sorted by id.
    pub fn statuses(&self) -> Vec<McpServerStatus> {
        let mut rows: Vec<McpServerStatus> = self.inner.refused.lock().values().cloned().collect();
        rows.extend(
            self.inner
                .connections
                .lock()
                .values()
                .map(|entry| entry.connection.status()),
        );
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        rows
    }

    /// Applies the settings tree. Synchronous; never fails.
    ///
    /// Must be called from inside a Tokio runtime: connecting and retiring are
    /// both done on tasks it spawns.
    pub fn reconcile(&self, servers: &IndexMap<String, McpServerConfig>) {
        if self.inner.closed.load(Ordering::SeqCst) {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        self.inner.refused.lock().clear();

        for (server_id, config) in servers {
            seen.insert(server_id.clone());

            if !config.enabled {
                self.retire(server_id);
                self.inner
                    .refused
                    .lock()
                    .insert(server_id.clone(), disabled_row(server_id, config));
                continue;
            }

            match resolve_spec(server_id, config) {
                Ok(spec) => self.apply(spec),
                Err(error) => {
                    // A misconfigured entry is a property of the settings tree,
                    // so it reads as a failed row rather than a refused save.
                    // The operator sees the sentence beside the server it is
                    // about.
                    self.retire(server_id);
                    self.inner.refused.lock().insert(
                        server_id.clone(),
                        McpServerStatus {
                            state: McpServerState::Failed,
                            enabled: true,
                            last_error: Some(error.message),
                            ..disabled_row(server_id, config)
                        },
                    );
                }
            }
        }

        let gone: Vec<String> = self
            .inner
            .connections
            .lock()
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect();
        for server_id in gone {
            self.retire(&server_id);
        }

        if let Some(listener) = &self.inner.options.on_status_changed {
            listener();
        }
    }

    fn apply(&self, spec: McpConnectionSpec) {
        let fingerprint = transport_fingerprint(&spec);
        let existing = self
            .inner
            .connections
            .lock()
            .get(&spec.server_id)
            .map(|entry| (entry.connection.clone(), entry.fingerprint.clone()));

        if let Some((connection, current)) = existing {
            if current == fingerprint {
                // Same process, same endpoint. Only what it exposes can have
                // moved, and that is a filter over descriptors this connection
                // already holds. The fingerprints matched, so this cannot fail.
                let _ = connection.rebridge(spec);
                return;
            }
            self.retire(&spec.server_id);
        }

        let authorization: Option<Arc<dyn AuthorizationBroker>> = spec.oauth().map(|oauth| {
            Arc::new(Broker {
                inner: Arc::clone(&self.inner),
                oauth: oauth.clone(),
            }) as Arc<dyn AuthorizationBroker>
        });

        let sink = Arc::clone(&self.inner.options.sink);
        let connection = McpConnection::new(McpConnectionOptions {
            spec: spec.clone(),
            connect: Arc::clone(&self.inner.options.connect),
            publish: Arc::new(move |server_id: &str, tools| {
                for name in sink.replace(server_id, tools) {
                    tracing::warn!(
                        server = %server_id,
                        tool = %name,
                        "mcp tool name is already registered by another source"
                    );
                }
            }),
            on_status_changed: self.inner.options.on_status_changed.clone(),
            authorization,
            clock: Arc::clone(&self.inner.options.clock),
            random: Arc::clone(&self.inner.options.random),
            backoff: self.inner.options.backoff.clone(),
        });

        self.inner.connections.lock().insert(
            spec.server_id.clone(),
            Entry {
                connection: connection.clone(),
                fingerprint,
            },
        );
        connection.start();
    }

    fn retire(&self, server_id: &str) {
        let Some(entry) = self.inner.connections.lock().shift_remove(server_id) else {
            return;
        };
        // The tools go now rather than when the close resolves: a turn starting
        // in the meantime must not be offered a tool whose server is being torn
        // down.
        self.inner.options.sink.replace(server_id, Vec::new());
        tokio::spawn(async move { entry.connection.close().await });
    }

    /// Closes every connection and the callback listener.
    pub async fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let entries: Vec<Entry> = self
            .inner
            .connections
            .lock()
            .drain(..)
            .map(|(_, e)| e)
            .collect();
        for entry in &entries {
            self.inner
                .options
                .sink
                .replace(&entry.connection.server_id(), Vec::new());
        }
        futures::future::join_all(entries.iter().map(|entry| entry.connection.close())).await;
        let callback = self.inner.callback.lock().take();
        if let Some(callback) = callback {
            callback.close().await;
        }
    }
}

/// One authorization attempt's flow, callback and code.
///
/// Built per connection rather than per manager because the `state` that
/// routes a redirect back is minted per attempt, and a flow holds the PKCE
/// verifier for exactly one exchange.
struct Broker {
    inner: Arc<Inner>,
    oauth: McpOAuthConfig,
}

impl AuthorizationBroker for Broker {
    fn begin(&self, server_id: &str) -> BoxFuture<'_, Result<AuthorizationAttempt>> {
        let server_id = server_id.to_owned();
        Box::pin(async move {
            let callback = {
                let mut slot = self.inner.callback.lock();
                slot.get_or_insert_with(|| {
                    CallbackListener::new(CallbackListenerOptions {
                        random: Arc::clone(&self.inner.options.random),
                        port: self.inner.options.callback_port,
                    })
                })
                .clone()
            };
            let handle = callback
                .begin(&server_id, self.oauth.callback_timeout_ms)
                .await?;
            let manager = Arc::clone(&self.inner);
            let reported_for = server_id.clone();
            let flow = OAuthFlow::new(OAuthFlowOptions {
                server_id: server_id.clone(),
                config: self.oauth.clone(),
                store: Arc::clone(&self.inner.secrets),
                redirect_url: handle.redirect_url.clone(),
                state: handle.state.clone(),
                http: self.inner.options.http.clone(),
                random: Arc::clone(&self.inner.options.random),
                clock: Arc::clone(&self.inner.options.clock),
                guard: self.inner.options.endpoint_guard.clone(),
                on_authorization_required: Arc::new(move |url: &str| {
                    let connection = manager
                        .connections
                        .lock()
                        .get(&reported_for)
                        .map(|entry| entry.connection.clone());
                    if let Some(connection) = connection {
                        connection.report_authorization_url(url);
                    }
                }),
            });
            Ok(AuthorizationAttempt {
                auth: Arc::new(flow),
                handle: Arc::new(handle),
            })
        })
    }
}
