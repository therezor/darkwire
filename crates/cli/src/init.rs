//! `darkwire init` — the terminal half of first-run setup.
//!
//! The browser gets a wizard behind a one-time code; this is the same handful
//! of questions for someone who never intends to open one. It writes exactly
//! two things — `config.yaml`, and a credential through the vault — and reads
//! the answers back through the same schema everything else validates against,
//! so an install configured here is indistinguishable from one configured in
//! the UI.
//!
//! Three decisions worth stating:
//!
//!  - **No prompt library.** The helpers are in [`crate::ask`], which is where
//!    they moved when the setup flow needed the same four, and they
//!    take their streams as arguments so a test drives them without a terminal.
//!
//!  - **Nothing is written until every question is answered.** An operator who
//!    walks away at the model prompt should not find a half-configured provider
//!    they now have to clean up. The answers are collected, then the config is
//!    merged and written once. End of input — Ctrl-D, or a script that ran out
//!    of answers — unwinds through the same path and writes nothing.
//!
//!  - **The model list is fetched, not guessed.** Every OpenAI-compatible
//!    endpoint answers `GET /models`, which covers every local server, so the
//!    question is a numbered list rather than a text box wherever it can be. A
//!    provider that cannot be reached says so and falls back to typing — an
//!    unreachable Ollama at this point usually means it is not running, and that
//!    is worth reading rather than working around.
//!
//! The model listing arrives as an injected [`ModelLister`] rather than through
//! the shared catalogue in [`crate::models`]. The catalogue answers "what can
//! this *install* reach", which is a question about configuration that does not
//! exist yet: the wizard is asking about an endpoint the operator has just
//! typed and has not saved.

use std::io::Write;
use std::sync::{Arc, Mutex};

use darkwire_core::{
    ErrorKind, LoadConfigOptions, LoadedConfig, Result, WireError, ensure_dir, load_config,
    save_config,
};
use darkwire_i18n::{DEFAULT_LOCALE, Locale, SUPPORTED_LOCALES, keys};
use darkwire_protocol::DEFAULT_AGENT_ID;
use darkwire_protocol::config::{Config, ProviderConfig};
use darkwire_providers::{
    BoxFuture, CreateProviderOptions, PROVIDERS, ProviderSpec, Resilience, create_provider,
    next_instance_id,
};
use darkwire_runtime::{PROVIDER_CREDENTIAL_NAMESPACE, open_vault};
use darkwire_security::CredentialVault;
use darkwire_tui::{Palette, TerminalInput, palette_for};
use tokio_util::sync::CancellationToken;

use crate::Streams;
use crate::ask::{Ask, LineReader, StdinReader};
use crate::i18n::{Env, Translations};
use crate::program::Globals;
use crate::runtime::load_options;

/// How long the model list gets before the question falls back to typing.
///
/// Short on purpose: the wizard is only as fast as its slowest question, and an
/// endpoint that has not started yet must not make setup look hung.
pub const MODEL_FETCH_TIMEOUT_MS: u64 = 5000;

/// One endpoint, asked what models it has.
///
/// A trait rather than a function so a test can answer without a socket, and so
/// the wizard's own implementation is the only thing in the file that dials
/// out.
pub trait ModelLister: Send + Sync {
    /// The model ids this endpoint offers. Empty is a normal answer.
    fn list<'a>(
        &'a self,
        spec: &'a ProviderSpec,
        api_base: &'a str,
        api_key: Option<&'a str>,
    ) -> BoxFuture<'a, Vec<String>>;
}

/// The real one: one bare adapter, asked once, with a deadline.
#[derive(Debug, Default)]
pub struct EndpointModels;

impl ModelLister for EndpointModels {
    fn list<'a>(
        &'a self,
        spec: &'a ProviderSpec,
        api_base: &'a str,
        api_key: Option<&'a str>,
    ) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async move {
            if !spec.supports_model_listing {
                return Vec::new();
            }
            let mut options = CreateProviderOptions::new(spec.clone());
            options.api_base = Some(api_base.to_owned());
            options.api_key = api_key.map(str::to_owned);
            // Retries are for a turn. A catalogue that does not answer promptly
            // should say so rather than spend fifteen seconds insisting.
            options.resilience = Resilience::Disabled;

            let Ok(provider) = create_provider(options) else {
                return Vec::new();
            };
            let token = CancellationToken::new();
            let deadline = token.clone();
            // The timeout cancels the token the request is watching, so the
            // socket is released rather than left to a dropped future.
            let timer = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(MODEL_FETCH_TIMEOUT_MS)).await;
                deadline.cancel();
            });
            let listed = provider.list_models(&token).await;
            timer.abort();
            provider.close().await;
            // A provider that cannot be reached is a normal answer here — it
            // usually means the server is not running yet — and the question
            // falls back to typing a model id rather than ending the wizard.
            listed
                .map(|models| models.into_iter().map(|model| model.id).collect())
                .unwrap_or_default()
        })
    }
}

/// Where a credential goes once the last question is answered.
///
/// Injected for the reason the model listing is: the real implementation mints
/// a keychain entry the first time it runs, and a test has no keychain.
pub trait CredentialSink {
    /// Stores one provider instance's key.
    fn save(&mut self, instance_id: &str, value: &str) -> Result<()>;
}

/// The real one: the install's own vault, opened on first write.
#[derive(Debug)]
pub struct VaultCredentials {
    paths: darkwire_core::WirePaths,
}

impl VaultCredentials {
    /// A sink over the vault at these paths.
    #[must_use]
    pub fn new(paths: darkwire_core::WirePaths) -> VaultCredentials {
        VaultCredentials { paths }
    }
}

impl CredentialSink for VaultCredentials {
    fn save(&mut self, instance_id: &str, value: &str) -> Result<()> {
        let mut vault: CredentialVault = open_vault(&self.paths)?;
        vault.set(PROVIDER_CREDENTIAL_NAMESPACE, instance_id, value)
    }
}

/// A sink that keeps what it was given, which is what the tests read back.
#[derive(Debug, Clone, Default)]
pub struct RecordedCredentials {
    written: Arc<Mutex<Vec<(String, String)>>>,
}

impl RecordedCredentials {
    /// A sink with nothing in it yet.
    #[must_use]
    pub fn new() -> RecordedCredentials {
        RecordedCredentials::default()
    }

    /// Every `(instance id, value)` pair written so far.
    #[must_use]
    pub fn written(&self) -> Vec<(String, String)> {
        match self.written.lock() {
            Ok(written) => written.clone(),
            // A poisoned lock means a panic happened mid-write. There is
            // nothing to recover in a test double, and an empty list reads as a
            // failed assertion rather than as a second panic.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl CredentialSink for RecordedCredentials {
    fn save(&mut self, instance_id: &str, value: &str) -> Result<()> {
        match self.written.lock() {
            Ok(mut written) => written.push((instance_id.to_owned(), value.to_owned())),
            Err(poisoned) => poisoned
                .into_inner()
                .push((instance_id.to_owned(), value.to_owned())),
        }
        Ok(())
    }
}

/// Everything the wizard is injected with.
pub struct InitOptions<'a> {
    /// `--home`, which beats `$DARKWIRE_HOME`.
    pub home: Option<String>,
    /// The environment to read.
    pub env: &'a Env,
    /// The three-state colour answer: `None` is "nobody said".
    pub colors: Option<bool>,
    /// Whether there is somebody there to answer.
    ///
    /// A pipe cannot answer a question, and a wizard that read end of input as
    /// an answer would write a config nobody chose.
    pub interactive: bool,
    /// Where typed lines come from.
    pub reader: &'a mut dyn LineReader,
    /// What the model question offers.
    pub models: &'a dyn ModelLister,
    /// Where the API key goes.
    pub credentials: &'a mut dyn CredentialSink,
}

/// Runs the wizard against the process's own terminal.
///
/// The seam between this and [`init`] is the whole of what makes the wizard
/// testable: everything that touches the machine is decided here, and the
/// questions themselves reach only what they were handed.
pub async fn run(globals: &Globals, env: &Env, streams: &mut Streams) -> Result<u8> {
    let loaded = load_config(LoadConfigOptions {
        paths: load_options(globals, None, env),
        file: None,
    })?;
    let mut reader = StdinReader::new();
    let mut credentials = VaultCredentials::new(loaded.paths.clone());
    let models = EndpointModels;

    init(
        InitOptions {
            home: globals.home.clone(),
            env,
            colors: globals.color,
            interactive: darkwire_tui::StandardInput.is_tty(),
            reader: &mut reader,
            models: &models,
            credentials: &mut credentials,
        },
        streams,
    )
    .await
}

/// Runs the wizard and returns the exit code.
///
/// Returns rather than ending the process, like every other subcommand: the
/// config write has to land before anything exits.
pub async fn init(options: InitOptions<'_>, streams: &mut Streams) -> Result<u8> {
    let InitOptions {
        home,
        env,
        colors,
        interactive,
        reader,
        models,
        credentials,
    } = options;

    let t = Translations::for_env(env, None);
    let palette = palette_for(colors);
    let loaded = load_config(LoadConfigOptions {
        paths: load_options(
            &Globals {
                home: home.clone(),
                ..Globals::default()
            },
            None,
            env,
        ),
        file: None,
    })?;

    if !interactive {
        write!(
            streams.err,
            "✖ `darkwire init` needs a terminal.\n  Edit {} directly, or run `darkwire serve` and \
             use the browser wizard.\n",
            loaded.file.display()
        )?;
        return Ok(1);
    }

    let mut ask = Ask::new(reader, colors, &t);

    writeln!(
        streams.out,
        "{}\n",
        palette.bold.apply(&t.t(keys::init::HEADING))
    )?;
    writeln!(
        streams.out,
        "  {}  {}",
        palette.dim.apply(&t.t(keys::init::CONFIG)),
        loaded.file.display()
    )?;
    writeln!(
        streams.out,
        "  {}    {}\n",
        palette.dim.apply("Home"),
        loaded.paths.root.display()
    )?;

    let answers = match collect(&mut ask, streams, &loaded, models, &palette, &t).await {
        Ok(answers) => answers,
        Err(error) if error.is_aborted() => {
            // Ctrl-C and Ctrl-D both arrive here, and neither is a failure
            // worth a page of detail: nothing has been written, which is the
            // whole reason the write happens last.
            writeln!(streams.out, "\nStopped. Nothing was written.")?;
            return Ok(1);
        }
        Err(error) => return Err(error),
    };

    write(&answers, &loaded, credentials)?;

    writeln!(
        streams.out,
        "\n{}\n",
        palette.bold.apply(&t.t(keys::init::DONE))
    )?;
    writeln!(
        streams.out,
        "  {}   {}",
        palette.dim.apply(&t.t(keys::init::PROVIDER)),
        answers.instance_id
    )?;
    writeln!(
        streams.out,
        "  {}      {}",
        palette.dim.apply(&t.t(keys::init::MODEL)),
        answers.model
    )?;
    writeln!(
        streams.out,
        "  {}  {}\n",
        palette.dim.apply(&t.t(keys::init::WORKSPACE)),
        answers.workspace
    )?;
    writeln!(
        streams.out,
        "\nRun {} to talk to it, or {} for the UI.",
        palette.cyan.apply("darkwire chat"),
        palette.cyan.apply("darkwire serve")
    )?;
    Ok(0)
}

/// What the wizard collected, before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answers {
    /// A BCP-47 tag, written to `ui.locale` — the same field the web UI reads.
    pub locale: String,
    /// The workspace root.
    pub workspace: String,
    /// The id the new provider instance takes.
    pub instance_id: String,
    /// The instance itself.
    pub instance: ProviderConfig,
    /// The model the default agent runs.
    pub model: String,
    /// The API key, empty when the endpoint needs none.
    pub api_key: String,
}

/// A language named in its own language, so the person who needs it can read it.
///
/// The tag itself, because this build carries no display-name database and a
/// wrong endonym is worse than a tag: the question is only asked when there is
/// more than one language to ask about, and a build that adds one can add the
/// names beside it.
fn name_of_locale(locale: Locale) -> String {
    locale.as_str().to_owned()
}

/// Which language the questions after this one are asked in.
///
/// First, and for the same reason the browser wizard asks first: everything
/// after it is prose. Only offered when there is a choice — a build shipping one
/// language would be asking which of one.
fn ask_locale(
    ask: &mut Ask<'_>,
    streams: &mut Streams,
    loaded: &LoadedConfig,
    t: &Translations,
) -> Result<String> {
    if SUPPORTED_LOCALES.len() <= 1 {
        return Ok(loaded.config.ui.locale.clone());
    }
    let names: Vec<String> = SUPPORTED_LOCALES
        .iter()
        .map(|tag| name_of_locale(*tag))
        .collect();
    let current = SUPPORTED_LOCALES
        .iter()
        .position(|tag| tag.as_str() == loaded.config.ui.locale)
        .unwrap_or(0);
    let index = ask.choose(
        &mut streams.out,
        &t.t(keys::init::LANGUAGE),
        &names,
        current,
    )?;
    Ok(SUPPORTED_LOCALES
        .get(index)
        .copied()
        .unwrap_or(DEFAULT_LOCALE)
        .as_str()
        .to_owned())
}

/// Which endpoint this install talks to.
///
/// Ollama is the suggestion because it is the one an operator is most likely to
/// already have running, and a local endpoint is the only kind that works with
/// no credential at all.
fn ask_provider<'s>(
    ask: &mut Ask<'_>,
    streams: &mut Streams,
    palette: &Palette,
    t: &Translations,
) -> Result<&'s ProviderSpec> {
    writeln!(streams.out, "\n{}", t.t(keys::init::WHICH_PROVIDER))?;
    let specs: &'s [ProviderSpec] = &PROVIDERS;
    let labels: Vec<String> = specs
        .iter()
        .map(|spec| {
            if spec.is_local {
                format!(
                    "{}{}",
                    spec.display_name,
                    palette.dim.apply(&t.t(keys::init::LOCAL))
                )
            } else {
                spec.display_name.clone()
            }
        })
        .collect();
    let chosen = ask.choose(
        &mut streams.out,
        &t.t(keys::init::PROVIDER),
        &labels,
        specs
            .iter()
            .position(|spec| spec.id == "ollama")
            .unwrap_or(0),
    )?;
    specs
        .get(chosen)
        .ok_or_else(|| WireError::new(ErrorKind::Config, "No provider chosen"))
}

/// Which model the default agent runs.
///
/// A numbered list wherever the endpoint published one, and a text box where it
/// did not. An endpoint that cannot be reached says so and falls back to typing
/// rather than ending the wizard: an unreachable Ollama usually means it is not
/// running, which is worth reading and not worth starting over for.
fn ask_model(
    ask: &mut Ask<'_>,
    streams: &mut Streams,
    offered: &[String],
    palette: &Palette,
    t: &Translations,
) -> Result<String> {
    if offered.is_empty() {
        write!(
            streams.out,
            "{}",
            palette.dim.apply(&t.t(keys::init::LIST_FAILED))
        )?;
        return ask.text(&mut streams.out, &t.t(keys::init::MODEL), None);
    }
    writeln!(streams.out, "{} models available.", offered.len())?;
    let index = ask.choose(&mut streams.out, &t.t(keys::init::MODEL), offered, 0)?;
    Ok(offered.get(index).cloned().unwrap_or_default())
}

/// Every question, in order. Writes nothing.
async fn collect(
    ask: &mut Ask<'_>,
    streams: &mut Streams,
    loaded: &LoadedConfig,
    models: &dyn ModelLister,
    palette: &Palette,
    t: &Translations,
) -> Result<Answers> {
    let locale = ask_locale(ask, streams, loaded, t)?;

    let workspace_default = loaded.paths.workspace.to_string_lossy().into_owned();
    let workspace = ask.text(
        &mut streams.out,
        &t.t(keys::init::WORKSPACE_DIR),
        Some(&workspace_default),
    )?;

    let spec = ask_provider(ask, streams, palette, t)?;

    let label = ask.text(
        &mut streams.out,
        &t.t(keys::init::ENDPOINT_NAME),
        Some(&spec.display_name),
    )?;
    let api_base = ask.text(
        &mut streams.out,
        &t.t(keys::init::API_BASE),
        spec.default_api_base.as_deref(),
    )?;

    // Offered for local providers too: a LAN model server behind an
    // authenticating proxy is a real configuration, and the credential lookup
    // reads the vault for one now.
    let api_key = ask.secret(
        &mut streams.out,
        &t.t(if spec.is_local {
            keys::init::API_TOKEN
        } else {
            keys::init::API_KEY
        }),
    )?;

    let instance_id =
        next_instance_id(&spec.id, loaded.config.providers.keys().map(String::as_str));

    writeln!(streams.out)?;
    let offered = models
        .list(
            spec,
            &api_base,
            if api_key.is_empty() {
                None
            } else {
                Some(api_key.as_str())
            },
        )
        .await;

    let model = ask_model(ask, streams, &offered, palette, t)?;

    Ok(Answers {
        locale,
        workspace,
        instance_id,
        instance: ProviderConfig {
            kind: spec.id.clone(),
            // An unchanged suggestion is not a label: leaving it empty is what
            // lets the type's display name keep improving.
            label: if label == spec.display_name {
                String::new()
            } else {
                label
            },
            api_base: Some(api_base),
            extra_headers: indexmap::IndexMap::new(),
            models: Vec::new(),
            enabled: true,
        },
        model,
        api_key,
    })
}

/// The only two writes the wizard makes, both at the end.
fn write(
    answers: &Answers,
    loaded: &LoadedConfig,
    credentials: &mut dyn CredentialSink,
) -> Result<()> {
    let mut merged: Config = loaded.config.clone();
    // The workspace is the install's; the model and provider are the default
    // agent's. Two homes because they are two kinds of thing — an agent works
    // *in* a workspace and does not own one.
    merged.workspace.clone_from(&answers.workspace);
    merged.ui.locale.clone_from(&answers.locale);
    let agent = merged
        .agents
        .list
        .entry(DEFAULT_AGENT_ID.to_owned())
        .or_default();
    agent.settings.provider.clone_from(&answers.instance_id);
    agent.settings.model.clone_from(&answers.model);
    merged
        .providers
        .insert(answers.instance_id.clone(), answers.instance.clone());

    ensure_dir(&loaded.paths.root)?;
    save_config(&loaded.file, &merged)?;

    if answers.api_key.is_empty() {
        return Ok(());
    }
    credentials.save(&answers.instance_id, &answers.api_key)
}
