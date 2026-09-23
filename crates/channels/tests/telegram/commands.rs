//! The command table: one list, three readers, and nothing that sends.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use darkwire_channels::channel::ChannelControlFrame;
use darkwire_channels::telegram::api::TelegramMessageEntity;
use darkwire_channels::telegram::chats::{ChatState, RenderPrefs};
use darkwire_channels::telegram::commands::{
    CommandInput, CommandResult, bot_commands, help_text, parse_command, resolve_seq, run_command,
};
use darkwire_channels::telegram::console::{SkillSummary, TelegramConsole};
use darkwire_channels::telegram::menus::{CallbackPayload, CallbackStore};
use darkwire_core::clock::Clock;
use darkwire_core::messages::{AssistantOptions, assistant_message, text_part, user_message};
use darkwire_core::session_store::{AppendOptions, CreateSession};
use darkwire_protocol::tasks::{TaskItem, TaskStatus};
use darkwire_protocol::{ChatMessage, ContextResponse};
use indexmap::IndexMap;
use parking_lot::Mutex;

use super::console_double::FakeConsole;

const CHANNEL: &str = "telegram";
const CHAT: i64 = 4471;
const SESSION: &str = "telegram:4471";

#[derive(Default)]
struct Effects {
    control: Mutex<Vec<ChannelControlFrame>>,
    attached: Mutex<Vec<String>>,
    prefs: Mutex<Vec<(String, bool)>>,
    ids: AtomicUsize,
}

struct Harness {
    console: Arc<FakeConsole>,
    menus: CallbackStore,
    effects: Effects,
    chat: ChatState,
    is_admin: bool,
}

fn harness() -> Harness {
    let console = FakeConsole::new().expect("the stores open");
    let clock = console.clock();
    Harness {
        menus: CallbackStore::new(clock as Arc<dyn Clock>),
        console,
        effects: Effects::default(),
        chat: ChatState {
            session_key: SESSION.to_owned(),
            live_message_id: None,
            live_turn_id: None,
            last_edit_ms: 0,
            prefs: RenderPrefs::default(),
            pending: None,
        },
        is_admin: true,
    }
}

fn entity(line: &str) -> Vec<TelegramMessageEntity> {
    let word = line.split(' ').next().unwrap_or_default();
    vec![TelegramMessageEntity {
        kind: "bot_command".to_owned(),
        offset: 0,
        length: u32::try_from(word.encode_utf16().count()).unwrap_or(0),
    }]
}

async fn run(harness: &Harness, line: &str) -> CommandResult {
    let parsed = parse_command(line, &entity(line), Some("ghost_test_bot")).expect("a command");
    let control = |frame: ChannelControlFrame| harness.effects.control.lock().push(frame);
    let attach = |key: &str| harness.effects.attached.lock().push(key.to_owned());
    let set_pref = |field: &str, value: bool| {
        harness.effects.prefs.lock().push((field.to_owned(), value));
    };
    let new_id = || format!("id{}", harness.effects.ids.fetch_add(1, Ordering::SeqCst));

    let input = CommandInput {
        args: parsed.args,
        tail: parsed.tail,
        chat_id: CHAT,
        chat: harness.chat.clone(),
        console: harness.console.as_ref(),
        menus: &harness.menus,
        channel_id: CHANNEL.to_owned(),
        is_admin: harness.is_admin,
        control: &control,
        attach: &attach,
        set_pref: &set_pref,
        new_id: &new_id,
    };
    run_command(&parsed.name, &input).await
}

/// A session with `count` alternating user and assistant messages.
fn seed(harness: &Harness, key: &str, count: usize) {
    let store = harness.console.store();
    store
        .ensure_session(
            key,
            CreateSession {
                origin: Some(CHANNEL.to_owned()),
                ..CreateSession::default()
            },
        )
        .expect("the session is created");
    for index in 0..count {
        let message: ChatMessage = if index % 2 == 0 {
            ChatMessage::User(user_message(format!("question {index}")))
        } else {
            ChatMessage::Assistant(assistant_message(
                format!("answer {index}"),
                AssistantOptions::default(),
            ))
        };
        store
            .append(key, message, &AppendOptions { turn_id: None })
            .expect("it appends");
    }
}

// Parsing

#[test]
fn a_message_that_merely_mentions_a_command_is_not_one() {
    // Telegram already did the parse: matching on the character would run it.
    assert_eq!(parse_command("please /clear it", &[], None), None);
    assert_eq!(
        parse_command("/clear", &[], None),
        None,
        "no entity means no command"
    );
}

#[test]
fn a_command_is_split_into_a_name_arguments_and_a_tail() {
    let parsed =
        parse_command("/rename my long title", &entity("/rename"), None).expect("a command");

    assert_eq!(parsed.name, "rename");
    assert_eq!(parsed.args, vec!["my", "long", "title"]);
    // Untouched, which is what `/rename` wants.
    assert_eq!(parsed.tail, "my long title");
}

#[test]
fn a_command_name_is_lowercased() {
    let parsed = parse_command("/HELP", &entity("/HELP"), None).expect("a command");

    assert_eq!(parsed.name, "help");
}

#[test]
fn a_command_addressed_to_this_bot_is_ours() {
    let parsed = parse_command(
        "/sessions@ghost_test_bot 5",
        &entity("/sessions@ghost_test_bot"),
        Some("ghost_test_bot"),
    )
    .expect("a command");

    assert_eq!(parsed.name, "sessions");
    assert_eq!(parsed.args, vec!["5"]);
}

#[test]
fn a_command_addressed_to_another_bot_in_the_group_is_not_ours() {
    assert_eq!(
        parse_command(
            "/sessions@other_bot",
            &entity("/sessions@other_bot"),
            Some("ghost_test_bot")
        ),
        None
    );
}

#[test]
fn the_address_is_matched_case_insensitively() {
    assert!(
        parse_command(
            "/help@Ghost_Test_Bot",
            &entity("/help@Ghost_Test_Bot"),
            Some("ghost_test_bot")
        )
        .is_some()
    );
}

#[test]
fn an_addressed_command_is_ours_when_we_do_not_know_our_own_name_yet() {
    assert!(parse_command("/help@anyone", &entity("/help@anyone"), None).is_some());
}

// The table's three readers

#[test]
fn the_registered_menu_is_the_table() {
    let registered = bot_commands();

    assert!(registered.iter().any(|command| command.command == "help"));
    assert!(registered.iter().any(|command| command.command == "start"));
    // Telegram's own spelling: lowercase, no slash, no spaces.
    for command in &registered {
        assert_eq!(command.command, command.command.to_lowercase());
        assert!(!command.command.contains('/'));
        assert!(!command.command.contains(' '));
        // `setMyCommands` refuses anything longer.
        assert!(command.description.len() < 256, "{}", command.command);
    }
}

#[test]
fn an_alias_is_dispatchable_but_not_registered() {
    assert!(
        !bot_commands()
            .iter()
            .any(|command| command.command == "quit")
    );
}

#[test]
fn help_is_measured_from_the_table_rather_than_typed_out() {
    let text = help_text(true);

    for command in bot_commands() {
        assert!(
            text.contains(&format!("/{}", command.command)),
            "/help omits /{}",
            command.command
        );
    }
}

#[test]
fn help_hides_the_admin_commands_from_everybody_else() {
    let plain = help_text(false);
    let admin = help_text(true);

    assert!(!plain.contains("/model"));
    assert!(admin.contains("/model"));
}

#[tokio::test]
async fn an_unknown_command_says_so_rather_than_being_ignored() {
    // A bot that silently drops a typo looks broken.
    let harness = harness();

    let result = run(&harness, "/nope").await;

    assert!(
        result.text.contains("No command `/nope`"),
        "{}",
        result.text
    );
    assert!(result.text.contains("/help"), "{}", result.text);
}

#[tokio::test]
async fn an_alias_dispatches_to_its_command() {
    let harness = harness();

    let result = run(&harness, "/quit").await;

    assert!(result.text.contains("Detached"), "{}", result.text);
}

// Reading the conversation

#[tokio::test]
async fn clear_forgets_the_history_and_keeps_the_session() {
    let harness = harness();
    seed(&harness, SESSION, 4);

    let result = run(&harness, "/clear").await;

    assert_eq!(result.text, "History cleared.");
    assert_eq!(
        harness
            .console
            .store()
            .message_count(SESSION)
            .expect("count"),
        0
    );
    assert!(
        harness
            .console
            .store()
            .get_session(SESSION)
            .expect("get")
            .is_some()
    );
}

#[tokio::test]
async fn session_shows_where_this_chat_is() {
    let harness = harness();
    seed(&harness, SESSION, 2);

    let result = run(&harness, "/session").await;

    assert!(result.text.contains(SESSION), "{}", result.text);
    assert!(result.text.contains("2 messages"), "{}", result.text);
    assert!(result.text.contains("workspace default"), "{}", result.text);
}

#[tokio::test]
async fn session_says_new_for_a_conversation_with_no_row_yet() {
    let harness = harness();

    let result = run(&harness, "/session").await;

    assert!(result.text.contains("(new)"), "{}", result.text);
}

#[tokio::test]
async fn session_attaches_to_a_key_this_channel_owns() {
    let harness = harness();

    let result = run(&harness, "/session telegram:4471:abc").await;

    assert!(result.text.contains("Attached"), "{}", result.text);
    assert_eq!(*harness.effects.attached.lock(), vec!["telegram:4471:abc"]);
}

#[tokio::test]
async fn session_refuses_a_key_from_another_channel() {
    // The manager would happily namespace `web-abc` into `telegram:web-abc` — a
    // real, empty conversation that nothing explains.
    let harness = harness();

    let result = run(&harness, "/session web-abc").await;

    assert!(result.text.contains("another channel"), "{}", result.text);
    assert!(harness.effects.attached.lock().is_empty());
}

// Moving between conversations

#[tokio::test]
async fn exit_detaches_to_the_chats_own_default() {
    // Not "exit": there is no process to leave.
    let harness = harness();
    harness.menus.put(
        CHAT,
        CallbackPayload::Session {
            session_key: "telegram:4471:abc".to_owned(),
        },
        None,
    );

    let result = run(&harness, "/exit").await;

    assert!(result.text.contains("Detached"), "{}", result.text);
    assert_eq!(*harness.effects.attached.lock(), vec!["telegram:4471"]);
    // The old menu's buttons go with it.
    assert!(harness.menus.is_empty());
}

#[tokio::test]
async fn new_starts_a_fresh_session_under_this_chat() {
    let harness = harness();

    let result = run(&harness, "/new").await;

    assert_eq!(result.text, "Started a new session.");
    let attached = harness.effects.attached.lock().clone();
    assert_eq!(attached, vec!["telegram:4471:id0"]);
    let created = harness
        .console
        .store()
        .get_session("telegram:4471:id0")
        .expect("get")
        .expect("created");
    assert_eq!(created.origin, CHANNEL);
}

#[tokio::test]
async fn new_takes_a_title() {
    let harness = harness();

    let result = run(&harness, "/new the release notes").await;

    assert!(result.text.contains("the release notes"), "{}", result.text);
    assert_eq!(
        harness
            .console
            .store()
            .get_session("telegram:4471:id0")
            .expect("get")
            .expect("created")
            .title,
        "the release notes"
    );
}

#[tokio::test]
async fn session_offers_a_picker_marked_where_you_are() {
    let harness = harness();
    seed(&harness, SESSION, 2);
    seed(&harness, "telegram:4471:abc", 2);

    let result = run(&harness, "/session").await;

    let keyboard = result.keyboard.expect("a picker");
    let labels: Vec<&str> = keyboard
        .inline_keyboard
        .iter()
        .flatten()
        .map(|button| button.text.as_str())
        .collect();
    // Two buttons a row now: the name attaches, the bin asks.
    assert_eq!(labels.len(), 4);
    assert!(
        labels.iter().any(|label| label.starts_with("• ")),
        "{labels:?}"
    );
}

#[tokio::test]
async fn bare_session_says_where_you_are_when_there_is_nowhere_else() {
    // The detail `/session` has always shown, and no picker under it: a menu
    // whose only row is the conversation you are already in.
    let harness = harness();

    let result = run(&harness, "/session").await;

    assert!(result.text.contains("workspace default"), "{}", result.text);
    assert!(result.keyboard.is_none());
}

#[tokio::test]
async fn rename_retitles_the_session_it_is_in() {
    let harness = harness();

    let result = run(&harness, "/rename the release notes").await;

    assert!(result.text.contains("the release notes"), "{}", result.text);
    assert_eq!(
        harness
            .console
            .store()
            .get_session(SESSION)
            .expect("get")
            .expect("created")
            .title,
        "the release notes"
    );
}

#[tokio::test]
async fn rename_says_how_when_given_nothing() {
    let harness = harness();

    assert_eq!(
        run(&harness, "/rename").await.text,
        "Usage: /rename <title>"
    );
}

#[tokio::test]
async fn branch_forks_at_a_message_and_continues_there() {
    let harness = harness();
    seed(&harness, SESSION, 4);

    let result = run(&harness, "/branch 3").await;

    assert!(result.text.contains("Branched at `3`"), "{}", result.text);
    assert_eq!(*harness.effects.attached.lock(), vec!["telegram:4471:id0"]);
    assert_eq!(
        harness
            .console
            .store()
            .message_count("telegram:4471:id0")
            .expect("count"),
        3
    );
}

// Message references

#[tokio::test]
async fn a_negative_reference_counts_back_over_what_you_said() {
    // `-1` is the last thing you said, not the last row: nobody counts
    // backwards over a tool result.
    let harness = harness();
    seed(&harness, SESSION, 6);

    assert_eq!(
        resolve_seq(harness.console.store(), SESSION, Some("-1")).expect("resolved"),
        5
    );
    assert_eq!(
        resolve_seq(harness.console.store(), SESSION, Some("-2")).expect("resolved"),
        3
    );
}

#[tokio::test]
async fn an_absent_reference_means_your_last_message() {
    let harness = harness();
    seed(&harness, SESSION, 6);

    assert_eq!(
        resolve_seq(harness.console.store(), SESSION, None).expect("resolved"),
        5
    );
}

#[tokio::test]
async fn a_positive_reference_is_the_seq_it_names() {
    let harness = harness();
    seed(&harness, SESSION, 6);

    assert_eq!(
        resolve_seq(harness.console.store(), SESSION, Some("2")).expect("resolved"),
        2
    );
}

#[tokio::test]
async fn a_reference_to_nothing_says_so() {
    let harness = harness();
    seed(&harness, SESSION, 2);

    let error =
        resolve_seq(harness.console.store(), SESSION, Some("99")).expect_err("no such message");
    assert!(error.message.contains("No message 99"), "{}", error.message);
}

#[tokio::test]
async fn a_reference_that_is_not_a_number_says_what_one_looks_like() {
    let harness = harness();

    for bad in ["abc", "0", "1.5"] {
        let error = resolve_seq(harness.console.store(), SESSION, Some(bad))
            .err()
            .unwrap_or_else(|| panic!("{bad} is not a reference"));
        assert!(
            error.message.contains("Not a message reference"),
            "{}",
            error.message
        );
    }
}

#[tokio::test]
async fn counting_back_past_what_you_said_says_how_much_there_was() {
    let harness = harness();
    seed(&harness, SESSION, 4);

    let error =
        resolve_seq(harness.console.store(), SESSION, Some("-9")).expect_err("too far back");
    assert!(
        error.message.contains("Only 2 of your messages"),
        "{}",
        error.message
    );

    let error = resolve_seq(
        harness.console.store(),
        "telegram:4471:untouched",
        Some("-1"),
    )
    .expect_err("nothing said");
    assert!(
        error.message.contains("have not said anything"),
        "{}",
        error.message
    );
}

// Frames, which go out the same door a browser uses

#[tokio::test]
async fn edit_replaces_a_message_and_re_runs_from_it_in_one_frame() {
    // Truncating and re-running are a single intent, and splitting them leaves
    // a window for another client's queued message.
    let harness = harness();
    seed(&harness, SESSION, 4);

    let result = run(&harness, "/edit actually, this").await;

    assert!(
        result.text.contains("Re-running from `3`"),
        "{}",
        result.text
    );
    let frames = harness.effects.control.lock().clone();
    let ChannelControlFrame::Edit(edit) = &frames[0] else {
        panic!("an edit frame");
    };
    assert_eq!(edit.seq, 3);
    assert_eq!(edit.content, "actually, this");
    assert_eq!(edit.session_key, SESSION);
}

#[tokio::test]
async fn edit_says_how_when_it_is_missing_half_of_what_it_needs() {
    let harness = harness();
    seed(&harness, SESSION, 4);

    assert_eq!(run(&harness, "/edit").await.text, "Usage: /edit <text>");
    assert_eq!(run(&harness, "/edit").await.text, "Usage: /edit <text>");
    assert!(harness.effects.control.lock().is_empty());
}

#[tokio::test]
async fn regenerate_runs_a_turn_again() {
    let harness = harness();
    seed(&harness, SESSION, 4);

    let result = run(&harness, "/regenerate 3").await;

    assert!(result.text.contains("Re-running `3`"), "{}", result.text);
    let frames = harness.effects.control.lock().clone();
    let ChannelControlFrame::Regenerate(body) = &frames[0] else {
        panic!("a regenerate frame");
    };
    assert_eq!(body.seq, Some(3));
}

#[tokio::test]
async fn stop_sends_the_frame_the_browsers_stop_button_sends() {
    let harness = harness();

    let result = run(&harness, "/stop").await;

    assert_eq!(result.text, "Stopping.");
    let frames = harness.effects.control.lock().clone();
    let ChannelControlFrame::StopTurn(body) = &frames[0] else {
        panic!("a stop frame");
    };
    assert_eq!(body.session_key, SESSION);
}

// Reads the hub has no frame for

#[tokio::test]
async fn context_reports_the_breakdown_not_the_transcript() {
    // A context report also carries the whole system prompt, every tool
    // definition and every stored message.
    let harness = harness();
    let mut breakdown = IndexMap::new();
    breakdown.insert("system".to_owned(), 400.0);
    harness.console.set_context(Some(ContextResponse {
        session_key: SESSION.to_owned(),
        system_prompt: "SECRET SYSTEM PROMPT".to_owned(),
        runtime_block: String::new(),
        tools: Vec::new(),
        messages: Vec::new(),
        estimated_tokens: 500,
        context_window_tokens: 1000,
        breakdown,
        agent_id: Some("researcher".to_owned()),
        requested_agent_id: None,
    }));

    let result = run(&harness, "/context").await;

    assert!(
        result.text.contains("500 of 1000 tokens (50%)"),
        "{}",
        result.text
    );
    assert!(result.text.contains("researcher"), "{}", result.text);
    assert!(result.text.contains("system: 400"), "{}", result.text);
    assert!(
        !result.text.contains("SECRET SYSTEM PROMPT"),
        "{}",
        result.text
    );
}

#[tokio::test]
async fn context_says_so_when_there_is_nothing_to_measure() {
    let harness = harness();

    assert_eq!(
        run(&harness, "/context").await.text,
        "Nothing to measure yet."
    );
}

#[tokio::test]
async fn memory_explains_the_tool_it_needs_when_the_agent_lacks_it() {
    let harness = harness();
    harness
        .console
        .set_memory(darkwire_channels::telegram::console::MemoryState {
            granted: false,
            count: 0,
            tokens: 0,
        });

    let result = run(&harness, "/memory").await;

    assert!(
        result.text.contains("does not have the `memory` tool"),
        "{}",
        result.text
    );
}

#[tokio::test]
async fn memory_reports_what_is_remembered_and_what_it_costs() {
    let harness = harness();
    harness
        .console
        .set_memory(darkwire_channels::telegram::console::MemoryState {
            granted: true,
            count: 7,
            tokens: 210,
        });

    let result = run(&harness, "/memory").await;

    assert!(result.text.contains("7 memories"), "{}", result.text);
    assert!(result.text.contains("210 tokens"), "{}", result.text);
}

#[tokio::test]
async fn memory_says_so_when_nothing_is_remembered_yet() {
    let harness = harness();

    let result = run(&harness, "/memory").await;

    assert!(
        result.text.contains("Nothing remembered yet"),
        "{}",
        result.text
    );
}

#[tokio::test]
async fn skills_marks_a_sheet_another_agent_owns_rather_than_hiding_it() {
    // A sheet missing from the list would be the harder thing to explain to
    // whoever just wrote it.
    let harness = harness();
    harness.console.set_skills(
        true,
        vec![
            SkillSummary {
                name: "release".to_owned(),
                description: "how to cut one".to_owned(),
                mine: true,
            },
            SkillSummary {
                name: "triage".to_owned(),
                description: "for the other one".to_owned(),
                mine: false,
            },
        ],
    );

    let result = run(&harness, "/skills").await;

    assert!(
        result.text.contains("`release`: how to cut one"),
        "{}",
        result.text
    );
    assert!(result.text.contains("_(other agents)_"), "{}", result.text);
}

#[tokio::test]
async fn skills_explains_the_tool_it_needs_and_the_empty_case() {
    let harness = harness();
    harness.console.set_skills(false, Vec::new());
    assert!(
        run(&harness, "/skills")
            .await
            .text
            .contains("does not have the `skill` tool")
    );

    harness.console.set_skills(true, Vec::new());
    assert!(
        run(&harness, "/skills")
            .await
            .text
            .contains("No skills here yet")
    );
}

// Rendering preferences

// Agents, models and workspaces

#[tokio::test]
async fn agent_offers_a_picker_when_nothing_is_named() {
    let harness = harness();

    let result = run(&harness, "/agent").await;

    assert_eq!(result.text, "Which agent?");
    let keyboard = result.keyboard.expect("a picker");
    assert_eq!(keyboard.inline_keyboard.len(), 2);
    assert!(keyboard.inline_keyboard[0][0].text.contains("Default"));
}

#[tokio::test]
async fn agent_binds_this_session_to_the_one_it_is_given() {
    let harness = harness();

    let result = run(&harness, "/agent researcher").await;

    assert!(result.text.contains("researcher"), "{}", result.text);
    assert_eq!(
        harness
            .console
            .store()
            .get_session(SESSION)
            .expect("get")
            .expect("created")
            .agent_id
            .as_deref(),
        Some("researcher")
    );
}

#[tokio::test]
async fn agent_refuses_one_that_does_not_exist() {
    let harness = harness();

    let result = run(&harness, "/agent nobody").await;

    assert!(result.text.contains("No agent `nobody`"), "{}", result.text);
}

#[tokio::test]
async fn model_is_for_an_administrator_because_it_moves_the_whole_process() {
    // It moves the browser and every other conversation with it.
    let mut harness = harness();
    harness.is_admin = false;

    let result = run(&harness, "/model gpt-4o").await;

    assert!(result.text.contains("administrator"), "{}", result.text);
    assert!(harness.console.models_set().is_empty());
}

#[tokio::test]
async fn model_moves_the_process_when_an_administrator_asks() {
    let harness = harness();

    let result = run(&harness, "/model o3").await;

    assert!(result.text.contains("Now running `o3`"), "{}", result.text);
    assert_eq!(harness.console.models_set(), vec!["o3"]);
}

#[tokio::test]
async fn model_offers_the_catalogue_when_nothing_is_named() {
    let harness = harness();

    let result = run(&harness, "/model").await;

    assert_eq!(result.text, "Which model?");
    let keyboard = result.keyboard.expect("a picker");
    assert_eq!(keyboard.inline_keyboard.len(), 2);
}

#[tokio::test]
async fn bare_workspace_lists_them_and_offers_a_picker_over_the_listing() {
    // One name for one subject, as the terminal spells it. `/workspaces` was
    // the listing and `/workspace` the picker, which is one question asked
    // twice; the listing is now the text the picker is posted with.
    let harness = harness();

    let result = run(&harness, "/workspace").await;

    assert!(result.text.contains("• `default`"), "{}", result.text);
    assert!(result.keyboard.is_some());
}

#[tokio::test]
async fn a_word_that_is_not_a_verb_is_read_as_a_workspace_id() {
    let harness = harness();

    let result = run(&harness, "/workspace default").await;

    assert!(
        result.text.contains("now lives in `default`"),
        "{}",
        result.text
    );
    assert_eq!(
        harness
            .console
            .store()
            .get_session(SESSION)
            .expect("get")
            .expect("created")
            .workspace_id,
        "default"
    );
}

#[tokio::test]
async fn switching_to_a_workspace_that_does_not_exist_says_so() {
    let harness = harness();

    let result = run(&harness, "/workspace nowhere").await;

    assert!(
        result.text.contains("No workspace `nowhere`"),
        "{}",
        result.text
    );
}

// The two listings

#[tokio::test]
async fn start_opens_with_a_sentence_and_then_the_same_list_help_shows() {
    // Two lists disagree eventually.
    let harness = harness();

    let start = run(&harness, "/start").await;
    let help = run(&harness, "/help").await;

    assert!(
        start.text.contains("This chat is a DarkWire session"),
        "{}",
        start.text
    );
    assert!(
        start.text.ends_with(&help.text),
        "start must end with /help"
    );
}

#[tokio::test]
async fn nothing_a_command_returns_has_been_sent() {
    // A command returns text and the channel does the sending, so a command is
    // a pure-ish function over a store.
    let harness = harness();

    let result = run(&harness, "/help").await;

    assert!(!result.text.is_empty());
    assert!(result.keyboard.is_none());
    assert_eq!(text_part("x"), text_part("x"));
}

// /tasks

#[tokio::test]
async fn tasks_says_there_is_no_plan_when_nothing_has_written_one() {
    let harness = harness();
    let result = run(&harness, "/tasks").await;
    assert!(result.text.contains("No plan"), "{}", result.text);
}

#[tokio::test]
async fn tasks_prints_the_plan_in_the_markers_the_prompt_uses() {
    let harness = harness();
    harness
        .console
        .store()
        .set_tasks(
            SESSION,
            &[
                TaskItem {
                    text: "Inspect auth".to_owned(),
                    status: TaskStatus::Done,
                },
                TaskItem {
                    text: "Add tests".to_owned(),
                    status: TaskStatus::Todo,
                },
            ],
        )
        .expect("the store writes");

    let result = run(&harness, "/tasks").await;
    assert_eq!(result.text, "[x] Inspect auth\n[ ] Add tests");

    // One button, and it empties the list. Dropping a single task was the
    // wrong verb: the `todo` tool replaces the whole plan on its next
    // planning step, so one removed by hand is back a moment later.
    let keyboard = result.keyboard.expect("a button");
    let labels: Vec<&str> = keyboard
        .inline_keyboard
        .iter()
        .flatten()
        .map(|button| button.text.as_str())
        .collect();
    assert_eq!(labels, ["Clear the list"]);
}

/// Reading a plan reaches no further than the chat's own session, so it is not
/// one of the admin-gated verbs.
#[tokio::test]
async fn tasks_is_not_admin_gated() {
    let mut harness = harness();
    harness.is_admin = false;
    let result = run(&harness, "/tasks").await;
    assert!(result.text.contains("No plan"), "{}", result.text);
}

/// One table, three readers: `setMyCommands` registers what `/help` renders and
/// what dispatch knows, so a row missing here is a command Telegram offers and
/// nothing answers.
#[test]
fn tasks_is_registered_with_telegram_and_listed_in_help() {
    assert!(
        bot_commands()
            .iter()
            .any(|command| command.command == "tasks")
    );
    assert!(help_text(false).contains("/tasks"));
}
