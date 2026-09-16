//! Which language, out of the ones that exist.
//!
//! Locale negotiation is three lines of matching and one decision, and the
//! decision is that a request is never refused. A browser sending `de-AT`, a
//! shell exporting `LANG=de_DE.UTF-8` and a `config.yaml` written by hand all
//! name a language in a slightly different dialect of the same standard, and
//! every one of them has to land somewhere renderable — an error here would be
//! a blank screen over a spelling difference.
//!
//! So the chain narrows rather than fails: `de-AT` → `de` → `en`. The last step
//! is [`DEFAULT_LOCALE`] and it always matches, which is what makes the return
//! type `Locale` rather than `Option<Locale>` and spares every caller a branch
//! it would have written the same way.

use std::fmt;

/// A BCP-47 tag the product ships strings for.
///
/// Wraps a `&'static str` because every locale that exists is named at compile
/// time — in [`SUPPORTED_LOCALES`] — and negotiation hands back one of those
/// entries rather than the caller's spelling of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Locale(&'static str);

impl Locale {
    /// Names a locale. Only meaningful for a tag that has a bundle.
    #[must_use]
    pub const fn new(tag: &'static str) -> Self {
        Self(tag)
    }

    /// The tag as it appears in the bundle directory, case preserved.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// The one locale that is always present, and the end of every fallback chain.
pub const DEFAULT_LOCALE: Locale = Locale::new("en");

/// Everything with a resource bundle.
///
/// English only today. A second entry here plus a `locales/<tag>/` directory is
/// the whole of adding a language — the negotiation below needs no changes,
/// because it matches against this list rather than against a hardcoded set.
pub const SUPPORTED_LOCALES: &[Locale] = &[DEFAULT_LOCALE];

/// The environment variables that name a language, most specific first, in the
/// order POSIX defines.
const POSIX_LOCALE_VARS: [&str; 4] = ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"];

/// The right-to-left languages, by ISO 639-1 code.
///
/// A maintained list of six rather than a library lookup: a wrong answer here
/// renders a whole page mirrored, and the set changes on a geological timescale.
const RTL_LANGUAGES: [&str; 6] = ["ar", "fa", "he", "ps", "ur", "yi"];

/// Normalises the shapes a locale arrives in.
///
/// POSIX environments say `de_DE.UTF-8` — an underscore, and a codeset suffix
/// that is about bytes rather than language. `LANG=C` and `LANG=POSIX` are not
/// languages at all; they mean "no localisation", which is `en` here rather than
/// a lookup failure. Everything else is lowercased so `DE-de` and `de-DE` are
/// one key rather than two. `None` and `""` both normalise to `""`.
#[must_use]
pub fn normalise_locale(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return String::new();
    };

    // Strip the codeset (`.UTF-8`) and the modifier (`@euro`); neither names a
    // language, and both are common in `LANG`.
    let bare = raw
        .split('.')
        .next()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("");
    let tag = bare.replace('_', "-").trim().to_lowercase();

    if tag == "c" || tag == "posix" {
        DEFAULT_LOCALE.as_str().to_owned()
    } else {
        tag
    }
}

/// The best available match, or `None` when there is genuinely none.
///
/// The distinction between "matched `en`" and "matched nothing, so `en`" is the
/// whole reason this returns `None` rather than the default: a preference order
/// needs to know that a source had nothing to say so it can ask the next one.
/// [`resolve_locale`] collapses that back down for the callers who only want an
/// answer.
///
/// `available` is a parameter rather than a read of [`SUPPORTED_LOCALES`] so a
/// test can prove the negotiation without the product having to ship a second
/// language for it to be provable.
#[must_use]
pub fn match_locale(requested: Option<&str>, available: &[Locale]) -> Option<Locale> {
    let tag = normalise_locale(requested);
    if tag.is_empty() {
        return None;
    }

    // `de-AT` before `de`: the most specific bundle that exists should win, and
    // walking the prefixes longest-first is what makes that true without ranking.
    let parts: Vec<&str> = tag.split('-').collect();
    (1..=parts.len()).rev().find_map(|length| {
        let prefix = parts[..length].join("-");
        available
            .iter()
            .copied()
            .find(|locale| locale.as_str().to_lowercase() == prefix)
    })
}

/// The best available match for what was asked for, falling back rather than failing.
#[must_use]
pub fn resolve_locale(requested: Option<&str>, available: &[Locale]) -> Locale {
    match_locale(requested, available).unwrap_or(DEFAULT_LOCALE)
}

/// The first request that matches something, or the default when none do.
///
/// This is the shape every consumer's preference order actually has — the CLI
/// asks `DARKWIRE_LANG`, then the config, then `LANG`; the web asks storage, then
/// the browser. Expressing it once keeps "first one that means anything wins"
/// from being re-implemented per surface with a different opinion about what
/// "anything" is.
///
/// A source naming a language nobody has translated is skipped rather than
/// treated as an answer, so `LANG=xx-YY` cannot shadow a perfectly good config
/// value by resolving to the default ahead of it.
#[must_use]
pub fn resolve_first_locale(requested: &[Option<&str>], available: &[Locale]) -> Locale {
    requested
        .iter()
        .find_map(|candidate| match_locale(*candidate, available))
        .unwrap_or(DEFAULT_LOCALE)
}

/// Whether this locale is written right-to-left, for a `dir` attribute.
#[must_use]
pub fn is_rtl(locale: &str) -> bool {
    let normalised = normalise_locale(Some(locale));
    let language = normalised.split('-').next().unwrap_or("");
    RTL_LANGUAGES.contains(&language)
}

/// The locale a CLI invocation should speak, from the environment and the config.
///
/// In order:
///
/// 1. `DARKWIRE_LANG` — the override, for a script that wants one language
///    regardless of the shell it runs in.
/// 2. `configured` — `config.ui.locale`, the install's own answer and the same
///    value the web UI uses. Only available once a command has loaded the
///    config, which is why it is passed in rather than read here.
/// 3. `LC_ALL`, `LC_MESSAGES`, `LANG`, `LANGUAGE` — the POSIX chain, in the
///    order POSIX defines. These arrive as `de_DE.UTF-8`, which
///    [`normalise_locale`] turns back into a language tag.
///
/// `--help` and an argument-parse error resolve without step 2, because both
/// run before any config has been read — and making `--help` load the config to
/// find out what language to print in would cost every invocation the start-up
/// budget. An install whose `config.yaml` disagrees with its shell therefore
/// gets help in the shell's language. That is the one seam, and it is a better
/// trade than a slow `--help`.
///
/// `env` is injected rather than read from the process so the precedence is a
/// pure function a test can hold still.
pub fn resolve_cli_locale<F>(env: F, configured: Option<&str>) -> Locale
where
    F: Fn(&str) -> Option<String>,
{
    let candidates = cli_locale_candidates(env, configured);
    let requested: Vec<Option<&str>> = candidates.iter().map(Option::as_deref).collect();
    resolve_first_locale(&requested, SUPPORTED_LOCALES)
}

/// The sources [`resolve_cli_locale`] consults, in the order it consults them,
/// with `None` where a source had nothing to say.
///
/// Separate from the resolution so the *order* can be proved while the product
/// ships one language: with only `en` available every request resolves to `en`,
/// and a test of `resolve_cli_locale` alone could not tell a right precedence
/// from a wrong one.
pub fn cli_locale_candidates<F>(env: F, configured: Option<&str>) -> Vec<Option<String>>
where
    F: Fn(&str) -> Option<String>,
{
    let mut candidates = vec![env("DARKWIRE_LANG"), configured.map(str::to_owned)];
    candidates.extend(POSIX_LOCALE_VARS.iter().map(|name| env(name)));
    candidates
}
