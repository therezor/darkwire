//! One configured lookup, built the same way everywhere.
//!
//! Four decisions about what a lookup does when the bundle is not perfect.
//! Each is a silent behaviour change rather than an error if left to chance, so
//! they are made here once instead of being rediscovered per consumer:
//!
//! - **Interpolated values are not escaped.** The web layer escapes what it
//!   renders, so escaping here would turn a workspace called `Tom & Jerry` into
//!   `Tom &amp; Jerry` — and the CLI writes to a terminal, where the entity is
//!   simply wrong.
//! - **An empty string is a missing string.** A key present but empty is a
//!   translation nobody has finished; falling back to English reads better than
//!   a blank label.
//! - **English is the fallback.** A partially translated locale renders English
//!   for what is missing rather than the key itself.
//! - **A missing key is the key, not a crash.** [`Translator::t`] returns the
//!   dotted key so a missing string is never worth a blank screen;
//!   [`Translator::try_t`] returns the miss as an error for the tests, which is
//!   where a typo should stop the suite. The generated `keys` constants make a
//!   misspelled key a compile error before either matters.
//!
//! What the bundles use of their format, and therefore what is implemented:
//! `{{name}}` interpolation and `_one` / `_other` plural forms chosen from a
//! `count` argument by CLDR cardinal rules. Nesting (`$t(...)`) appears nowhere
//! in the bundles and is not implemented.

use std::fmt;

use icu_plurals::{PluralCategory, PluralOperands, PluralRules};

use crate::bundle::{Bundle, CLI_NAMESPACE, cli_bundle};
use crate::format::plural_rules;
use crate::locale::{DEFAULT_LOCALE, Locale};

/// A value an interpolation placeholder can take.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Rendered as-is, unescaped.
    Text(String),
    /// Rendered as plain digits; a caller wanting grouping formats it first.
    Int(i64),
    /// Rendered the short way: `9.4`, and `3` rather than `3.0`.
    Float(f64),
}

impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<i64> for Value {
    fn from(number: i64) -> Self {
        Self::Int(number)
    }
}

impl From<usize> for Value {
    fn from(number: usize) -> Self {
        Self::Int(i64::try_from(number).unwrap_or(i64::MAX))
    }
}

impl From<f64> for Value {
    fn from(number: f64) -> Self {
        Self::Float(number)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => f.write_str(text),
            Self::Int(number) => write!(f, "{number}"),
            Self::Float(number) => write!(f, "{number}"),
        }
    }
}

impl Value {
    /// The plural operands this value contributes when it is the `count`.
    ///
    /// Only an integer counts. The bundles count things — files, steps,
    /// messages — never measures, and a text or fractional `count` selects no
    /// plural form rather than a guessed one.
    fn operands(&self) -> Option<PluralOperands> {
        match self {
            Self::Int(number) => Some(PluralOperands::from(*number)),
            Self::Float(_) | Self::Text(_) => None,
        }
    }
}

/// Named arguments for one lookup: the `{{name}}` values and the `count`.
///
/// A slice rather than a map because a call site supplies one to four of them
/// and the template is scanned once either way. Build one with [`args!`].
pub type Args<'a> = [(&'a str, Value)];

/// Builds the arguments for a lookup: `args!["name" => "Ada", "count" => 3]`.
#[macro_export]
macro_rules! args {
    () => { &[] as &$crate::Args<'static> };
    ($($name:expr => $value:expr),+ $(,)?) => {
        &[$(($name, $crate::Value::from($value))),+] as &$crate::Args<'_>
    };
}

/// A key no bundle in the chain could answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingKey {
    /// The namespace searched.
    pub namespace: &'static str,
    /// The dotted key, as requested.
    pub key: String,
}

impl fmt::Display for MissingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Missing translation: {}:{}", self.namespace, self.key)
    }
}

impl std::error::Error for MissingKey {}

/// Lookup over a chain of bundles for one locale, English last.
pub struct Translator<'b> {
    locale: Locale,
    namespace: &'static str,
    chain: Vec<&'b Bundle>,
    rules: Option<PluralRules>,
}

impl fmt::Debug for Translator<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Translator")
            .field("locale", &self.locale)
            .field("namespace", &self.namespace)
            .field("bundles", &self.chain.len())
            .finish_non_exhaustive()
    }
}

impl Translator<'static> {
    /// The terminal's translator: the `cli` bundle for `locale`, then English.
    ///
    /// Only the one namespace is embedded, so this parses one bundle rather
    /// than every string the browser renders — `--help` is on the path.
    #[must_use]
    pub fn cli(locale: Locale) -> Self {
        let mut chain = Vec::with_capacity(2);
        chain.extend(cli_bundle(locale));
        if locale != DEFAULT_LOCALE {
            chain.extend(cli_bundle(DEFAULT_LOCALE));
        }
        Self::new(locale, CLI_NAMESPACE, chain)
    }
}

impl<'b> Translator<'b> {
    /// A translator over an explicit chain, most specific bundle first.
    ///
    /// The chain is searched in order and the first bundle with a non-empty
    /// template wins, which is how a half-translated locale falls back to the
    /// default one key at a time.
    #[must_use]
    pub fn new(locale: Locale, namespace: &'static str, chain: Vec<&'b Bundle>) -> Self {
        Self {
            locale,
            namespace,
            chain,
            rules: plural_rules(locale.as_str()),
        }
    }

    /// The locale this translator speaks.
    #[must_use]
    pub fn locale(&self) -> Locale {
        self.locale
    }

    /// The string for `key`, or the key itself when no bundle has it.
    #[must_use]
    pub fn t(&self, key: &str, args: &Args<'_>) -> String {
        self.try_t(key, args).unwrap_or_else(|missing| missing.key)
    }

    /// The string for `key`, or the miss as an error.
    ///
    /// With a `count` argument the `_one` / `_other` form is chosen by the
    /// locale's cardinal rules, falling back to `_other` and then to the bare
    /// key. Every `{{name}}` in the template is replaced by the matching
    /// argument's text, unescaped; a placeholder with no argument is left in
    /// place, and in a debug build that is an assertion failure, because a
    /// template that names a value the call site did not pass is a bug in the
    /// call site.
    pub fn try_t(&self, key: &str, args: &Args<'_>) -> Result<String, MissingKey> {
        let template = self
            .candidates(key, args)
            .into_iter()
            .find_map(|candidate| self.lookup(&candidate))
            .ok_or_else(|| MissingKey {
                namespace: self.namespace,
                key: key.to_owned(),
            })?;
        Ok(interpolate(template, args))
    }

    /// Whether any bundle in the chain answers `key`, given these arguments.
    #[must_use]
    pub fn has(&self, key: &str, args: &Args<'_>) -> bool {
        self.candidates(key, args)
            .iter()
            .any(|candidate| self.lookup(candidate).is_some())
    }

    /// The first non-empty template for a key across the chain.
    fn lookup(&self, key: &str) -> Option<&'b str> {
        self.chain
            .iter()
            .find_map(|bundle| bundle.get(key).filter(|found| !found.is_empty()))
    }

    /// The keys to try, most specific first: the plural form for `count`, then
    /// `_other`, then the bare key.
    fn candidates(&self, key: &str, args: &Args<'_>) -> Vec<String> {
        let count = args
            .iter()
            .find(|(name, _)| *name == "count")
            .and_then(|(_, v)| v.operands());
        let Some(operands) = count else {
            return vec![key.to_owned()];
        };
        let category = self
            .rules
            .as_ref()
            .map_or(PluralCategory::Other, |rules| rules.category_for(operands));
        let mut candidates = vec![format!("{key}_{}", suffix(category))];
        if category != PluralCategory::Other {
            candidates.push(format!("{key}_other"));
        }
        candidates.push(key.to_owned());
        candidates
    }
}

/// The CLDR category as the suffix the bundle spells it.
fn suffix(category: PluralCategory) -> &'static str {
    match category {
        PluralCategory::Zero => "zero",
        PluralCategory::One => "one",
        PluralCategory::Two => "two",
        PluralCategory::Few => "few",
        PluralCategory::Many => "many",
        PluralCategory::Other => "other",
    }
}

/// Replaces every `{{name}}` with the matching argument, unescaped.
fn interpolate(template: &str, args: &Args<'_>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start..].find("}}") else {
            break;
        };
        let name = rest[start + 2..start + end].trim();
        out.push_str(&rest[..start]);
        if let Some((_, value)) = args.iter().find(|(candidate, _)| *candidate == name) {
            out.push_str(&value.to_string());
        } else {
            debug_assert!(
                false,
                "template {template:?} names {{{{{name}}}}} but no argument supplies it"
            );
            out.push_str(&rest[start..start + end + 2]);
        }
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    out
}
