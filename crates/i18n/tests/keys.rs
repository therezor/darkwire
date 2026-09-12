//! The generated constants: every one resolves, and the list is the bundle.

use std::collections::BTreeSet;

use ghostai_i18n::{DEFAULT_LOCALE, Translator, args, cli_bundle, keys};

const PLURAL_SUFFIXES: [&str; 6] = ["_zero", "_one", "_two", "_few", "_many", "_other"];

fn base_key(key: &str) -> &str {
    PLURAL_SUFFIXES
        .iter()
        .find_map(|suffix| key.strip_suffix(suffix))
        .unwrap_or(key)
}

#[test]
fn every_constant_resolves_to_a_string_in_the_bundle() {
    let t = Translator::cli(DEFAULT_LOCALE);
    for key in keys::ALL {
        // `count` is harmless on a singular key and required on a plural one.
        for count in [1_i64, 2] {
            assert!(
                t.has(key, args!["count" => count]),
                "{key} has no template for count {count}"
            );
        }
    }
}

#[test]
fn the_list_is_the_bundle_with_plural_suffixes_stripped_sorted_once() {
    let bundle = cli_bundle(DEFAULT_LOCALE).unwrap();
    let expected: BTreeSet<&str> = bundle.iter().map(|(key, _)| base_key(key)).collect();
    let listed: Vec<&str> = keys::ALL.to_vec();
    assert_eq!(listed, expected.into_iter().collect::<Vec<_>>());
}

#[test]
fn constants_spell_the_dotted_path_with_the_bundle_casing() {
    assert_eq!(keys::program::DESCRIPTION, "program.description");
    assert_eq!(
        keys::agent::install::options::WORKSPACE_ID,
        "agent.install.options.workspaceId"
    );
    assert_eq!(keys::chat::header::HINT_MENU, "chat.header.hintMenu");
    // One constant for both plural forms.
    assert_eq!(keys::slash::notes::MEMORY_COUNT, "slash.notes.memoryCount");
    assert!(keys::ALL.contains(&keys::slash::notes::MEMORY_COUNT));
    assert!(
        !keys::ALL
            .iter()
            .any(|key| key.ends_with("_one") || key.ends_with("_other"))
    );
}

#[test]
fn plural_constants_choose_their_form_from_count() {
    let t = Translator::cli(DEFAULT_LOCALE);
    let one = t.t(
        keys::slash::notes::MEMORY_COUNT,
        args!["count" => 1_i64, "path" => "m", "tokens" => "5"],
    );
    let many = t.t(
        keys::slash::notes::MEMORY_COUNT,
        args!["count" => 2_i64, "path" => "m", "tokens" => "5"],
    );
    assert!(one.starts_with("1 memory "), "{one}");
    assert!(many.starts_with("2 memories "), "{many}");
}
