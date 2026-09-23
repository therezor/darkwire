//! Applying a settings patch to the live config.
//!
//! A patch is the deep-partial of [`Config`] — every field optional and
//! stripped of its default — so a settings panel can save its own section
//! without restating the whole tree. Turning one back into a `Config` is a deep
//! merge, and four rules make it correct:
//!
//!  - **A key the patch does not mention is not touched.** This is the whole
//!    point of a patch over a partial, and it is undone by any merge that walks
//!    the *schema* rather than the patch.
//!  - **An array replaces.** `envAllowlist: ["PATH"]` means that variable, not
//!    it plus whatever was there; there is no way to express a removal
//!    otherwise.
//!  - **A record of values edited as a unit replaces too** — see
//!    [`REPLACE_WHOLESALE`]. Merging `extraHeaders` key by key would make
//!    deleting a header impossible, since a patch has no syntax for "absent".
//!  - **`null` deletes, but only where deletion is meaningful** — see
//!    [`DELETE_BY_NULL`]. A record whose *entries* an operator creates and
//!    removes — provider instances, MCP servers, agents — needs a way to say
//!    "remove this one", and the alternative was a bespoke delete method on
//!    every port that touches settings.
//!
//! The merged tree is re-parsed as a whole [`Config`] rather than assumed. A
//! patch validates field by field, but only the full schema knows the result is
//! a `Config` — and if a future cross-field rule lands, this is where the bad
//! combination is caught instead of reaching a provider as a 400.
//!
//! **The patch arrives as raw JSON, not as a typed `ConfigPatch`.** The typed
//! DTO is the right thing for a route to validate an incoming body against, but
//! it cannot carry this function's most delicate distinction: an `Option<T>`
//! field deserialises both an absent key and an explicit `null` to `None`, and
//! those two mean opposite things here. Only the raw tree still knows which one
//! the client sent.

use std::collections::BTreeSet;

use darkwire_core::config::validation_issues;
use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::Config;
use serde_json::{Map, Value};

/// Dotted paths whose object value is replaced, not merged. `*` matches one key.
///
/// The distinction a generic merge cannot make: a *struct* (`server`, `tools`)
/// is a set of independently editable fields, and a *record* is one value the UI
/// edits as a whole. Everything here is the latter.
pub const REPLACE_WHOLESALE: &[&str] = &[
    "providers.*.extraHeaders",
    // The same argument as `extraHeaders`, twice: both are edited as one block
    // of text in the MCP editor, and merging key by key would make removing an
    // entry impossible to express — an absent key means "not mentioned".
    "tools.mcpServers.*.env",
    "tools.mcpServers.*.headers",
    // An agent is edited as a whole. Merging per field would make clearing one
    // impossible to express: an absent key means "not mentioned", so an
    // operator emptying the temperature box would silently keep the value they
    // just deleted. Replacing means the patch *is* the agent — which is what
    // the editor sends, and what makes an empty box mean "send nothing" all the
    // way through. The re-parse then fills whatever the patch did not name from
    // the schema, so a replaced entry is still complete.
    //
    // Unlike `providers.*`, which merges per instance: a provider's fields all
    // have values, so none of them has a "cleared" state to express.
    "agents.list.*",
    // An extension parses its own block, so this layer does not know its shape
    // and cannot know which of its keys is a struct field and which a record
    // entry. Replacing is the only rule that is right without that knowledge —
    // and it is what the extension's own settings form sends anyway.
    "extensions.settings.*",
];

/// Dotted paths where a `null` removes the key rather than merging into it.
///
/// Deliberately a list rather than a blanket rule. `null` anywhere else is a
/// value the schema either accepts or rejects, and letting it delete would mean
/// a patch could punch a hole in a struct — dropping `server.port` and failing
/// the re-parse at best, silently reverting it at worst. Every entry here names
/// a *record whose entries an operator adds and removes*.
///
/// There is one leaf, `tools.mcpServers.*.oauth`, and it earns its place:
/// `oauth` is genuinely optional in the schema, so "this server does not use
/// OAuth" is a real state that needs a way to be said. Without it, switching
/// authorization off in the editor would send a patch that does not mention
/// `oauth` — and an absent key means "not mentioned", so the flow would survive
/// a save that looked like it had removed one.
pub const DELETE_BY_NULL: &[&str] = &[
    "providers.*",
    "tools.mcpServers.*",
    "tools.mcpServers.*.oauth",
    "agents.list.*",
    // A record an operator adds to and removes from, like `providers.*`:
    // uninstalling an extension has to be able to take its settings with it.
    "extensions.settings.*",
];

/// Whether one of `patterns` matches `path`, `*` standing for any one segment.
fn matches_path(patterns: &[&str], at: &[&str]) -> bool {
    patterns.iter().any(|pattern| {
        let mut matched = 0usize;
        for segment in pattern.split('.') {
            match at.get(matched) {
                Some(actual) if segment == "*" || segment == *actual => matched += 1,
                _ => return false,
            }
        }
        matched == at.len()
    })
}

/// One level of the merge: `patch` over `base`, at `path`.
fn merge_value(base: Option<&Value>, over: &Value, at: &[&str]) -> Value {
    let Value::Object(patch) = over else {
        return over.clone();
    };
    if matches_path(REPLACE_WHOLESALE, at) {
        return Value::Object(patch.clone());
    }

    // A patch that *creates* something still gets walked. Returning the patch
    // verbatim when there is nothing to merge into is right for the merge and
    // wrong for the deletions: it skips the `DELETE_BY_NULL` pass below, and a
    // `null` meaning "unset" then survives into the merged tree and fails the
    // re-parse as "expected object, received null". Deleting a key from nothing
    // is a no-op; leaving the token that says so in the result is not.
    let empty = Map::new();
    let source = match base {
        Some(Value::Object(object)) => object,
        _ => &empty,
    };

    let mut merged = source.clone();
    let mut deleted: BTreeSet<&str> = BTreeSet::new();
    for (key, value) in patch {
        let mut child: Vec<&str> = at.to_vec();
        child.push(key);
        // `null` is the one token that survives JSON and can only arrive from a
        // caller that wrote it, which is what makes it available to mean
        // "remove this".
        if value.is_null() && matches_path(DELETE_BY_NULL, &child) {
            deleted.insert(key);
            continue;
        }
        let next = merge_value(source.get(key), value, &child);
        merged.insert(key.clone(), next);
    }

    for key in deleted {
        merged.shift_remove(key);
    }
    Value::Object(merged)
}

/// The live config with a patch applied.
///
/// Pure: the caller decides whether the result is written to `config.yaml`,
/// handed to [`crate::WireRuntime::reconfigure`], or only previewed.
///
/// `patch` is the raw JSON body, for the reason the module header gives.
pub fn merge_config_patch(config: &Config, patch: &Value) -> Result<Config> {
    let base = serde_json::to_value(config).map_err(|error| {
        WireError::new(
            ErrorKind::Internal,
            "The live settings could not be represented as JSON.",
        )
        .with_source(error)
    })?;
    let merged = merge_value(Some(&base), patch, &[]);
    parse_merged(merged)
}

/// The merged tree as a `Config`, or the `config` error naming every path that
/// refused it.
fn parse_merged(merged: Value) -> Result<Config> {
    // A struct also deserialises from a JSON array, positionally, and every
    // field has a default — so a patch of `[]` would replace the whole tree and
    // read as a complete config rather than as the nonsense it is.
    if !merged.is_object() {
        return Err(invalid(&[format!(
            "(root): expected an object, got {}",
            json_kind(&merged)
        )]));
    }
    let config: Config = match serde_path_to_error::deserialize(merged) {
        Ok(config) => config,
        Err(error) => {
            let issue = shape_issue(&error);
            return Err(invalid(&[issue]).with_source(error));
        }
    };
    let issues = validation_issues(&config);
    if issues.is_empty() {
        Ok(config)
    } else {
        Err(invalid(&issues))
    }
}

/// What a value is, for the one message that has to say so.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// A deserialisation failure as `path: message`.
///
/// A missing field is reported at the *parent* by serde, so the field's own name
/// is appended to make the path the one an operator would search for:
/// `providers.ollama.type`, not `providers.ollama`.
fn shape_issue(error: &serde_path_to_error::Error<serde_json::Error>) -> String {
    let mut path = error.path().to_string();
    if path == "." {
        path.clear();
    }
    let message = error.inner().to_string();
    if let Some(field) = message
        .strip_prefix("missing field `")
        .and_then(|rest| rest.strip_suffix('`'))
    {
        if path.is_empty() {
            field.clone_into(&mut path);
        } else {
            path = format!("{path}.{field}");
        }
    }
    let label = if path.is_empty() { "(root)" } else { &path };
    format!("{label}: {message}")
}

/// The one error this module raises, carrying every issue for a form to render.
fn invalid(issues: &[String]) -> WireError {
    let body = issues
        .iter()
        .map(|issue| format!("  {issue}"))
        .collect::<Vec<_>>()
        .join("\n");
    WireError::new(
        ErrorKind::Config,
        format!("Settings patch produces invalid settings:\n{body}"),
    )
    .with_detail("issues", Value::from(issues.to_vec()))
}
