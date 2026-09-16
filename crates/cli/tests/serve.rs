//! The composition root, started for real.
//!
//! These bring the whole stack up on a temporary home and an operating-system
//! port. That is deliberate rather than heavy: every bug this file exists to
//! catch — a shutdown ordering that closes the database under a writer, a ready
//! file written before the bind, a UI root resolved to the wrong thing — is one
//! that only appears when the pieces are actually wired to each other.
//!
//! Nothing here reaches a network: the install has no provider configured,
//! which is a *state* rather than an error, and every route but a turn answers
//! on it.

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

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use darkwire::i18n::Env;
use darkwire::i18n::Translations;
use darkwire::program::{Globals, ServeArgs};
use darkwire::serve::{
    ReadyRecord, ServeOptions, banner, coalesce, read_workspace_file, resolve_ui_root, start,
    write_ready_file,
};
use darkwire_channels::{Channel, ChannelFactory};
use darkwire_core::message_bus::OutboundMessage;
use darkwire_core::{ErrorKind, WireError};
use darkwire_server::UiRoot;
use darkwire_server::scheduler::ReadTaskFile;
use futures::future::BoxFuture;

/// A serve run over a temporary home, on a port the operating system picks.
fn options(home: &Path, args: ServeArgs) -> ServeOptions {
    ServeOptions {
        args: ServeArgs {
            // Loopback unless the case named a host, because the boot policy
            // refuses a wider bind with authentication off — which this install
            // has. The case's own host wins, so a bad one reaches the parser.
            host: args.host.clone().or_else(|| Some("127.0.0.1".to_owned())),
            port: args.port.or(Some(0)),
            ..args
        },
        globals: Globals {
            home: Some(home.display().to_string()),
            ..Globals::default()
        },
        env: Env::empty(),
        channels: Vec::new(),
    }
}

#[tokio::test]
async fn comes_up_on_a_bare_install_and_shuts_down_cleanly() {
    let home = tempfile::tempdir().unwrap();
    let running = start(options(home.path(), ServeArgs::default()))
        .await
        .unwrap();

    // A port of 0 asked the operating system for one, so the only way to learn
    // it is to ask what was bound.
    assert_ne!(running.address.port(), 0);
    assert!(running.url.starts_with("http://127.0.0.1:"));

    let health = reqwest::get(format!("{}/api/health", running.url))
        .await
        .unwrap();
    assert_eq!(health.status(), 200);

    running.close().await;
    // Idempotent, and safe to call from a signal handler.
    running.close().await;
}

#[tokio::test]
async fn mints_a_setup_code_for_an_install_with_no_password() {
    // The whole reason the server starts unclaimed instead of refusing: the
    // code is the only way in, and the terminal printing it is the only place
    // it will ever appear.
    let home = tempfile::tempdir().unwrap();
    let running = start(options(home.path(), ServeArgs::default()))
        .await
        .unwrap();
    assert!(running.setup_code.is_some());
    running.close().await;
}

#[tokio::test]
async fn mints_no_setup_code_when_a_password_was_given() {
    // Minting a code for an install that was just given a password would print
    // a credential nobody needs.
    let home = tempfile::tempdir().unwrap();
    let running = start(options(
        home.path(),
        ServeArgs {
            password: Some("a-long-enough-password".to_owned()),
            ..ServeArgs::default()
        },
    ))
    .await
    .unwrap();
    assert_eq!(running.setup_code, None);
    running.close().await;
}

#[tokio::test]
async fn refuses_a_username_without_a_password() {
    // Rotating a name without a password would leave sessions minted under the
    // old credential alive, and ignoring the flag would leave an operator
    // convinced they had changed something.
    let home = tempfile::tempdir().unwrap();
    let error = start(options(
        home.path(),
        ServeArgs {
            username: Some("operator".to_owned()),
            ..ServeArgs::default()
        },
    ))
    .await
    .unwrap_err();
    assert!(error.message.contains("username"), "{}", error.message);
}

#[tokio::test]
async fn an_unknown_api_path_is_json_rather_than_the_shell() {
    // An unknown API path answered with HTML surfaces as a JSON parse error
    // somewhere entirely unrelated, which is a much longer bug than a 404.
    let home = tempfile::tempdir().unwrap();
    let running = start(options(home.path(), ServeArgs::default()))
        .await
        .unwrap();

    let response = reqwest::get(format!("{}/api/nope", running.url))
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let kind = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(kind.contains("json"), "{kind}");

    running.close().await;
}

#[tokio::test]
async fn refuses_a_host_that_is_not_an_address_before_anything_binds() {
    let home = tempfile::tempdir().unwrap();
    let error = start(options(
        home.path(),
        ServeArgs {
            host: Some("not-an-address".to_owned()),
            ..ServeArgs::default()
        },
    ))
    .await
    .unwrap_err();
    assert!(
        error.message.contains("not-an-address"),
        "{}",
        error.message
    );
}

// The ready file

#[test]
fn the_ready_record_is_written_atomically_and_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ready.json");
    let record = ReadyRecord {
        port: 51_234,
        setup_code: Some("ABCD-EFGH".to_owned()),
        pid: 4242,
    };
    write_ready_file(&path, &record).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    let back: ReadyRecord = serde_json::from_str(&text).unwrap();
    assert_eq!(back, record);

    // The field names a supervisor reads, spelled as the wire spells them.
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["port"], 51_234);
    assert_eq!(value["setupCode"], "ABCD-EFGH");
    assert_eq!(value["pid"], 4242);

    // No temporary left behind: the write is a rename within one directory.
    let strays: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "ready.json")
        .collect();
    assert!(strays.is_empty(), "{strays:?}");
}

#[test]
fn a_first_run_writes_a_null_setup_code_rather_than_omitting_it() {
    // A supervisor reads the field either way; an absent key would make "no
    // code" and "an old binary" the same observation.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ready.json");
    write_ready_file(
        &path,
        &ReadyRecord {
            port: 1,
            setup_code: None,
            pid: 2,
        },
    )
    .unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(value.get("setupCode").is_some());
    assert!(value["setupCode"].is_null());
}

#[tokio::test]
async fn the_ready_file_names_the_port_that_was_actually_bound() {
    let home = tempfile::tempdir().unwrap();
    let ready = home.path().join("ready.json");
    let running = start(options(
        home.path(),
        ServeArgs {
            ready_file: Some(ready.display().to_string()),
            ..ServeArgs::default()
        },
    ))
    .await
    .unwrap();

    // `start` binds; writing the file is the caller's step, so this asserts the
    // record a caller would build rather than reaching for a side effect.
    let record = ReadyRecord {
        port: running.address.port(),
        setup_code: running.setup_code.clone(),
        pid: std::process::id(),
    };
    write_ready_file(&ready, &record).unwrap();
    let back: ReadyRecord =
        serde_json::from_str(&std::fs::read_to_string(&ready).unwrap()).unwrap();
    assert_eq!(back.port, running.address.port());
    assert_ne!(back.port, 0);

    running.close().await;
}

// The UI root

#[test]
fn an_explicit_ui_directory_must_hold_an_index() {
    // Pointing at the wrong directory and getting a silently API-only server is
    // a worse afternoon than an error at startup.
    let dir = tempfile::tempdir().unwrap();
    let error = resolve_ui_root(Some(&dir.path().display().to_string())).unwrap_err();
    assert!(error.message.contains("index.html"), "{}", error.message);
}

#[test]
fn an_explicit_ui_directory_with_an_index_is_served_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<!doctype html>").unwrap();
    let root = resolve_ui_root(Some(&dir.path().display().to_string())).unwrap();
    match root {
        UiRoot::Dir(path) => assert!(path.join("index.html").exists()),
        other => panic!("expected a directory, got {other:?}"),
    }
}

#[test]
fn no_flag_takes_whatever_this_build_carries() {
    // Either answer is correct and which one it is depends on how the crate was
    // compiled, so the assertion is that the two possibilities are the only
    // ones — a headless build serves the API alone and says so.
    let root = resolve_ui_root(None).unwrap();
    assert!(
        matches!(root, UiRoot::Embedded | UiRoot::None),
        "{root:?} is neither the embedded bundle nor headless"
    );
    assert_eq!(
        root == UiRoot::Embedded,
        darkwire_server::ui::has_embedded_bundle()
    );
}

// ------------------------------------------------------------ the banner

/// The banner over a server that is actually up.
async fn banner_for(home: &Path, args: ServeArgs, colors: Option<bool>) -> String {
    let running = start(options(home, args)).await.unwrap();
    let text = banner(&running, colors, &Translations::default());
    running.close().await;
    text
}

#[tokio::test]
async fn the_banner_names_the_url_the_workspace_and_where_the_ui_came_from() {
    // The five things an operator needs in the second after it starts. Never
    // coloured in a test: an assertion against escape sequences is an assertion
    // about a palette rather than about what was said.
    let home = tempfile::tempdir().unwrap();
    let text = banner_for(home.path(), ServeArgs::default(), Some(false)).await;

    assert!(text.contains("http://127.0.0.1:"), "{text}");
    assert!(text.contains(&home.path().display().to_string()), "{text}");
    // A bare install has nothing configured, which is a state rather than an
    // error — and the banner is where an operator finds that out.
    let t = Translations::default();
    assert!(
        text.contains(&t.t(darkwire_i18n::keys::serve::AGENT_UNCONFIGURED)),
        "{text}"
    );
    assert!(
        text.contains(&t.t(darkwire_i18n::keys::serve::PRESS_CTRL_C)),
        "{text}"
    );
}

#[tokio::test]
async fn the_banner_prints_the_setup_code_and_only_on_a_first_run() {
    // The whole reason the server starts unclaimed instead of refusing: the
    // terminal printing the code is the only place it will ever appear.
    let home = tempfile::tempdir().unwrap();
    let running = start(options(home.path(), ServeArgs::default()))
        .await
        .unwrap();
    let code = running.setup_code.clone().unwrap();
    let text = banner(&running, Some(false), &Translations::default());
    running.close().await;
    assert!(text.contains(&code), "{text}");

    // A second home, given a password: minting a code for an install that was
    // just given one would print a credential nobody needs.
    let claimed = tempfile::tempdir().unwrap();
    let text = banner_for(
        claimed.path(),
        ServeArgs {
            password: Some("correct horse battery staple".to_owned()),
            ..ServeArgs::default()
        },
        Some(false),
    )
    .await;
    assert!(
        !text.contains(&Translations::default().t(darkwire_i18n::keys::serve::FIRST_RUN)),
        "{text}"
    );
}

#[tokio::test]
async fn the_banner_colours_what_it_prints_when_it_is_asked_to() {
    let home = tempfile::tempdir().unwrap();
    let plain = banner_for(home.path(), ServeArgs::default(), Some(false)).await;
    let second = tempfile::tempdir().unwrap();
    let coloured = banner_for(second.path(), ServeArgs::default(), Some(true)).await;

    assert!(!plain.contains('\u{1b}'), "{plain}");
    assert!(coloured.contains('\u{1b}'), "{coloured}");
}

// ---------------------------------------------------------- the channels

/// A channel that does nothing but count how often it was built and started.
struct Counted {
    id: String,
    started: Arc<AtomicUsize>,
    refuses: bool,
}

impl Channel for Counted {
    fn id(&self) -> &str {
        &self.id
    }

    fn start(&self) -> BoxFuture<'_, darkwire_core::Result<()>> {
        Box::pin(async move {
            if self.refuses {
                return Err(WireError::new(ErrorKind::Config, "the token was refused"));
            }
            self.started.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn send(&self, _message: OutboundMessage) -> BoxFuture<'_, darkwire_core::Result<()>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// A factory under `id`, and the counter its channels increment on start.
fn counting_factory(id: &str, refuses: bool) -> (ChannelFactory, Arc<AtomicUsize>) {
    let started = Arc::new(AtomicUsize::new(0));
    let for_build = Arc::clone(&started);
    let owned = id.to_owned();
    let factory = ChannelFactory::new(
        id,
        Arc::new(move |_context| {
            Ok(Arc::new(Counted {
                id: owned.clone(),
                started: Arc::clone(&for_build),
                refuses,
            }) as Arc<dyn Channel>)
        }),
    );
    (factory, started)
}

#[tokio::test]
async fn an_injected_channel_starts_with_the_server_and_is_named_in_the_banner() {
    let home = tempfile::tempdir().unwrap();
    let (factory, started) = counting_factory("loopback", false);
    let running = start(ServeOptions {
        channels: vec![factory],
        ..options(home.path(), ServeArgs::default())
    })
    .await
    .unwrap();

    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(running.channel_ids(), vec!["loopback".to_owned()]);
    let text = banner(&running, Some(false), &Translations::default());
    assert!(text.contains("loopback"), "{text}");

    running.close().await;
    // Stopped with the server, and the manager taken: a second close must not
    // stop a channel twice.
    assert!(running.channel_ids().is_empty());
}

#[tokio::test]
async fn a_channel_that_will_not_start_fails_the_boot() {
    // Deliberately fatal at boot: a bad token should stop the process rather
    // than leave a channel silently dead.
    let home = tempfile::tempdir().unwrap();
    let (factory, _started) = counting_factory("loopback", true);
    let error = start(ServeOptions {
        channels: vec![factory],
        ..options(home.path(), ServeArgs::default())
    })
    .await
    .unwrap_err();

    assert!(error.message.contains("refused"), "{}", error.message);
}

#[tokio::test]
async fn a_server_with_a_channel_up_still_answers_the_api() {
    // Composition order is load-bearing, and the channels go last precisely so
    // that a channel which blocks on its transport cannot stop the listener
    // from answering. A channel that is up must not change what the API does.
    let home = tempfile::tempdir().unwrap();
    let (factory, started) = counting_factory("loopback", false);
    let running = start(ServeOptions {
        channels: vec![factory],
        ..options(home.path(), ServeArgs::default())
    })
    .await
    .unwrap();
    assert_eq!(started.load(Ordering::SeqCst), 1);

    let response = reqwest::Client::new()
        .get(format!("{}/api/health", running.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    running.close().await;
}

// ------------------------------------------------- the heartbeat's file

#[tokio::test]
async fn a_task_file_is_read_through_the_jail_and_capped() {
    let home = tempfile::tempdir().unwrap();
    let running = start(options(home.path(), ServeArgs::default()))
        .await
        .unwrap();
    let root = running.runtime.jail().root().to_path_buf();
    std::fs::write(root.join("TASK.md"), "x".repeat(100)).unwrap();

    let read = read_workspace_file(
        &running.runtime,
        &ReadTaskFile {
            workspace_id: "default".to_owned(),
            path: "TASK.md".to_owned(),
            max_bytes: 10,
        },
    )
    .await
    .unwrap();
    // Capped rather than read whole, because this runs every interval forever
    // and a large file would be paid for on each one.
    assert_eq!(read, "x".repeat(10));

    running.close().await;
}

#[tokio::test]
async fn a_task_file_outside_the_workspace_never_reaches_the_model() {
    // The jail is what stops `../../.ssh/id_rsa` reaching a heartbeat model
    // that would then read it aloud. It clamps rather than rejecting, so the
    // traversal lands back inside the root — which is why the assertion is
    // about *which* file was read rather than about an error kind.
    let home = tempfile::tempdir().unwrap();
    let running = start(options(home.path(), ServeArgs::default()))
        .await
        .unwrap();
    let root = running.runtime.jail().root().to_path_buf();
    std::fs::create_dir_all(root.join(".ssh")).unwrap();
    std::fs::write(root.join(".ssh/id_rsa"), "inside the workspace").unwrap();

    let read = read_workspace_file(
        &running.runtime,
        &ReadTaskFile {
            workspace_id: "default".to_owned(),
            path: "../../.ssh/id_rsa".to_owned(),
            max_bytes: 4096,
        },
    )
    .await
    .unwrap();
    assert_eq!(read, "inside the workspace");

    // A path the jail refuses outright is a refusal rather than a read.
    let error = read_workspace_file(
        &running.runtime,
        &ReadTaskFile {
            workspace_id: "default".to_owned(),
            path: "TASK\0.md".to_owned(),
            max_bytes: 4096,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::JailEscape);
    assert_eq!(
        error
            .details
            .get("path")
            .and_then(serde_json::Value::as_str),
        Some("TASK\0.md")
    );

    running.close().await;
}

#[tokio::test]
async fn a_missing_task_file_is_nothing_to_do_rather_than_a_fault() {
    // An install with no `TASK.md` is the normal case, not a broken one, and
    // the scheduler reads `not_found` as "nothing to do".
    let home = tempfile::tempdir().unwrap();
    let running = start(options(home.path(), ServeArgs::default()))
        .await
        .unwrap();

    let error = read_workspace_file(
        &running.runtime,
        &ReadTaskFile {
            workspace_id: "default".to_owned(),
            path: "TASK.md".to_owned(),
            max_bytes: 4096,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);

    running.close().await;
}

// ----------------------------------------------------- the tools producer

#[tokio::test]
async fn many_registry_mutations_become_one_frame() {
    // A mutation is one tool: an MCP server registering forty is forty
    // notifications, and a settings save unregisters every built-in and
    // registers them again before it is done. A frame per mutation would be a
    // `tools.changed` storm on every save.
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let notify = coalesce(Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    }));

    for _ in 0..40 {
        notify();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing fires inline");

    // The batch closes on the next tick, before anything can observe the
    // intermediate state.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // And the next storm is its own batch rather than being swallowed.
    notify();
    notify();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_coalescer_off_a_runtime_runs_through_rather_than_dropping_the_call() {
    // Off a runtime there is nothing to yield to, and a caller that is not
    // inside the server should still be told.
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let notify = coalesce(Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    }));
    notify();
    notify();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
