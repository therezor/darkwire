//! Workspaces, as rows you can act on.
//!
//! **The session count is on the row**, because this is where a workspace is
//! managed and not only chosen. The count is what decides whether a removal
//! will be refused, so a person about to press the remove key can see the
//! answer before they press it. The id goes there too: it is what the verbs
//! take, and what `/workspace <id>` takes.
//!
//! **Every verb happens here.** The first shape of this put the command on the
//! composer instead — `/workspace rename research ` with the caret after it —
//! and it worked, but it made the window a place you leave in order to finish
//! what you started there. A manager you have to exit to manage with is not a
//! manager. A rename opens a line to type on, a removal asks, and the verbs
//! are gone from the command: one way to do a thing, and it is the way that
//! shows you what you are doing it to.

use darkwire_core::WorkspaceRecord;
use darkwire_i18n::{args, keys};
use darkwire_tui::{SelectAction, SelectItem};

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, Placement, act_on, position_of, with_current};

/// What a row in the workspace menu stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceRow {
    /// One that exists.
    Workspace(String),
    /// The row at the foot that makes another.
    New,
}

/// What was asked of a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceVerb {
    /// Give it another name.
    Rename,
    /// Detach it, keeping the files.
    Remove,
    /// Send every session in it somewhere else.
    Move,
}

impl WorkspaceVerb {
    /// The verbs the menu offers, in the order their keys are listed.
    const ALL: [WorkspaceVerb; 3] = [
        WorkspaceVerb::Rename,
        WorkspaceVerb::Remove,
        WorkspaceVerb::Move,
    ];

    /// The letter it is fired with, held under Control.
    ///
    /// None of these is one [`Select`] already binds; it drops an action that
    /// names one rather than losing the movement key, so a clash here is a verb
    /// that silently does nothing.
    ///
    /// [`Select`]: darkwire_tui::Select
    fn chord(self) -> char {
        match self {
            WorkspaceVerb::Rename => 'r',
            WorkspaceVerb::Remove => 'x',
            WorkspaceVerb::Move => 'v',
        }
    }

    fn label(self, t: &Translations) -> String {
        match self {
            WorkspaceVerb::Rename => t.t(keys::menu::actions::RENAME),
            WorkspaceVerb::Remove => t.t(keys::menu::actions::REMOVE),
            WorkspaceVerb::Move => t.t(keys::menu::actions::MOVE),
        }
    }
}

/// One row per workspace, with the count a removal would have to move first.
///
/// `counts` is parallel to `workspaces`; a workspace with no count is drawn
/// without one rather than with a nought, because a missing number and a real
/// nought are different things and only one of them is news.
pub fn workspace_items(
    workspaces: &[WorkspaceRecord],
    counts: &[usize],
    current: Option<&str>,
    t: &Translations,
) -> Vec<SelectItem<WorkspaceRow>> {
    let mut items: Vec<SelectItem<WorkspaceRow>> = workspaces
        .iter()
        .enumerate()
        .map(|(at, workspace)| {
            let hint = match counts.get(at) {
                Some(count) => format!(
                    "{}  ·  {}",
                    workspace.id,
                    t.tr(keys::menu::SESSIONS, args!["count" => *count])
                ),
                None => workspace.id.clone(),
            };
            let hint = if Some(workspace.id.as_str()) == current {
                with_current(&hint, t)
            } else {
                hint
            };
            SelectItem {
                value: WorkspaceRow::Workspace(workspace.id.clone()),
                label: workspace.name.clone(),
                hint: Some(hint),
                keywords: None,
                disabled: false,
            }
        })
        .collect();
    items.push(SelectItem {
        value: WorkspaceRow::New,
        label: t.t(keys::menu::NEW_WORKSPACE),
        hint: None,
        keywords: None,
        disabled: false,
    });
    items
}

/// The verbs the menu offers, already translated.
pub fn workspace_actions(t: &Translations) -> Vec<SelectAction> {
    WorkspaceVerb::ALL
        .iter()
        .map(|verb| SelectAction {
            chord: verb.chord(),
            label: verb.label(t),
        })
        .collect()
}

/// Opens the menu over the whole window. `None` if it was closed.
///
/// Over the window rather than under the composer, unlike the vocabularies
/// `/agent` and `/model` open: this one is read for its counts and acted on,
/// and five rows of that is a list you manage through a keyhole.
pub async fn manage_workspaces(
    menu: &dyn PickerMenu,
    workspaces: &[WorkspaceRecord],
    counts: &[usize],
    current: Option<&str>,
    t: &Translations,
) -> Option<(WorkspaceRow, Option<WorkspaceVerb>)> {
    let items = workspace_items(workspaces, counts, current, t);
    let at = current.and_then(|id| position_of(&items, &WorkspaceRow::Workspace(id.to_owned())));
    let (row, action) = act_on(
        menu,
        items,
        &t.t(keys::menu::titles::WORKSPACE),
        at,
        t,
        Placement::Window,
        workspace_actions(t),
    )
    .await?;
    // A verb on the row that makes a workspace is a verb on nothing, so it
    // reads as the row being chosen. Nothing exists there to rename.
    let verb = match row {
        WorkspaceRow::New => None,
        WorkspaceRow::Workspace(_) => action.and_then(|at| WorkspaceVerb::ALL.get(at).copied()),
    };
    Some((row, verb))
}

/// Where a workspace's sessions should go, out of the others.
///
/// Its own function rather than a second call to [`manage_workspaces`]: the
/// question is different, so the title is, and a list of somewhere-to-put-this
/// has no verbs on its rows and no row that makes another one.
pub async fn pick_destination(
    menu: &dyn PickerMenu,
    workspaces: &[WorkspaceRecord],
    t: &Translations,
) -> Option<String> {
    let items: Vec<SelectItem<String>> = workspaces
        .iter()
        .map(|workspace| SelectItem {
            value: workspace.id.clone(),
            label: workspace.name.clone(),
            hint: Some(workspace.id.clone()),
            keywords: None,
            disabled: false,
        })
        .collect();
    crate::pickers::choose_from(
        menu,
        items,
        &t.t(keys::menu::titles::MOVE_TO),
        None,
        t,
        Placement::Window,
    )
    .await
}
