//! The plan, as rows with one verb under them.
//!
//! **The marker is the prompt's, not a tick.** `/tasks` answers "what does the
//! model see", so the rows read the way the section it reads does.
//!
//! **One verb, and it empties the list.** Dropping a single task was the first
//! shape of this and it was the wrong one: the `todo` tool replaces the whole
//! list on its next planning step, so a task removed by hand is back a moment
//! later and the gesture taught nothing. Emptying it is the edit that holds,
//! because an empty list is what the model is asked to write a plan into.

use darkwire_i18n::keys;
use darkwire_protocol::tasks::TaskItem;
use darkwire_tui::{SelectAction, SelectItem};

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, Placement, act_on};

/// The verb the list offers, as the only one.
const CLEAR: usize = 0;

/// One row per task, valued with its position in the list.
pub fn task_items(tasks: &[TaskItem]) -> Vec<SelectItem<usize>> {
    tasks
        .iter()
        .enumerate()
        .map(|(at, task)| SelectItem {
            value: at,
            label: format!("{} {}", task.status.marker(), task.text),
            hint: None,
            keywords: None,
            disabled: false,
        })
        .collect()
}

/// The verbs the menu offers, already translated.
pub fn task_actions(t: &Translations) -> Vec<SelectAction> {
    vec![SelectAction {
        chord: 'x',
        label: t.t(keys::menu::actions::CLEAR),
    }]
}

/// Opens the plan over the window. `true` if it was asked to be emptied.
///
/// Enter is a close here rather than a choice: there is nothing to attach to
/// and nothing to switch to, so the only answer worth having is the verb.
pub async fn show_tasks(menu: &dyn PickerMenu, tasks: &[TaskItem], t: &Translations) -> bool {
    let Some((_, action)) = act_on(
        menu,
        task_items(tasks),
        &t.t(keys::menu::titles::TASK),
        None,
        t,
        Placement::Window,
        task_actions(t),
    )
    .await
    else {
        return false;
    };
    action == Some(CLEAR)
}
