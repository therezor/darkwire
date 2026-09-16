//! The embedded bundle, and the one rule about what it may say.
//!
//! The vocabulary half: the product once called a single object three things —
//! "session", "conversation" and "chat" — and "session" won because everything
//! underneath already said it. So "conversation" is banned outright, and "chat"
//! is banned as a noun for a session while staying allowed for the two other
//! things it genuinely names: `darkwire chat` is a command, and the `chat.*`
//! namespace belongs to that subcommand. Key paths are checked as well as
//! values, because a key is what the next person greps for.

use darkwire_i18n::{Bundle, BundleError, CLI_NAMESPACE, DEFAULT_LOCALE, Locale, cli_bundle};

#[test]
fn the_cli_bundle_is_embedded_for_the_default_locale_and_nothing_else() {
    let bundle = cli_bundle(DEFAULT_LOCALE).unwrap();
    assert!(!bundle.is_empty());
    assert!(bundle.len() > 200);
    assert_eq!(
        bundle.get("program.description"),
        Some("A self-hosted agent that runs where your files are.")
    );
    assert_eq!(bundle.get("program"), None);
    assert!(cli_bundle(Locale::new("xx")).is_none());
    assert_eq!(CLI_NAMESPACE, "cli");
}

#[test]
fn from_json_flattens_nested_objects_to_dotted_keys() {
    let bundle = Bundle::from_json(r#"{"a": {"b": {"c": "deep"}}, "top": "flat"}"#).unwrap();
    assert_eq!(bundle.get("a.b.c"), Some("deep"));
    assert_eq!(bundle.get("top"), Some("flat"));
    assert_eq!(bundle.len(), 2);
    let mut keys: Vec<&str> = bundle.iter().map(|(key, _)| key).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["a.b.c", "top"]);
    assert_eq!(Bundle::default(), Bundle::from_json("{}").unwrap());
}

#[test]
fn from_json_refuses_what_is_not_a_bundle() {
    assert!(matches!(
        Bundle::from_json("not json"),
        Err(BundleError::Json(_))
    ));
    assert!(matches!(
        Bundle::from_json("[1]"),
        Err(BundleError::NotAnObject)
    ));
    let error = Bundle::from_json(r#"{"a": {"b": 1}}"#).unwrap_err();
    assert!(matches!(&error, BundleError::NotAString(path) if path == "a.b"));
    assert_eq!(error.to_string(), "bundle leaf a.b is not a string");
    assert_eq!(
        BundleError::NotAnObject.to_string(),
        "bundle is not a JSON object"
    );
    assert!(
        Bundle::from_json("{")
            .unwrap_err()
            .to_string()
            .starts_with("bundle is not JSON: ")
    );
    let _: &dyn std::error::Error = &error;
}

/// Namespaces whose *paths* legitimately contain "chat": the `darkwire chat`
/// subcommand's own strings. Their values are still checked.
const CHAT_NAMESPACES: &[&str] = &["chat."];

/// Whether `text` contains `word` or its plural as a whole word, case-insensitively.
fn says(text: &str, word: &str) -> bool {
    let plural = format!("{word}s");
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .any(|token| token == word || token == plural)
}

/// `openConversation` holds a banned word with no boundary in front of it, so a
/// path is split at its humps before the same rule is applied.
fn spaced(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    let mut previous_lower = false;
    for ch in path.chars() {
        if ch.is_ascii_uppercase() && previous_lower {
            out.push(' ');
        }
        previous_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        out.push(ch);
    }
    out
}

fn offence(path: &str, value: &str) -> Option<&'static str> {
    let in_namespace = CHAT_NAMESPACES
        .iter()
        .any(|prefix| path.starts_with(prefix));
    let path = spaced(path);
    if says(&path, "conversation") {
        return Some("key says \"conversation\"");
    }
    if says(value, "conversation") {
        return Some("says \"conversation\"");
    }
    if !in_namespace && says(&path, "chat") {
        return Some("key says \"chat\"");
    }
    if says(value, "chat") {
        return Some("says \"chat\"");
    }
    None
}

#[test]
fn the_product_vocabulary_calls_a_session_a_session() {
    let bundle = cli_bundle(DEFAULT_LOCALE).unwrap();
    let mut offenders: Vec<String> = bundle
        .iter()
        .filter_map(|(path, value)| {
            offence(path, value).map(|why| format!("{path} {why}: {value}"))
        })
        .collect();
    offenders.sort();
    assert_eq!(offenders, Vec::<String>::new());
}

#[test]
fn the_vocabulary_rule_catches_what_it_is_for() {
    assert_eq!(
        offence("session.openConversation", "Open"),
        Some("key says \"conversation\"")
    );
    assert_eq!(
        offence("session.open", "Open the conversation"),
        Some("says \"conversation\"")
    );
    assert_eq!(offence("workspace.chats", "12"), Some("key says \"chat\""));
    assert_eq!(
        offence("workspace.count", "12 chats"),
        Some("says \"chat\"")
    );
    assert_eq!(offence("chat.description", "Talk to an agent"), None);
    assert_eq!(
        offence("chat.description", "Chat with an agent"),
        Some("says \"chat\"")
    );
    assert_eq!(
        offence("program.description", "Chatter is not chat"),
        Some("says \"chat\"")
    );
    assert_eq!(offence("program.description", "Chatter is fine"), None);
}
