//! The skills catalogue as a prompt section.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot be written is a failing test either way"
)]

use std::fs;
use std::path::Path;

use darkwire_agent::prompt::{ContextContributor, StaticPromptContext};
use darkwire_agent::skills::{SKILL_FILENAME, SKILLS_DIRNAME, Skill};
use darkwire_agent::skills_contributor::{SkillsContributor, render_skills};
use tempfile::TempDir;

fn skill(name: &str, description: &str, agents: Vec<String>) -> Skill {
    Skill {
        name: name.to_owned(),
        description: description.to_owned(),
        body: String::new(),
        path: format!("skills/{name}/SKILL.md"),
        agents,
    }
}

fn write_skill(root: &Path, name: &str, description: &str, agents: Option<&str>) {
    let dir = root.join(SKILLS_DIRNAME).join(name);
    fs::create_dir_all(&dir).expect("a skill directory");
    let scope = agents.map(|a| format!("agents: {a}\n")).unwrap_or_default();
    fs::write(
        dir.join(SKILL_FILENAME),
        format!("---\ndescription: {description}\n{scope}---\n\nBody."),
    )
    .expect("a skill file");
}

fn context(root: &Path) -> StaticPromptContext {
    StaticPromptContext {
        workspace_root: root.to_string_lossy().into_owned(),
        workspace_id: "default".to_owned(),
        session_key: "web:1".to_owned(),
        agent_id: None,
        channel: "cli".to_owned(),
    }
}

#[test]
fn an_empty_catalogue_is_no_section_rather_than_a_bare_heading() {
    assert_eq!(render_skills(&[], None), "");
}

#[test]
fn a_catalogue_is_one_index_line_per_sheet() {
    // The model opens the file itself when the description tells it the skill
    // applies: about twenty tokens per skill instead of the whole sheet.
    let section = render_skills(
        &[
            skill("deploy", "How to ship it.", Vec::new()),
            skill("test", "How to test it.", Vec::new()),
        ],
        None,
    );

    assert!(section.starts_with("## Skills"));
    assert!(section.contains("`skills/deploy/SKILL.md` — **deploy**: How to ship it."));
    assert!(section.contains("`skills/test/SKILL.md` — **test**: How to test it."));
    // It names `read_file`, because a list of paths with no instruction to open
    // them reads as a list of things that exist.
    assert!(section.contains("read_file"));
}

#[test]
fn an_operator_may_reword_the_section_or_delete_it() {
    let skills = [skill("deploy", "Ship it.", Vec::new())];

    let reworded = render_skills(&skills, Some("# Sheets ({{count}}){{index}}"));
    assert!(reworded.starts_with("# Sheets (1)"));
    assert!(reworded.contains("**deploy**"));

    // The same "empty inherits, whitespace deletes" contract every section
    // template keeps.
    assert_eq!(render_skills(&skills, Some(" ")), "");
    assert!(render_skills(&skills, Some("")).starts_with("## Skills"));

    // `indexLines` is the same lines without the leading break, for a template
    // that places its own.
    let raw = render_skills(&skills, Some("[{{indexLines}}]"));
    assert!(raw.starts_with("[- `skills/deploy"));
}

#[tokio::test]
async fn the_contributor_reads_the_workspace_every_turn() {
    let workspace = TempDir::new().unwrap();
    let contributor = SkillsContributor::for_default_agent();
    let context = context(workspace.path());

    // A workspace with nothing in it places no section.
    assert_eq!(contributor.static_section(&context).await, None);
    assert_eq!(contributor.name(), "skills");

    // Nothing is cached on the instance: one loop serves every session on an
    // agent, and those sessions can be bound to different workspaces.
    write_skill(workspace.path(), "deploy", "Ship it.", None);
    let section = contributor
        .static_section(&context)
        .await
        .expect("a section");
    assert!(section.contains("**deploy**"));
    // And no runtime half at all.
    assert_eq!(
        contributor.runtime_section(&darkwire_agent::prompt::RuntimePromptContext::default()),
        None
    );
}

#[tokio::test]
async fn an_agent_every_sheet_is_scoped_away_from_gets_no_section() {
    let workspace = TempDir::new().unwrap();
    write_skill(workspace.path(), "deploy", "Ship it.", Some("coder"));
    let context = context(workspace.path());

    // The filter runs before the emptiness check, so this is no section rather
    // than a heading with nothing under it.
    assert_eq!(
        SkillsContributor::new("writer")
            .static_section(&context)
            .await,
        None
    );
    assert!(
        SkillsContributor::new("coder")
            .static_section(&context)
            .await
            .is_some()
    );
}

#[tokio::test]
async fn a_deleted_template_places_nothing_even_with_sheets_to_show() {
    let workspace = TempDir::new().unwrap();
    write_skill(workspace.path(), "deploy", "Ship it.", None);

    let contributor = SkillsContributor::for_default_agent().with_template(" ");
    assert_eq!(
        contributor.static_section(&context(workspace.path())).await,
        None
    );
}
