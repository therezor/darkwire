//! Agents, as rows.
//!
//! Takes the resolved agents, never the runtime that produced them — see the
//! module above for why a picker is handed data rather than something it could
//! act with.
//!
//! The server's own agent summary is deliberately not reused. It is a shape the
//! *server* needs, and the terminal already holds the resolved agents, whose
//! label is documented as never empty; adopting it would be adopting a
//! translation nobody here has to make.

use ghostai_i18n::keys;
use ghostai_runtime::EffectiveAgent;
use ghostai_tui::SelectItem;

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, choose_from, position_of, with_current};

/// What the row's right-hand column says about an agent.
fn hint_for(agent: &EffectiveAgent, t: &Translations) -> String {
    if agent.settings.model.is_empty() {
        t.t(keys::menu::NO_MODEL)
    } else {
        agent.settings.model.clone()
    }
}

/// One row per agent, in the order the runtime gives them — which is
/// default-first and then the operator's own, and is documented there as the
/// order a picker should show.
pub fn agent_items(
    agents: &[EffectiveAgent],
    current: Option<&str>,
    t: &Translations,
) -> Vec<SelectItem<String>> {
    agents
        .iter()
        .map(|agent| {
            let hint = hint_for(agent, t);
            let hint = if Some(agent.id.as_str()) == current {
                with_current(&hint, t)
            } else {
                hint
            };
            SelectItem {
                value: agent.id.clone(),
                label: agent.label.clone(),
                hint: Some(hint),
                // So typing the id finds the agent even when the operator gave
                // it a label that shares none of its letters.
                keywords: Some(agent.id.clone()),
                disabled: false,
            }
        })
        .collect()
}

/// The listing, for a terminal that cannot draw a menu.
///
/// The same shape the workspace listing takes, and for the same reason: a pipe
/// still deserves an answer, and the answer it deserves is the one a person
/// would have read off the menu.
pub fn agent_listing(agents: &[EffectiveAgent], current: Option<&str>, t: &Translations) -> String {
    agents
        .iter()
        .map(|agent| {
            let mark = if Some(agent.id.as_str()) == current {
                '*'
            } else {
                ' '
            };
            format!(
                "{mark} {}  ·  {}  ·  {}",
                agent.id,
                agent.label,
                hint_for(agent, t)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Opens the menu on the current agent. `None` if it was cancelled.
pub async fn pick_agent(
    menu: &dyn PickerMenu,
    agents: &[EffectiveAgent],
    current: Option<&str>,
    t: &Translations,
) -> Option<String> {
    let items = agent_items(agents, current, t);
    let at = current.and_then(|id| position_of(&items, &id.to_owned()));
    choose_from(menu, items, &t.t(keys::menu::titles::AGENT), at, t).await
}
