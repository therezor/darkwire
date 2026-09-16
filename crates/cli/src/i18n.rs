//! The terminal's translation layer, and the environment it reads.
//!
//! Where the locale comes from, in order:
//!
//!  1. `DARKWIRE_LANG` — the override, for a script that wants one language
//!     regardless of the shell it runs in.
//!  2. `config.ui.locale` — the install's own answer, and the same value the web
//!     UI uses. Only available once a command has loaded the config, which is
//!     why it is passed in rather than read here.
//!  3. `LC_ALL`, `LC_MESSAGES`, `LANG`, `LANGUAGE` — the POSIX chain, in the
//!     order POSIX defines.
//!
//! `darkwire --help` and an argument-parse error resolve without step 2, because
//! both run before any config has been read — and making `--help` load the
//! config to find out what language to print in would cost every invocation a
//! config read. An install whose `config.yaml` disagrees with its shell
//! therefore gets help in the shell's language. That is the one seam, and it is
//! a better trade than a slow `--help`.
//!
//! The order itself lives in `darkwire-i18n`, which is where the web surface
//! reads its own; this module supplies the environment and holds the result.

use std::collections::BTreeMap;

use darkwire_core::WireError;
use darkwire_i18n::{Args, Locale, Translator, resolve_cli_locale};

/// The process environment, or the map a test supplies instead.
///
/// A value rather than a read of `std::env`, for the reason every seam in this
/// repository is injected: a test that had to mutate the process environment
/// would be a test that cannot run beside another one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    entries: BTreeMap<String, String>,
}

impl Env {
    /// The real environment of this process.
    #[must_use]
    pub fn from_process() -> Env {
        Env {
            entries: std::env::vars().collect(),
        }
    }

    /// An environment with nothing in it.
    #[must_use]
    pub fn empty() -> Env {
        Env::default()
    }

    /// One variable, or `None` when it is unset.
    ///
    /// An *empty* value is reported as set, because the difference matters:
    /// `DARKWIRE_PASSWORD=` is an operator clearing a variable in a wrapper
    /// script, and the caller that cares treats it as absent explicitly.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(String::as_str)
    }

    /// One variable, with an empty value read as absent.
    #[must_use]
    pub fn non_empty(&self, name: &str) -> Option<&str> {
        self.get(name).filter(|value| !value.is_empty())
    }

    /// Sets one variable, which is how a test builds a fixture environment.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.entries.insert(name.into(), value.into());
    }

    /// Whether `DARKWIRE_DEBUG` asks for stack-shaped detail.
    #[must_use]
    pub fn debug(&self) -> bool {
        self.non_empty("DARKWIRE_DEBUG").is_some()
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Env {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(pairs: I) -> Env {
        Env {
            entries: pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }
}

/// A `t` bound to one locale, which every command carries.
///
/// Held as a value rather than passed as a function so the locale travels with
/// it: a command that renders a number or a duration needs to know which
/// language it is in, and a bare closure could not say.
#[derive(Debug)]
pub struct Translations {
    locale: Locale,
    translator: Translator<'static>,
}

impl Translations {
    /// The translations for one locale.
    #[must_use]
    pub fn new(locale: Locale) -> Translations {
        Translations {
            locale,
            translator: Translator::cli(locale),
        }
    }

    /// The translations for whatever the environment says.
    ///
    /// `configured` is `config.ui.locale` once a command has read it, and
    /// `None` before that.
    #[must_use]
    pub fn for_env(env: &Env, configured: Option<&str>) -> Translations {
        Translations::new(resolve_cli_locale(
            |name| env.get(name).map(str::to_owned),
            configured,
        ))
    }

    /// The locale this invocation speaks.
    #[must_use]
    pub fn locale(&self) -> Locale {
        self.locale
    }

    /// One string with no interpolation.
    #[must_use]
    pub fn t(&self, key: &str) -> String {
        self.translator.t(key, darkwire_i18n::args![])
    }

    /// One string with named arguments, built by [`darkwire_i18n::args!`].
    #[must_use]
    pub fn tr(&self, key: &str, args: &Args<'_>) -> String {
        self.translator.t(key, args)
    }

    /// Whether the bundle answers `key`. Used by the tests, not by a command.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.translator.has(key, darkwire_i18n::args![])
    }
}

impl Default for Translations {
    fn default() -> Translations {
        Translations::new(darkwire_i18n::DEFAULT_LOCALE)
    }
}

/// The sentence to print when something a person asked for failed.
///
/// A `WireError`'s message is authoritative — logs, pipes and `curl` all want
/// the same original — so this is a funnel rather than a translation:
/// everything that prints an error to a person comes through here, and the
/// structured detail a `WireError` carries stays out of the sentence.
#[must_use]
pub fn describe_error(error: &WireError) -> String {
    error.message.clone()
}
