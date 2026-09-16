//! `src/pickers/agents.rs` — the rows, the listing, and what the menu is asked.

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

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use darkwire::i18n::Translations;
use darkwire::pickers::agents::{agent_items, agent_listing, pick_agent};
use darkwire::pickers::{MenuRequest, NoMenu, PickerMenu};
use darkwire_protocol::config::{AgentEnvironment, AgentSettings, PromptMode, ToolsConfig};
use darkwire_runtime::EffectiveAgent;
use indexmap::IndexMap;

/// Only the fields the picker reads.
///
/// A resolved agent carries a dozen more — prompts, tool permissions, a
/// container and subagent bindings — and a fixture that filled them in would be
/// asserting that the picker ignores them, at length.
fn agent(id: &str, label: &str, model: &str) -> EffectiveAgent {
    EffectiveAgent {
        id: id.to_owned(),
        label: label.to_owned(),
        system_prompt: String::new(),
        live_prompt: String::new(),
        wrap_up_prompt: String::new(),
        platform_prompt: String::new(),
        tool_policy_prompt: String::new(),
        memory_prompt: String::new(),
        skills_prompt: String::new(),
        prompt_mode: PromptMode::default(),
        tool_prompts: IndexMap::default(),
        settings: AgentSettings {
            model: model.to_owned(),
            ..Default::default()
        },
        tools: IndexMap::default(),
        tools_config: ToolsConfig::default(),
        environment: AgentEnvironment::default(),
        subagents: Vec::new(),
    }
}

fn agents() -> Vec<EffectiveAgent> {
    vec![
        agent("default", "Default", "claude-opus-5"),
        agent("reviewer", "Reviewer", "claude-sonnet-5"),
        agent("scout", "Scout", ""),
    ]
}

fn english() -> Translations {
    Translations::default()
}

/// Records what it was asked and answers with whatever it was told to.
struct Recording {
    answer: Option<usize>,
    asked: Mutex<Vec<MenuRequest>>,
}

impl Recording {
    fn answering(answer: Option<usize>) -> Recording {
        Recording {
            answer,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn index(&self) -> Option<usize> {
        self.asked.lock().unwrap()[0].index
    }
}

impl PickerMenu for Recording {
    fn available(&self) -> bool {
        true
    }

    fn choose<'a>(
        &'a self,
        request: MenuRequest,
    ) -> Pin<Box<dyn Future<Output = Option<usize>> + Send + 'a>> {
        self.asked.lock().unwrap().push(request);
        let answer = self.answer;
        Box::pin(async move { answer })
    }
}

#[test]
fn makes_one_row_per_agent_in_the_order_it_was_given_them() {
    // The runtime hands them over default-first and then in the operator's own
    // order, and documents that as the order a picker should show.
    let t = english();
    let values: Vec<String> = agent_items(&agents(), None, &t)
        .into_iter()
        .map(|item| item.value)
        .collect();
    assert_eq!(values, ["default", "reviewer", "scout"]);
}

#[test]
fn shows_the_label_and_the_model_beside_it() {
    let t = english();
    let items = agent_items(&agents(), None, &t);
    assert_eq!(items[0].label, "Default");
    assert_eq!(items[0].hint.as_deref(), Some("claude-opus-5"));
    assert_eq!(items[1].hint.as_deref(), Some("claude-sonnet-5"));
}

#[test]
fn says_so_rather_than_showing_an_empty_column_when_an_agent_has_no_model() {
    let t = english();
    let items = agent_items(&agents(), None, &t);
    assert_eq!(items[2].hint.as_deref(), Some("no model set"));
}

#[test]
fn marks_the_one_this_conversation_already_runs_on() {
    let t = english();
    let items = agent_items(&agents(), Some("reviewer"), &t);
    assert!(items[1].hint.as_deref().unwrap().contains("current"));
}

#[test]
fn keeps_the_id_searchable_when_the_label_shares_none_of_its_letters() {
    let t = english();
    let items = agent_items(&agents(), None, &t);
    assert_eq!(items[1].keywords.as_deref(), Some("reviewer"));
}

#[test]
fn makes_no_rows_at_all_for_no_agents() {
    let t = english();
    assert!(agent_items(&[], None, &t).is_empty());
}

#[test]
fn the_listing_is_what_a_pipe_gets_and_marks_the_current_one() {
    let t = english();
    let listing = agent_listing(&agents(), Some("reviewer"), &t);
    assert!(listing.contains("* reviewer"));
    assert!(listing.contains("  default"));
    assert!(listing.contains("claude-sonnet-5"));
    assert_eq!(listing.lines().count(), 3);
}

#[tokio::test]
async fn opens_on_the_agent_the_conversation_already_runs_on() {
    let t = english();
    let menu = Recording::answering(Some(2));
    let chosen = pick_agent(&menu, &agents(), Some("reviewer"), &t).await;
    assert_eq!(chosen.as_deref(), Some("scout"));
    assert_eq!(menu.index(), Some(1));
}

#[tokio::test]
async fn opens_at_the_top_when_nothing_is_current_yet() {
    let t = english();
    let menu = Recording::answering(None);
    pick_agent(&menu, &agents(), Some("gone"), &t).await;
    assert_eq!(menu.index(), None);
}

#[tokio::test]
async fn answers_nothing_when_the_menu_was_cancelled() {
    let t = english();
    let menu = Recording::answering(None);
    assert!(pick_agent(&menu, &agents(), None, &t).await.is_none());
}

#[tokio::test]
async fn answers_nothing_when_there_is_no_menu_to_open() {
    let t = english();
    assert!(pick_agent(&NoMenu, &agents(), None, &t).await.is_none());
}
