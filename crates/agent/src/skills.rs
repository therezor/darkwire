//! Skills: instruction sheets kept in the workspace, under `skills/`.
//!
//! One directory per skill, each holding a `SKILL.md` whose frontmatter carries
//! a description and whose body is the instructions. The directory may hold
//! whatever else the skill needs — a checklist, a template, a script — and the
//! model reaches those with `read_file` like any other workspace file. That is
//! the whole reason a skill is a directory rather than a file.
//!
//! This module is the disk half and nothing else: bytes to a list of [`Skill`].
//! What reaches the prompt, and at what budget, is `skills_contributor`.
//!
//! ## Two decisions worth stating
//!
//! **Which agents see a sheet is declared in the file, not by where it sits.**
//! An `agents:` line in the frontmatter narrows a sheet to the agents it names;
//! without one it is every agent's, which is what every sheet written before
//! this existed keeps doing. Scope is a property of the *catalogue* — it
//! decides what an agent is told about, not what it may read, and `read_file`
//! and the `skill` tool will still open a sheet that was not advertised.
//!
//! **The directory name is the skill's id.** Not the frontmatter `name` — the
//! directory name is what appears in the path the model is given, so it is the
//! one identifier that cannot disagree with anything. A `name` field that says
//! something else is a warning and loses.
//!
//! **Nothing here fails.** A skill folder is workspace content, which means it
//! is whatever a person or a previous turn left there. A malformed file must
//! cost that one skill, not every turn on the workspace — the same position
//! `render_prompt_template` takes on a typo in a prompt template.
//!
//! ## Where these live, and what it costs
//!
//! In the workspace, which is inside the jail, which means `write_file` and
//! `exec` can both edit them. An agent's own directory sits *beside* the
//! workspace for exactly this reason, and the tradeoff is deliberate rather
//! than overlooked: a skill folder is meant to be committed beside the project
//! it describes, and a location the agent cannot see in a directory listing is
//! not that. What follows from it here: a skill directory that is a symlink is
//! skipped, so a link pointing out of the workspace never resolves, and every
//! body is bounded.

use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use darkwire_core::frontmatter::parse_frontmatter;
use darkwire_protocol::is_agent_id;
use regex::Regex;

/// The folder inside the workspace that holds them.
pub const SKILLS_DIRNAME: &str = "skills";

/// The one file a skill directory must contain.
pub const SKILL_FILENAME: &str = "SKILL.md";

/// The most of one skill's body that reaches the prompt.
///
/// 12 KB is roughly three thousand tokens. The bound exists because a body the
/// model opens with `read_file` lands in the transcript and is re-sent on every
/// later iteration of that turn at full price, so its size is a decision rather
/// than an accident.
pub const SKILL_MAX_BYTES: usize = 12 * 1024;

/// The most skills one workspace advertises.
///
/// A bound rather than a courtesy. The index costs a line per skill on every
/// request, and a workspace that has accumulated a thousand directories under
/// `skills/` should meet a wall and a log line rather than a prompt nobody
/// budgeted for.
pub const MAX_SKILLS: usize = 100;

/// How much of a description an index line carries.
pub const MAX_DESCRIPTION_CHARS: usize = 200;

/// One instruction sheet, as it is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// The directory name.
    pub name: String,
    /// One line, collapsed and bounded. The basis on which a model opens the
    /// file.
    pub description: String,
    /// Everything after the frontmatter, already bounded by
    /// [`SKILL_MAX_BYTES`].
    pub body: String,
    /// Workspace-relative, as the model would pass it to `read_file`.
    pub path: String,
    /// The agents whose catalogue advertises this sheet. Empty means every
    /// agent, which is both the default and what a malformed `agents:` falls
    /// back to.
    pub agents: Vec<String>,
}

/// Whitespace to single spaces, so a wrapped description stays one index line.
static WHITESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+").unwrap_or_else(|_| unreachable!()));

/// Every loadable skill in a workspace, sorted by name.
///
/// Sorted because the result lands in the provider's cached prefix, and a
/// directory order that varies between hosts would move that prefix for no
/// reason anyone could see. The sort key is UTF-16 code units, the order the
/// web client and `read_memories` already agree on.
///
/// A workspace with no `skills/` directory is the empty list. That is the
/// ordinary case rather than a misconfiguration, so it is not logged.
pub fn read_skills(workspace_root: &Path) -> Vec<Skill> {
    let dir = workspace_root.join(SKILLS_DIRNAME);

    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        // `is_dir` follows a symlink; `file_type` does not, which is what keeps
        // a link pointing out of the workspace from ever resolving.
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();
    names.sort_by_key(|name| name.encode_utf16().collect::<Vec<u16>>());

    if names.len() > MAX_SKILLS {
        tracing::warn!(
            dir = %dir.display(),
            found = names.len(),
            max = MAX_SKILLS,
            "more skill directories than the cap; the rest are not advertised"
        );
        names.truncate(MAX_SKILLS);
    }

    names
        .into_iter()
        .filter_map(|name| read_skill(&dir, &name))
        .collect()
}

fn read_skill(dir: &Path, name: &str) -> Option<Skill> {
    let file = dir.join(name).join(SKILL_FILENAME);

    let Ok(text) = fs::read_to_string(&file) else {
        tracing::warn!(skill = name, file = %file.display(), "skill has no readable SKILL.md");
        return None;
    };

    let parsed = parse_frontmatter(&text);

    // The description is the entire basis on which a model decides to open the
    // file, so a skill without one cannot be advertised — an index line reading
    // "**deploy**: " teaches it that the skill is about nothing.
    let description = collapse(parsed.fields.get("description").map_or("", String::as_str));
    if description.is_empty() {
        tracing::warn!(
            skill = name,
            file = %file.display(),
            "skill has no description and is not advertised"
        );
        return None;
    }

    if let Some(declared) = parsed.fields.get("name").map(|value| value.trim())
        && !declared.is_empty()
        && declared != name
    {
        tracing::warn!(
            skill = name,
            declared,
            "skill name disagrees with its directory; the directory wins"
        );
    }

    Some(Skill {
        agents: parse_skill_agents(parsed.fields.get("agents").map(String::as_str), name),
        name: name.to_owned(),
        description: truncate_chars(&description, MAX_DESCRIPTION_CHARS),
        body: truncate_bytes(&parsed.body, SKILL_MAX_BYTES),
        // Built with `/` rather than the host separator, because this string is
        // handed to the model to pass back to `read_file`, which takes POSIX
        // separators on every host.
        path: format!("{SKILLS_DIRNAME}/{name}/{SKILL_FILENAME}"),
    })
}

/// The `agents:` line, as a list of agent ids. Empty means every agent.
///
/// ## Comma-separated is the only form, and that is a property of the reader
///
/// `parse_frontmatter` is not YAML and must never become YAML. Its field
/// pattern is anchored on a letter, so a `- coder` block item does not match: it
/// clears the parent and is discarded. A block list is therefore
/// *indistinguishable* from a bare `agents:` by the time the value reaches here,
/// which is why the empty case warns rather than passing silently — somebody who
/// wrote the YAML they know needs to be told why nothing happened.
///
/// A surrounding `[…]` is stripped first, because `agents: [coder, writer]` is
/// the other thing that person writes and it is three lines to accept.
///
/// ## It fails open, deliberately
///
/// A value that yields no usable id leaves the sheet visible to **every** agent.
/// A skill is prose, not a capability, so the two ways of being wrong are not
/// symmetric: showing a sheet too widely is prompt cost a person can see, and
/// hiding one from everybody is a sheet that silently stopped working with
/// nothing to find.
///
/// At most one warning per sheet. The catalogue is re-read on every turn, so a
/// second line here is a second line forever.
pub fn parse_skill_agents(value: Option<&str>, skill: &str) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };

    let mut ids: Vec<String> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();

    for part in strip_brackets(value.trim()).split(',') {
        let item = unquote(part.trim()).trim().to_lowercase();
        if item.is_empty() {
            continue;
        }
        if !is_agent_id(&item) {
            dropped.push(item);
            continue;
        }
        if !ids.contains(&item) {
            ids.push(item);
        }
    }

    if ids.is_empty() {
        tracing::warn!(
            skill,
            dropped = ?dropped,
            "skill `agents:` names no usable agent id, so the sheet stays visible to every agent; \
             write it as `agents: coder, writer`"
        );
    } else if !dropped.is_empty() {
        tracing::warn!(
            skill,
            dropped = ?dropped,
            "skill `agents:` names something that is not an agent id; those entries are ignored"
        );
    }

    ids
}

/// The sheets one agent's catalogue advertises.
///
/// Separate from [`read_skills`] rather than an option on it, for the reason
/// `render_skills` is separate from its contributor: of the three callers only
/// the contributor filters — both `/skills` surfaces want every sheet so they
/// can say which ones are out of scope — and this way the rule is testable
/// without a filesystem.
///
/// Note that [`MAX_SKILLS`] is applied by [`read_skills`], before this. That is
/// the right order — the cap bounds the per-turn read, and applying it after
/// would mean opening a thousand directories to find the twelve one agent sees
/// — but it does mean that past the cap, which sheets an agent sees follows
/// alphabetical order rather than scope.
pub fn skills_for_agent(skills: &[Skill], agent_id: &str) -> Vec<Skill> {
    let id = agent_id.to_lowercase();
    skills
        .iter()
        .filter(|skill| skill.agents.is_empty() || skill.agents.contains(&id))
        .cloned()
        .collect()
}

/// One surrounding `[…]` pair, so the YAML flow form parses.
fn strip_brackets(text: &str) -> &str {
    text.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(text)
}

/// One matching quote pair.
///
/// Restated rather than imported: `parse_frontmatter` keeps its own copy
/// private, and this one runs per *item* rather than per value —
/// `[coder, "writer"]` is only unquotable after the split.
fn unquote(text: &str) -> &str {
    for quote in ['"', '\''] {
        if text.len() >= 2
            && let Some(rest) = text.strip_prefix(quote)
            && let Some(inner) = rest.strip_suffix(quote)
        {
            return inner;
        }
    }
    text
}

fn collapse(text: &str) -> String {
    WHITESPACE.replace_all(text, " ").trim().to_owned()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    // UTF-16 units, the length JavaScript measures and the one the stored
    // catalogue was written against.
    if text.encode_utf16().count() <= max_chars {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

fn truncate_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    // Cut on a character boundary rather than mid-sequence: the bytes are a
    // budget, and a split multi-byte character would corrupt what follows.
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[Truncated. Read {SKILL_FILENAME} for the rest.]",
        &text[..end]
    )
}
