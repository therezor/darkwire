//! The adapter between the composition root and the server's narrow port.
//!
//! The port exists so a route test needs neither a provider nor a vault nor a
//! workspace. This is the other side of it, and what is worth testing here is
//! exactly the five things it does that the runtime does not: persisting a
//! save, sweeping a deleted instance's credential, rebuilding after a
//! credential write, opening the vault only when there is one, and answering
//! for the model catalogue.

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

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use ghostai::server_runtime::{CliServerRuntime, ModelSource, ServerRuntimeOptions};
use ghostai_core::parse_config;
use ghostai_protocol::config::ConfigPatch;
use ghostai_protocol::rest::{
    CredentialNamespace, ModelsResponse, ProviderTestRequest, ProviderTestResponse,
    SetCredentialRequest,
};
use ghostai_runtime::{GhostRuntime, RuntimeOptions, VaultChoice};
use ghostai_security::CredentialVault;
use ghostai_server::ServerRuntime;
use ghostai_server::runtime::DirectChatInput;
use parking_lot::Mutex;

/// A runtime over a temporary home that never opens the real vault.
fn runtime(dir: &tempfile::TempDir) -> Arc<GhostRuntime> {
    ghostai_runtime::create_runtime(RuntimeOptions {
        home: Some(dir.path().display().to_string()),
        vault: VaultChoice::None,
        env: Some(HashMap::new()),
        ..RuntimeOptions::default()
    })
    .unwrap()
}

/// A vault in a temporary file under a fixed key, so nothing reaches a keychain.
fn vault(dir: &tempfile::TempDir) -> Arc<Mutex<CredentialVault>> {
    Arc::new(Mutex::new(
        CredentialVault::open(
            &dir.path().join("vault.json"),
            &[7u8; 32],
            Arc::new(ghostai_security::OsRandom),
        )
        .unwrap(),
    ))
}

fn adapter(dir: &tempfile::TempDir, options: ServerRuntimeOptions) -> Arc<CliServerRuntime> {
    CliServerRuntime::new(runtime(dir), options)
}

fn patch(value: serde_json::Value) -> ConfigPatch {
    serde_json::from_value(value).unwrap()
}

#[test]
fn a_settings_save_persists_rather_than_only_taking_effect() {
    // The runtime deliberately does not write `config.json` — previewing a
    // patch and saving one are different operations — so this is the step that
    // makes a reload see the change.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());

    let merged = port
        .apply_settings(patch(
            serde_json::json!({"ui": {"timezone": "Europe/Berlin"}}),
        ))
        .unwrap();
    assert_eq!(merged.ui.timezone, "Europe/Berlin");

    let file = dir.path().join("config.json");
    let reread = parse_config(&std::fs::read_to_string(&file).unwrap(), &file).unwrap();
    assert_eq!(reread.ui.timezone, "Europe/Berlin");
}

#[test]
fn a_patch_that_cannot_be_built_moves_neither_the_server_nor_the_file() {
    // The write runs after the rebuild, so a failure leaves both on the
    // settings that worked a moment ago.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    let before = port.config();

    // An agent stored under a key that cannot name an agent: the merge refuses
    // rather than writing a tree whose own keys it could not resolve.
    let refused = port.apply_settings(patch(
        serde_json::json!({"agents": {"list": {"Not A Valid Id": {"model": "x"}}}}),
    ));
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(port.config().agents.list.len(), before.agents.list.len());
    assert!(
        !dir.path().join("config.json").exists(),
        "a patch that could not be built still wrote the file"
    );
}

#[test]
fn deleting_a_provider_instance_takes_its_credential_with_it() {
    // The config is only half of an instance; the other half is a vault entry
    // keyed by the same id, and leaving it behind means the next instance to
    // reuse that id silently inherits somebody else's key.
    let dir = tempfile::tempdir().unwrap();
    let store = vault(&dir);
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            vault: Some(Arc::clone(&store)),
            ..ServerRuntimeOptions::default()
        },
    );

    port.apply_settings(patch(serde_json::json!({
        "providers": {"local": {"type": "ollama"}}
    })))
    .unwrap();
    port.set_credential(&SetCredentialRequest {
        namespace: CredentialNamespace::Providers,
        key: "local".to_owned(),
        value: Some("sk-local".to_owned()),
    })
    .unwrap();
    assert!(store.lock().has("providers", "local"));

    port.apply_settings(patch(serde_json::json!({"providers": {"local": null}})))
        .unwrap();
    assert!(
        !store.lock().has("providers", "local"),
        "the vault entry outlived the instance it belonged to"
    );
}

#[test]
fn credentials_are_reported_by_instance_and_never_by_value() {
    // By instance, not by provider type: two endpoints of one type can hold
    // different keys, and reporting the type would light both up for one.
    let dir = tempfile::tempdir().unwrap();
    let store = vault(&dir);
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            vault: Some(Arc::clone(&store)),
            ..ServerRuntimeOptions::default()
        },
    );

    port.apply_settings(patch(serde_json::json!({
        "providers": {"one": {"type": "ollama"}, "two": {"type": "ollama"}}
    })))
    .unwrap();
    port.set_credential(&SetCredentialRequest {
        namespace: CredentialNamespace::Providers,
        key: "one".to_owned(),
        value: Some("sk-one".to_owned()),
    })
    .unwrap();

    let present = port.credentials_present();
    assert_eq!(present.get("one"), Some(&true));
    assert_eq!(present.get("two"), Some(&false));
}

#[test]
fn an_exported_key_variable_counts_as_a_credential() {
    let dir = tempfile::tempdir().unwrap();
    let key = ghostai_providers::PROVIDERS
        .iter()
        .find(|spec| spec.env_key.is_some())
        .expect("some provider declares a key variable");
    let mut env = HashMap::new();
    env.insert(
        key.env_key.clone().unwrap_or_default(),
        "from-the-env".to_owned(),
    );

    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            env,
            ..ServerRuntimeOptions::default()
        },
    );
    port.apply_settings(patch(serde_json::json!({
        "providers": {"endpoint": {"type": key.id}}
    })))
    .unwrap();

    assert_eq!(port.credentials_present().get("endpoint"), Some(&true));
}

#[test]
fn clearing_a_credential_removes_the_entry_rather_than_storing_an_empty_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = vault(&dir);
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            vault: Some(Arc::clone(&store)),
            ..ServerRuntimeOptions::default()
        },
    );

    port.set_credential(&SetCredentialRequest {
        namespace: CredentialNamespace::Providers,
        key: "local".to_owned(),
        value: Some("sk-local".to_owned()),
    })
    .unwrap();
    port.set_credential(&SetCredentialRequest {
        namespace: CredentialNamespace::Providers,
        key: "local".to_owned(),
        value: None,
    })
    .unwrap();

    assert!(!store.lock().has("providers", "local"));
}

#[test]
fn a_credential_write_reaches_a_namespace_that_is_not_the_providers_one() {
    // A channel's bot token lives under its own namespace, keyed by channel id.
    let dir = tempfile::tempdir().unwrap();
    let store = vault(&dir);
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            vault: Some(Arc::clone(&store)),
            ..ServerRuntimeOptions::default()
        },
    );

    port.set_credential(&SetCredentialRequest {
        namespace: CredentialNamespace::Channels,
        key: "telegram".to_owned(),
        value: Some("bot-token".to_owned()),
    })
    .unwrap();

    assert!(store.lock().has("channels", "telegram"));
    assert!(!store.lock().has("providers", "telegram"));
}

#[test]
fn no_vault_is_opened_for_an_install_that_has_none() {
    // Resolving a vault key mints a keychain entry the first time it runs, and
    // an install that talks to a local model and never stores a credential
    // should not acquire one because someone opened the settings panel.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());

    let _ = port.credentials_present();
    assert!(
        !dir.path().join("vault.json").exists(),
        "reading presence created a vault"
    );
}

#[test]
fn a_reload_reads_the_file_and_does_not_write_it_back() {
    // Writing would turn a reload into a save, which is how a config edited by
    // hand gets reformatted by the button that was meant to read it.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());

    let file = dir.path().join("config.json");
    let hand_written = "{\n  \"ui\": { \"timezone\": \"Asia/Tokyo\" }\n}\n";
    std::fs::write(&file, hand_written).unwrap();

    let reloaded = port.reload().unwrap();
    assert_eq!(reloaded.ui.timezone, "Asia/Tokyo");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        hand_written,
        "a reload rewrote the file it was asked to read"
    );
}

#[test]
fn a_load_error_is_declared_and_has_no_source() {
    // Loading refuses an unreadable file, so a running server has no load error
    // to report — stated rather than left absent, which is what let the old
    // optional signature quietly never report one at all.
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        adapter(&dir, ServerRuntimeOptions::default()).load_error(),
        None
    );
}

#[test]
fn an_agent_id_naming_nothing_runnable_is_a_refusal() {
    // Silently describing the default would report tools and a prompt for an
    // agent nobody asked about.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    assert!(port.agent(Some("no-such-agent")).is_err());
    assert!(port.agent(None).is_ok());
}

#[test]
fn the_default_agent_reports_itself_as_unconfigured_on_a_bare_install() {
    // A state rather than an error: every route but a turn works on it.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    let agent = port.agent(None).unwrap();

    assert!(!agent.configured());
    // Empty rather than a sentinel: `configured` is the flag to branch on, so
    // nothing has to read meaning into a string.
    assert_eq!(agent.provider(), "");
    assert_eq!(agent.model(), "");
}

#[test]
fn the_agent_list_has_the_default_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    let agents = port.agents();
    assert!(
        agents.iter().any(|agent| agent.id == "default"),
        "{agents:?}"
    );
}

#[test]
fn the_registry_is_reported_whole_rather_than_narrowed_by_an_agent() {
    // The catalogue, not a grant: the tool list route used to return the
    // default agent's subset, which made a tool grantable only if the default
    // agent already held it.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    let registered = port.registered_tools();
    let advertised = port.agent(None).unwrap().tools();
    assert!(
        registered.len() >= advertised.len(),
        "the registry is a superset of what one agent advertises"
    );
}

#[test]
fn an_install_with_no_extension_host_counts_none() {
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    let counts = port.extensions();
    assert_eq!(counts.extensions_loaded, 0);
    assert_eq!(counts.mcp_servers_connected, 0);
}

#[test]
fn channels_are_read_through_the_callback_the_composition_root_supplies() {
    // A function rather than a list, because the composition root replaces the
    // channel manager whenever the settings that configure it are saved.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            channels: Some(Arc::new(|| {
                vec![ghostai_protocol::rest::ChannelStatus {
                    id: "telegram".to_owned(),
                    enabled: true,
                    configured: false,
                    running: false,
                    detail: None,
                }]
            })),
            ..ServerRuntimeOptions::default()
        },
    );
    assert_eq!(port.channels().len(), 1);
    assert_eq!(port.channels()[0].id, "telegram");
}

#[test]
fn an_install_with_no_channel_reports_none_rather_than_failing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        adapter(&dir, ServerRuntimeOptions::default())
            .channels()
            .is_empty()
    );
}

// ------------------------------------------------------ the model catalogue

/// A probe of one endpoint, naming the stored instance its key comes from.
fn probe(instance_id: Option<&str>) -> ProviderTestRequest {
    ProviderTestRequest {
        kind: "ollama".to_owned(),
        api_base: String::new(),
        extra_headers: indexmap::IndexMap::new(),
        api_key: None,
        instance_id: instance_id.map(str::to_owned),
    }
}

/// A catalogue that answers from memory and records what it was asked.
#[derive(Default)]
struct ScriptedModels {
    listed: Mutex<Vec<bool>>,
    invalidated: Mutex<usize>,
    tested: Mutex<Vec<String>>,
}

impl ModelSource for ScriptedModels {
    fn list(&self, refresh: bool) -> BoxFuture<'_, ghostai_core::Result<ModelsResponse>> {
        self.listed.lock().push(refresh);
        Box::pin(std::future::ready(Ok(ModelsResponse {
            models: Vec::new(),
            errors: indexmap::IndexMap::new(),
        })))
    }

    fn test<'a>(
        &'a self,
        request: &'a ProviderTestRequest,
    ) -> BoxFuture<'a, ghostai_core::Result<ProviderTestResponse>> {
        self.tested
            .lock()
            .push(request.instance_id.clone().unwrap_or_default());
        Box::pin(std::future::ready(Ok(ProviderTestResponse {
            ok: true,
            models: Vec::new(),
            reason: None,
            message: None,
        })))
    }

    fn invalidate(&self) {
        *self.invalidated.lock() += 1;
    }
}

#[tokio::test]
async fn the_model_list_is_asked_of_the_catalogue_and_carries_the_refresh_flag() {
    // That work belongs to the catalogue rather than to this file, which is why
    // it arrives as a port: the terminal's `/model` asks the same question and
    // there is one implementation of the answer.
    let dir = tempfile::tempdir().unwrap();
    let models = Arc::new(ScriptedModels::default());
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            models: Some(Arc::clone(&models) as Arc<dyn ModelSource>),
            ..ServerRuntimeOptions::default()
        },
    );

    port.models(false).unwrap().await.unwrap();
    port.models(true).unwrap().await.unwrap();
    assert_eq!(*models.listed.lock(), vec![false, true]);
}

#[tokio::test]
async fn a_build_with_no_injected_catalogue_falls_back_to_the_real_one() {
    // The default is the catalogue the terminal's `/model` reads, built over
    // the same credential reader the presence flags use — a second one would
    // open a second vault. A bare install names no instance, so the answer is
    // empty without anything being dialled.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    let listed = port.models(false).unwrap().await.unwrap();
    assert!(listed.models.is_empty());
    assert!(listed.errors.is_empty());
}

#[tokio::test]
async fn a_provider_test_reaches_the_catalogue_under_the_instance_it_named() {
    let dir = tempfile::tempdir().unwrap();
    let models = Arc::new(ScriptedModels::default());
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            models: Some(Arc::clone(&models) as Arc<dyn ModelSource>),
            ..ServerRuntimeOptions::default()
        },
    );

    port.test_provider(&probe(Some("ollama")))
        .unwrap()
        .await
        .unwrap();
    assert_eq!(*models.tested.lock(), vec!["ollama".to_owned()]);
}

#[test]
fn a_reload_drops_the_cached_catalogue() {
    // A reload is how an operator picks up an endpoint that moved or a model
    // they have just pulled, so the cached list cannot survive it.
    let dir = tempfile::tempdir().unwrap();
    let models = Arc::new(ScriptedModels::default());
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            models: Some(Arc::clone(&models) as Arc<dyn ModelSource>),
            ..ServerRuntimeOptions::default()
        },
    );

    port.reload().unwrap();
    assert_eq!(*models.invalidated.lock(), 1);
}

#[test]
fn a_credential_write_drops_the_cached_catalogue_too() {
    // A key is often exactly what stood between an endpoint and its catalogue.
    let dir = tempfile::tempdir().unwrap();
    let models = Arc::new(ScriptedModels::default());
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            vault: Some(vault(&dir)),
            models: Some(Arc::clone(&models) as Arc<dyn ModelSource>),
            ..ServerRuntimeOptions::default()
        },
    );

    port.set_credential(&SetCredentialRequest {
        namespace: CredentialNamespace::Providers,
        key: "ollama".to_owned(),
        value: Some("sk-test".to_owned()),
    })
    .unwrap();
    assert_eq!(*models.invalidated.lock(), 1);
}

// ------------------------------------------------------- the channel rebuild

#[test]
fn a_settings_save_asks_the_composition_root_to_rebuild_the_channels() {
    // `config.channels` is where a channel is turned on, and the manager fixes
    // its factories at construction — so a save means a new manager, and only
    // the composition root knows one exists.
    let dir = tempfile::tempdir().unwrap();
    let rebuilds = Arc::new(Mutex::new(0usize));
    let counted = Arc::clone(&rebuilds);
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            channels_changed: Some(Arc::new(move || {
                *counted.lock() += 1;
                Box::pin(std::future::ready(()))
            })),
            ..ServerRuntimeOptions::default()
        },
    );

    port.apply_settings(patch(
        serde_json::json!({"ui": {"timezone": "Europe/Berlin"}}),
    ))
    .unwrap();
    assert_eq!(*rebuilds.lock(), 1);
}

#[test]
fn only_a_channels_credential_bounces_a_channel() {
    // A provider key save must not bounce a bot that has nothing to do with it.
    let dir = tempfile::tempdir().unwrap();
    let rebuilds = Arc::new(Mutex::new(0usize));
    let counted = Arc::clone(&rebuilds);
    let port = adapter(
        &dir,
        ServerRuntimeOptions {
            vault: Some(vault(&dir)),
            channels_changed: Some(Arc::new(move || {
                *counted.lock() += 1;
                Box::pin(std::future::ready(()))
            })),
            ..ServerRuntimeOptions::default()
        },
    );

    port.set_credential(&SetCredentialRequest {
        namespace: CredentialNamespace::Providers,
        key: "ollama".to_owned(),
        value: Some("sk-test".to_owned()),
    })
    .unwrap();
    assert_eq!(*rebuilds.lock(), 0);

    port.set_credential(&SetCredentialRequest {
        namespace: CredentialNamespace::Channels,
        key: "telegram".to_owned(),
        value: Some("bot-token".to_owned()),
    })
    .unwrap();
    assert_eq!(*rebuilds.lock(), 1);
}

// ------------------------------------------------------------- the agent view

#[test]
fn an_agent_reports_its_own_jail_and_a_named_workspace_s() {
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    let agent = port.agent(None).unwrap();

    assert!(agent.jail().root().exists());
    // A named workspace gets its own root rather than the default one's. A
    // heartbeat in a named workspace that read the default workspace's files
    // would skip forever on a file it could not see.
    let named = agent.jail_for("reports").unwrap();
    assert_ne!(named.root(), agent.jail().root());
}

#[test]
fn an_agent_names_the_model_and_the_window_it_runs_in() {
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    let agent = port.agent(None).unwrap();

    assert_eq!(agent.id(), ghostai_protocol::DEFAULT_AGENT_ID);
    assert!(!agent.label().is_empty());
    assert!(agent.context_window_tokens() > 0);
    // Every tool the registry holds, not a narrowed set: the view answers for
    // the install rather than for one turn's permissions.
    assert!(!agent.tools().is_empty());
}

#[test]
fn the_workspace_store_and_the_session_store_are_the_runtime_s_own() {
    // One connection, shared: a second store over the same file would not see
    // the first one's uncommitted writes.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    assert!(!port.workspaces().list().unwrap().is_empty());
    // Releasing a workspace nobody holds is a no-op rather than a refusal.
    port.release_workspace("default");
}

#[test]
fn an_install_with_no_mcp_server_and_no_command_reports_neither() {
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    assert!(port.mcp_servers().is_empty());
    assert!(port.commands().is_empty());
    assert!(port.extension_statuses().is_empty());
    assert!(port.toolboxes().is_empty());
}

#[test]
fn approving_an_extension_this_build_does_not_have_is_answered_rather_than_ignored() {
    // The routes need to tell "no such extension" from "this build cannot
    // approve extensions at all", and `None` is the second.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    assert!(port.approve_extension("nothing").is_some());
    assert!(port.revoke_extension("nothing").is_some());
}

#[tokio::test]
async fn a_direct_chat_on_a_bare_install_is_refused_rather_than_sent_somewhere() {
    // Resolution answers nothing rather than picking a provider, because a
    // request landing at an endpoint nobody chose fails as a 401 from somewhere
    // unexpected.
    let dir = tempfile::tempdir().unwrap();
    let port = adapter(&dir, ServerRuntimeOptions::default());
    let error = port
        .chat(DirectChatInput {
            agent_id: None,
            model: None,
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: ghostai_providers::ToolChoice::Auto,
            max_tokens: None,
            token: tokio_util::sync::CancellationToken::new(),
        })
        .unwrap()
        .await
        .unwrap_err();
    assert!(!error.message.is_empty());
}
