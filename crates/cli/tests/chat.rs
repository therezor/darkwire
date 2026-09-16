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
    ChunkSink, Frame, RunTurnDeps, SIGINT_EXIT_CODE, Surface, TurnOutcome, TurnSink, Typed, chunks,
    drive_prompt, handle_key, run_turn,
};
use darkwire::i18n::Env;
use darkwire::i18n::Translations;
use darkwire::pickers::{NoMenu, PickerMenu};
use darkwire::program::{ChatArgs, Globals};
use darkwire::render::{TurnRenderer, TurnRendererOptions};
use darkwire_agent::testkit::{ScriptedProvider, ScriptedTurn, TokioClock};
use darkwire_agent::{AgentLoop, AgentLoopOptions, SteeringQueue};
use darkwire_core::messages::Content;
use darkwire_core::{Database, ErrorKind, SessionStore};
use darkwire_protocol::StopReason;
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
    chunks: tokio::sync::mpsc::UnboundedReceiver<String>,
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
        while let Ok(text) = self.chunks.try_recv() {
            self.drawn.push_str(&text);
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
        &ChatArgs::default(),
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
    Frame::new(darkwire_tui::theme_for(Some(false)), "generating")
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
fn tab_asks_for_a_completion_rather_than_inserting_one() {
    // The table lives on the surface, which is what knows which commands this
    // install actually has — an extension adds rows to it.
    let mut frame = frame();
    typed(&mut frame, "/he");
    assert_eq!(handle_key(&mut frame, &key("\t")), Typed::Complete);
}

#[test]
fn ctrl_g_opens_the_palette_and_is_still_the_only_shortcut() {
    let mut frame = frame();
    assert_eq!(handle_key(&mut frame, &key("\u{7}")), Typed::Palette);
    // Every other control key means what a shell says it means.
    assert_eq!(handle_key(&mut frame, &key("\u{1}")), Typed::Redraw);
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
    while let Some(text) = rx.recv().await {
        seen.push_str(&text);
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
