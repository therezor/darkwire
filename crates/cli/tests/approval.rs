//! The terminal's approval gate: what it asks, what each answer means, and what
//! it does when there is nobody to ask.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that does not hold is a failing test either way"
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use darkwire::approval::{RuleSaver, TerminalGate};
use darkwire::pickers::{AskRequest, ListingRequest, MenuAnswer, MenuRequest, PickerMenu};
use darkwire_agent::{ApprovalGate, ApprovalRequest};
use darkwire_core::{ErrorKind, WireError};
use darkwire_i18n::DEFAULT_LOCALE;
use darkwire_protocol::{ApprovalScope, CommandPolicy, ExecRule, ToolPermission, ToolRisk};
use parking_lot::Mutex;
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// A menu answering from a script, and keeping what it was shown.
#[derive(Default)]
struct ScriptedMenu {
    answers: Mutex<Vec<Option<usize>>>,
    shown: Mutex<Vec<(String, Vec<String>)>>,
}

impl ScriptedMenu {
    fn answering(answers: &[Option<usize>]) -> Arc<ScriptedMenu> {
        Arc::new(ScriptedMenu {
            answers: Mutex::new(answers.iter().rev().copied().collect()),
            shown: Mutex::new(Vec::new()),
        })
    }

    fn shown(&self) -> Vec<(String, Vec<String>)> {
        self.shown.lock().clone()
    }
}

impl PickerMenu for ScriptedMenu {
    fn available(&self) -> bool {
        true
    }

    fn choose<'a>(
        &'a self,
        request: MenuRequest,
    ) -> Pin<Box<dyn Future<Output = Option<MenuAnswer>> + Send + 'a>> {
        self.shown.lock().push((
            request.labels.title.clone(),
            request
                .items
                .iter()
                .map(|item| item.label.clone())
                .collect(),
        ));
        let answer = self.answers.lock().pop().flatten();
        Box::pin(async move { answer.map(|row| MenuAnswer { row, action: None }) })
    }

    fn ask<'a>(
        &'a self,
        _request: AskRequest,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        Box::pin(async { None })
    }

    fn show<'a>(
        &'a self,
        _request: ListingRequest,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
}

type Saved = Arc<Mutex<Vec<(String, ExecRule)>>>;

fn saving() -> (RuleSaver, Saved) {
    let saved: Saved = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&saved);
    let writer: RuleSaver = Arc::new(move |agent_id, _command, rule| {
        into.lock().push((agent_id.to_owned(), rule.clone()));
        Ok(())
    });
    (writer, saved)
}

fn refusing() -> RuleSaver {
    Arc::new(|_, _, _| {
        Err(WireError::new(
            ErrorKind::InvalidInput,
            "The rule \"git *\" is more specific.",
        ))
    })
}

fn gate(saver: RuleSaver, menu: Option<Arc<ScriptedMenu>>) -> TerminalGate {
    let gate = TerminalGate::new(saver, DEFAULT_LOCALE);
    if let Some(menu) = menu {
        gate.attend(menu);
    }
    gate
}

fn exec(call_id: &str, argv: &[&str]) -> ApprovalRequest {
    let argv: Vec<String> = argv.iter().map(|&token| token.to_owned()).collect();
    ApprovalRequest {
        session_key: "cli:1".to_owned(),
        root_session_key: "cli:1".to_owned(),
        agent_id: "default".to_owned(),
        turn_id: "turn-1".to_owned(),
        call_id: call_id.to_owned(),
        name: "exec".to_owned(),
        args: json!({ "argv": argv }),
        risk: ToolRisk::Exec,
        memory_key: format!("exec:{}", argv.join(" ")),
        command: Some(CommandPolicy {
            shell: argv.first().is_some_and(|program| program == "sh"),
            argv,
            rule: None,
        }),
        expires_at_ms: 4_000_000_000_000,
        token: CancellationToken::new(),
    }
}

// Rows: 0 once, 1 session, 2 always (when offered), then deny.

#[tokio::test]
async fn asks_with_the_command_and_every_answer_it_can_take() {
    let menu = ScriptedMenu::answering(&[Some(0)]);
    let gate = gate(saving().0, Some(Arc::clone(&menu)));

    let decision = gate.ask(&exec("c1", &["git", "log", "-5"])).await.unwrap();

    assert!(decision.approved);
    assert_eq!(decision.scope, Some(ApprovalScope::Once));
    let (title, rows) = &menu.shown()[0];
    assert!(title.contains("git log -5"), "{title}");
    assert_eq!(
        rows,
        &vec![
            "Yes, once".to_owned(),
            "Yes, for this session".to_owned(),
            "Yes, and always allow git log -5 *".to_owned(),
            "No".to_owned(),
        ]
    );
}

#[tokio::test]
async fn once_asks_again_next_time() {
    let menu = ScriptedMenu::answering(&[Some(0), Some(0)]);
    let gate = gate(saving().0, Some(Arc::clone(&menu)));

    gate.ask(&exec("c1", &["ls"])).await.unwrap();
    assert!(gate.remembered(&exec("c2", &["ls"])).is_none());
}

#[tokio::test]
async fn this_session_stops_the_question_for_the_same_command() {
    let menu = ScriptedMenu::answering(&[Some(1)]);
    let gate = gate(saving().0, Some(menu));

    let decision = gate.ask(&exec("c1", &["ls"])).await.unwrap();
    assert_eq!(decision.scope, Some(ApprovalScope::Session));

    assert!(gate.remembered(&exec("c2", &["ls"])).unwrap().approved);
    // Another command is another question.
    assert!(gate.remembered(&exec("c3", &["rm", "x"])).is_none());
}

#[tokio::test]
async fn a_subagents_answer_holds_for_the_conversation() {
    let menu = ScriptedMenu::answering(&[Some(1)]);
    let gate = gate(saving().0, Some(menu));
    let mut nested = exec("s1", &["ls"]);
    nested.session_key = "cli:1:sub:1".to_owned();

    gate.ask(&nested).await.unwrap();

    assert!(gate.remembered(&exec("c2", &["ls"])).is_some());
}

#[tokio::test]
async fn always_saves_the_rule_on_the_agent_that_asked() {
    let menu = ScriptedMenu::answering(&[Some(2)]);
    let (writer, saved) = saving();
    let gate = gate(writer, Some(menu));

    let decision = gate.ask(&exec("c1", &["cargo", "test"])).await.unwrap();

    assert!(decision.approved);
    assert_eq!(
        *saved.lock(),
        vec![(
            "default".to_owned(),
            ExecRule {
                action: ToolPermission::Allow,
                argv: vec!["cargo".to_owned(), "test".to_owned(), "*".to_owned()],
            }
        )]
    );
}

#[tokio::test]
async fn a_refused_rule_asks_again_without_it_and_says_why() {
    let menu = ScriptedMenu::answering(&[Some(2), Some(0)]);
    let gate = gate(refusing(), Some(Arc::clone(&menu)));

    let decision = gate.ask(&exec("c1", &["git", "log"])).await.unwrap();

    assert!(decision.approved);
    let shown = menu.shown();
    assert_eq!(shown.len(), 2);
    assert!(shown[1].0.contains("more specific"), "{}", shown[1].0);
    assert_eq!(shown[1].1.len(), 3);
}

#[tokio::test]
async fn a_shell_is_never_offered_a_rule() {
    let menu = ScriptedMenu::answering(&[Some(0)]);
    let gate = gate(saving().0, Some(Arc::clone(&menu)));

    gate.ask(&exec("c1", &["sh", "-c", "ls"])).await.unwrap();

    assert_eq!(menu.shown()[0].1.len(), 3);
}

#[tokio::test]
async fn no_and_escape_both_refuse() {
    for answer in [Some(3), None] {
        let menu = ScriptedMenu::answering(&[answer]);
        let gate = gate(saving().0, Some(menu));

        let decision = gate.ask(&exec("c1", &["ls"])).await.unwrap();

        assert!(!decision.approved, "{answer:?}");
        assert!(gate.remembered(&exec("c2", &["ls"])).is_none());
    }
}

#[tokio::test]
async fn a_tool_without_a_command_is_asked_about_by_its_arguments() {
    let menu = ScriptedMenu::answering(&[Some(0)]);
    let gate = gate(saving().0, Some(Arc::clone(&menu)));
    let mut request = exec("c1", &["ls"]);
    request.name = "write".to_owned();
    request.command = None;
    request.args = json!({ "path": "notes.md" });

    gate.ask(&request).await.unwrap();

    let (title, rows) = &menu.shown()[0];
    assert!(title.contains("write"), "{title}");
    assert!(title.contains("notes.md"), "{title}");
    assert_eq!(rows.len(), 3);
}

#[test]
fn with_nobody_to_ask_it_refuses_and_says_so_afterwards() {
    // A one-shot, `--json` and a pipe never open a prompt.
    let gate = gate(saving().0, None);

    assert!(gate.cannot_ask(&exec("c1", &["ls"])));
    assert!(gate.cannot_ask(&exec("c2", &["ls"])));

    assert_eq!(gate.refused(), 2);
    let hint = gate.refused_hint().unwrap();
    assert!(hint.contains("2 tool calls"), "{hint}");
    assert!(hint.contains("--yes"), "{hint}");
}

#[test]
fn an_attended_gate_asks_and_owes_no_hint() {
    let gate = gate(saving().0, Some(ScriptedMenu::answering(&[])));

    assert!(!gate.cannot_ask(&exec("c1", &["ls"])));
    assert_eq!(gate.refused_hint(), None);
}
