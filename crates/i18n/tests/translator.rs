//! Lookup over a fixture chain that is deliberately *not* the product's, so the
//! semantics can be pinned without depending on the copy; the last tests use the
//! embedded bundle and the generated keys.

use ghostai_i18n::{Bundle, DEFAULT_LOCALE, Locale, MissingKey, Translator, Value, args, keys};

const EN: &str = r#"{
  "greeting": "Hello {{name}}",
  "spaced": "Hello {{ name }}",
  "files_one": "{{count}} file",
  "files_other": "{{count}} files",
  "blank": "",
  "settings": { "title": "Settings" },
  "ratio": "{{value}} of {{max}}",
  "dangling": "open {{brace"
}"#;

const DE: &str = r#"{
  "greeting": "Hallo {{name}}",
  "blank": ""
}"#;

// A fixture that cannot parse is a failing test either way.
#[allow(clippy::unwrap_used)]
fn bundles() -> (Bundle, Bundle) {
    (
        Bundle::from_json(EN).unwrap(),
        Bundle::from_json(DE).unwrap(),
    )
}

#[test]
fn translates_a_nested_key() {
    let (en, _) = bundles();
    let t = Translator::new(DEFAULT_LOCALE, "web", vec![&en]);
    assert_eq!(t.t("settings.title", args![]), "Settings");
    assert_eq!(t.locale(), DEFAULT_LOCALE);
    assert!(format!("{t:?}").contains("bundles: 1"));
}

#[test]
fn interpolates_without_escaping_the_value() {
    // Escaping would render a workspace called `Tom & Jerry` as
    // `Tom &amp; Jerry` — wrong in a browser that escapes again, and simply
    // wrong in a terminal.
    let (en, _) = bundles();
    let t = Translator::new(DEFAULT_LOCALE, "web", vec![&en]);
    assert_eq!(
        t.t("greeting", args!["name" => "Tom & Jerry"]),
        "Hello Tom & Jerry"
    );
    assert_eq!(t.t("spaced", args!["name" => "<b>"]), "Hello <b>");
    assert_eq!(
        t.t("ratio", args!["value" => 3_i64, "max" => 9.5]),
        "3 of 9.5"
    );
    assert_eq!(t.t("dangling", args![]), "open {{brace");
}

#[test]
fn pluralises_through_cldr_rules() {
    let (en, _) = bundles();
    let t = Translator::new(DEFAULT_LOCALE, "web", vec![&en]);
    assert_eq!(t.t("files", args!["count" => 1_i64]), "1 file");
    assert_eq!(t.t("files", args!["count" => 4_i64]), "4 files");
    assert_eq!(t.t("files", args!["count" => 0_i64]), "0 files");
    assert_eq!(t.t("files", args!["count" => 2_usize]), "2 files");
}

#[test]
fn pluralises_by_the_locale_not_by_english() {
    // Polish has `few` for 2; a bundle without that form falls back to `_other`.
    let pl = Bundle::from_json(
        r#"{"files_one": "{{count}} plik", "files_few": "{{count}} pliki", "files_many": "{{count}} plików"}"#,
    )
    .unwrap();
    let t = Translator::new(Locale::new("pl"), "web", vec![&pl]);
    assert_eq!(t.t("files", args!["count" => 1_i64]), "1 plik");
    assert_eq!(t.t("files", args!["count" => 2_i64]), "2 pliki");
    assert_eq!(t.t("files", args!["count" => 5_i64]), "5 plików");
    let (en, _) = bundles();
    let t = Translator::new(Locale::new("pl"), "web", vec![&en]);
    assert_eq!(t.t("files", args!["count" => 2_i64]), "2 files");
}

#[test]
fn a_count_that_is_not_a_whole_number_selects_no_plural_form() {
    let (en, _) = bundles();
    let t = Translator::new(DEFAULT_LOCALE, "web", vec![&en]);
    assert!(t.try_t("files", args!["count" => 1.5]).is_err());
    assert!(t.try_t("files", args!["count" => "one"]).is_err());
    // A non-plural key with a count still resolves.
    assert_eq!(t.t("settings.title", args!["count" => 1_i64]), "Settings");
}

#[test]
fn falls_back_to_english_for_a_key_the_locale_has_not_translated() {
    let (en, de) = bundles();
    let t = Translator::new(Locale::new("de"), "web", vec![&de, &en]);
    assert_eq!(t.t("greeting", args!["name" => "Ada"]), "Hallo Ada");
    // Only in the English bundle — this is the half-translated case.
    assert_eq!(t.t("settings.title", args![]), "Settings");
}

#[test]
fn falls_back_for_a_key_that_is_present_but_empty() {
    // A blank translation is one nobody has finished, and an empty label reads
    // as a broken UI rather than an untranslated one.
    let (en, de) = bundles();
    let t = Translator::new(Locale::new("de"), "web", vec![&de, &en]);
    assert_eq!(t.t("blank", args![]), "blank");
    assert!(!t.has("blank", args![]));
    assert!(t.has("greeting", args![]));
}

#[test]
fn a_missing_key_is_an_error_to_ask_for_and_the_key_to_render() {
    let (en, _) = bundles();
    let t = Translator::new(DEFAULT_LOCALE, "web", vec![&en]);
    let missing = t.try_t("nope.not.a.key", args![]).unwrap_err();
    assert_eq!(
        missing,
        MissingKey {
            namespace: "web",
            key: "nope.not.a.key".to_owned()
        }
    );
    assert_eq!(
        missing.to_string(),
        "Missing translation: web:nope.not.a.key"
    );
    let _: &dyn std::error::Error = &missing;
    // Production behaviour: a missing string is not worth a blank screen.
    assert_eq!(t.t("nope.not.a.key", args![]), "nope.not.a.key");
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "no argument supplies it")]
fn a_placeholder_nobody_supplied_is_a_bug_at_the_call_site() {
    let (en, _) = bundles();
    let t = Translator::new(DEFAULT_LOCALE, "web", vec![&en]);
    let _ = t.t("greeting", args![]);
}

#[test]
fn values_render_the_way_a_template_expects() {
    assert_eq!(Value::from("x").to_string(), "x");
    assert_eq!(Value::from(String::from("y")).to_string(), "y");
    assert_eq!(Value::from(-3_i64).to_string(), "-3");
    assert_eq!(Value::from(7_usize), Value::Int(7));
    assert_eq!(Value::from(9.4).to_string(), "9.4");
    assert_eq!(Value::from(3.0).to_string(), "3");
    assert_eq!(Value::from(usize::MAX), Value::Int(i64::MAX));
}

#[test]
fn the_terminal_translator_speaks_the_embedded_bundle() {
    let t = Translator::cli(DEFAULT_LOCALE);
    assert_eq!(
        t.t(keys::program::DESCRIPTION, args![]),
        "A self-hosted agent that runs where your files are."
    );
    assert_eq!(t.locale(), DEFAULT_LOCALE);
}

#[test]
fn the_terminal_translator_falls_back_to_english_for_a_locale_with_no_bundle() {
    let t = Translator::cli(Locale::new("de"));
    assert_eq!(t.locale(), Locale::new("de"));
    assert_eq!(
        t.t(keys::program::DESCRIPTION, args![]),
        "A self-hosted agent that runs where your files are."
    );
}
