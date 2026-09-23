//! Asking the person at the keyboard before a tool set to `ask` runs.
//!
//! The gate is built with the runtime, before anyone knows whether a prompt
//! will be drawn, so it starts with nobody to ask. The prompt hands it a menu
//! when it opens. Until then, and for every path that never opens one (a
//! one-shot, `--json`, a pipe), it refuses: nothing there can answer, and a
//! refusal the model is told about beats a turn that hangs for five minutes.
//!
//! The question is asked inside `ask`, so the menu is on screen exactly while
//! the loop is waiting. When the turn is stopped or the deadline passes, the
//! loop drops the future, the menu's channel closes, and the menu goes away on
//! its own.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use darkwire_agent::{ApprovalDecision, ApprovalGate, ApprovalRequest};
use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_i18n::{Locale, args, keys};
use darkwire_protocol::{ApprovalScope, CommandPolicy, ExecRule};
use darkwire_providers::BoxFuture;
use darkwire_runtime::WireRuntime;
use darkwire_security::{format_argv, preferred_rule};
use darkwire_server::exec_rules::checked_rule_patch;
use darkwire_tui::{SelectItem, SelectLabels};
use parking_lot::Mutex;

use crate::i18n::Translations;
use crate::pickers::{MenuRequest, PickerMenu, Placement};
use crate::render::summarise_args;

/// Saves a command rule on an agent, or says why it cannot.
pub type RuleSaver = Arc<dyn Fn(&str, &CommandPolicy, &ExecRule) -> Result<()> + Send + Sync>;

/// How much of a non-`exec` call's arguments the question shows.
const ARGS_PREVIEW_CHARS: usize = 60;

/// What the person chose.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Choice {
    Once,
    Session,
    Always(ExecRule),
    Deny,
}

/// The gate `darkwire chat` installs.
pub struct TerminalGate {
    menu: Mutex<Option<Arc<dyn PickerMenu>>>,
    /// Calls approved for the session: conversation, then memory key. A no is
    /// always for this call only, as on the web prompt and Telegram.
    remembered: Mutex<HashMap<String, HashSet<String>>>,
    save_rule: RuleSaver,
    /// Set once the runtime has read `ui.locale`, which is after this is built.
    locale: Mutex<Locale>,
    refused: AtomicUsize,
}

impl std::fmt::Debug for TerminalGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalGate")
            .field("attended", &self.menu.lock().is_some())
            .finish_non_exhaustive()
    }
}

impl TerminalGate {
    /// A gate with nobody to ask yet.
    #[must_use]
    pub fn new(save_rule: RuleSaver, locale: Locale) -> TerminalGate {
        TerminalGate {
            menu: Mutex::new(None),
            remembered: Mutex::new(HashMap::new()),
            save_rule,
            locale: Mutex::new(locale),
            refused: AtomicUsize::new(0),
        }
    }

    /// The language the question is asked in.
    pub fn set_locale(&self, locale: Locale) {
        *self.locale.lock() = locale;
    }

    fn t(&self) -> Translations {
        Translations::new(*self.locale.lock())
    }

    /// Where the question is put from now on.
    pub fn attend(&self, menu: Arc<dyn PickerMenu>) {
        *self.menu.lock() = Some(menu);
    }

    /// How many calls were refused because nobody could be asked.
    #[must_use]
    pub fn refused(&self) -> usize {
        self.refused.load(Ordering::Relaxed)
    }

    /// The line to print after a run that refused calls, if it refused any.
    #[must_use]
    pub fn refused_hint(&self) -> Option<String> {
        let count = self.refused();
        (count > 0).then(|| {
            self.t()
                .tr(keys::chat::approval::REFUSED, args!["count" => count])
        })
    }

    /// The conversation a `session` answer belongs to: a subagent's call is
    /// answered for the conversation that delegated it.
    fn scope_of(request: &ApprovalRequest) -> &str {
        if request.root_session_key.is_empty() {
            &request.session_key
        } else {
            &request.root_session_key
        }
    }

    fn menu(&self) -> Option<Arc<dyn PickerMenu>> {
        self.menu
            .lock()
            .as_ref()
            .filter(|menu| menu.available())
            .cloned()
    }

    /// Puts the question up once and reads the answer.
    async fn choose(
        &self,
        menu: &dyn PickerMenu,
        request: &ApprovalRequest,
        rule: Option<&ExecRule>,
        error: Option<&str>,
    ) -> Choice {
        let mut choices = vec![Choice::Once, Choice::Session];
        if let Some(rule) = rule {
            choices.push(Choice::Always(rule.clone()));
        }
        choices.push(Choice::Deny);

        // Built before the await: the translator is not `Send`.
        let request = {
            let t = self.t();
            let mut title = title(&t, request);
            if let Some(error) = error {
                title = format!(
                    "{} {title}",
                    t.tr(keys::chat::approval::RULE_FAILED, args!["reason" => error])
                );
            }
            MenuRequest {
                items: choices
                    .iter()
                    .enumerate()
                    .map(|(index, choice)| SelectItem::new(index, &label(&t, choice)))
                    .collect(),
                labels: SelectLabels {
                    title,
                    empty: String::new(),
                    footer: t.t(keys::chat::approval::FOOTER),
                    filter_prefix: None,
                },
                index: None,
                placement: Placement::Prompt,
                actions: Vec::new(),
            }
        };
        let answer = menu.choose(request).await;
        // Esc is a no, the same as the web prompt's deny: the question does
        // not stay open behind a closed menu.
        answer
            .and_then(|answer| choices.get(answer.row).cloned())
            .unwrap_or(Choice::Deny)
    }

    fn remember(&self, request: &ApprovalRequest) {
        self.remembered
            .lock()
            .entry(TerminalGate::scope_of(request).to_owned())
            .or_default()
            .insert(request.memory_key.clone());
    }

    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let Some(menu) = self.menu() else {
            return ApprovalDecision::refuse();
        };
        // A shell is never offered a rule: one for it would cover every
        // program. The server side refuses the same.
        let mut rule = request
            .command
            .as_ref()
            .filter(|command| !command.shell)
            .and_then(|command| preferred_rule(&command.argv));
        let mut error: Option<String> = None;
        loop {
            match self
                .choose(menu.as_ref(), request, rule.as_ref(), error.as_deref())
                .await
            {
                Choice::Once => return approved(ApprovalScope::Once),
                Choice::Session => {
                    self.remember(request);
                    return approved(ApprovalScope::Session);
                }
                Choice::Always(chosen) => {
                    let saved = request.command.as_ref().map_or(Ok(()), |command| {
                        (self.save_rule)(&request.agent_id, command, &chosen)
                    });
                    match saved {
                        Ok(()) => {
                            self.remember(request);
                            return approved(ApprovalScope::Session);
                        }
                        // Asked again without the rule, and saying why.
                        Err(failure) => {
                            error = Some(failure.message);
                            rule = None;
                        }
                    }
                }
                Choice::Deny => return ApprovalDecision::refuse(),
            }
        }
    }
}

fn title(t: &Translations, request: &ApprovalRequest) -> String {
    let command = request.command.as_ref().map_or_else(
        || summarise_args(&request.args, ARGS_PREVIEW_CHARS),
        |command| format_argv(&command.argv),
    );
    if command.is_empty() {
        return t.tr(
            keys::chat::approval::TITLE,
            args!["tool" => request.name.as_str()],
        );
    }
    t.tr(
        keys::chat::approval::TITLE_COMMAND,
        args!["tool" => request.name.as_str(), "command" => command],
    )
}

fn label(t: &Translations, choice: &Choice) -> String {
    match choice {
        Choice::Once => t.t(keys::chat::approval::ONCE),
        Choice::Session => t.t(keys::chat::approval::SESSION),
        Choice::Always(rule) => t.tr(
            keys::chat::approval::ALWAYS,
            args!["rule" => format_argv(&rule.argv)],
        ),
        Choice::Deny => t.t(keys::chat::approval::DENY),
    }
}

fn approved(scope: ApprovalScope) -> ApprovalDecision {
    ApprovalDecision {
        scope: Some(scope),
        ..ApprovalDecision::allow()
    }
}

impl ApprovalGate for TerminalGate {
    fn ask<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<ApprovalDecision>> {
        Box::pin(async move { Ok(self.decide(request).await) })
    }

    fn remembered(&self, request: &ApprovalRequest) -> Option<ApprovalDecision> {
        self.remembered
            .lock()
            .get(TerminalGate::scope_of(request))?
            .contains(&request.memory_key)
            .then(|| approved(ApprovalScope::Session))
    }

    fn cannot_ask(&self, request: &ApprovalRequest) -> bool {
        let _ = request;
        if self.menu().is_some() {
            return false;
        }
        self.refused.fetch_add(1, Ordering::Relaxed);
        true
    }
}

/// Saves a rule through the same check the server makes.
///
/// Weak, because the runtime holds the gate that holds this.
#[must_use]
pub fn rule_saver(runtime: Arc<OnceLock<Weak<WireRuntime>>>) -> RuleSaver {
    // One save at a time: each reads the settings and writes them back.
    let serial = Arc::new(Mutex::new(()));
    Arc::new(move |agent_id, command, rule| {
        let Some(runtime) = runtime.get().and_then(Weak::upgrade) else {
            return Err(WireError::new(
                ErrorKind::Internal,
                "The runtime has closed, so the rule cannot be saved.",
            ));
        };
        let _serial = serial.lock();
        let patch = checked_rule_patch(&runtime.config(), agent_id, command, rule)?;
        runtime.apply_patch(&patch)?;
        Ok(())
    })
}
