//! Workspaces, as rows.
//!
//! **No session count.** A workspace is where a conversation *starts*, and one
//! can be moved to another afterwards — so a count beside the name answers a
//! question nobody asked and implies an ownership that does not hold. The id is
//! what goes there instead, because it is what `/workspace <id>` takes.

use ghostai_core::WorkspaceRecord;
use ghostai_i18n::keys;
use ghostai_tui::SelectItem;

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, choose_from, position_of, with_current};

/// One row per workspace, in the order the registry returns them.
pub fn workspace_items(
    workspaces: &[WorkspaceRecord],
    current: Option<&str>,
    t: &Translations,
) -> Vec<SelectItem<String>> {
    workspaces
        .iter()
        .map(|workspace| {
            let hint = if Some(workspace.id.as_str()) == current {
                with_current(&workspace.id, t)
            } else {
                workspace.id.clone()
            };
            SelectItem {
                value: workspace.id.clone(),
                label: workspace.name.clone(),
                hint: Some(hint),
                keywords: None,
                disabled: false,
            }
        })
        .collect()
}

/// Opens the menu on the workspace new sessions land in. `None` if cancelled.
pub async fn pick_workspace(
    menu: &dyn PickerMenu,
    workspaces: &[WorkspaceRecord],
    current: Option<&str>,
    t: &Translations,
) -> Option<String> {
    let items = workspace_items(workspaces, current, t);
    let at = current.and_then(|id| position_of(&items, &id.to_owned()));
    choose_from(menu, items, &t.t(keys::menu::titles::WORKSPACE), at, t).await
}
