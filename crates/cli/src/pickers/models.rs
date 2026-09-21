//! Models, as rows.
//!
//! The list is whatever the endpoints themselves answered — every
//! OpenAI-compatible server publishes a model list, which is most of them and
//! all the local ones — so this is a real catalogue rather than a hardcoded one
//! that goes stale the week after it ships.
//!
//! A provider that could not be reached is not an error here. It arrives in the
//! response's `errors` map, and the caller says which endpoint went quiet before
//! opening the picker on the ones that answered — the alternative is a silently
//! shorter list, which reads as "that model is gone" rather than "that laptop is
//! shut".

use darkwire_i18n::{args, keys};
use darkwire_protocol::ModelsResponse;
use darkwire_tui::SelectItem;

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, Placement, choose_from, position_of, with_current};

/// One row per model, in the order the catalogue lists them.
pub fn model_items(
    catalogue: &ModelsResponse,
    current: &str,
    t: &Translations,
) -> Vec<SelectItem<String>> {
    catalogue
        .models
        .iter()
        .map(|model| {
            let where_from = model.provider_id.clone();
            let hint = if model.id == current {
                with_current(&where_from, t)
            } else {
                where_from
            };
            SelectItem {
                value: model.id.clone(),
                label: model
                    .display_name
                    .clone()
                    .unwrap_or_else(|| model.id.clone()),
                hint: Some(hint),
                // The id is not always the label, and it is what an operator
                // types.
                keywords: Some(model.id.clone()),
                disabled: false,
            }
        })
        .collect()
}

/// The listing, for a terminal that cannot draw a menu.
pub fn model_listing(catalogue: &ModelsResponse, current: &str) -> String {
    catalogue
        .models
        .iter()
        .map(|model| {
            let mark = if model.id == current { '*' } else { ' ' };
            format!("{mark} {}  ·  {}", model.id, model.provider_id)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every endpoint that did not answer, one line each.
pub fn model_errors(catalogue: &ModelsResponse, t: &Translations) -> Vec<String> {
    catalogue
        .errors
        .iter()
        .map(|(id, message)| {
            t.tr(
                keys::slash::notes::PROVIDER_QUIET,
                args!["id" => id.as_str(), "message" => message.as_str()],
            )
        })
        .collect()
}

/// Opens the menu on the model a turn would use. `None` if it was cancelled.
pub async fn pick_model(
    menu: &dyn PickerMenu,
    catalogue: &ModelsResponse,
    current: &str,
    t: &Translations,
) -> Option<String> {
    let items = model_items(catalogue, current, t);
    let at = position_of(&items, &current.to_owned());
    choose_from(
        menu,
        items,
        &t.t(keys::menu::titles::MODEL),
        at,
        t,
        Placement::Prompt,
    )
    .await
}
