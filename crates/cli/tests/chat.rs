//! One turn, and the three shapes that drive it.
//!
//! Every case here builds a real loop over a scripted provider, so nothing
//! reaches a network — and every one of them exercises the same [`run_turn`]
//! the prompt, the pipe and the one-shot all share, which is the property the
//! module exists to hold.

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

use std::sync::Arc;

use std::future::Future;
use std::pin::Pin;

use darkwire::chat::{
    ChunkSink, FoldDefaults, Frame, FrameEvent, RunTurnDeps, SIGINT_EXIT_CODE, Surface,
    TurnOutcome, TurnSink, Typed, chunks, drive_prompt, handle_key, run_turn,
};
use darkwire::commands::command_rows;
use darkwire::i18n::Env;
use darkwire::i18n::Translations;
use darkwire::pickers::palette::{PaletteRow, command_items};
use darkwire::pickers::{NoMenu, PickerMenu};
use darkwire::program::{ChatArgs, Globals};
use darkwire::render::{TurnRenderer, TurnRendererOptions};
use darkwire_agent::testkit::{ScriptedProvider, ScriptedTurn, TokioClock};
use darkwire_agent::{AgentLoop, AgentLoopOptions, SteeringQueue};
use darkwire_core::messages::Content;
use darkwire_core::{Database, ErrorKind, SessionStore};
use darkwire_protocol::StopReason;
use darkwire_protocol::config::ReasoningDisplay;
use darkwire_protocol::tasks::TaskStatus;
use darkwire_security::jail::{JailOptions, WorkspaceJail, single_jail};
use darkwire_tools::{ToolRegistry, ToolScope};
use darkwire_tui::{Key, parse_key};
use indexmap::IndexMap;
use tokio_util::sync::CancellationToken;

/// A collector the renderer writes into, which a case then reads back.
#[derive(Clone, Default)]
struct Sink(Arc<std::sync::Mutex<String>>);

impl Sink {
    fn text(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(buf));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A loop over a scripted provider, a temporary workspace and a fresh store.
struct Harness {
    agent_loop: AgentLoop,
    _home: tempfile::TempDir,
}

fn harness(turns: Vec<ScriptedTurn>) -> Harness {
    let home = tempfile::tempdir().unwrap();
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let database = Database::in_memory().unwrap();
    let clock = TokioClock::new();
    let ids = std::sync::atomic::AtomicU64::new(0);
    let store = Arc::new(
        SessionStore::new(
            database,
            Arc::clone(&clock) as _,
            Box::new(move || {
                format!(
                    "id-{}",
                    ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                )
            }),
        )
        .unwrap(),
    );
    let jail = Arc::new(WorkspaceJail::new(JailOptions::new(&workspace)).unwrap());
    let registry = Arc::new(ToolRegistry::default());
    let scope: Arc<dyn ToolScope> = registry.select(IndexMap::new());

    let agent_loop = AgentLoop::new(AgentLoopOptions {
        model: Some("test-model".to_owned()),
        clock: Arc::clone(&clock) as _,
        steering: Arc::new(SteeringQueue::new()),
        time_zone: Some(Arc::new(|| "UTC".to_owned())),
        ..AgentLoopOptions::new(
            ScriptedProvider::new(turns),
            scope,
            store,
            Arc::new(single_jail(jail)),
        )
    })
    .unwrap();

    Harness {
        agent_loop,
        _home: home,
    }
}

fn renderer(sink: &Sink) -> TurnRenderer {
    TurnRenderer::new(TurnRendererOptions {
        // Never coloured in a test: an assertion against escape sequences is an
        // assertion about a palette rather than about what was said.
        colors: Some(false),
        t: Translations::default(),
        ..TurnRendererOptions::new(Box::new(sink.clone()))
    })
}

fn deps<'a>(
    harness: &'a Harness,
    sink: TurnSink<'a>,
    token: &CancellationToken,
) -> RunTurnDeps<'a> {
    RunTurnDeps {
        agent_loop: &harness.agent_loop,
        sink,
        session_key: "cli:default".to_owned(),
        workspace_id: None,
        agent_id: None,
        token: token.clone(),
    }
}

#[tokio::test]
async fn streams_the_answer_and_reports_how_the_turn_ended() {
    let harness = harness(vec![ScriptedTurn::text("hello from the model")]);
    let sink = Sink::default();
    let mut rendered = renderer(&sink);
    let token = CancellationToken::new();

    let outcome = run_turn(
        deps(&harness, TurnSink::Rendered(&mut rendered), &token),
        Content::Text("hi".to_owned()),
    )
    .await
    .unwrap();

    assert_eq!(outcome.stop_reason, StopReason::Complete);
    assert!(!outcome.aborted);
    assert!(!outcome.failed);
    assert!(
        sink.text().contains("hello from the model"),
        "{}",
        sink.text()
    );
}

#[tokio::test]
async fn does_not_reprint_the_answer_at_the_end() {
    // The deltas already streamed it, and a driver that also printed the result
    // would show every answer twice.
    let harness = harness(vec![ScriptedTurn::text("once")]);
    let sink = Sink::default();
    let mut rendered = renderer(&sink);
    let token = CancellationToken::new();

    run_turn(
        deps(&harness, TurnSink::Rendered(&mut rendered), &token),
        Content::Text("hi".to_owned()),
    )
    .await
    .unwrap();

    assert_eq!(sink.text().matches("once").count(), 1, "{}", sink.text());
}

#[tokio::test]
async fn json_writes_one_object_per_event_and_renders_no_prose() {
    let harness = harness(vec![ScriptedTurn::text("answer")]);
    let mut buffer: Vec<u8> = Vec::new();
    let token = CancellationToken::new();

    run_turn(
        deps(&harness, TurnSink::Json(&mut buffer), &token),
        Content::Text("hi".to_owned()),
    )
    .await
    .unwrap();

    let text = String::from_utf8(buffer).unwrap();
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    assert!(lines.len() >= 2, "{text}");
    for line in &lines {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(value["type"].is_string(), "{line}");
    }
    // The first frame is the turn opening and the last is it closing, which is
    // the pair a script brackets its own work with.
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["type"],
        "turn.start"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[lines.len() - 1]).unwrap()["type"],
        "turn.end"
    );
}

#[tokio::test]
async fn a_failing_turn_is_reported_as_failed_rather_than_as_a_refusal() {
    // The terminal has already printed the error; unwinding on top of it would
    // print it twice and lose the exit code that says which kind it was.
    let harness = harness(vec![ScriptedTurn::failing(
        ErrorKind::Provider,
        "the endpoint refused",
    )]);
    let sink = Sink::default();
    let mut rendered = renderer(&sink);
    let token = CancellationToken::new();

    let outcome = run_turn(
        deps(&harness, TurnSink::Rendered(&mut rendered), &token),
        Content::Text("hi".to_owned()),
    )
    .await
    .unwrap();

    assert!(outcome.failed);
    assert!(!outcome.aborted);
    assert!(
        sink.text().contains("the endpoint refused"),
        "{}",
        sink.text()
    );
}

#[tokio::test]
async fn a_cancelled_token_stops_the_turn_and_reports_an_abort() {
    // One token per turn, threaded from the interrupt through the loop, the
    // provider request and into a child process.
    let harness = harness(vec![ScriptedTurn::text("never arrives").after(10_000)]);
    let sink = Sink::default();
    let mut rendered = renderer(&sink);
    let token = CancellationToken::new();

    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        cancel.cancel();
    });

    let outcome = run_turn(
        deps(&harness, TurnSink::Rendered(&mut rendered), &token),
        Content::Text("hi".to_owned()),
    )
    .await
    .unwrap();

    assert!(outcome.aborted);
    assert_eq!(outcome.stop_reason, StopReason::Aborted);
    // An interruption is not a failure: a script tells them apart by the exit
    // code, and a turn the operator stopped did not go wrong.
    assert!(!outcome.failed);
}

#[test]
fn the_interrupt_code_is_the_conventional_one() {
    // 128 + SIGINT, so a shell script branching on "the user pressed Ctrl-C"
    // reads the same number from this program as from `cat`.
    assert_eq!(SIGINT_EXIT_CODE, 130);
}

#[tokio::test]
async fn a_session_created_by_a_turn_lands_in_the_workspace_it_was_given() {
    // Never moves one that already exists — the loop reads the stored row and
    // ignores this — so the assertion is about creation.
    let harness = harness(vec![ScriptedTurn::text("ok")]);
    let sink = Sink::default();
    let mut rendered = renderer(&sink);
    let token = CancellationToken::new();

    run_turn(
        RunTurnDeps {
            workspace_id: Some("default".to_owned()),
            ..deps(&harness, TurnSink::Rendered(&mut rendered), &token)
        },
        Content::Text("hi".to_owned()),
    )
    .await
    .unwrap();

    assert!(sink.text().contains("ok"), "{}", sink.text());
}

// ------------------------------------------------------------ the prompt

/// A surface that answers lines from a list and records what was drawn.
///
/// The whole point of [`Surface`] being a trait: the loop's decisions — what a
/// slash command leaves to do, what happens to a refused turn, when the
/// context is re-measured — are asserted here without a terminal, a keyboard
/// or a signal.
struct Scripted {
    lines: std::collections::VecDeque<String>,
    sink: ChunkSink,
    chunks: tokio::sync::mpsc::UnboundedReceiver<FrameEvent>,
    menu: NoMenu,
    /// Every line the loop echoed back, in order.
    echoed: Vec<String>,
    /// Everything written through the sink, drained at each refresh.
    drawn: String,
    /// How many times the prompt was redrawn.
    refreshes: usize,
    /// How many turns the surface was asked to run.
    turns: usize,
    closed: bool,
}

impl Scripted {
    fn new(lines: &[&str]) -> Scripted {
        let (sink, chunks) = chunks();
        Scripted {
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            sink,
            chunks,
            menu: NoMenu,
            echoed: Vec::new(),
            drawn: String::new(),
            refreshes: 0,
            turns: 0,
            closed: false,
        }
    }

    fn drain(&mut self) {
        while let Ok(event) = self.chunks.try_recv() {
            self.drawn.push_str(event.as_text());
        }
    }
}

impl Surface for Scripted {
    fn menu(&self) -> &dyn PickerMenu {
        &self.menu
    }

    fn sink(&self) -> ChunkSink {
        self.sink.clone()
    }

    fn next_line(&mut self) -> Pin<Box<dyn Future<Output = Option<String>> + '_>> {
        Box::pin(async move { self.lines.pop_front() })
    }

    fn run<'a>(
        &'a mut self,
        token: &'a CancellationToken,
        body: Pin<Box<dyn Future<Output = darkwire_core::Result<TurnOutcome>> + 'a>>,
    ) -> Pin<Box<dyn Future<Output = darkwire_core::Result<TurnOutcome>> + 'a>> {
        let _ = token;
        self.turns += 1;
        body
    }

    fn echo<'a>(&'a mut self, content: &'a str) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        self.echoed.push(content.to_owned());
        Box::pin(std::future::ready(()))
    }

    fn refresh<'a>(
        &'a mut self,
        view: &'a darkwire::header::HeaderView,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        let _ = view;
        self.refreshes += 1;
        self.drain();
        Box::pin(std::future::ready(()))
    }

    fn close(&mut self) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        self.closed = true;
        self.drain();
        Box::pin(std::future::ready(()))
    }
}

/// A session over a temporary home that names no provider.
///
/// Nothing here reaches a network, and that is a property of the fixture
/// rather than of the cases: an install with no provider builds a runtime with
/// no loop, which is exactly the state a fresh machine is in.
fn session(home: &tempfile::TempDir) -> darkwire::chat::ChatSession {
    darkwire::chat::open(
        &Globals {
            home: Some(home.path().display().to_string()),
            color: Some(false),
            ..Globals::default()
        },
        // `--home` moves DarkWire's state and nothing else, so the workspaces
        // are named too. Without this they resolve under the home directory of
        // whoever is running the tests.
        &ChatArgs {
            workspaces: Some(home.path().join("workspaces").display().to_string()),
            ..ChatArgs::default()
        },
        &Env::empty(),
    )
    .unwrap()
}

#[tokio::test]
async fn an_empty_line_is_not_a_turn() {
    let home = tempfile::tempdir().unwrap();
    let mut chat = session(&home);
    let mut surface = Scripted::new(["", "   ", "/exit"].as_slice());

    let code = drive_prompt(&mut chat, &ChatArgs::default(), &mut surface)
        .await
        .unwrap();

    assert_eq!(code, 0);
    // Only `/exit` was echoed: a blank line never became anything.
    assert_eq!(surface.echoed, vec!["/exit".to_owned()]);
    assert_eq!(surface.turns, 0);
}

#[tokio::test]
async fn exit_leaves_and_puts_the_terminal_back() {
    let home = tempfile::tempdir().unwrap();
    let mut chat = session(&home);
    let mut surface = Scripted::new(["/exit"].as_slice());

    let code = drive_prompt(&mut chat, &ChatArgs::default(), &mut surface)
        .await
        .unwrap();

    assert_eq!(code, 0);
    assert!(surface.closed, "the surface was left open");
}

#[tokio::test]
async fn a_closed_input_leaves_the_same_way_a_command_does() {
    // Ctrl-D at the prompt and the end of a piped script are the same event,
    // and neither is a failure.
    let home = tempfile::tempdir().unwrap();
    let mut chat = session(&home);
    let mut surface = Scripted::new([].as_slice());

    let code = drive_prompt(&mut chat, &ChatArgs::default(), &mut surface)
        .await
        .unwrap();

    assert_eq!(code, 0);
    assert!(surface.closed);
}

#[tokio::test]
async fn a_slash_command_runs_no_turn_and_redraws_the_prompt() {
    let home = tempfile::tempdir().unwrap();
    let mut chat = session(&home);
    let mut surface = Scripted::new(["/help", "/exit"].as_slice());

    drive_prompt(&mut chat, &ChatArgs::default(), &mut surface)
        .await
        .unwrap();

    assert_eq!(surface.turns, 0);
    // `/help` renders through the same sink a turn does, which is what lets a
    // pipe and a frame show it without either knowing about the other.
    assert!(surface.drawn.contains("/help"), "{}", surface.drawn);
    assert_eq!(surface.refreshes, 1);
}

#[tokio::test]
async fn an_unknown_command_is_reported_and_the_prompt_comes_back() {
    // Never a failed run: a mistyped command is a typo, and throwing away the
    // conversation for one would be the wrong trade every time.
    let home = tempfile::tempdir().unwrap();
    let mut chat = session(&home);
    let mut surface = Scripted::new(["/nosuchcommand", "/exit"].as_slice());

    let code = drive_prompt(&mut chat, &ChatArgs::default(), &mut surface)
        .await
        .unwrap();

    assert_eq!(code, 0);
    assert_eq!(surface.turns, 0);
    assert!(!surface.drawn.is_empty(), "nothing was said about it");
}

#[tokio::test]
async fn a_message_on_an_install_with_no_provider_warns_and_keeps_the_prompt() {
    // The refusal names what to set; unwinding here would end the session over
    // a problem the operator can fix without losing the conversation.
    let home = tempfile::tempdir().unwrap();
    let mut chat = session(&home);
    let mut surface = Scripted::new(["hello", "/exit"].as_slice());

    let code = drive_prompt(&mut chat, &ChatArgs::default(), &mut surface)
        .await
        .unwrap();

    assert_eq!(code, 0);
    assert_eq!(surface.turns, 1);
    assert!(surface.drawn.contains("darkwire init"), "{}", surface.drawn);
}

#[tokio::test]
async fn attaching_moves_the_prompt_to_another_conversation() {
    let home = tempfile::tempdir().unwrap();
    let mut chat = session(&home);
    let before = chat.attachment().session_key.clone();
    let mut surface = Scripted::new(["/new", "/exit"].as_slice());

    drive_prompt(&mut chat, &ChatArgs::default(), &mut surface)
        .await
        .unwrap();

    assert_ne!(
        chat.attachment().session_key,
        before,
        "the prompt stayed on the old conversation"
    );
    assert_eq!(surface.turns, 0);
    // The row exists before the first message, because the prompt has just
    // said it is attached to this conversation and `/sessions` listing nothing
    // under that name would make the statement look untrue.
    let row = chat
        .runtime()
        .store()
        .get_session(&chat.attachment().session_key)
        .unwrap();
    assert!(row.is_some(), "the conversation was never created");
    assert!(surface.drawn.contains("attached to"), "{}", surface.drawn);
}

// ------------------------------------------------------------- keystrokes

fn frame() -> Frame {
    frame_with(FoldDefaults::default())
}

fn frame_with(folds: FoldDefaults) -> Frame {
    let t = Translations::new(darkwire_i18n::DEFAULT_LOCALE);
    let mut frame = Frame::new(
        darkwire_tui::theme_for(Some(false)),
        "generating",
        darkwire::chat::FoldLabels::from(&t),
    );
    frame.set_folds(folds);
    // The real table, so the list a slash command opens is the one the
    // operator would see rather than a fixture that cannot go stale.
    let rows: Vec<PaletteRow> = command_rows().iter().map(PaletteRow::from).collect();
    frame.set_commands(command_items(&rows, &t));
    frame
}

/// A key as the terminal actually sends it, decoded by the real parser.
fn key(bytes: &str) -> Key {
    parse_key(bytes).expect("the toolkit decodes this sequence")
}

fn typed(frame: &mut Frame, text: &str) {
    for character in text.chars() {
        assert_eq!(
            handle_key(frame, &key(&character.to_string())),
            Typed::Redraw,
            "an ordinary character submitted something"
        );
    }
}

#[test]
fn return_submits_what_was_typed() {
    let mut frame = frame();
    typed(&mut frame, "hello");
    assert_eq!(frame.typing(), "hello");
    assert_eq!(
        handle_key(&mut frame, &key("\r")),
        Typed::Line("hello".to_owned())
    );
}

#[test]
fn return_on_an_empty_line_is_not_a_line() {
    // Otherwise every stray Return would run a turn on nothing.
    let mut frame = frame();
    typed(&mut frame, "   ");
    assert_eq!(handle_key(&mut frame, &key("\r")), Typed::Redraw);
}

#[test]
fn typing_a_slash_opens_the_list_beside_the_line() {
    // Beside, not over: what was typed stays visible and editable, which is the
    // whole difference from the palette.
    let mut frame = frame();
    typed(&mut frame, "/he");

    assert!(has(&mut frame, "/help"));
    assert_eq!(frame.typing(), "/he");
}

#[test]
fn the_list_closes_once_the_command_is_a_command_and_arguments_follow() {
    // A slash command is one token. Past the space the operator is typing
    // arguments, and a list of commands is no longer an answer to anything.
    let mut frame = frame();
    typed(&mut frame, "/rename ");
    assert!(!has(&mut frame, "/help"));
}

#[test]
fn the_list_is_not_offered_for_ordinary_prose() {
    let mut frame = frame();
    typed(&mut frame, "what is a slash command");
    assert!(!has(&mut frame, "/help"));
}

#[test]
fn tab_takes_the_row_under_the_cursor() {
    // What Tab used to do only when exactly one command matched. An ambiguous
    // prefix used to leave the line alone and say nothing; now it is a list
    // with a cursor on it.
    let mut frame = frame();
    typed(&mut frame, "/he");
    assert_eq!(handle_key(&mut frame, &key("\t")), Typed::Redraw);
    assert_eq!(frame.typing(), "/help ");
}

#[test]
fn return_runs_a_command_that_needs_nothing_else() {
    let mut frame = frame();
    typed(&mut frame, "/hel");
    assert_eq!(
        handle_key(&mut frame, &key("\r")),
        Typed::Line("/help".to_owned()),
    );
    assert_eq!(frame.typing(), "");
}

#[test]
fn return_leaves_a_command_that_wants_an_argument_on_the_line() {
    // Submitting `/rename` with no title would be running a command the
    // operator has not finished writing.
    let mut frame = frame();
    typed(&mut frame, "/renam");
    assert_eq!(handle_key(&mut frame, &key("\r")), Typed::Redraw);
    assert_eq!(frame.typing(), "/rename ");
}

#[test]
fn escape_puts_the_list_away_and_keeps_what_was_typed() {
    let mut frame = frame();
    typed(&mut frame, "/he");
    assert_eq!(handle_key(&mut frame, &key("\u{1b}")), Typed::Redraw);
    assert!(!has(&mut frame, "/help"));
    assert_eq!(frame.typing(), "/he");
}

#[test]
fn tab_outside_a_slash_command_still_asks_for_a_completion() {
    let mut frame = frame();
    typed(&mut frame, "ordinary prose");
    assert_eq!(handle_key(&mut frame, &key("\t")), Typed::Complete);
}

#[test]
fn ctrl_g_opens_the_palette() {
    let mut frame = frame();
    assert_eq!(handle_key(&mut frame, &key("\u{7}")), Typed::Palette);
    // Ctrl-A is the editor's, and every control key this frame does not claim
    // still means what a shell says it means.
    assert_eq!(handle_key(&mut frame, &key("\u{1}")), Typed::Redraw);
}

// ------------------------------------------------------------------- folds

/// Everything the frame would draw above its own chrome.
fn shown(frame: &mut Frame) -> Vec<String> {
    darkwire_tui::Component::render(frame, 80)
}

fn has(frame: &mut Frame, needle: &str) -> bool {
    shown(frame).iter().any(|row| row.contains(needle))
}

#[test]
fn reasoning_arrives_folded_and_tool_output_with_it() {
    // A turn is read for its answer. The working out is available, not first.
    let mut frame = frame();
    frame.absorb(&FrameEvent::ReasoningStart);
    frame.absorb(&FrameEvent::Text("a private thought\n".to_owned()));
    frame.absorb(&FrameEvent::ReasoningEnd);
    frame.absorb(&FrameEvent::ToolBodyStart("  ok 1.2s\n".to_owned()));
    frame.absorb(&FrameEvent::Text("    a line of output\n".to_owned()));
    frame.absorb(&FrameEvent::ToolBodyEnd);

    assert!(!has(&mut frame, "a private thought"));
    assert!(!has(&mut frame, "a line of output"));
    // Folded, not hidden: both still say they happened.
    assert!(has(&mut frame, "thought"));
    assert!(has(&mut frame, "ok 1.2s"));
}

#[test]
fn ctrl_t_opens_the_reasoning_that_is_still_on_screen() {
    let mut frame = frame();
    frame.absorb(&FrameEvent::ReasoningStart);
    frame.absorb(&FrameEvent::Text("a private thought\n".to_owned()));
    frame.absorb(&FrameEvent::ReasoningEnd);

    assert_eq!(handle_key(&mut frame, &key("\u{14}")), Typed::FoldReasoning);
    assert!(has(&mut frame, "a private thought"));

    handle_key(&mut frame, &key("\u{14}"));
    assert!(!has(&mut frame, "a private thought"));
}

#[test]
fn ctrl_t_also_says_how_the_next_run_arrives() {
    // The half that keeps working once the screen has filled: a run already in
    // the terminal's scrollback cannot be rewritten, so a key that only reached
    // what was visible would quietly stop meaning anything.
    let mut frame = frame();
    handle_key(&mut frame, &key("\u{14}"));

    frame.absorb(&FrameEvent::ReasoningStart);
    frame.absorb(&FrameEvent::Text("a later thought\n".to_owned()));
    frame.absorb(&FrameEvent::ReasoningEnd);
    assert!(has(&mut frame, "a later thought"));
}

#[test]
fn ctrl_o_leaves_the_reasoning_alone_and_the_other_way_round() {
    let mut frame = frame();
    frame.absorb(&FrameEvent::ReasoningStart);
    frame.absorb(&FrameEvent::Text("a private thought\n".to_owned()));
    frame.absorb(&FrameEvent::ReasoningEnd);
    frame.absorb(&FrameEvent::ToolBodyStart("  ok 1.2s\n".to_owned()));
    frame.absorb(&FrameEvent::Text("    a line of output\n".to_owned()));
    frame.absorb(&FrameEvent::ToolBodyEnd);

    assert_eq!(handle_key(&mut frame, &key("\u{f}")), Typed::FoldTools);
    assert!(has(&mut frame, "a line of output"));
    assert!(!has(&mut frame, "a private thought"));
}

#[test]
fn the_answer_itself_never_folds() {
    // Text outside a run is the thing the reader came for. There is no key for
    // hiding it and no state in which it is hidden.
    let mut frame = frame();
    frame.absorb(&FrameEvent::Text("the answer\n".to_owned()));
    handle_key(&mut frame, &key("\u{14}"));
    handle_key(&mut frame, &key("\u{f}"));
    assert!(has(&mut frame, "the answer"));
}

#[test]
fn a_folded_run_of_reasoning_keeps_moving_while_it_runs() {
    // The one thing on screen while the model thinks. A row that never changed
    // would be indistinguishable from a terminal that had stopped.
    let mut frame = frame();
    frame.absorb(&FrameEvent::ReasoningStart);
    frame.absorb(&FrameEvent::Text("thinking hard\n".to_owned()));

    let before = shown(&mut frame);
    for _ in 0..40 {
        frame.tick();
    }
    assert_ne!(shown(&mut frame), before);
}

#[test]
fn the_spinner_survives_a_run_of_reasoning_that_says_nothing_visible() {
    // `absorb` used to clear it on the first byte of anything. With reasoning
    // folded away, that byte is one nobody sees, so the frame would go blank
    // and stay blank for the length of the run.
    let mut frame = frame();
    frame.start_turn();
    frame.absorb(&FrameEvent::ReasoningStart);
    frame.absorb(&FrameEvent::Text("a private thought\n".to_owned()));
    assert!(frame.is_waiting());

    frame.absorb(&FrameEvent::ReasoningEnd);
    frame.absorb(&FrameEvent::Text("the answer".to_owned()));
    assert!(!frame.is_waiting());
}

#[test]
fn ctrl_t_on_a_session_with_reasoning_off_says_where_the_switch_is() {
    // Doing nothing would be defensible and would read as a broken key. The
    // reasoning never reached this frame, so there is nothing to unfold and the
    // only useful answer is the sentence saying so.
    let mut frame = frame_with(FoldDefaults {
        reasoning: ReasoningDisplay::Hidden,
        tools: true,
        stats: true,
    });
    handle_key(&mut frame, &key("\u{14}"));
    assert!(has(&mut frame, "reasoning is off"));
}

#[test]
fn the_install_settings_decide_how_a_run_arrives() {
    // The one place the config becomes frame behaviour. `expandToolOutput` is
    // the inverse of "folded", which is exactly the sort of flip that is right
    // once and wrong afterwards.
    use darkwire_protocol::config::UiConfig;

    let folds = FoldDefaults::from(&UiConfig::default());
    assert_eq!(folds.reasoning, ReasoningDisplay::Collapsed);
    assert!(folds.tools, "tool output is folded by default");
    assert!(folds.stats, "what a turn cost is folded by default");

    let opened = FoldDefaults::from(&UiConfig {
        reasoning: ReasoningDisplay::Expanded,
        expand_tool_output: true,
        expand_turn_stats: true,
        ..UiConfig::default()
    });
    assert_eq!(opened.reasoning, ReasoningDisplay::Expanded);
    assert!(!opened.tools);
    assert!(!opened.stats);
}

#[test]
fn switching_reasoning_off_mid_session_makes_the_key_say_so() {
    // `/output reasoning off` and `ui.reasoning: hidden` are the same state
    // reached two ways, and Ctrl-T has to read them the same way. Otherwise
    // the key claims there is something folded away after the command that
    // stopped anything arriving.
    let mut frame = frame();
    frame.absorb(&FrameEvent::ReasoningShown(false));
    handle_key(&mut frame, &key("\u{14}"));
    assert!(has(&mut frame, "reasoning is off"));
}

#[test]
fn switching_reasoning_back_on_makes_the_key_work_again() {
    let mut frame = frame();
    frame.absorb(&FrameEvent::ReasoningShown(false));
    frame.absorb(&FrameEvent::ReasoningShown(true));
    frame.absorb(&FrameEvent::ReasoningStart);
    frame.absorb(&FrameEvent::Text("a thought\n".to_owned()));
    frame.absorb(&FrameEvent::ReasoningEnd);

    assert!(!has(&mut frame, "a thought"));
    handle_key(&mut frame, &key("\u{14}"));
    assert!(has(&mut frame, "a thought"));
}

#[test]
fn tool_output_arrives_open_when_the_install_asked_for_that() {
    let mut frame = frame_with(FoldDefaults {
        reasoning: ReasoningDisplay::Collapsed,
        tools: false,
        stats: true,
    });
    frame.absorb(&FrameEvent::ToolBodyStart("  ok 1.2s\n".to_owned()));
    frame.absorb(&FrameEvent::Text("    a line of output\n".to_owned()));
    frame.absorb(&FrameEvent::ToolBodyEnd);
    assert!(has(&mut frame, "a line of output"));
}

#[test]
fn reasoning_arrives_open_when_the_install_asked_for_that() {
    let mut frame = frame_with(FoldDefaults {
        reasoning: ReasoningDisplay::Expanded,
        tools: true,
        stats: true,
    });
    frame.absorb(&FrameEvent::ReasoningStart);
    frame.absorb(&FrameEvent::Text("a thought\n".to_owned()));
    frame.absorb(&FrameEvent::ReasoningEnd);
    assert!(has(&mut frame, "a thought"));
}

// ------------------------------------------------------------------- tasks

fn plan(rows: &[(TaskStatus, &str)]) -> Vec<(TaskStatus, String)> {
    rows.iter()
        .map(|(status, text)| (*status, (*text).to_owned()))
        .collect()
}

#[test]
fn a_session_with_no_plan_draws_no_plan() {
    // A blank box above the composer of a conversation that has not started
    // answers a question nobody asked.
    let mut frame = frame();
    let rows = shown(&mut frame);
    assert!(!rows.iter().any(|row| row.contains("▸")));
}

#[test]
fn the_plan_sits_above_the_input_and_not_in_the_conversation() {
    // Frame state, not transcript. A turn that revises its plan six times must
    // leave one list on screen and nothing at all in the scrollback.
    let mut frame = frame();
    frame.absorb(&FrameEvent::Tasks(plan(&[
        (TaskStatus::Done, "inspect auth"),
        (TaskStatus::Doing, "update sessions"),
    ])));
    frame.absorb(&FrameEvent::Tasks(plan(&[
        (TaskStatus::Done, "inspect auth"),
        (TaskStatus::Done, "update sessions"),
        (TaskStatus::Doing, "add tests"),
    ])));

    let rows = shown(&mut frame);
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("inspect auth"))
            .count(),
        1,
    );
    assert!(rows.iter().any(|row| row.contains("add tests")));
}

#[test]
fn a_long_plan_shows_a_window_around_what_is_in_hand() {
    // "Where has this got to" is answered worse by ten rows above the box you
    // type into than by three.
    let mut frame = frame();
    frame.absorb(&FrameEvent::Tasks(plan(&[
        (TaskStatus::Done, "one"),
        (TaskStatus::Done, "two"),
        (TaskStatus::Done, "three"),
        (TaskStatus::Doing, "four"),
        (TaskStatus::Todo, "five"),
        (TaskStatus::Todo, "six"),
    ])));

    let rows = shown(&mut frame);
    assert!(rows.iter().any(|row| row.contains("four")));
    assert!(rows.iter().any(|row| row.contains("three")));
    assert!(!rows.iter().any(|row| row.contains("one")));
    // Everything off the window, above as well as below: two finished tasks
    // scrolled off the top are as hidden as the one below.
    assert!(rows.iter().any(|row| row.contains("+3 more")));
}

#[test]
fn an_emptied_plan_leaves_nothing_behind() {
    let mut frame = frame();
    frame.absorb(&FrameEvent::Tasks(plan(&[(TaskStatus::Doing, "a task")])));
    assert!(has(&mut frame, "a task"));

    frame.absorb(&FrameEvent::Tasks(Vec::new()));
    assert!(!has(&mut frame, "a task"));
}

#[test]
fn ctrl_l_asks_for_the_screen_to_be_thrown_away() {
    // Distinct from `Redraw`, which draws the frame against what the renderer
    // believes is on the screen. Ctrl-L is asked for precisely when that belief
    // is wrong, so it has to be an answer the renderer cannot reach by diffing.
    let mut frame = frame();
    assert_eq!(handle_key(&mut frame, &key("\u{c}")), Typed::Reset);
}

#[test]
fn ctrl_l_leaves_what_is_typed_alone() {
    // It repaints the screen; it does not clear the line. A shell's does the
    // same, and losing a half-written question to a smudge on the terminal
    // would be a worse bargain than the smudge.
    let mut frame = frame();
    typed(&mut frame, "half a question");
    handle_key(&mut frame, &key("\u{c}"));
    assert_eq!(frame.typing(), "half a question");
}

#[test]
fn the_interrupt_and_the_end_of_input_are_told_apart() {
    // They mean the same thing at an idle prompt and different things while a
    // turn runs, so the frame reports which one happened rather than deciding.
    let mut frame = frame();
    assert_eq!(handle_key(&mut frame, &key("\u{3}")), Typed::Interrupt);
    assert_eq!(handle_key(&mut frame, &key("\u{4}")), Typed::Leave);
}

// --------------------------------------------------------------- streaming

#[tokio::test]
async fn a_sink_hands_every_write_to_whoever_is_draining_it() {
    use darkwire::render::RenderTarget as _;

    let (mut sink, mut rx) = chunks();
    sink.write("one");
    sink.write("two");
    drop(sink);

    let mut seen = String::new();
    while let Some(event) = rx.recv().await {
        seen.push_str(event.as_text());
    }
    assert_eq!(seen, "onetwo");
}

#[tokio::test]
async fn a_write_after_the_drain_is_gone_is_dropped_rather_than_fatal() {
    // The surface has already been torn down; the turn behind it has nothing
    // useful to do about the loss and must not fail because of it.
    use darkwire::render::RenderTarget as _;

    let (mut sink, rx) = chunks();
    drop(rx);
    sink.write("nobody is listening");
}

#[test]
fn the_chrome_it_measures_is_the_chrome_it_draws() {
    // One description of the layout written twice, and they disagreed: the gap
    // above the frame was counted with a predicate that could not tell a
    // transcript holding an open line from one holding nothing. When these two
    // drift, a row of conversation goes to the scrollback that would have
    // fitted, and nothing says so.
    let mut frame = frame();
    assert_eq!(frame.chrome_rows(80), shown(&mut frame).len());

    frame.absorb(&FrameEvent::Text("\n› hi\n".to_owned()));
    assert_eq!(
        frame.chrome_rows(80) + frame.conversation_rows(80),
        shown(&mut frame).len(),
    );

    // And mid-line, which used to be the other way the count went wrong.
    frame.absorb(&FrameEvent::Text("half a sen".to_owned()));
    assert_eq!(
        frame.chrome_rows(80) + frame.conversation_rows(80),
        shown(&mut frame).len(),
    );
}

#[test]
fn one_blank_row_between_the_message_and_the_frame() {
    // What `FramedSurface::echo` writes, and the gap under it. Two rows sat
    // here for the whole of the provider's latency, and with reasoning hidden
    // no fold ever opened to collapse them.
    let mut frame = frame();
    frame.absorb(&FrameEvent::Text("\n› hi\n".to_owned()));

    let rows = shown(&mut frame);
    let at = rows.iter().position(|row| row.contains("› hi")).unwrap();
    assert_eq!(rows[at + 1], "");
    assert_ne!(rows[at + 2], "");
}

#[test]
fn the_spinner_sits_one_row_under_the_message() {
    // The shape somebody actually sees: a message, a gap, and the thing that
    // says the model is working.
    let mut frame = frame();
    frame.absorb(&FrameEvent::Text("\n› hi\n".to_owned()));
    frame.start_turn();

    let rows = shown(&mut frame);
    let message = rows.iter().position(|row| row.contains("› hi")).unwrap();
    let spinner = rows
        .iter()
        .position(|row| row.contains("generating"))
        .unwrap();
    assert_eq!(spinner, message + 2);
}

#[test]
fn what_the_turn_cost_arrives_folded_away() {
    // A turn is read for its answer. The figures are worth having and are not
    // worth a row under every one of them.
    let mut frame = frame();
    frame.absorb(&FrameEvent::Text("the answer\n".to_owned()));
    frame.absorb(&FrameEvent::TurnStats {
        line: "  · 2 steps · 26ms\n".to_owned(),
        shown: false,
    });

    assert!(!has(&mut frame, "2 steps"));
    assert!(has(&mut frame, "the answer"));
    // Folded away to nothing, not to a summary row: there is no row left
    // behind, so the answer above it does not grow a blank under it.
    assert!(!frame.stats_shown());
}

#[test]
fn ctrl_y_shows_what_a_turn_that_has_already_run_cost() {
    let mut frame = frame();
    frame.absorb(&FrameEvent::TurnStats {
        line: "  · 2 steps · 26ms\n".to_owned(),
        shown: false,
    });
    assert!(!has(&mut frame, "2 steps"));

    assert_eq!(handle_key(&mut frame, &key("\u{19}")), Typed::FoldStats);
    assert!(has(&mut frame, "2 steps"));

    handle_key(&mut frame, &key("\u{19}"));
    assert!(!has(&mut frame, "2 steps"));
}

#[test]
fn ctrl_y_also_says_how_the_next_turn_arrives() {
    // The half that keeps working once the screen has filled: a row already in
    // the scrollback cannot be rewritten, so the key sets the default too.
    let mut frame = frame();
    handle_key(&mut frame, &key("\u{19}"));
    frame.absorb(&FrameEvent::TurnStats {
        line: "  · 2 steps · 26ms\n".to_owned(),
        shown: true,
    });

    assert!(has(&mut frame, "2 steps"));
}

#[test]
fn ctrl_y_leaves_the_other_two_folds_alone() {
    let mut frame = frame();
    frame.absorb(&FrameEvent::ReasoningStart);
    frame.absorb(&FrameEvent::Text("a private thought\n".to_owned()));
    frame.absorb(&FrameEvent::ReasoningEnd);
    frame.absorb(&FrameEvent::ToolBodyStart("  ok 1.2s\n".to_owned()));
    frame.absorb(&FrameEvent::Text("    a line of output\n".to_owned()));
    frame.absorb(&FrameEvent::ToolBodyEnd);

    handle_key(&mut frame, &key("\u{19}"));
    assert!(!has(&mut frame, "a private thought"));
    assert!(!has(&mut frame, "a line of output"));
}

#[test]
fn the_command_and_the_key_are_one_switch() {
    // `/output stats on` reaches the frame through this, and it sets rather
    // than flips: a flip would undo what the command asked for.
    let mut frame = frame();
    frame.absorb(&FrameEvent::TurnStats {
        line: "  · 2 steps · 26ms\n".to_owned(),
        shown: false,
    });
    frame.absorb(&FrameEvent::StatsShown(true));

    assert!(has(&mut frame, "2 steps"));
    // The command's half arriving must not bounce back to the renderer.
    assert_eq!(frame.take_stats_toggle(), None);
}

#[test]
fn the_key_hands_its_answer_to_whoever_prints_output() {
    // One switch with two owners. The frame folds what is drawn; the renderer
    // decides whether a pipe ever sees the next one.
    let mut frame = frame();
    handle_key(&mut frame, &key("\u{19}"));

    assert_eq!(frame.take_stats_toggle(), Some(true));
    assert_eq!(frame.take_stats_toggle(), None);
}

#[test]
fn the_bar_at_the_bottom_is_drawn_at_the_width_it_is_asked_for() {
    // It used to be built once and kept as rows. The rows are justified to the
    // window, so a copy built at one width is wrong at every other: a narrower
    // window had the renderer cut the row, and what it cut was the right-hand
    // side, which is the half naming the model.
    let mut frame = frame();
    frame.set_view(darkwire::header::HeaderView {
        agent: "default".to_owned(),
        model: "qwen3:8b".to_owned(),
        provider: "Custom".to_owned(),
        workspace_name: "Default".to_owned(),
        ..darkwire::header::HeaderView::default()
    });

    for width in [92_usize, 56, 40] {
        let rows = darkwire_tui::Component::render(&mut frame, width);
        let last = rows.last().unwrap();
        assert!(
            darkwire_tui::visible_width(last) <= width,
            "at {width}: {last}"
        );
        assert!(last.contains("qwen3:8b"), "at {width}: {last}");
    }
}
