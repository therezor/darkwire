//! Negotiation, proved against a second and third language the product does not
//! ship. `de-AT` is here because the interesting case is a regional bundle
//! sitting beside its base.

use std::collections::HashMap;

use ghostai_i18n::{
    DEFAULT_LOCALE, Locale, SUPPORTED_LOCALES, cli_locale_candidates, is_rtl, match_locale,
    normalise_locale, resolve_cli_locale, resolve_first_locale, resolve_locale,
};

const AVAILABLE: &[Locale] = &[
    Locale::new("en"),
    Locale::new("de"),
    Locale::new("de-AT"),
    Locale::new("ar"),
];

#[test]
fn normalise_accepts_the_shapes_a_locale_arrives_in() {
    assert_eq!(normalise_locale(Some("de-DE")), "de-de");
    // POSIX: an underscore, and a codeset that is about bytes not language.
    assert_eq!(normalise_locale(Some("de_DE.UTF-8")), "de-de");
    assert_eq!(normalise_locale(Some("de_DE@euro")), "de-de");
    assert_eq!(normalise_locale(Some("  DE-de  ")), "de-de");
}

#[test]
fn normalise_reads_the_posix_no_localisation_locales_as_english() {
    // `LANG=C` does not mean "the C language"; it means "do not localise",
    // which is this product's default rather than a lookup failure.
    assert_eq!(normalise_locale(Some("C")), DEFAULT_LOCALE.as_str());
    assert_eq!(normalise_locale(Some("POSIX")), DEFAULT_LOCALE.as_str());
    assert_eq!(normalise_locale(Some("C.UTF-8")), DEFAULT_LOCALE.as_str());
}

#[test]
fn normalise_is_empty_for_nothing() {
    assert_eq!(normalise_locale(None), "");
    assert_eq!(normalise_locale(Some("")), "");
}

#[test]
fn match_prefers_the_most_specific_bundle_that_exists() {
    assert_eq!(
        match_locale(Some("de-AT"), AVAILABLE),
        Some(Locale::new("de-AT"))
    );
}

#[test]
fn match_narrows_to_the_base_language_when_the_region_has_no_bundle() {
    assert_eq!(
        match_locale(Some("de-CH"), AVAILABLE),
        Some(Locale::new("de"))
    );
}

#[test]
fn match_reports_no_match_rather_than_quietly_answering_english() {
    // The distinction this function exists for: a source that said nothing
    // must be distinguishable from one that asked for English.
    assert_eq!(match_locale(Some("ja"), AVAILABLE), None);
    assert_eq!(match_locale(None, AVAILABLE), None);
    assert_eq!(match_locale(Some("en"), AVAILABLE), Some(DEFAULT_LOCALE));
}

#[test]
fn match_is_case_insensitive_without_changing_the_bundle_name() {
    assert_eq!(
        match_locale(Some("DE-at"), AVAILABLE),
        Some(Locale::new("de-AT"))
    );
}

#[test]
fn resolve_falls_back_rather_than_failing() {
    assert_eq!(resolve_locale(Some("ja"), AVAILABLE), DEFAULT_LOCALE);
    assert_eq!(resolve_locale(None, AVAILABLE), DEFAULT_LOCALE);
    assert_eq!(
        resolve_locale(Some("not a locale at all"), AVAILABLE),
        DEFAULT_LOCALE
    );
}

#[test]
fn resolve_defaults_to_what_the_product_ships() {
    assert_eq!(
        resolve_locale(Some("en"), SUPPORTED_LOCALES),
        DEFAULT_LOCALE
    );
    assert!(SUPPORTED_LOCALES.contains(&DEFAULT_LOCALE));
    assert_eq!(DEFAULT_LOCALE.to_string(), "en");
}

#[test]
fn resolve_first_takes_the_first_source_that_names_a_language_we_have() {
    assert_eq!(
        resolve_first_locale(&[None, Some("de-AT"), Some("ar")], AVAILABLE),
        Locale::new("de-AT")
    );
}

#[test]
fn resolve_first_skips_a_source_naming_a_language_nobody_has_translated() {
    // The bug this prevents: `LANG=ja_JP.UTF-8` resolving to `en` and shadowing
    // a perfectly good config value that comes after it.
    assert_eq!(
        resolve_first_locale(&[Some("ja"), Some("de")], AVAILABLE),
        Locale::new("de")
    );
}

#[test]
fn resolve_first_falls_back_when_no_source_says_anything_usable() {
    assert_eq!(
        resolve_first_locale(&[None, Some(""), Some("ja")], AVAILABLE),
        DEFAULT_LOCALE
    );
    assert_eq!(resolve_first_locale(&[], AVAILABLE), DEFAULT_LOCALE);
}

#[test]
fn rtl_knows_the_right_to_left_languages_region_and_case_regardless() {
    assert!(is_rtl("ar"));
    assert!(is_rtl("ar-EG"));
    assert!(is_rtl("HE"));
    assert!(!is_rtl("en"));
    assert!(!is_rtl("de-AT"));
}

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| map.get(name).cloned()
}

#[test]
fn cli_precedence_is_override_then_config_then_the_posix_chain() {
    let env = env_of(&[
        ("GHOSTAI_LANG", "de"),
        ("LC_ALL", "fr_FR.UTF-8"),
        ("LC_MESSAGES", "it"),
        ("LANG", "es"),
        ("LANGUAGE", "pt"),
    ]);
    let candidates = cli_locale_candidates(&env, Some("ja"));
    let seen: Vec<Option<&str>> = candidates.iter().map(Option::as_deref).collect();
    assert_eq!(
        seen,
        [
            Some("de"),
            Some("ja"),
            Some("fr_FR.UTF-8"),
            Some("it"),
            Some("es"),
            Some("pt")
        ]
    );
}

#[test]
fn cli_precedence_leaves_a_hole_where_a_source_is_silent() {
    // `--help` runs before any config is read, so the second slot is empty and
    // the shell's language is what gets used.
    let env = env_of(&[("LANG", "C")]);
    let candidates = cli_locale_candidates(&env, None);
    assert_eq!(
        candidates,
        [None, None, None, None, Some("C".to_owned()), None]
    );
}

#[test]
fn cli_locale_resolves_to_a_shipped_language() {
    assert_eq!(
        resolve_cli_locale(env_of(&[("GHOSTAI_LANG", "en_US.UTF-8")]), None),
        DEFAULT_LOCALE
    );
    assert_eq!(
        resolve_cli_locale(env_of(&[("LANG", "ja_JP.UTF-8")]), Some("en")),
        DEFAULT_LOCALE
    );
    assert_eq!(resolve_cli_locale(env_of(&[]), None), DEFAULT_LOCALE);
}
