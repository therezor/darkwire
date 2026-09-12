//! The bundles, embedded so that the JSON is also the type.
//!
//! JSON rather than Rust literals for one reason that outranks the convenience
//! of comments: the extractor writes these files and a translation service reads
//! them. A Rust catalogue would mean hand-maintaining the output of every
//! extraction run, which is the chore the format was chosen to avoid.
//!
//! The CLI bundle is embedded at compile time and parsed once, on first use.
//! `build.rs` reads the same file to write the key constants, so the constants
//! and the templates cannot come from two revisions of the bundle. Adding a
//! locale is one line in [`CLI_SOURCES`] and one `locales/<tag>/` directory —
//! nothing else in this crate names a language.

use std::collections::HashMap;
use std::fmt;
use std::sync::LazyLock;

use serde_json::Value;

use crate::locale::{DEFAULT_LOCALE, Locale};

/// The namespace the terminal's strings live under.
pub const CLI_NAMESPACE: &str = "cli";

/// The CLI bundle per locale, as JSON text. The order is the fallback order.
const CLI_SOURCES: &[(Locale, &str)] = &[(
    DEFAULT_LOCALE,
    include_str!("../../../packages/i18n/locales/en/cli.json"),
)];

static CLI_BUNDLES: LazyLock<Vec<(Locale, Bundle)>> = LazyLock::new(|| {
    CLI_SOURCES
        .iter()
        .map(|(locale, source)| (*locale, Bundle::from_json(source).unwrap_or_default()))
        .collect()
});

/// The CLI bundle for a locale, or `None` when that locale has none.
///
/// The embedded JSON is the file `build.rs` already parsed to write the key
/// constants, so it cannot fail to parse here; if it somehow did, the bundle
/// would be empty and every key would render as itself rather than the build
/// failing at first use.
#[must_use]
pub fn cli_bundle(locale: Locale) -> Option<&'static Bundle> {
    CLI_BUNDLES
        .iter()
        .find(|(tag, _)| *tag == locale)
        .map(|(_, bundle)| bundle)
}

/// Why a bundle could not be read.
#[derive(Debug)]
pub enum BundleError {
    /// The text was not JSON.
    Json(serde_json::Error),
    /// A leaf under this dotted path was not a string.
    NotAString(String),
    /// The document was not an object at the top.
    NotAnObject,
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "bundle is not JSON: {error}"),
            Self::NotAString(path) => write!(f, "bundle leaf {path} is not a string"),
            Self::NotAnObject => f.write_str("bundle is not a JSON object"),
        }
    }
}

impl std::error::Error for BundleError {}

/// One namespace's strings, flattened to dotted keys.
///
/// `{"chat": {"noModel": "..."}}` becomes `chat.noModel`, which is the key a
/// call site holds and the key the generated constants spell.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Bundle {
    templates: HashMap<String, String>,
}

impl Bundle {
    /// Parses a nested JSON object of strings into a flat bundle.
    pub fn from_json(source: &str) -> Result<Self, BundleError> {
        let root: Value = serde_json::from_str(source).map_err(BundleError::Json)?;
        let Value::Object(_) = root else {
            return Err(BundleError::NotAnObject);
        };
        let mut templates = HashMap::new();
        flatten(&root, "", &mut templates)?;
        Ok(Self { templates })
    }

    /// The template under this dotted key, if the bundle has one.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.templates.get(key).map(String::as_str)
    }

    /// Every `(key, template)` pair, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.templates
            .iter()
            .map(|(key, template)| (key.as_str(), template.as_str()))
    }

    /// How many templates the bundle holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.templates.len()
    }

    /// Whether the bundle holds nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }
}

fn flatten(
    node: &Value,
    prefix: &str,
    into: &mut HashMap<String, String>,
) -> Result<(), BundleError> {
    match node {
        Value::String(template) => {
            into.insert(prefix.to_owned(), template.clone());
            Ok(())
        }
        Value::Object(entries) => entries.iter().try_for_each(|(name, child)| {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            flatten(child, &path, into)
        }),
        _ => Err(BundleError::NotAString(prefix.to_owned())),
    }
}
