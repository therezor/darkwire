//! `darkwire init`, driven through its own streams.
//!
//! Two properties decide whether this landed, and neither is about the prompts:
//!
//!  - **What it writes is a config the rest of the system reads.** The file goes
//!    through `save_config`, so it validates against the same schema every other
//!    reader uses. A wizard that produced a file only it could read would be a
//!    second config format.
//!  - **It writes nothing until every question is answered.** An operator who
//!    walks away at the model prompt has a clean install, not a half-configured
//!    provider to clean up.

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

use darkwire::Streams;
use darkwire::ask::ScriptedReader;
use darkwire::i18n::Env;
use darkwire::init::{
    EndpointModels, InitOptions, ModelLister, RecordedCredentials, VaultCredentials, init,
};
use darkwire::program::Globals;
use darkwire_core::paths::ResolveWirePaths;
use darkwire_core::{WirePaths, parse_config};
use darkwire_providers::testkit::{ScriptedResponse, ScriptedServer, models_body};
use darkwire_providers::{BoxFuture, ProviderSpec, find_builtin};

/// A built-in spec, cloned so a case can bend one field of it.
fn spec_of(id: &str) -> ProviderSpec {
    find_builtin(id)
        .unwrap_or_else(|| panic!("no built-in provider {id}"))
        .clone()
}

/// A sink that keeps what was written to it, so a test can read it back.
#[derive(Clone, Default)]
struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl Sink {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A lister that answers with a fixed catalogue, so nothing opens a socket.
struct Offering(Vec<String>);

impl ModelLister for Offering {
    fn list<'a>(
        &'a self,
        _spec: &'a ProviderSpec,
        _api_base: &'a str,
        _api_key: Option<&'a str>,
    ) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async move { self.0.clone() })
    }
}

/// What one run produced.
struct Run {
    code: u8,
    output: String,
    errors: String,
    credentials: Vec<(String, String)>,
}

/// Runs the wizard over a temporary home, answering with `answers`.
async fn run_in(home: &Path, answers: &[&str], offered: &[&str]) -> Run {
    run_with(home, answers, offered, true).await
}

async fn run_with(home: &Path, answers: &[&str], offered: &[&str], interactive: bool) -> Run {
    let out = Sink::default();
    let err = Sink::default();
    let mut streams = Streams {
        out: Box::new(out.clone()),
        err: Box::new(err.clone()),
    };
    let mut reader = ScriptedReader::new(answers.iter().copied());
    let mut credentials = RecordedCredentials::new();
    let models = Offering(offered.iter().map(|id| (*id).to_owned()).collect());
    let env = Env::empty();

    let code = init(
        InitOptions {
            home: Some(home.to_string_lossy().into_owned()),
            env: &env,
            colors: Some(false),
            interactive,
            reader: &mut reader,
            models: &models,
            credentials: &mut credentials,
        },
        &mut streams,
    )
    .await
    .expect("the wizard answers with an exit code rather than failing");

    Run {
        code,
        output: out.text(),
        errors: err.text(),
        credentials: credentials.written(),
    }
}

/// The config the wizard wrote, as JSON.
fn config_in(home: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(home.join("config.yaml")).expect("a config was written");
    serde_yaml_ng::from_str(&text).expect("the config is YAML")
}

/// The two models every case is offered unless it says otherwise.
const OFFERED: [&str; 2] = ["qwen3:8b", "llama3"];

#[tokio::test]
async fn writes_a_config_the_schema_accepts_naming_the_instance_it_created() {
    let home = tempfile::tempdir().unwrap();
    let run = run_in(
        home.path(),
        &[
            "",       // workspace: the default
            "ollama", // provider, by name rather than by number
            "Laptop", // label
            "",       // API base: the provider's default
            "",       // no token
            "1",      // the first model offered
        ],
        &OFFERED,
    )
    .await;

    assert_eq!(run.code, 0, "{}", run.errors);
    let written = config_in(home.path());
    let text = std::fs::read_to_string(home.path().join("config.yaml")).unwrap();
    parse_config(&text, home.path()).expect("the schema accepts what the wizard wrote");

    assert_eq!(written["agents"]["list"]["default"]["provider"], "ollama");
    assert_eq!(written["agents"]["list"]["default"]["model"], "qwen3:8b");
    assert_eq!(written["providers"]["ollama"]["type"], "ollama");
    assert_eq!(written["providers"]["ollama"]["label"], "Laptop");
}

#[tokio::test]
async fn stores_a_token_for_a_local_endpoint() {
    // A LAN model server behind an authenticating proxy is a real
    // configuration, so the token question is asked for local providers too.
    let home = tempfile::tempdir().unwrap();
    let run = run_in(
        home.path(),
        &["", "ollama", "", "", "proxy-token", "1"],
        &OFFERED,
    )
    .await;

    assert_eq!(
        run.credentials,
        vec![("ollama".to_owned(), "proxy-token".to_owned())]
    );
}

#[tokio::test]
async fn writes_no_credential_when_the_token_is_left_blank() {
    let home = tempfile::tempdir().unwrap();
    let run = run_in(home.path(), &["", "ollama", "", "", "", "1"], &OFFERED).await;

    assert!(run.credentials.is_empty(), "{:?}", run.credentials);
}

#[tokio::test]
async fn leaves_the_label_empty_when_the_suggestion_was_accepted() {
    // An unchanged suggestion is not a label: keeping it empty is what lets the
    // type's own display name keep improving.
    let home = tempfile::tempdir().unwrap();
    run_in(home.path(), &["", "ollama", "", "", "", "1"], &OFFERED).await;

    assert_eq!(config_in(home.path())["providers"]["ollama"]["label"], "");
}

#[tokio::test]
async fn names_a_second_endpoint_of_the_same_type_rather_than_overwriting_the_first() {
    let home = tempfile::tempdir().unwrap();
    run_in(
        home.path(),
        &["", "ollama", "Laptop", "", "", "1"],
        &OFFERED,
    )
    .await;
    run_in(
        home.path(),
        &["", "ollama", "GPU box", "http://gpu.lan:11434/v1", "", "2"],
        &OFFERED,
    )
    .await;

    let written = config_in(home.path());
    let providers = written["providers"].as_object().expect("a provider map");
    assert_eq!(
        providers.keys().collect::<Vec<_>>(),
        vec!["ollama", "ollama-2"]
    );
    assert_eq!(providers["ollama-2"]["label"], "GPU box");
    assert_eq!(providers["ollama-2"]["apiBase"], "http://gpu.lan:11434/v1");
}

#[tokio::test]
async fn falls_back_to_typing_a_model_when_the_endpoint_cannot_be_listed() {
    // An unreachable Ollama usually means it is not running, which is worth
    // reading rather than working around — but it must not end the wizard.
    let home = tempfile::tempdir().unwrap();
    let run = run_in(
        home.path(),
        &["", "ollama", "", "", "", "typed-by-hand"],
        &[],
    )
    .await;

    assert_eq!(run.code, 0, "{}", run.errors);
    assert!(
        run.output.contains("Could not list models"),
        "{}",
        run.output
    );
    assert_eq!(
        config_in(home.path())["agents"]["list"]["default"]["model"],
        "typed-by-hand"
    );
}

#[tokio::test]
async fn refuses_a_pipe_rather_than_reading_end_of_input_as_an_answer() {
    let home = tempfile::tempdir().unwrap();
    let run = run_with(home.path(), &[], &OFFERED, false).await;

    assert_eq!(run.code, 1);
    assert!(run.errors.contains("needs a terminal"), "{}", run.errors);
    assert!(!home.path().join("config.yaml").exists());
}

#[tokio::test]
async fn keeps_asking_until_the_provider_answer_is_one_of_the_offered_ones() {
    let home = tempfile::tempdir().unwrap();
    let run = run_in(
        home.path(),
        &["", "not-a-provider", "99", "ollama", "", "", "", "1"],
        &OFFERED,
    )
    .await;

    assert_eq!(run.code, 0, "{}", run.errors);
    assert!(
        run.output.contains("Enter a number between"),
        "{}",
        run.output
    );
    assert_eq!(
        config_in(home.path())["providers"]["ollama"]["type"],
        "ollama"
    );
}

#[tokio::test]
async fn writes_nothing_when_the_answers_run_out() {
    // The whole reason the write happens last: somebody who walks away at the
    // model prompt has a clean install rather than a half-configured provider.
    let home = tempfile::tempdir().unwrap();
    let run = run_in(home.path(), &["", "ollama"], &OFFERED).await;

    assert_eq!(run.code, 1);
    assert!(run.output.contains("Nothing was written"), "{}", run.output);
    assert!(!home.path().join("config.yaml").exists());
}

// ------------------------------------------------ the real endpoint lister

#[tokio::test]
async fn the_real_lister_offers_what_the_endpoint_published() {
    // The wizard's model question is a numbered list wherever the endpoint
    // answers with one, and this is the half that dials.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(
        200,
        &models_body(&["qwen3:8b", "llama3"]),
    ));

    let offered = EndpointModels
        .list(&spec_of("ollama"), &server.base_url(), None)
        .await;

    assert_eq!(offered, vec!["qwen3:8b".to_owned(), "llama3".to_owned()]);
}

#[tokio::test]
async fn the_real_lister_never_dials_an_endpoint_that_publishes_no_list() {
    // Asking would be a request with no answer to wait for; the question falls
    // back to typing an id.
    let server = ScriptedServer::start().await;
    let mut spec = spec_of("ollama");
    spec.supports_model_listing = false;

    let offered = EndpointModels.list(&spec, &server.base_url(), None).await;

    assert!(offered.is_empty());
    assert!(
        server.calls().is_empty(),
        "nothing should have been dialled"
    );
}

#[tokio::test]
async fn an_endpoint_that_cannot_be_reached_is_an_empty_list_rather_than_a_failure() {
    // An unreachable Ollama usually means it is not running yet. The wizard
    // says so and lets the model be typed rather than starting over.
    let offered = EndpointModels
        .list(&spec_of("ollama"), "http://127.0.0.1:1", None)
        .await;

    assert!(offered.is_empty());
}

#[tokio::test]
async fn the_real_lister_sends_the_key_it_was_given() {
    // A LAN model server behind an authenticating proxy is a real
    // configuration, and the wizard asks for a token before it lists.
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(200, &models_body(&["qwen3:8b"])));

    let offered = EndpointModels
        .list(&spec_of("ollama"), &server.base_url(), Some("proxy-token"))
        .await;

    assert_eq!(offered, vec!["qwen3:8b".to_owned()]);
    let seen = server.calls();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].headers.get("authorization").map(String::as_str),
        Some("Bearer proxy-token")
    );
}

#[tokio::test]
async fn a_refusal_is_an_empty_list_rather_than_an_error_the_wizard_raises() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::json(
        401,
        &serde_json::json!({"error": {"message": "invalid api key"}}),
    ));

    let offered = EndpointModels
        .list(&spec_of("ollama"), &server.base_url(), Some("wrong"))
        .await;

    assert!(offered.is_empty());
}

// ----------------------------------------------------- the process wiring

#[tokio::test]
async fn the_process_entry_point_refuses_a_run_with_no_terminal_behind_it() {
    // Nothing under a test runner has a tty, which is exactly the case the
    // guard exists for: the wizard reaches the real stdin and the real vault
    // only after it has decided somebody is there to answer, so this reaches
    // neither.
    let home = tempfile::tempdir().unwrap();
    let out = Sink::default();
    let err = Sink::default();
    let mut streams = Streams {
        out: Box::new(out.clone()),
        err: Box::new(err.clone()),
    };
    let globals = Globals {
        home: Some(home.path().to_string_lossy().into_owned()),
        color: Some(false),
        ..Globals::default()
    };

    let code = darkwire::init::run(&globals, &Env::empty(), &mut streams)
        .await
        .expect("the wizard answers with an exit code rather than failing");

    assert_eq!(code, 1);
    assert!(err.text().contains("needs a terminal"), "{}", err.text());
    assert!(!home.path().join("config.yaml").exists());
}

#[test]
fn the_vault_sink_opens_nothing_until_something_is_written() {
    // Constructing it must not mint a keychain entry: `darkwire init` builds one
    // before it knows whether the endpoint even needs a key.
    let home = tempfile::tempdir().unwrap();
    let paths = WirePaths::resolve(ResolveWirePaths {
        root: Some(home.path().to_string_lossy().into_owned()),
        workspaces: None,
        env: Some(std::collections::HashMap::new()),
        home: Some(home.path().to_path_buf()),
    })
    .expect("the paths resolve under a temporary home");

    let sink = VaultCredentials::new(paths);

    assert!(format!("{sink:?}").contains("VaultCredentials"));
    assert!(!home.path().join("vault.json").exists());
}
