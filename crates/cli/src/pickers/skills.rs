//! The workspace's instruction sheets, as rows with the switch under them.
//!
//! The same shape the memories window takes, and for the same reason: a list
//! of one-line descriptions is a reminder of what is there, and what the model
//! is actually sent is the sheet. Choosing a row opens it.
//!
//! **Out-of-scope sheets are marked rather than hidden.** A `SKILL.md` may name
//! the agents it is for, and one that does not name this agent is not in its
//! catalogue. It still goes on the list, dimmed and labelled: somebody opening
//! this window because a sheet is not working needs to see the sheet and be
//! told why, not to find it missing from a second place.
//!
//! **Nothing here writes.** There is no `save_skill` or `delete_skill` to call:
//! a skill is a directory a person commits beside the project it describes, and
//! the tool that opens one only reads. The one verb is the permission.

use darkwire_agent::Skill;
use darkwire_i18n::{args, keys};
use darkwire_tui::{SelectAction, SelectItem};

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, Placement, act_on};

/// The verb the list offers, as the only one.
const TOGGLE: usize = 0;

/// One row per sheet, valued with its position in the list.
///
/// The summary sits on a disabled first row rather than a tab of its own: it is
/// one line, and a tab holding one line is a tab somebody has to go and find.
pub fn skill_items(skills: &[Skill], agent_id: &str, summary: &str) -> Vec<SelectItem<usize>> {
    let mut items = vec![SelectItem {
        value: usize::MAX,
        label: summary.to_owned(),
        hint: None,
        keywords: None,
        disabled: true,
    }];
    items.extend(skills.iter().enumerate().map(|(at, skill)| SelectItem {
        value: at,
        label: skill.name.clone(),
        hint: Some(skill.description.clone()),
        keywords: None,
        // Not disabled: out of the catalogue is not out of reach, and `read`
        // and the `skill` tool will both still open it.
        disabled: false,
    }));
    let _ = agent_id;
    items
}

/// Which sheets this agent's catalogue does not advertise, as a note per row.
///
/// Kept beside the rows rather than folded into them, so the pure row builder
/// stays a function of the sheet alone.
pub fn out_of_scope(skill: &Skill, agent_id: &str, t: &Translations) -> Option<String> {
    (!skill.agents.is_empty() && !skill.agents.contains(&agent_id.to_owned())).then(|| {
        t.tr(
            keys::slash::notes::SKILLS_SCOPE,
            args!["agents" => skill.agents.join(", ")],
        )
    })
}

/// The verb the menu offers, worded for the state it would leave things in.
pub fn skill_actions(granted: bool, t: &Translations) -> Vec<SelectAction> {
    vec![SelectAction {
        chord: 'g',
        label: t.t(if granted {
            keys::menu::actions::SKILLS_OFF
        } else {
            keys::menu::actions::SKILLS_ON
        }),
    }]
}

/// What the list was closed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillChoice {
    /// Read this one, by position in the list.
    Read(usize),
    /// Flip whether this agent has the `skill` tool at all.
    Toggle,
}

/// Opens the catalogue over the window, at the row `at`.
pub async fn show_skills(
    menu: &dyn PickerMenu,
    items: Vec<SelectItem<usize>>,
    granted: bool,
    at: Option<usize>,
    t: &Translations,
) -> Option<SkillChoice> {
    let (row, action) = act_on(
        menu,
        items,
        &t.t(keys::menu::titles::SKILLS),
        at.map(|at| at.saturating_add(1)),
        t,
        Placement::Window,
        skill_actions(granted, t),
    )
    .await?;
    if action == Some(TOGGLE) {
        return Some(SkillChoice::Toggle);
    }
    // The summary sits on a disabled first row, so it can never be chosen and
    // `usize::MAX` never comes back here. Guarded anyway: a value that cannot
    // happen is one refactor away from happening.
    (row != usize::MAX).then_some(SkillChoice::Read(row))
}
