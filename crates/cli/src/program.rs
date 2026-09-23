//! The `darkwire` command line.
//!
//! Two rules shape this file.
//!
//!  - **Nothing here runs at construction.** [`build_command`] describes the
//!    tree, [`parse`] turns an argv into a [`Invocation`], and [`run`] executes
//!    one. Neither touches the process: a test drives the whole parser in
//!    memory, and the exit code is a return value rather than a call into the
//!    runtime.
//!  - **Every description comes out of the bundle.** The tree is built with
//!    clap's builder API rather than its derive, because `--help` *is* the
//!    thing a translation has to reach: a derive attribute is a literal fixed
//!    at compile time, and a page of translated flag descriptions wrapped in
//!    English chrome is worse than either alone.
//!
//! Errors become exit codes here and nowhere else. A `WireError` from a
//! misconfigured provider is a message an operator can act on, not a stack
//! trace, so the structured detail is shown only when `DARKWIRE_DEBUG` asks.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use darkwire_core::{ErrorKind, LogLevel, Result, WireError};
use darkwire_i18n::{args, keys};

use crate::i18n::{Env, Translations, describe_error};

/// The workspace version, shared by `--version` and `GET /api/status`.
///
/// Read from the manifest at compile time. `tests/version.rs` asserts it equals
/// the root `package.json`, which is what replaced the hand-edited literal the
/// TypeScript CLI carried.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The levels `--log-level` accepts, in the order the help text lists them.
pub const LOG_LEVELS: [&str; 6] = ["trace", "debug", "info", "warn", "error", "fatal"];

/// What a minted conversation key starts with.
///
/// A `chat` with no `-s` starts a new conversation rather than continuing one,
/// so there is no fixed key to name here any more. What there is instead is
/// the prefix the minted ones carry, which is what makes a conversation
/// started at the prompt recognisable beside one started anywhere else.
pub const CLI_SESSION_PREFIX: &str = "cli";

/// Options that apply to every subcommand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Globals {
    /// `--home <dir>`: the DarkWire root, beating `$DARKWIRE_HOME`.
    pub home: Option<String>,
    /// `--log-level <level>`, already validated.
    pub log_level: Option<LogLevel>,
    /// `--verbose`.
    pub verbose: bool,
    /// `None` unless a colour flag was actually typed.
    ///
    /// This is the whole of the `--no-color` design. The flag is negated, so
    /// its value is `true` on every ordinary run — and passing that on is an
    /// explicit "yes, colour", which stops `NO_COLOR`, `FORCE_COLOR`,
    /// `TERM=dumb` and "stdout is a file" from being consulted at all.
    /// `darkwire chat > log` wrote escape codes into the log for exactly that
    /// reason. So the value is not the signal; the *source* is, and clap
    /// records it.
    pub color: Option<bool>,
}

impl Globals {
    /// The log level a one-shot turn runs at.
    ///
    /// An explicit `--log-level` wins over `--verbose`, because it is the more
    /// specific request: someone who named `debug` has asked for something
    /// `--verbose` cannot spell. Neither leaves the chat command to apply its
    /// own default of `error`.
    #[must_use]
    pub fn chat_log_level(&self) -> Option<LogLevel> {
        self.log_level.or(if self.verbose {
            Some(LogLevel::Info)
        } else {
            None
        })
    }

    /// The log level a server runs at.
    ///
    /// `info` is already the floor: a server that says nothing about the
    /// requests it is serving is a server nobody can debug. So `--verbose`
    /// means `debug` — the same relative move it makes on a chat.
    #[must_use]
    pub fn serve_log_level(&self) -> LogLevel {
        self.log_level.unwrap_or(if self.verbose {
            LogLevel::Debug
        } else {
            LogLevel::Info
        })
    }
}

/// Everything `chat` was asked for.
//
// Independent flags rather than a state machine, which is what the lint below
// would ask for: each is a separate thing the operator typed, no combination is
// illegal, and folding them into an enum would make the parser answer a
// question the command line never asked.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent command-line flags"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatArgs {
    /// A single turn to run, instead of opening the prompt.
    pub message: Option<String>,
    /// `-s, --session <key>`, or `None` to start a new conversation.
    pub session_key: Option<String>,
    /// `-a, --agent <id>`.
    pub agent_id: Option<String>,
    /// `-m, --model <id>`.
    pub model: Option<String>,
    /// `-p, --provider <id>`.
    pub provider: Option<String>,
    /// `-w, --workspaces <dir>`: the folder the workspaces live in.
    pub workspaces: Option<String>,
    /// `-W, --workspace-id <id>`: which workspace inside that folder.
    pub workspace_id: Option<String>,
    /// `--new`.
    pub fresh: bool,
    /// `--json`.
    pub json: bool,
    /// `--no-reasoning` clears it.
    pub show_reasoning: bool,
    /// `--no-tools` clears it.
    pub tools: bool,
    /// `--yes`: run tools set to `ask` without asking.
    pub yes: bool,
}

impl Default for ChatArgs {
    fn default() -> ChatArgs {
        ChatArgs {
            message: None,
            session_key: None,
            agent_id: None,
            model: None,
            provider: None,
            workspaces: None,
            workspace_id: None,
            fresh: false,
            json: false,
            show_reasoning: true,
            tools: true,
            yes: false,
        }
    }
}

/// Everything `serve` was asked for, after the environment fallbacks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServeArgs {
    /// `-H, --host <host>`.
    pub host: Option<String>,
    /// `-P, --port <port>`, already validated. `0` asks the OS for a free one.
    pub port: Option<u16>,
    /// `-w, --workspaces <dir>`.
    pub workspaces: Option<String>,
    /// `--password`, or `DARKWIRE_PASSWORD`. Empty is absent.
    pub password: Option<String>,
    /// `--username`, or `DARKWIRE_USERNAME`. Empty is absent.
    pub username: Option<String>,
    /// `--ui <dir>`.
    pub ui: Option<String>,
    /// `--ready-file <path>`: where to write the listening record.
    pub ready_file: Option<String>,
    /// `--json`: print the listening record on stdout as one line.
    pub json: bool,
}

/// The three verbs `environment` and `extension` share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreAction {
    /// Show what is installed and the state each is in.
    List,
    /// Record the current contents as approved.
    Approve,
    /// Withdraw an approval, leaving the files in place.
    Revoke,
}

/// What `agent` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    /// `agent list`.
    List,
}

/// What `darkwire sandbox` was asked to do.
///
/// An enum rather than the string clap validated, so the dispatch is
/// exhaustive. A `_ => list` arm would silently turn a verb somebody added to
/// the parser and forgot to wire up into a listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxAction {
    /// Every live instance.
    List,
    /// Whether the service and its engine are reachable.
    Health,
    /// Warm one instance without running a tool.
    Start,
    /// Stop one instance.
    Stop,
    /// Stop and start one instance.
    Restart,
}

/// One parsed subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subcommand {
    /// `chat`, which is also what a bare `darkwire` runs.
    Chat(Box<ChatArgs>),
    /// `init`.
    Init,
    /// `serve`.
    Serve(Box<ServeArgs>),
    /// `environment list`.
    Environment,
    /// Manage the sandbox service through its constrained socket API.
    Sandbox {
        /// Lifecycle operation.
        action: SandboxAction,
        /// Exact managed instance identifier.
        id: Option<String>,
        /// Environment definition used when warming an instance.
        environment: Option<String>,
        /// Registered workspace identifier.
        workspace: Option<String>,
        /// Service socket override.
        socket: Option<String>,
    },
    /// `extension list|approve|revoke`.
    Extension(StoreAction, Option<String>),

    /// `agent list`.
    Agent(AgentCommand),
}

/// A parsed command line, ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The options that apply whichever subcommand ran.
    pub globals: Globals,
    /// The subcommand itself.
    pub command: Subcommand,
}

/// What parsing produced: something to run, or something already answered.
#[derive(Debug)]
pub enum Parsed {
    /// Run this.
    Run(Box<Invocation>),
    /// clap has already written the page; exit with this code.
    ///
    /// `--help` and `--version` are successes that arrive as errors, which is
    /// why they are not failures here.
    Printed(String, u8),
    /// The line could not be parsed. The text is what to write to stderr.
    Refused(String, u8),
}

/// A translated heading, as the `&'static str` clap's headings require.
///
/// clap takes a section heading as a borrowed string with the program's
/// lifetime, and a translation is produced at run time — so the two are
/// reconciled by interning. Deduplicated rather than leaked per call, because
/// `build_command` runs once per process in the binary and once per case in the
/// tests, and a leak per case would grow with the suite. There are four
/// headings in one locale, so the table is four entries.
fn heading(text: &str) -> &'static str {
    static INTERNED: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();

    // The bundle spells a heading the way the published CLI prints it, with the
    // colon. clap appends its own, so the colon is stripped here rather than
    // the bundle edited: those strings are shared with a surface that still
    // wants them.
    let text = text.trim_end_matches(':').to_owned();
    let table = INTERNED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut table = match table.lock() {
        Ok(table) => table,
        // A poisoned table means a panic happened while a heading was being
        // interned. The headings are chrome; recovering the set is better than
        // failing `--help`.
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(existing) = table.get(text.as_str()) {
        return existing;
    }
    let leaked: &'static str = Box::leak(text.into_boxed_str());
    table.insert(leaked);
    leaked
}

/// The one option flag that is not a subcommand's, built once.
fn global_arg(name: &'static str, long: &'static str) -> Arg {
    Arg::new(name).long(long).global(true)
}

/// The same, under the heading a subcommand's help page files inherited options
/// under.
fn global_option(name: &'static str, long: &'static str, t: &Translations) -> Arg {
    global_arg(name, long).help_heading(heading(&t.t(keys::help::GLOBAL_OPTIONS)))
}

/// `darkwire sandbox`, which talks to the service over its socket.
///
/// Its own builder rather than a [`store_command`], because the service is not
/// an approval ledger: these verbs manage *running* instances, and each takes
/// options: an environment to warm, a workspace to warm it in, the socket to
/// reach — that approving a definition has no use for.
fn sandbox_command(t: &Translations) -> Command {
    Command::new("sandbox")
        .about(t.t(keys::sandbox::DESCRIPTION))
        .arg(
            Arg::new("action")
                .value_parser(["list", "health", "start", "stop", "restart"])
                .default_value("list")
                .help(t.t(keys::sandbox::action::DESCRIPTION)),
        )
        .arg(Arg::new("id").help(t.t(keys::sandbox::id::DESCRIPTION)))
        .arg(
            Arg::new("environment")
                .long("environment")
                .help(t.t(keys::sandbox::environment::DESCRIPTION)),
        )
        .arg(
            Arg::new("workspace")
                .long("workspace")
                .help(t.t(keys::sandbox::workspace::DESCRIPTION)),
        )
        .arg(
            Arg::new("socket")
                .long("socket")
                .help(t.t(keys::sandbox::socket::DESCRIPTION)),
        )
}

/// The whole command tree.
///
/// Public so `tests/help_parity.rs` can render every `--help` page without a
/// process, and so a test can assert the shape of the tree rather than the
/// prose it prints.
#[must_use]
pub fn build_command(t: &Translations) -> Command {
    Command::new("darkwire")
        .about(t.t(keys::program::DESCRIPTION))
        .version(VERSION)
        // `-V` is clap's, and this program spends `-v` on the version. A short
        // flag meaning one thing before the subcommand and another after it is
        // worse than no short flag, which is why `--verbose` has none.
        .disable_version_flag(true)
        // A plain flag rather than `ArgAction::Version`, for two reasons that
        // both matter. clap's own action prints `darkwire 0.8.1` and this
        // program has always printed the bare number, which is what a script
        // reading `$(darkwire -v)` parses. And a global version action has to be
        // answerable by every subcommand, so `darkwire chat -v` would need each
        // of them to declare a version of its own.
        .arg(
            global_option("version", "version", t)
                .short('v')
                .action(ArgAction::SetTrue)
                .help(t.t(keys::help::OUTPUT_VERSION)),
        )
        .arg(
            global_option("help", "help", t)
                .short('h')
                .action(ArgAction::Help)
                .help(t.t(keys::help::DISPLAY_HELP)),
        )
        .disable_help_flag(true)
        .subcommand_help_heading(heading(&t.t(keys::help::COMMANDS)))
        .help_template(help_template(t))
        .arg(
            global_option("home", "home", t)
                .value_name("dir")
                .help(t.t(keys::program::options::HOME)),
        )
        .arg(
            global_option("log-level", "log-level", t)
                .value_name("level")
                .help(t.tr(
                    keys::program::options::LOG_LEVEL,
                    args!["levels" => LOG_LEVELS.join(", ")],
                )),
        )
        // Global, beside `--log-level` rather than on `chat`, because that is
        // where someone looks for it: `chat` is the default command, so
        // `darkwire --help` is the help for what plain `darkwire` does, and a flag
        // that governs plain `darkwire` and is absent from that page may as well
        // not exist.
        .arg(
            global_option("verbose", "verbose", t)
                .action(ArgAction::SetTrue)
                .help(t.t(keys::program::options::VERBOSE)),
        )
        .arg(
            global_option("color", "no-color", t)
                .action(ArgAction::SetFalse)
                .default_value("true")
                .help(t.t(keys::program::options::NO_COLOR)),
        )
        .subcommand(chat_command(t))
        .subcommand(Command::new("init").about(t.t(keys::init::DESCRIPTION)))
        .subcommand(serve_command(t))
        .subcommand(sandbox_command(t))
        .subcommand(list_command(
            "environment",
            t.t(keys::environment::DESCRIPTION),
            t.t(keys::environment::list::DESCRIPTION),
        ))
        .subcommand(store_command(
            "extension",
            t.t(keys::extension::DESCRIPTION),
            t.t(keys::extension::list::DESCRIPTION),
            t.t(keys::extension::approve::DESCRIPTION),
            t.t(keys::extension::revoke::DESCRIPTION),
        ))
        .subcommand(agent_command(t))
        // clap's own `help` subcommand carries an English sentence with no seam
        // to translate it, and `darkwire help <command>` is on the documented
        // surface. So it is declared here and answered in `parse`.
        .disable_help_subcommand(true)
        .subcommand(
            Command::new("help")
                .about(t.t(keys::help::DISPLAY_HELP))
                .arg(Arg::new("command").num_args(0..).value_name("command")),
        )
        // A bare `darkwire "what changed today"` is a chat, so the words that
        // name no subcommand are the message.
        .allow_external_subcommands(false)
        .args_conflicts_with_subcommands(false)
        .subcommand_negates_reqs(true)
        .args(chat_args(t, true))
}

/// The help page layout, with the four headings the bundle names.
///
/// clap builds `Usage:` into its own template rather than exposing it as a
/// heading, so the literal is replaced here rather than through a hook. A
/// heading clap adds later falls through untranslated rather than throwing,
/// which is the right failure for chrome: an English word in the right place
/// beats a crash on `--help`.
fn help_template(t: &Translations) -> String {
    format!(
        "{{about-with-newline}}\n{usage}{{usage}}\n\n{{all-args}}",
        usage = format_args!("{} ", t.t(keys::help::USAGE))
    )
}

/// The arguments `chat` takes, shared by the subcommand and the bare root.
fn chat_args(t: &Translations, hidden: bool) -> Vec<Arg> {
    let options = heading(&t.t(keys::help::OPTIONS));
    let arguments = heading(&t.t(keys::help::ARGUMENTS));
    let args = vec![
        Arg::new("message")
            .num_args(0..)
            .value_name("message")
            .help_heading(arguments)
            .help(t.t(keys::chat::ARGUMENT)),
        Arg::new("session")
            .short('s')
            .long("session")
            .value_name("key")
            .help(t.t(keys::chat::options::SESSION)),
        Arg::new("agent")
            .short('a')
            .long("agent")
            .value_name("id")
            .help(t.t(keys::chat::options::AGENT)),
        Arg::new("model")
            .short('m')
            .long("model")
            .value_name("id")
            .help(t.t(keys::chat::options::MODEL)),
        Arg::new("provider")
            .short('p')
            .long("provider")
            .value_name("id")
            .help(t.t(keys::chat::options::PROVIDER)),
        // Two different things, deliberately spelled differently. `-w` names
        // the folder the workspaces live in; `-W` picks one inside it.
        // Accepting either on one flag and guessing by whether the string
        // exists on disk is how a typo'd id silently becomes a path.
        Arg::new("workspaces")
            .short('w')
            .long("workspaces")
            .value_name("dir")
            .help(t.t(keys::chat::options::WORKSPACES)),
        Arg::new("workspace-id")
            .short('W')
            .long("workspace-id")
            .value_name("id")
            .help(t.t(keys::chat::options::WORKSPACE_ID)),
        Arg::new("new")
            .long("new")
            .action(ArgAction::SetTrue)
            .help(t.t(keys::chat::options::NEW)),
        Arg::new("json")
            .long("json")
            .action(ArgAction::SetTrue)
            .help(t.t(keys::chat::options::JSON)),
        Arg::new("reasoning")
            .long("no-reasoning")
            .action(ArgAction::SetFalse)
            .help(t.t(keys::chat::options::NO_REASONING)),
        Arg::new("tools")
            .long("no-tools")
            .action(ArgAction::SetFalse)
            .help(t.t(keys::chat::options::NO_TOOLS)),
        Arg::new("yes")
            .long("yes")
            .action(ArgAction::SetTrue)
            .help(t.t(keys::chat::options::YES)),
    ];
    // Hidden on the root page, shown on `darkwire chat --help`. `chat` is the
    // default command, so the flags have to be *parseable* at the root — a bare
    // `darkwire -m qwen3 "hello"` is the shape people type — but listing them
    // twice would make the front page of the program a wall of chat options
    // with the seven commands below it.
    args.into_iter()
        .map(|arg| {
            if hidden {
                arg.hide(true)
            } else {
                arg.help_heading(options)
            }
        })
        .collect()
}

fn chat_command(t: &Translations) -> Command {
    Command::new("chat")
        .about(t.t(keys::chat::DESCRIPTION))
        .args(chat_args(t, false))
}

fn serve_command(t: &Translations) -> Command {
    Command::new("serve")
        .about(t.t(keys::serve::DESCRIPTION))
        .arg(
            Arg::new("host")
                .short('H')
                .long("host")
                .value_name("host")
                .help(t.t(keys::serve::options::HOST)),
        )
        .arg(
            Arg::new("port")
                .short('P')
                .long("port")
                .value_name("port")
                .help(t.t(keys::serve::options::PORT)),
        )
        .arg(
            Arg::new("workspaces")
                .short('w')
                .long("workspaces")
                .value_name("dir")
                .help(t.t(keys::serve::options::WORKSPACES)),
        )
        .arg(
            Arg::new("password")
                .long("password")
                .value_name("password")
                .help(t.t(keys::serve::options::PASSWORD)),
        )
        .arg(
            Arg::new("username")
                .long("username")
                .value_name("username")
                .help(t.t(keys::serve::options::USERNAME)),
        )
        .arg(
            Arg::new("ui")
                .long("ui")
                .value_name("dir")
                .help(t.t(keys::serve::options::UI)),
        )
        // Two flags with no key in the bundle, which is why their help is the
        // English the bundle would otherwise carry. Both are real features
        // rather than test seams: a port of `0` is unknowable until the
        // listener is bound, and a file is a contract where stdout is logs.
        .arg(
            Arg::new("ready-file")
                .long("ready-file")
                .value_name("path")
                .help("write the listening record to this file once the port is bound"),
        )
        .arg(
            Arg::new("json")
                .long("json")
                .action(ArgAction::SetTrue)
                .help("print the listening record on stdout as one line of JSON"),
        )
}

/// A definition directory with nothing to decide: `environment`.
///
/// Both are read-only because the file on disk *is* the policy. `extension`
/// still has the three verbs, which is why [`store_command`] stays.
fn list_command(name: &'static str, about: String, list: String) -> Command {
    Command::new(name)
        .about(about)
        .subcommand(Command::new("list").about(list))
}

/// `extension`, whose approval is a record in a store rather than a file.
fn store_command(
    name: &'static str,
    about: String,
    list: String,
    approve: String,
    revoke: String,
) -> Command {
    Command::new(name)
        .about(about)
        .subcommand(Command::new("list").about(list))
        .subcommand(
            Command::new("approve")
                .about(approve)
                .arg(Arg::new("id").required(true).value_name("id")),
        )
        .subcommand(
            Command::new("revoke")
                .about(revoke)
                .arg(Arg::new("id").required(true).value_name("id")),
        )
}

fn agent_command(t: &Translations) -> Command {
    Command::new("agent")
        .about(t.t(keys::agent::DESCRIPTION))
        .subcommand(Command::new("list").about(t.t(keys::agent::list::DESCRIPTION)))
}

/// A port from the command line, refused before anything binds.
fn resolve_port(value: Option<&String>, t: &Translations) -> Result<Option<u16>> {
    let Some(value) = value else { return Ok(None) };
    value.parse::<u16>().map(Some).map_err(|_| {
        WireError::new(
            ErrorKind::InvalidInput,
            t.tr(keys::program::NOT_A_PORT, args!["value" => value.as_str()]),
        )
    })
}

fn resolve_log_level(value: Option<&String>, t: &Translations) -> Result<Option<LogLevel>> {
    let Some(value) = value else { return Ok(None) };
    // Against this program's own list rather than the parser's: `silent` is a
    // valid configuration spelling and not one of the six `--log-level` offers,
    // and a flag that accepts a level its own help does not list is a flag
    // nobody can reason about.
    LogLevel::parse(value)
        .filter(|_| LOG_LEVELS.contains(&value.as_str()))
        .map(Some)
        .ok_or_else(|| {
            WireError::new(
                ErrorKind::InvalidInput,
                t.tr(
                    keys::program::UNKNOWN_LOG_LEVEL,
                    args!["value" => value.as_str()],
                ),
            )
        })
}

fn string_of(matches: &ArgMatches, name: &str) -> Option<String> {
    matches.get_one::<String>(name).cloned()
}

fn flag(matches: &ArgMatches, name: &str) -> bool {
    matches.get_flag(name)
}

/// Reads the chat options off whichever level of the tree carried them.
fn chat_args_of(matches: &ArgMatches) -> ChatArgs {
    let words: Vec<String> = matches
        .get_many::<String>("message")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    let message = words.join(" ").trim().to_owned();

    ChatArgs {
        message: if message.is_empty() {
            None
        } else {
            Some(message)
        },
        session_key: string_of(matches, "session"),
        agent_id: string_of(matches, "agent"),
        model: string_of(matches, "model"),
        provider: string_of(matches, "provider"),
        workspaces: string_of(matches, "workspaces"),
        workspace_id: string_of(matches, "workspace-id"),
        fresh: flag(matches, "new"),
        json: flag(matches, "json"),
        show_reasoning: flag(matches, "reasoning"),
        tools: flag(matches, "tools"),
        yes: flag(matches, "yes"),
    }
}

fn serve_args_of(matches: &ArgMatches, env: &Env, t: &Translations) -> Result<ServeArgs> {
    // The environment is read here rather than in the serve command, which then
    // stays testable without anyone mutating the process environment.
    let password = string_of(matches, "password")
        .or_else(|| env.get("DARKWIRE_PASSWORD").map(str::to_owned))
        .filter(|value| !value.is_empty());
    let username = string_of(matches, "username")
        .or_else(|| env.get("DARKWIRE_USERNAME").map(str::to_owned))
        .filter(|value| !value.is_empty());

    Ok(ServeArgs {
        host: string_of(matches, "host"),
        port: resolve_port(matches.get_one::<String>("port"), t)?,
        workspaces: string_of(matches, "workspaces"),
        password,
        username,
        ui: string_of(matches, "ui"),
        ready_file: string_of(matches, "ready-file"),
        json: flag(matches, "json"),
    })
}

/// The verb and the id an `environment` or `extension` invocation named.
///
/// A bare `darkwire environment` lists, which is the one an operator means by it,
/// so there is no "no verb" answer to give back.
/// The verb clap already restricted to this set.
fn sandbox_action_of(matches: &ArgMatches) -> SandboxAction {
    match string_of(matches, "action").as_deref() {
        Some("health") => SandboxAction::Health,
        Some("start") => SandboxAction::Start,
        Some("stop") => SandboxAction::Stop,
        Some("restart") => SandboxAction::Restart,
        _ => SandboxAction::List,
    }
}

fn store_action_of(matches: &ArgMatches) -> (StoreAction, Option<String>) {
    match matches.subcommand() {
        Some(("approve", sub)) => (StoreAction::Approve, string_of(sub, "id")),
        Some(("revoke", sub)) => (StoreAction::Revoke, string_of(sub, "id")),
        _ => (StoreAction::List, None),
    }
}

/// The globals, including the colour source that `--no-color` turns on.
fn globals_of(matches: &ArgMatches, t: &Translations) -> Result<Globals> {
    Ok(Globals {
        home: string_of(matches, "home"),
        log_level: resolve_log_level(matches.get_one::<String>("log-level"), t)?,
        verbose: flag(matches, "verbose"),
        color: match matches.value_source("color") {
            // "Nobody said", which is the one case that has to reach the
            // palette as nothing at all.
            Some(ValueSource::DefaultValue) | None => None,
            Some(_) => Some(flag(matches, "color")),
        },
    })
}

impl Parsed {
    /// The invocation, when there is one.
    ///
    /// For the callers — the tests, mostly — that have already established the
    /// line parses and want the shape rather than the branch.
    #[must_use]
    pub fn into_run(self) -> Option<Invocation> {
        match self {
            Parsed::Run(invocation) => Some(*invocation),
            Parsed::Printed(..) | Parsed::Refused(..) => None,
        }
    }
}

/// The help page for one command path, or `None` when it names nothing.
///
/// Rendered from the same tree the parser uses rather than from a second
/// description of it, which is the property `tests/help_parity.rs` leans on.
#[must_use]
pub fn render_help(t: &Translations, path: &[String]) -> Option<String> {
    let mut command = build_command(t);
    // `build` before walking: clap fills in the propagated globals and the
    // generated usage strings there, and an unbuilt subcommand renders a page
    // missing every inherited option.
    command.build();
    let mut current = &mut command;
    for name in path {
        current = current.find_subcommand_mut(name.as_str())?;
    }
    Some(current.render_help().to_string())
}

/// Turns an argv into something to run, or into what to print instead.
///
/// Never exits and never writes: `--help` and `--version` come back as
/// [`Parsed::Printed`] carrying the page, so a test reads it as a value and the
/// binary writes it to the stream it chose.
#[must_use]
pub fn parse<I, S>(argv: I, env: &Env, t: &Translations) -> Parsed
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString> + Clone,
{
    let matches = match build_command(t).try_get_matches_from(argv) {
        Ok(matches) => matches,
        Err(error) => {
            let text = error.render().to_string();
            return match error.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayVersion
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                    Parsed::Printed(text, 0)
                }
                // clap exits 2 on a parse error and commander exited 1. One
                // code for "you typed something this program does not accept"
                // is what a shell script branches on, and 1 is the one the
                // published CLI has always used.
                _ => Parsed::Refused(text, 1),
            };
        }
    };

    // Before anything else, and before the globals are validated: `darkwire
    // --log-level nonsense --version` is somebody asking what version this is,
    // and answering with a complaint about an unrelated flag helps nobody.
    if matches.get_flag("version") {
        return Parsed::Printed(format!("{VERSION}\n"), 0);
    }

    if let Some(("help", sub)) = matches.subcommand() {
        let path: Vec<String> = sub
            .get_many::<String>("command")
            .map(|values| values.cloned().collect())
            .unwrap_or_default();
        return match render_help(t, &path) {
            Some(page) => Parsed::Printed(page, 0),
            None => Parsed::Refused(format!("Unknown command: {}\n", path.join(" ")), 1),
        };
    }

    let globals = match globals_of(&matches, t) {
        Ok(globals) => globals,
        Err(error) => return Parsed::Refused(format!("{}\n", describe_error(&error)), 1),
    };

    let command = match matches.subcommand() {
        Some(("chat", sub)) => Subcommand::Chat(Box::new(chat_args_of(sub))),
        Some(("init", _)) => Subcommand::Init,
        Some(("serve", sub)) => match serve_args_of(sub, env, t) {
            Ok(serve) => Subcommand::Serve(Box::new(serve)),
            Err(error) => return Parsed::Refused(format!("{}\n", describe_error(&error)), 1),
        },
        Some(("environment", _)) => Subcommand::Environment,
        Some(("sandbox", sub)) => Subcommand::Sandbox {
            action: sandbox_action_of(sub),
            id: string_of(sub, "id"),
            environment: string_of(sub, "environment"),
            workspace: string_of(sub, "workspace"),
            socket: string_of(sub, "socket"),
        },
        Some(("extension", sub)) => {
            let (action, id) = store_action_of(sub);
            Subcommand::Extension(action, id)
        }
        Some(("agent", _)) => Subcommand::Agent(AgentCommand::List),
        // No subcommand: the words are a chat, which is the default.
        _ => Subcommand::Chat(Box::new(chat_args_of(&matches))),
    };

    Parsed::Run(Box::new(Invocation { globals, command }))
}
