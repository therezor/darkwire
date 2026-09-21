//! The parser, driven in memory.
//!
//! Every case here parses an argv and reads the [`Invocation`] back, rather than
//! spawning the binary: a process per case would make the suite slow enough that
//! nobody would add the twentieth case, and the thing under test is the tree
//! rather than the operating system's argument passing.

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

use darkwire::i18n::{Env, Translations};
use darkwire::program::{
    AgentCommand, ChatArgs, Globals, Invocation, Parsed, StoreAction, Subcommand, VERSION, parse,
};
use darkwire_core::LogLevel;

/// Parses one command line with an empty environment.
fn run(args: &[&str]) -> Parsed {
    let env = Env::empty();
    let t = Translations::default();
    let mut line = vec!["darkwire"];
    line.extend_from_slice(args);
    parse(line, &env, &t)
}

/// Parses one command line against a supplied environment.
fn run_in(args: &[&str], env: &Env) -> Parsed {
    let t = Translations::default();
    let mut line = vec!["darkwire"];
    line.extend_from_slice(args);
    parse(line, env, &t)
}

fn invocation(args: &[&str]) -> Invocation {
    match run(args) {
        Parsed::Run(invocation) => *invocation,
        Parsed::Printed(text, _) => panic!("expected a command, printed: {text}"),
        Parsed::Refused(text, _) => panic!("expected a command, refused: {text}"),
    }
}

fn chat(args: &[&str]) -> (Globals, ChatArgs) {
    let invocation = invocation(args);
    match invocation.command {
        Subcommand::Chat(args) => (invocation.globals, *args),
        other => panic!("expected chat, got {other:?}"),
    }
}

fn printed(args: &[&str]) -> (String, u8) {
    match run(args) {
        Parsed::Printed(text, code) => (text, code),
        other => panic!("expected a printed page, got {other:?}"),
    }
}

#[test]
fn version_matches_the_manifest() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../package.json"),
    )
    .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(manifest["version"], VERSION);
}

#[test]
fn prints_help_and_exits_zero() {
    let (text, code) = printed(&["--help"]);
    assert_eq!(code, 0);
    assert!(text.contains("Usage: darkwire"), "{text}");
    assert!(text.contains("chat"), "{text}");
}

#[test]
fn prints_the_bare_version() {
    // The bare number, not `darkwire 0.8.1`: a script reading `$(darkwire -v)`
    // parses what this prints.
    let (text, code) = printed(&["--version"]);
    assert_eq!(code, 0);
    assert_eq!(text.trim(), VERSION);
}

#[test]
fn offers_extension_and_environment_commands() {
    // Approving code the agent will run is the one operator action that cannot
    // be delegated to the agent, and an install driven from a terminal needs a
    // way to do it without opening a browser.
    let (root, _) = printed(&["--help"]);
    assert!(root.contains("extension"));
    assert!(root.contains("environment"));

    let (help, code) = printed(&["extension", "--help"]);
    assert_eq!(code, 0);
    for verb in ["list", "approve", "revoke"] {
        assert!(help.contains(verb), "{verb} missing from {help}");
    }
}

#[test]
fn environment_is_a_listing_with_nothing_to_decide() {
    // The definition file is the policy, so there is no verb here that changes
    // one. A bare `environment` and `environment list` are the same request.
    for argv in [vec!["environment"], vec!["environment", "list"]] {
        match invocation(&argv).command {
            Subcommand::Environment => {}
            other => panic!("expected an environment listing, got {other:?}"),
        }
    }
    match run(&["environment", "approve", "dev"]) {
        Parsed::Refused(_, code) => assert_ne!(code, 0),
        other => panic!("expected approve to be gone, got {other:?}"),
    }
}

#[test]
fn offers_agent_with_its_listing() {
    let (root, _) = printed(&["--help"]);
    assert!(root.contains("agent"));

    let (help, code) = printed(&["agent", "--help"]);
    assert_eq!(code, 0);
    assert!(help.contains("list"));
}

#[test]
fn fails_on_an_unknown_option_instead_of_guessing() {
    match run(&["--nonsense"]) {
        Parsed::Refused(_, code) => assert_ne!(code, 0),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn treats_a_bare_message_as_chat() {
    let (_, args) = chat(&["what", "is", "here?"]);
    assert_eq!(args.message.as_deref(), Some("what is here?"));
}

#[test]
fn opens_the_prompt_when_there_is_no_message() {
    let (_, args) = chat(&["chat"]);
    assert_eq!(args.message, None);
}

#[test]
fn maps_every_flag_onto_the_chat_options() {
    let (globals, args) = chat(&[
        "--home",
        "/srv/ghost",
        "chat",
        "-s",
        "work",
        "-m",
        "qwen3",
        "-p",
        "ollama",
        "-a",
        "reviewer",
        "-w",
        "/tmp/ws",
        "--new",
        "--no-reasoning",
        "--no-tools",
        "go",
    ]);

    assert_eq!(args.message.as_deref(), Some("go"));
    assert_eq!(args.session_key.as_deref(), Some("work"));
    assert_eq!(args.model.as_deref(), Some("qwen3"));
    assert_eq!(args.provider.as_deref(), Some("ollama"));
    assert_eq!(args.agent_id.as_deref(), Some("reviewer"));
    assert_eq!(args.workspaces.as_deref(), Some("/tmp/ws"));
    assert_eq!(globals.home.as_deref(), Some("/srv/ghost"));
    assert!(args.fresh);
    assert!(!args.show_reasoning);
    assert!(!args.tools);
}

#[test]
fn leaves_the_agent_unset_when_no_flag_named_one() {
    // So the stored binding decides.
    let (_, args) = chat(&["chat", "hello"]);
    assert_eq!(args.agent_id, None);
}

#[test]
fn names_no_session_when_no_flag_named_one() {
    // A prompt with no `-s` starts a conversation of its own, so there is no
    // key to parse here: minting one needs the runtime's clock.
    let (_, args) = chat(&["chat", "hello"]);
    assert_eq!(args.session_key, None);
    // Absent, not empty: the runtime distinguishes "no override" from a value.
    assert_eq!(args.model, None);
}

#[test]
fn distinguishes_the_two_workspace_flags() {
    // `-w` names the folder the workspaces live in; `-W` picks one inside it.
    // Accepting either on one flag and guessing by whether the string exists on
    // disk is how a typo'd id silently becomes a path.
    let (_, args) = chat(&["chat", "-w", "/tmp/tree", "-W", "acme", "hi"]);
    assert_eq!(args.workspaces.as_deref(), Some("/tmp/tree"));
    assert_eq!(args.workspace_id.as_deref(), Some("acme"));
}

#[test]
fn says_nothing_about_colour_when_nobody_asked() {
    // The seam, and the whole of the fix. `--no-color` is a negated flag, so its
    // value is `true` on every ordinary run — and passing that on is an explicit
    // "yes", which stops `NO_COLOR`, `FORCE_COLOR`, `TERM=dumb` and "stdout is a
    // file" from being consulted at all.
    let (globals, _) = chat(&["chat", "hi"]);
    assert_eq!(globals.color, None);
}

#[test]
fn honours_no_color_for_prose_output() {
    let (globals, _) = chat(&["--no-color", "chat", "hi"]);
    assert_eq!(globals.color, Some(false));
}

#[test]
fn leaves_the_log_level_unset_without_verbose() {
    // A chat prints the conversation, and a warning about the install would
    // interrupt it on every turn to say the same thing.
    let (globals, _) = chat(&["chat", "hi"]);
    assert_eq!(globals.chat_log_level(), None);
}

#[test]
fn verbose_asks_for_the_installs_own_reporting() {
    // Before the subcommand: it is a global, listed on `darkwire --help`, which
    // is where someone looks for it because `chat` is the default command.
    let (globals, _) = chat(&["--verbose", "chat", "hi"]);
    assert_eq!(globals.chat_log_level(), Some(LogLevel::Info));
}

#[test]
fn takes_verbose_after_the_subcommand_too() {
    // Where a hand lands: `darkwire chat --verbose` is what someone types who has
    // already started the sentence.
    let (globals, _) = chat(&["chat", "--verbose", "hi"]);
    assert_eq!(globals.chat_log_level(), Some(LogLevel::Info));
}

#[test]
fn leaves_v_as_version_rather_than_shadowing_it_per_subcommand() {
    // `--verbose` has no short flag on purpose. A `-v` that printed the version
    // before `chat` and raised the log level after it is the kind of thing
    // nobody discovers until it bites.
    let (text, code) = printed(&["chat", "-v"]);
    assert_eq!(code, 0);
    assert_eq!(text.trim(), VERSION);
}

#[test]
fn lets_an_explicit_log_level_win_over_verbose() {
    // The more specific request: someone who named `debug` has asked for
    // something `--verbose` cannot spell.
    let (globals, _) = chat(&["--log-level", "debug", "chat", "--verbose", "hi"]);
    assert_eq!(globals.chat_log_level(), Some(LogLevel::Debug));
}

#[test]
fn rejects_a_log_level_the_logger_would_not_understand() {
    match run(&["--log-level", "chatty", "chat", "hi"]) {
        Parsed::Refused(text, code) => {
            assert_eq!(code, 1);
            assert!(text.contains("chatty"), "{text}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn rejects_silent_which_the_flag_does_not_offer() {
    // A valid configuration spelling that is not one of the six `--log-level`
    // lists. A flag that accepts a level its own help does not name is a flag
    // nobody can reason about.
    match run(&["--log-level", "silent", "chat", "hi"]) {
        Parsed::Refused(_, code) => assert_eq!(code, 1),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn accepts_every_level_the_help_lists() {
    for level in darkwire::program::LOG_LEVELS {
        let (globals, _) = chat(&["--log-level", level, "chat", "hi"]);
        assert_eq!(globals.log_level, LogLevel::parse(level));
    }
}

#[test]
fn store_commands_carry_their_verb_and_id() {
    // Only `extension` still has three verbs: its approval is a record in a
    // store rather than a file an operator edits.
    match invocation(&["extension", "approve", "hello"]).command {
        Subcommand::Extension(action, id) => {
            assert_eq!(action, StoreAction::Approve);
            assert_eq!(id.as_deref(), Some("hello"));
        }
        other => panic!("expected extension, got {other:?}"),
    }
}

#[test]
fn a_bare_extension_command_lists() {
    match invocation(&["extension"]).command {
        Subcommand::Extension(StoreAction::List, None) => {}
        other => panic!("expected a listing, got {other:?}"),
    }
}

#[test]
fn a_bare_agent_command_lists() {
    match invocation(&["agent"]).command {
        Subcommand::Agent(AgentCommand::List) => {}
        other => panic!("expected agent list, got {other:?}"),
    }

    match invocation(&["agent", "list"]).command {
        Subcommand::Agent(AgentCommand::List) => {}
        other => panic!("expected agent list, got {other:?}"),
    }
}

#[test]
fn a_missing_required_argument_is_refused_rather_than_defaulted() {
    match run(&["agent", "install"]) {
        Parsed::Refused(_, code) => assert_eq!(code, 1),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// serve

fn serve(args: &[&str], env: &Env) -> darkwire::program::ServeArgs {
    let mut line = vec!["serve"];
    line.extend_from_slice(args);
    match run_in(&line, env).into_run() {
        Some(invocation) => match invocation.command {
            Subcommand::Serve(args) => *args,
            other => panic!("expected serve, got {other:?}"),
        },
        None => panic!("expected serve to parse"),
    }
}

#[test]
fn serve_drops_one_notch_under_its_own_default_for_verbose() {
    // `info` is already the floor here, because a server that says nothing
    // about the requests it is serving is a server nobody can debug.
    let verbose = invocation(&["--verbose", "serve"]);
    assert_eq!(verbose.globals.serve_log_level(), LogLevel::Debug);

    let plain = invocation(&["serve"]);
    assert_eq!(plain.globals.serve_log_level(), LogLevel::Info);
}

#[test]
fn serve_maps_every_flag() {
    let args = serve(
        &[
            "--host",
            "0.0.0.0",
            "--port",
            "8080",
            "--workspaces",
            "/tmp/ws",
            "--password",
            "hunter2",
            "--ui",
            "/tmp/dist",
        ],
        &Env::empty(),
    );

    assert_eq!(args.host.as_deref(), Some("0.0.0.0"));
    assert_eq!(args.port, Some(8080));
    assert_eq!(args.workspaces.as_deref(), Some("/tmp/ws"));
    assert_eq!(args.password.as_deref(), Some("hunter2"));
    assert_eq!(args.ui.as_deref(), Some("/tmp/dist"));
}

#[test]
fn serve_takes_the_short_forms_too() {
    let args = serve(
        &["-H", "127.0.0.1", "-P", "0", "-w", "/tmp/ws"],
        &Env::empty(),
    );
    assert_eq!(args.host.as_deref(), Some("127.0.0.1"));
    // Zero is a real request — the operating system picks — and has to survive
    // the "is it set" question that an `Option` answers and a sentinel cannot.
    assert_eq!(args.port, Some(0));
    assert_eq!(args.workspaces.as_deref(), Some("/tmp/ws"));
}

#[test]
fn serve_reads_the_password_from_the_environment() {
    let env: Env = [("DARKWIRE_PASSWORD", "from-the-env")]
        .into_iter()
        .collect();
    assert_eq!(
        serve(&[], &env).password.as_deref(),
        Some("from-the-env"),
        "the environment is the fallback for the flag"
    );
}

#[test]
fn serve_leaves_an_empty_environment_credential_unset() {
    let env: Env = [("DARKWIRE_PASSWORD", ""), ("DARKWIRE_USERNAME", "")]
        .into_iter()
        .collect();
    let args = serve(&[], &env);
    assert_eq!(args.password, None);
    assert_eq!(args.username, None);
}

#[test]
fn serve_passes_the_username_through_from_either_source() {
    let flag = serve(
        &["--password", "hunter2hunter2", "--username", "operator"],
        &Env::empty(),
    );
    assert_eq!(flag.username.as_deref(), Some("operator"));

    let env: Env = [
        ("DARKWIRE_PASSWORD", "hunter2hunter2"),
        ("DARKWIRE_USERNAME", "operator"),
    ]
    .into_iter()
    .collect();
    assert_eq!(serve(&[], &env).username.as_deref(), Some("operator"));
}

#[test]
fn serve_refuses_a_port_that_is_not_one_before_anything_binds() {
    match run(&["serve", "--port", "http"]) {
        Parsed::Refused(text, code) => {
            assert_eq!(code, 1);
            assert!(text.contains("http"), "{text}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn serve_refuses_a_port_above_the_range() {
    match run(&["serve", "--port", "70000"]) {
        Parsed::Refused(_, code) => assert_eq!(code, 1),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn serve_carries_the_two_flags_a_supervisor_needs() {
    let args = serve(
        &["--ready-file", "/tmp/ready.json", "--json"],
        &Env::empty(),
    );
    assert_eq!(args.ready_file.as_deref(), Some("/tmp/ready.json"));
    assert!(args.json);
}

#[test]
fn serve_is_listed_in_the_help_without_loading_the_server() {
    let (text, _) = printed(&["--help"]);
    assert!(text.contains("serve"));
}
