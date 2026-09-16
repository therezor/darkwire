//! `src/pickers/models.rs` — the rows, the listing, and the quiet endpoints.

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

use darkwire::i18n::Translations;
use darkwire::pickers::models::{model_errors, model_items, model_listing};
use darkwire_protocol::{ModelInfo, ModelsResponse};
use indexmap::IndexMap;

fn english() -> Translations {
    Translations::default()
}

fn model(id: &str, provider_id: &str, display_name: Option<&str>) -> ModelInfo {
    ModelInfo {
        id: id.to_owned(),
        provider_id: provider_id.to_owned(),
        provider_type: None,
        display_name: display_name.map(str::to_owned),
        context_window_tokens: None,
        supports_tools: None,
        supports_vision: None,
        supports_reasoning: None,
    }
}

fn catalogue() -> ModelsResponse {
    ModelsResponse {
        models: vec![
            model("qwen3", "ollama", None),
            model("gpt-5", "openai", Some("GPT-5")),
        ],
        errors: IndexMap::new(),
    }
}

#[test]
fn shows_the_display_name_when_the_endpoint_published_one_and_the_id_otherwise() {
    let t = english();
    let items = model_items(&catalogue(), "nothing", &t);
    assert_eq!(items[0].label, "qwen3");
    assert_eq!(items[1].label, "GPT-5");
}

#[test]
fn says_which_endpoint_each_model_came_from() {
    let t = english();
    assert_eq!(
        model_items(&catalogue(), "nothing", &t)[0].hint.as_deref(),
        Some("ollama")
    );
}

#[test]
fn marks_the_one_a_turn_would_use_right_now() {
    let t = english();
    let items = model_items(&catalogue(), "qwen3", &t);
    assert!(items[0].hint.as_deref().unwrap().contains("current"));
}

#[test]
fn keeps_the_id_searchable_since_it_is_not_always_the_label() {
    let t = english();
    let items = model_items(&catalogue(), "nothing", &t);
    assert_eq!(items[1].keywords.as_deref(), Some("gpt-5"));
}

#[test]
fn makes_no_rows_for_an_empty_catalogue() {
    let t = english();
    let empty = ModelsResponse {
        models: Vec::new(),
        errors: IndexMap::new(),
    };
    assert!(model_items(&empty, "", &t).is_empty());
}

#[test]
fn the_listing_is_what_a_pipe_gets_marking_the_current_model() {
    let listing = model_listing(&catalogue(), "gpt-5");
    assert!(listing.contains("* gpt-5"));
    assert!(listing.contains("  qwen3"));
    assert!(listing.contains("ollama"));
}

#[test]
fn names_every_endpoint_that_did_not_answer_and_what_it_said() {
    let t = english();
    let mut errors = IndexMap::new();
    errors.insert("openai".to_owned(), "connect ECONNREFUSED".to_owned());
    let lines = model_errors(
        &ModelsResponse {
            models: Vec::new(),
            errors,
        },
        &t,
    );
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("openai"));
    assert!(lines[0].contains("ECONNREFUSED"));
}

#[test]
fn says_nothing_when_everything_answered() {
    let t = english();
    assert!(model_errors(&catalogue(), &t).is_empty());
}
