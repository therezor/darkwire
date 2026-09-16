//! Which language the terminal speaks, and where it looks to find out.
//!
//! The order is the thing under test rather than the answer: with one locale
//! shipped, every request resolves to English, so a test that only asserted the
//! result could not tell a right precedence from a wrong one. `ghostai-i18n`
//! publishes the candidate list for exactly that reason, and this asserts it.

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

use ghostai::i18n::{Env, Translations, describe_error};
use ghostai_core::{ErrorKind, GhostError};
use ghostai_i18n::{DEFAULT_LOCALE, args, cli_locale_candidates, keys, resolve_cli_locale};

fn env(pairs: &[(&str, &str)]) -> Env {
    pairs.iter().copied().collect()
}

/// The sources, in the order they are consulted, as plain strings.
fn candidates(env: &Env, configured: Option<&str>) -> Vec<Option<String>> {
    cli_locale_candidates(|name| env.get(name).map(str::to_owned), configured)
}

#[test]
fn ghostai_lang_outranks_the_shell() {
    // The override exists for a script that wants one language regardless of
    // the machine it lands on.
    let env = env(&[("GHOSTAI_LANG", "en"), ("LANG", "de_DE.UTF-8")]);
    let order = candidates(&env, None);
    assert_eq!(order[0].as_deref(), Some("en"));
    assert!(
        order
            .iter()
            .position(|value| value.as_deref() == Some("de_DE.UTF-8"))
            .is_some_and(|at| at > 0)
    );
}

#[test]
fn the_configured_locale_outranks_the_shell_and_not_the_override() {
    // `config.ui.locale` is the install's own answer and the same value the web
    // UI uses; it is only available once a command has read the config, which is
    // why it is passed in rather than read here.
    let shell = env(&[("LANG", "de_DE.UTF-8")]);
    let order = candidates(&shell, Some("en"));
    assert_eq!(order[1].as_deref(), Some("en"));

    let both = env(&[("GHOSTAI_LANG", "fr"), ("LANG", "de_DE.UTF-8")]);
    let order = candidates(&both, Some("en"));
    assert_eq!(order[0].as_deref(), Some("fr"));
    assert_eq!(order[1].as_deref(), Some("en"));
}

#[test]
fn the_posix_chain_is_read_in_the_order_posix_defines() {
    // `LC_ALL` outranks `LC_MESSAGES`, which outranks `LANG`, which outranks
    // `LANGUAGE`.
    let env = env(&[
        ("LC_ALL", "a"),
        ("LC_MESSAGES", "b"),
        ("LANG", "c"),
        ("LANGUAGE", "d"),
    ]);
    let order: Vec<Option<String>> = candidates(&env, None).into_iter().skip(2).collect();
    assert_eq!(
        order
            .iter()
            .map(std::option::Option::as_deref)
            .collect::<Vec<_>>(),
        vec![Some("a"), Some("b"), Some("c"), Some("d")]
    );
}

#[test]
fn an_empty_or_unknown_environment_falls_back_rather_than_failing() {
    for pairs in [vec![], vec![("LANG", "ja_JP.UTF-8")], vec![("LANG", "C")]] {
        let env = env(&pairs);
        let locale = resolve_cli_locale(|name| env.get(name).map(str::to_owned), None);
        assert_eq!(locale, DEFAULT_LOCALE, "{pairs:?}");
    }
}

#[test]
fn translations_scope_t_to_the_terminal_bundle() {
    let t = Translations::for_env(&env(&[("LANG", "en_GB.UTF-8")]), None);
    assert_eq!(t.locale(), DEFAULT_LOCALE);
    assert_eq!(
        t.t(keys::program::DESCRIPTION),
        "A self-hosted agent that runs where your files are."
    );
}

#[test]
fn interpolation_does_not_escape_because_a_terminal_has_no_entities() {
    // The web layer escapes what it renders. Escaping here would turn a
    // workspace called `Tom & Jerry` into `Tom &amp; Jerry`, which on a
    // terminal is simply wrong.
    let t = Translations::default();
    assert_eq!(
        t.tr(keys::program::NOT_A_PORT, args!["value" => "a & b"]),
        "\"a & b\" is not a port number"
    );
}

#[test]
fn every_key_a_command_reaches_for_is_in_the_bundle() {
    // The generated constants make a misspelling a compile error, so what is
    // left to check is that the bundle actually answers them — a key present as
    // a constant and empty in the JSON would render as a blank label.
    let t = Translations::default();
    for key in [
        keys::program::DESCRIPTION,
        keys::chat::DESCRIPTION,
        keys::serve::DESCRIPTION,
        keys::init::DESCRIPTION,
        keys::environment::DESCRIPTION,
        keys::extension::DESCRIPTION,
        keys::agent::DESCRIPTION,
        keys::preset::DESCRIPTION,
        keys::help::USAGE,
        keys::help::OPTIONS,
        keys::help::COMMANDS,
        keys::help::GLOBAL_OPTIONS,
        keys::help::ARGUMENTS,
    ] {
        assert!(t.has(key), "{key} is not answered by the bundle");
        assert_ne!(t.t(key), key, "{key} rendered as its own path");
    }
}

#[test]
fn describe_error_reports_the_message_the_error_carries() {
    // A funnel rather than a translation: logs, pipes and `curl` all want the
    // same original, so the sentence is authoritative.
    let error = GhostError::new(ErrorKind::Internal, "something specific went wrong");
    assert_eq!(describe_error(&error), "something specific went wrong");
}

#[test]
fn describe_error_says_nothing_about_the_structured_detail() {
    // The details map is for a log line. A person reading a refusal wants the
    // sentence, and the sentence already names the file and what to do next.
    let error = GhostError::new(ErrorKind::Config, "no provider could be resolved")
        .with_detail("file", "/tmp/config.yaml");
    assert_eq!(describe_error(&error), "no provider could be resolved");
}

#[test]
fn an_empty_variable_is_set_but_not_usable() {
    // The difference matters: `GHOSTAI_PASSWORD=` is an operator clearing a
    // variable in a wrapper script, and the caller that cares treats it as
    // absent explicitly rather than by accident.
    let env = env(&[("GHOSTAI_PASSWORD", "")]);
    assert_eq!(env.get("GHOSTAI_PASSWORD"), Some(""));
    assert_eq!(env.non_empty("GHOSTAI_PASSWORD"), None);
}

#[test]
fn ghostai_debug_is_any_non_empty_value() {
    assert!(!Env::empty().debug());
    assert!(!env(&[("GHOSTAI_DEBUG", "")]).debug());
    assert!(env(&[("GHOSTAI_DEBUG", "0")]).debug());
    assert!(env(&[("GHOSTAI_DEBUG", "1")]).debug());
}
