//! The `---` block at the top of a Markdown file.
//!
//! **This is not a YAML parser, and it must not grow into one.** Nothing in this
//! repository parses YAML. What a skill's frontmatter actually holds is a `name`
//! and a `description`: two strings, not a tree. A parser for two strings is
//! thirty lines here; a parser for YAML is a dependency plus every construct it
//! accepts and this module's callers do not handle.
//!
//! So the grammar is deliberately smaller than YAML and stops where YAML would
//! get interesting:
//!
//! ```text
//!   document    := fence entry* fence body
//!   fence       := "---" EOL
//!   entry       := field | nest
//!   field       := key ":" value EOL
//!   nest        := key ":" EOL (indent key ":" value EOL)+
//!   key         := [A-Za-z][A-Za-z0-9_-]*
//!   value       := any text, optionally wrapped in one pair of quotes
//! ```
//!
//! Anything else inside the fence (a list, a deeper mapping, a stray line) is
//! skipped rather than refused. A skill is loaded on the strength of the two
//! fields it must have, and failing the whole file because someone left a
//! `tags:` list in it would refuse a skill over a field nobody reads.
//!
//! ## One level of nesting, flattened to a dotted key
//!
//! ```yaml
//! metadata:
//!   type: user
//! ```
//!
//! yields `{"metadata": "", "metadata.type": "user"}`. The result is still a
//! flat map, so no caller learns about a tree: the memory store asks for
//! `metadata.type` and reads it as the thing that is in the file.
//!
//! This exists to close a hazard, not as a feature. Trimming every line before
//! matching stores an indented `type: user` as a *top-level* `type`, and lets a
//! nested `name:` under any key silently overwrite the real one, in a skill as
//! much as in a memory. Only one level, and only under
//! a key whose own value is empty: that is the whole of what the memory format
//! needs, and every step past it is a step towards the YAML parser this module
//! exists not to be.
//!
//! It lives in core because both the skill loader (agent) and the memory tool
//! (tools) read it, and one parser at the bottom of the graph beats two copies
//! of the same thirty lines drifting apart.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use ghostai_protocol::json::js_trim;
use regex::Regex;

/// A parsed document: its frontmatter fields, and everything after the fence.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frontmatter {
    /// `key` to value, with one level of nesting flattened to `parent.key`.
    pub fields: BTreeMap<String, String>,
    /// Everything after the closing fence, trimmed.
    pub body: String,
}

const FENCE: &str = "---";

/// `key: value`, anchored, with the value running to end of line.
static FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Za-z][A-Za-z0-9_-]*)\s*:\s*(.*)$")
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// Splits a document into its frontmatter fields and its body.
///
/// **Never fails.** A file with no fence, or one whose fence is never closed,
/// is a document with no fields and a body of the whole text. That is the
/// reading that loses nothing: the alternative for an unterminated fence is to
/// swallow the entire skill as frontmatter, which turns a missing line into a
/// silently empty instruction sheet.
pub fn parse_frontmatter(text: &str) -> Frontmatter {
    // Both line endings, without normalising the text first: a `\r` left on
    // the closing fence is the difference between finding it and reading the
    // whole file as frontmatter.
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();

    let whole = || Frontmatter {
        fields: BTreeMap::new(),
        body: js_trim(text).to_owned(),
    };

    if lines.first().is_none_or(|line| js_trim(line) != FENCE) {
        return whole();
    }
    let Some(close) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (js_trim(line) == FENCE).then_some(index))
    else {
        return whole();
    };

    let mut fields = BTreeMap::new();
    // The key an indented line hangs off: the last one at column zero whose own
    // value was empty. Cleared by anything else, so an indented line under
    // `name: Deploy` is a stray rather than `name.something`.
    let mut parent: Option<String> = None;

    for line in &lines[1..close] {
        let Some((key, value)) = parse_field(line) else {
            parent = None;
            continue;
        };

        let indented = line
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == '\u{FEFF}');
        if indented {
            if let Some(parent) = &parent {
                fields.insert(format!("{parent}.{key}"), value);
            }
            continue;
        }

        // Last wins, which is the only rule that does not need explaining when
        // a key appears twice.
        parent = value.is_empty().then(|| key.clone());
        fields.insert(key, value);
    }

    Frontmatter {
        fields,
        body: js_trim(&lines[close + 1..].join("\n")).to_owned(),
    }
}

fn parse_field(line: &str) -> Option<(String, String)> {
    let trimmed = js_trim(line);
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let captures = FIELD.captures(trimmed)?;
    let key = captures.get(1)?.as_str().to_owned();
    let value = unquote(js_trim(captures.get(2)?.as_str())).to_owned();
    Some((key, value))
}

/// Strips one matching pair of surrounding quotes.
///
/// One pair, and only when both ends agree, so `"a"` is `a` while `"a` keeps
/// its quote rather than losing a character to a rule that guessed.
fn unquote(value: &str) -> &str {
    let mut chars = value.chars();
    let Some(first @ ('"' | '\'')) = chars.next() else {
        return value;
    };
    if chars.next().is_none() {
        return value;
    }
    value
        .strip_suffix(first)
        .and_then(|rest| rest.strip_prefix(first))
        .unwrap_or(value)
}
