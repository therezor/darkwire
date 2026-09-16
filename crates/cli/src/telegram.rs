//! Telegram, wired into `darkwire serve`.
//!
//! Two things live here, and both are here rather than in `darkwire-channels`
//! for the same reason: this is the composition root, and it is the only place
//! that has a credential vault and an environment to read. A
//! `ChannelContext` has neither, deliberately — a channel that could open the
//! vault could read every provider key in it.
//!
//!  - **Resolving the bot token.** Vault first, then the environment, then the
//!    config file, which is the order provider keys already use and for the
//!    same reason: the vault is the documented home, and a token sitting in
//!    `config.yaml` is plaintext on disk.
//!
//!  - **Filling in [`TelegramConsole`].** The half of a chat command's world
//!    that is not a hub frame — stores, agents, the model catalogue, memory and
//!    skills. The channel states the port; this satisfies it.
//!
//! The factory is registered **only when a token resolves**, so an install that
//! has never configured Telegram starts exactly as it did before this existed.

use std::sync::Arc;

use darkwire_agent::read_skills;
use darkwire_channels::telegram::channel::NewId;
use darkwire_channels::telegram::{MemoryState, SkillSummary, SkillsState, TelegramConsole};
use darkwire_channels::{BoxFuture, ChannelFactory, TelegramChannelOptions, telegram_channel};
use darkwire_core::memory::read_memories;
use darkwire_core::{Result, SessionRecord, SessionStore, WirePaths, WorkspaceStore};
use darkwire_protocol::config::{
    AgentEntryPatch, AgentSettingsPatch, AgentsConfigPatch, Config, ConfigPatch,
};
use darkwire_protocol::{
    AgentSummary, ChannelStatus, ContextResponse, DEFAULT_AGENT_ID, DEFAULT_WORKSPACE_ID,
    ModelsResponse, ToolPermission,
};
use darkwire_providers::estimate_tokens;
use darkwire_runtime::{EffectiveAgent, WireRuntime, open_vault};
use darkwire_server::{ServerRuntime, build_context_response};
use serde_json::{Map, Value};

use crate::i18n::Env;

/// The vault namespace a channel's credentials live under.
pub const CHANNEL_CREDENTIAL_NAMESPACE: &str = "channels";

/// The environment variable consulted when the vault holds no token.
pub const TELEGRAM_TOKEN_ENV_VAR: &str = "TELEGRAM_BOT_TOKEN";

/// The channel id, which is also its `config.channels` key and its vault key.
pub const TELEGRAM_CHANNEL_ID: &str = "telegram";

/// The warning a token found in `config.yaml` earns.
///
/// Said out loud once at startup rather than left to be discovered: a bot token
/// is a credential, and `config.yaml` is a plain file that backups, dotfile
/// repositories and screen shares all reach.
pub const PLAINTEXT_TOKEN_WARNING: &str = "the Telegram bot token is in config.yaml as plain text; \
     move it to the credential vault under channels/telegram";

/// Where a resolved token came from.
///
/// Returned beside the token rather than logged from inside the lookup, which
/// is the seam that keeps the lookup a pure function of three inputs: the
/// warning below is a decision about one source, and a test can assert the
/// source without installing a subscriber to watch for a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// The credential vault, which is the documented home.
    Vault,
    /// `TELEGRAM_BOT_TOKEN`.
    Environment,
    /// `channels.telegram.token`, which is plaintext on disk.
    Config,
}

/// A bot token and where it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedToken {
    /// The token itself.
    pub token: String,
    /// Which of the three places had it.
    pub source: TokenSource,
}

/// `config.channels.telegram`, narrowed. Unknown to the type, loose by design.
///
/// The channels block is deliberately a loose object so that each channel owns
/// its own settings; anything that is not an object is the same as nothing at
/// all, and the channel reports the mistake by refusing to start rather than by
/// behaving oddly later.
#[must_use]
pub fn telegram_settings_of(config: &Config) -> Map<String, Value> {
    config
        .channels
        .extra
        .get(TELEGRAM_CHANNEL_ID)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// The bot token, from the first place that has one.
///
/// The vault is opened only when one already exists on disk — the same
/// condition the provider credential lookup applies, and for the same reason:
/// resolving the vault key writes one to the OS keychain the first time it
/// runs, and an install that never stores a credential should not acquire a
/// keychain entry just by booting.
///
/// A vault that exists but will not open is an error rather than a miss. It
/// means the wrong key or a modified file, and falling through to the
/// environment would reach the Bot API as an unexplained 401 with nothing
/// anywhere saying why.
pub fn resolve_telegram_token(
    paths: &WirePaths,
    env: &Env,
    settings: &Map<String, Value>,
) -> Result<Option<ResolvedToken>> {
    if paths.vault_file.exists() {
        let vault = open_vault(paths)?;
        if let Some(stored) = vault
            .get(CHANNEL_CREDENTIAL_NAMESPACE, TELEGRAM_CHANNEL_ID)
            .filter(|value| !value.is_empty())
        {
            return Ok(Some(ResolvedToken {
                token: stored.to_owned(),
                source: TokenSource::Vault,
            }));
        }
    }

    if let Some(from_env) = env.non_empty(TELEGRAM_TOKEN_ENV_VAR) {
        return Ok(Some(ResolvedToken {
            token: from_env.to_owned(),
            source: TokenSource::Environment,
        }));
    }

    if let Some(from_config) = settings
        .get("token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(ResolvedToken {
            token: from_config.to_owned(),
            source: TokenSource::Config,
        }));
    }

    Ok(None)
}

/// Says the plaintext warning once, when it is the one that applies.
///
/// Separate from the lookup so the lookup stays assertable without a subscriber,
/// and so a caller that resolves a token twice — the status route and the
/// factory builder both do — warns on the path that boots rather than on every
/// panel refresh.
pub fn warn_if_plaintext(resolved: &ResolvedToken) {
    if resolved.source == TokenSource::Config {
        tracing::warn!(channel = TELEGRAM_CHANNEL_ID, "{PLAINTEXT_TOKEN_WARNING}");
    }
}

/// What the settings panel shows for Telegram.
#[derive(Debug)]
pub struct TelegramStatusOptions<'a> {
    /// The live settings tree, which holds the `channels.telegram` block.
    pub config: &'a Config,
    /// Where the vault would be.
    pub paths: &'a WirePaths,
    /// The environment `TELEGRAM_BOT_TOKEN` would be in.
    pub env: &'a Env,
    /// Whether the manager currently holds a started channel under this id.
    pub running: bool,
    /// The bot's username, when the caller can name it.
    ///
    /// Taken rather than read off the channel: `ChannelManager` answers with
    /// `Arc<dyn Channel>`, and the username lives on the concrete Telegram
    /// channel, which this crate cannot recover from the trait object.
    pub username: Option<String>,
    /// Why the last start failed, when it did.
    pub start_error: Option<String>,
}

/// What the settings panel shows for Telegram.
///
/// Four separate answers, because "is my bot working" has four and the operator
/// has to act on a different one in each case. `configured` is a boolean and
/// never the token: the vault is write-only over HTTP, so this is the only way
/// the panel can say a token is saved rather than showing an empty box over a
/// bot that is running perfectly well.
///
/// Never fails, because the port it feeds cannot: a vault that will not open is
/// reported as "not configured" with the reason in `detail`, which is the same
/// place a failed start's reason goes and the only field an operator reads.
#[must_use]
pub fn telegram_status(options: &TelegramStatusOptions<'_>) -> ChannelStatus {
    let settings = telegram_settings_of(options.config);
    let resolved = resolve_telegram_token(options.paths, options.env, &settings);
    let configured = matches!(resolved, Ok(Some(_)));
    let vault_error = resolved.err().map(|error| error.message);

    ChannelStatus {
        id: TELEGRAM_CHANNEL_ID.to_owned(),
        // Absent means enabled: the manager only skips a channel whose block
        // says `enabled: false`, so the panel has to read the same default.
        enabled: settings.get("enabled").and_then(Value::as_bool) != Some(false),
        configured,
        running: options.running,
        detail: detail_of(options, vault_error),
    }
}

/// The one line under the four booleans, or nothing.
///
/// A running bot is named; a stopped one says why it is not running, and a
/// vault that would not open outranks a start error because it is upstream of
/// one.
fn detail_of(options: &TelegramStatusOptions<'_>, vault_error: Option<String>) -> Option<String> {
    if options.running {
        return options.username.as_ref().map(|name| format!("@{name}"));
    }
    vault_error.or_else(|| options.start_error.clone())
}

/// Everything the Telegram factory needs from the composition root.
pub struct TelegramFactoriesOptions {
    /// The composition root, for the stores, the agents and the jails.
    pub runtime: Arc<WireRuntime>,
    /// The server's own port, for the agent list, the model catalogue and the
    /// context report — all of which it already answers for the REST API.
    pub server: Arc<dyn ServerRuntime>,
    /// Where the vault would be.
    pub paths: WirePaths,
    /// The environment `TELEGRAM_BOT_TOKEN` would be in.
    pub env: Env,
    /// Ids for `/new` and `/branch`, injected so a test is not at the mercy of
    /// a uuid.
    pub new_id: NewId,
}

impl std::fmt::Debug for TelegramFactoriesOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramFactoriesOptions")
            .field("paths", &self.paths.root)
            .finish_non_exhaustive()
    }
}

/// The Telegram factory, or nothing.
///
/// Nothing is the normal case, and it has to stay cheap: `darkwire serve` must
/// come up unchanged on the overwhelming majority of installs that have never
/// heard of a bot. A token that *does* resolve but is refused by the Bot API is
/// a different matter — that fails startup, which is what the channel contract
/// documents.
pub fn telegram_factories(options: &TelegramFactoriesOptions) -> Result<Vec<ChannelFactory>> {
    let settings = telegram_settings_of(&options.runtime.config());
    let Some(resolved) = resolve_telegram_token(&options.paths, &options.env, &settings)? else {
        return Ok(Vec::new());
    };
    warn_if_plaintext(&resolved);

    let console: Arc<dyn TelegramConsole> = Arc::new(RuntimeConsole {
        runtime: Arc::clone(&options.runtime),
        server: Arc::clone(&options.server),
    });
    Ok(vec![telegram_channel(TelegramChannelOptions::new(
        resolved.token,
        console,
        Arc::clone(&options.new_id),
    ))])
}

/// [`TelegramConsole`] over the running install.
///
/// The two stores are handed over concretely, exactly as the server's own port
/// hands them to the routes: the port is narrow about *behaviour* — what a chat
/// may reach — not about types.
struct RuntimeConsole {
    runtime: Arc<WireRuntime>,
    server: Arc<dyn ServerRuntime>,
}

impl RuntimeConsole {
    /// The agent and session a `/memory` or `/skills` call is about.
    ///
    /// Read together because both come from the stored row rather than the
    /// incoming message — a chat is bound to a conversation, and the
    /// conversation names the agent.
    fn targets(&self, session_key: &str) -> (Option<EffectiveAgent>, Option<SessionRecord>) {
        let session = self.runtime.store().get_session(session_key).ok().flatten();
        let agent_id = session
            .as_ref()
            .and_then(|record| record.agent_id.clone())
            .unwrap_or_else(|| DEFAULT_AGENT_ID.to_owned());
        let agent = self
            .runtime
            .agents()
            .into_iter()
            .find(|entry| entry.id == agent_id);
        (agent, session)
    }

    /// The workspace root a session's files live under.
    fn workspace_root(&self, session: Option<&SessionRecord>) -> std::path::PathBuf {
        let workspace_id =
            session.map_or(DEFAULT_WORKSPACE_ID, |record| record.workspace_id.as_str());
        self.runtime
            .jails()
            .for_workspace(workspace_id)
            .root()
            .to_path_buf()
    }
}

/// Whether a permission grants the tool at all. Absent counts as denied.
fn granted(agent: Option<&EffectiveAgent>, tool: &str) -> bool {
    agent
        .and_then(|entry| entry.tools.get(tool))
        .is_some_and(|permission| *permission != ToolPermission::Deny)
}

impl TelegramConsole for RuntimeConsole {
    fn store(&self) -> &SessionStore {
        self.runtime.store()
    }

    fn workspaces(&self) -> &WorkspaceStore {
        self.runtime.workspaces()
    }

    fn agents(&self) -> Vec<AgentSummary> {
        // Two structurally identical types, deliberately kept apart: the server
        // states its own `AgentSummary` on the port so that a route test need
        // not depend on the wire crate, and this is the wire one. Converting
        // here is what keeps the port from having to know about the channel.
        self.server
            .agents()
            .into_iter()
            .map(|agent| AgentSummary {
                id: agent.id,
                label: agent.label,
                model: agent.model,
                provider: agent.provider,
                reasoning_effort: agent.reasoning_effort,
            })
            .collect()
    }

    fn models(&self) -> BoxFuture<'_, Result<ModelsResponse>> {
        Box::pin(async move {
            // The catalogue is optional on the port the server states, because
            // a route test standing in for a runtime has no provider to ask.
            // The real adapter always has one.
            match self.server.models(false) {
                Some(future) => future.await,
                None => Ok(ModelsResponse {
                    models: Vec::new(),
                    errors: indexmap::IndexMap::new(),
                }),
            }
        })
    }

    fn set_model(&self, id: &str) {
        // A patch rather than a save: this moves the process without rewriting
        // `config.yaml`, so a restart returns to whatever the operator actually
        // configured.
        //
        // Onto the default agent, because that is the one the bot's own
        // conversations run on unless they have been bound elsewhere. Moving an
        // agent the chat is *not* on is the bug this alignment is about.
        let mut list = indexmap::IndexMap::new();
        list.insert(
            DEFAULT_AGENT_ID.to_owned(),
            Some(AgentEntryPatch {
                settings: AgentSettingsPatch {
                    model: Some(id.to_owned()),
                    ..AgentSettingsPatch::default()
                },
                ..AgentEntryPatch::default()
            }),
        );
        let patch = ConfigPatch {
            agents: Some(AgentsConfigPatch { list: Some(list) }),
            ..ConfigPatch::default()
        };
        // The port returns nothing, so a refusal is reported rather than
        // raised: the chat command that called this says what it did, and a
        // model id the settings will not accept is the operator's typing.
        if let Err(error) = self.runtime.apply_patch(&patch) {
            tracing::warn!(
                channel = TELEGRAM_CHANNEL_ID,
                model = id,
                error = %error.message,
                "the model could not be changed"
            );
        }
    }

    fn context<'a>(
        &'a self,
        session_key: &'a str,
    ) -> BoxFuture<'a, Result<Option<ContextResponse>>> {
        Box::pin(async move { build_context_response(self.server.as_ref(), session_key).await })
    }

    fn memory<'a>(&'a self, session_key: &'a str) -> BoxFuture<'a, Result<MemoryState>> {
        Box::pin(async move {
            let (agent, session) = self.targets(session_key);
            let granted = granted(agent.as_ref(), "memory");
            let memories = if granted {
                read_memories(&self.workspace_root(session.as_ref()))
            } else {
                Vec::new()
            };

            let index = memories
                .iter()
                .map(|memory| memory.description.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            Ok(MemoryState {
                granted,
                count: memories.len(),
                // What the *index* costs, which is what reaches the prompt. The
                // bodies are on disk until something opens one.
                tokens: estimate_tokens(&index) as u64,
            })
        })
    }

    fn skills<'a>(&'a self, session_key: &'a str) -> BoxFuture<'a, Result<SkillsState>> {
        Box::pin(async move {
            // The same targets `/memory` reads, and the same gate: a denied
            // tool takes the catalogue out of the prompt, so there is nothing
            // to offer.
            let (agent, session) = self.targets(session_key);
            let granted = granted(agent.as_ref(), "skill");
            let skills = if granted {
                read_skills(&self.workspace_root(session.as_ref()))
            } else {
                Vec::new()
            };

            // Name and description only. A body runs to 12 KB and this is a
            // listing. Every sheet is listed, with the ones this agent will not
            // be told about marked — the catalogue is a property of the
            // workspace, and hiding a sheet from the listing as well as from
            // the prompt leaves nowhere to find out why it is not working.
            let agent_id = agent
                .as_ref()
                .map_or(DEFAULT_AGENT_ID, |entry| entry.id.as_str());
            Ok(SkillsState {
                granted,
                skills: skills
                    .into_iter()
                    .map(|skill| SkillSummary {
                        mine: skill.agents.is_empty()
                            || skill.agents.iter().any(|id| id == agent_id),
                        name: skill.name,
                        description: skill.description,
                    })
                    .collect(),
            })
        })
    }
}
