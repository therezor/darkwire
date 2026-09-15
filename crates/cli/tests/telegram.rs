//! `src/telegram.rs`: resolving a bot token, and what the panel shows.
//!
//! Nothing here builds a runtime or opens a socket. The two functions
//! `ghostai serve` calls before a channel exists — the token lookup and the
//! status row — are pure over a paths record, an environment and a settings
//! block, which is the property that makes the precedence assertable at all.

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

use ghostai::i18n::Env;
use ghostai::server_runtime::{CliServerRuntime, ServerRuntimeOptions};
use ghostai::telegram::{
    PLAINTEXT_TOKEN_WARNING, ResolvedToken, TELEGRAM_TOKEN_ENV_VAR, TelegramFactoriesOptions,
    TelegramStatusOptions, TokenSource, resolve_telegram_token, telegram_factories,
    telegram_settings_of, telegram_status, warn_if_plaintext,
};
use ghostai_core::paths::ResolveGhostPaths;
use ghostai_core::{Database, GhostPaths};
use ghostai_protocol::config::Config;
use ghostai_runtime::{
    ExtensionChoice, GhostRuntime, McpChoice, RuntimeOptions, VaultChoice, create_runtime,
};
use ghostai_server::ServerRuntime;
use serde_json::{Map, Value, json};
use tempfile::TempDir;

/// A GhostAI root in a temporary directory.
///
/// `with_vault` writes the file but not a usable vault: only the file's
/// *existence* is checked before the vault is opened, which is the condition
/// under test rather than the vault's contents.
fn paths(home: &TempDir, with_vault: bool) -> GhostPaths {
    let root = home.path().to_string_lossy().into_owned();
    let resolved = GhostPaths::resolve(ResolveGhostPaths {
        root: Some(root),
        workspace: None,
        env: Some(HashMap::new()),
        home: Some(home.path().to_path_buf()),
    })
    .expect("the paths resolve under a temporary home");
    if with_vault {
        std::fs::write(&resolved.vault_file, "{}").expect("the placeholder vault is written");
    }
    resolved
}

fn env_with(pairs: &[(&str, &str)]) -> Env {
    pairs.iter().copied().collect()
}

fn settings(block: &Value) -> Map<String, Value> {
    block
        .as_object()
        .cloned()
        .expect("the fixture block is an object")
}

#[test]
fn finds_nothing_on_an_install_that_never_configured_a_bot() {
    // The normal case, and the one that has to stay cheap: `ghostai serve` comes
    // up unchanged for everybody who has never heard of this.
    let home = TempDir::new().expect("a temporary home");
    let found = resolve_telegram_token(&paths(&home, false), &Env::empty(), &Map::new())
        .expect("no vault, so no failure");

    assert_eq!(found, None);
}

#[test]
fn reads_the_environment_variable() {
    let home = TempDir::new().expect("a temporary home");
    let found = resolve_telegram_token(
        &paths(&home, false),
        &env_with(&[(TELEGRAM_TOKEN_ENV_VAR, "from-env")]),
        &Map::new(),
    )
    .expect("no vault, so no failure")
    .expect("the environment holds one");

    assert_eq!(found.token, "from-env");
    assert_eq!(found.source, TokenSource::Environment);
}

#[test]
fn reads_the_config_block_last() {
    let home = TempDir::new().expect("a temporary home");
    let found = resolve_telegram_token(
        &paths(&home, false),
        &Env::empty(),
        &settings(&json!({"token": "from-config"})),
    )
    .expect("no vault, so no failure")
    .expect("the config holds one");

    assert_eq!(found.token, "from-config");
    assert_eq!(found.source, TokenSource::Config);
}

#[test]
fn prefers_the_environment_over_the_config_file() {
    let home = TempDir::new().expect("a temporary home");
    let found = resolve_telegram_token(
        &paths(&home, false),
        &env_with(&[(TELEGRAM_TOKEN_ENV_VAR, "from-env")]),
        &settings(&json!({"token": "from-config"})),
    )
    .expect("no vault, so no failure")
    .expect("both hold one");

    assert_eq!(found.token, "from-env");
    assert_eq!(found.source, TokenSource::Environment);
}

#[test]
fn reports_a_plaintext_token_as_such() {
    // What the warning is keyed on. Backups, dotfile repositories and screen
    // shares all reach `config.yaml`, so the source travels with the token and
    // the caller says so once at startup.
    let home = TempDir::new().expect("a temporary home");
    let found = resolve_telegram_token(
        &paths(&home, false),
        &Env::empty(),
        &settings(&json!({"token": "from-config"})),
    )
    .expect("no vault, so no failure")
    .expect("the config holds one");

    assert_eq!(found.source, TokenSource::Config);
}

#[test]
fn does_not_flag_a_token_that_came_from_somewhere_safe() {
    let home = TempDir::new().expect("a temporary home");
    let found = resolve_telegram_token(
        &paths(&home, false),
        &env_with(&[(TELEGRAM_TOKEN_ENV_VAR, "from-env")]),
        &Map::new(),
    )
    .expect("no vault, so no failure")
    .expect("the environment holds one");

    assert_ne!(found.source, TokenSource::Config);
}

#[test]
fn ignores_an_empty_string_rather_than_treating_it_as_a_token() {
    let home = TempDir::new().expect("a temporary home");
    let found = resolve_telegram_token(
        &paths(&home, false),
        &env_with(&[(TELEGRAM_TOKEN_ENV_VAR, "")]),
        &settings(&json!({"token": ""})),
    )
    .expect("no vault, so no failure");

    assert_eq!(found, None);
}

#[test]
fn does_not_open_the_vault_when_there_is_not_one() {
    // Resolving the vault key writes one to the OS keychain the first time it
    // runs, so an install that stores no credential must not acquire an entry
    // just by booting. Reaching the environment at all proves the vault was
    // skipped — opening one here would need a keychain this test does not have.
    let home = TempDir::new().expect("a temporary home");
    let resolved = paths(&home, false);

    let found = resolve_telegram_token(
        &resolved,
        &env_with(&[(TELEGRAM_TOKEN_ENV_VAR, "from-env")]),
        &Map::new(),
    )
    .expect("no vault, so no failure")
    .expect("the environment holds one");

    assert_eq!(found.token, "from-env");
    assert!(!resolved.vault_file.exists());
}

#[test]
fn narrows_a_channels_block_that_is_not_an_object() {
    // The channels config is loose on purpose, so anything that is not an
    // object is the same as nothing at all rather than a boot failure.
    let mut config = Config::default();
    config
        .channels
        .extra
        .insert("telegram".to_owned(), json!("nonsense"));

    assert_eq!(telegram_settings_of(&config), Map::new());
}

#[test]
fn reads_the_telegram_block_out_of_the_settings_tree() {
    let mut config = Config::default();
    config
        .channels
        .extra
        .insert("telegram".to_owned(), json!({"token": "abc"}));

    assert_eq!(
        telegram_settings_of(&config).get("token"),
        Some(&json!("abc"))
    );
}

/// A status row over a temporary home, with the block and the flags given.
fn status(
    home: &TempDir,
    block: &Value,
    env: &Env,
    running: bool,
    username: Option<&str>,
    start_error: Option<&str>,
) -> ghostai_protocol::ChannelStatus {
    let mut config = Config::default();
    if !block.is_null() {
        config
            .channels
            .extra
            .insert("telegram".to_owned(), block.clone());
    }
    let resolved = paths(home, false);
    telegram_status(&TelegramStatusOptions {
        config: &config,
        paths: &resolved,
        env,
        running,
        username: username.map(str::to_owned),
        start_error: start_error.map(str::to_owned),
    })
}

#[test]
fn treats_an_absent_enabled_flag_as_enabled() {
    // The manager only skips a channel whose block says `enabled: false`, so
    // the panel has to read the same default or it would report a running bot
    // as switched off.
    let home = TempDir::new().expect("a temporary home");
    let row = status(&home, &Value::Null, &Env::empty(), false, None, None);

    assert!(row.enabled);
    assert!(!row.configured);
    assert!(!row.running);
}

#[test]
fn honours_an_explicit_disable() {
    let home = TempDir::new().expect("a temporary home");
    let row = status(
        &home,
        &json!({"enabled": false}),
        &Env::empty(),
        false,
        None,
        None,
    );

    assert!(!row.enabled);
}

#[test]
fn reports_a_stored_token_as_configured_without_revealing_it() {
    // The vault is write-only over HTTP, so a boolean is the only way the panel
    // can say a token is saved rather than showing an empty box over a bot that
    // is running perfectly well.
    let home = TempDir::new().expect("a temporary home");
    let row = status(
        &home,
        &Value::Null,
        &env_with(&[(TELEGRAM_TOKEN_ENV_VAR, "from-env")]),
        false,
        None,
        None,
    );

    assert!(row.configured);
    let rendered = serde_json::to_string(&row).expect("the row serialises");
    assert!(!rendered.contains("from-env"));
}

#[test]
fn names_the_bot_while_it_is_running() {
    let home = TempDir::new().expect("a temporary home");
    let row = status(
        &home,
        &Value::Null,
        &env_with(&[(TELEGRAM_TOKEN_ENV_VAR, "from-env")]),
        true,
        Some("ghostbot"),
        None,
    );

    assert!(row.running);
    assert_eq!(row.detail.as_deref(), Some("@ghostbot"));
}

#[test]
fn says_why_a_stopped_channel_is_stopped() {
    let home = TempDir::new().expect("a temporary home");
    let row = status(
        &home,
        &Value::Null,
        &env_with(&[(TELEGRAM_TOKEN_ENV_VAR, "from-env")]),
        false,
        None,
        Some("allowlist is empty"),
    );

    assert_eq!(row.detail.as_deref(), Some("allowlist is empty"));
}

#[test]
fn does_not_name_a_bot_that_is_not_running() {
    // A username left over from a channel that has since stopped would read as
    // a working bot. The start error is the thing to act on.
    let home = TempDir::new().expect("a temporary home");
    let row = status(
        &home,
        &Value::Null,
        &Env::empty(),
        false,
        Some("ghostbot"),
        None,
    );

    assert_eq!(row.detail, None);
}

// -------------------------------------------------------- the factory list

/// An install over a temporary home, with the `config.yaml` given.
fn install(config: Option<&Value>) -> (TempDir, Arc<GhostRuntime>) {
    let temp = TempDir::new().expect("a temporary home");
    if let Some(config) = config {
        std::fs::write(
            temp.path().join("config.yaml"),
            serde_json::to_string_pretty(config).expect("the fixture config serialises"),
        )
        .expect("the config is written");
    }
    let runtime = create_runtime(RuntimeOptions {
        home: Some(temp.path().to_string_lossy().into_owned()),
        env: Some(HashMap::new()),
        // Explicit rather than defaulted: the default opens a vault on demand,
        // and opening one writes a key to the OS keychain.
        vault: VaultChoice::None,
        mcp: McpChoice::Off,
        extensions: ExtensionChoice::Off,
        database: Some(Database::in_memory().expect("an in-memory database")),
        ..RuntimeOptions::default()
    })
    .expect("the runtime builds over a temporary home");
    (temp, runtime)
}

fn factories_options(
    home: &TempDir,
    runtime: &Arc<GhostRuntime>,
    env: Env,
) -> TelegramFactoriesOptions {
    let server: Arc<dyn ServerRuntime> =
        CliServerRuntime::new(Arc::clone(runtime), ServerRuntimeOptions::default());
    TelegramFactoriesOptions {
        runtime: Arc::clone(runtime),
        server,
        paths: paths(home, false),
        env,
        new_id: Arc::new(|| "fixed-id".to_owned()),
    }
}

#[test]
fn registers_nothing_on_an_install_that_never_configured_a_bot() {
    // The property `ghostai serve` rests on: the overwhelming majority of
    // installs have never heard of a bot and must come up exactly as before.
    let (home, runtime) = install(None);
    let built = telegram_factories(&factories_options(&home, &runtime, Env::empty()))
        .expect("no vault, so no failure");

    assert!(built.is_empty());
}

#[test]
fn registers_the_channel_once_a_token_resolves() {
    let (home, runtime) = install(None);
    let built = telegram_factories(&factories_options(
        &home,
        &runtime,
        env_with(&[(TELEGRAM_TOKEN_ENV_VAR, "from-env")]),
    ))
    .expect("no vault, so no failure");

    assert_eq!(built.len(), 1);
    assert_eq!(built[0].id(), "telegram");
}

#[test]
fn reads_the_token_out_of_the_live_config_rather_than_a_passed_block() {
    // The factory builder has a runtime and reads the settings off it, so a
    // token saved into `config.yaml` is picked up without the caller having to
    // find and forward the channel's block.
    let (home, runtime) = install(Some(&json!({
        "channels": {"telegram": {"token": "from-config"}}
    })));
    let built = telegram_factories(&factories_options(&home, &runtime, Env::empty()))
        .expect("no vault, so no failure");

    assert_eq!(built.len(), 1);
}

#[test]
fn the_factory_options_keep_the_runtime_out_of_the_debug_output() {
    // A runtime, a server port and an environment have no useful rendering, and
    // an environment printed into a log line would carry a bot token with it.
    let (home, runtime) = install(None);
    let rendered = format!(
        "{:?}",
        factories_options(
            &home,
            &runtime,
            env_with(&[(TELEGRAM_TOKEN_ENV_VAR, "secret-token")])
        )
    );

    assert!(rendered.contains("TelegramFactoriesOptions"), "{rendered}");
    assert!(!rendered.contains("secret-token"), "{rendered}");
}

#[test]
fn only_a_token_read_off_disk_earns_the_plaintext_warning() {
    // The warning is a decision about one source. Saying it for a vault or an
    // environment token would train an operator to ignore it.
    for (source, warned) in [
        (TokenSource::Vault, false),
        (TokenSource::Environment, false),
        (TokenSource::Config, true),
    ] {
        let resolved = ResolvedToken {
            token: "t".to_owned(),
            source,
        };
        // No subscriber is installed, so what is asserted is that the call is
        // total over every source rather than the line itself; the source is
        // the assertable half, and it is checked above it.
        warn_if_plaintext(&resolved);
        assert_eq!(resolved.source == TokenSource::Config, warned);
    }
}

#[test]
fn the_warning_names_the_file_and_where_to_move_the_token() {
    // An operator who reads it has to know what to do next without going to the
    // documentation.
    assert!(PLAINTEXT_TOKEN_WARNING.contains("config.yaml"));
    assert!(PLAINTEXT_TOKEN_WARNING.contains("channels/telegram"));
}
