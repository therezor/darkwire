//! Reading instruction sheets off a workspace.
//!
//! Nothing here fails: a skill folder is whatever a person or a previous turn
//! left there, and a malformed file must cost that one skill rather than every
//! turn on the workspace.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot be written is a failing test either way"
)]

use std::fs;
use std::path::Path;

use ghostai_agent::skills::{
    MAX_DESCRIPTION_CHARS, MAX_SKILLS, SKILL_FILENAME, SKILL_MAX_BYTES, SKILLS_DIRNAME, Skill,
    parse_skill_agents, read_skills, skills_for_agent,
};
use tempfile::TempDir;

fn write_skill(root: &Path, name: &str, contents: &str) {
    let dir = root.join(SKILLS_DIRNAME).join(name);
    fs::create_dir_all(&dir).expect("a skill directory");
    fs::write(dir.join(SKILL_FILENAME), contents).expect("a skill file");
}

fn sheet(description: &str, body: &str) -> String {
    format!("---\ndescription: {description}\n---\n\n{body}")
}

#[test]
fn a_workspace_with_no_skills_directory_is_the_empty_list() {
    // The ordinary case rather than a misconfiguration, so it is not logged.
    let workspace = TempDir::new().unwrap();
    assert!(read_skills(workspace.path()).is_empty());
}

#[test]
fn it_reads_a_sheet_and_names_it_after_its_directory() {
    let workspace = TempDir::new().unwrap();
    write_skill(
        workspace.path(),
        "deploy",
        // The frontmatter `name` disagrees, and the directory wins: it is what
        // appears in the path the model is given.
        "---\nname: something-else\ndescription: How to ship it.\n---\n\nRun the script.",
    );

    let skills = read_skills(workspace.path());
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "deploy");
    assert_eq!(skills[0].description, "How to ship it.");
    assert_eq!(skills[0].body, "Run the script.");
    // POSIX separators on every host, because the model passes this back to a
    // file tool.
    assert_eq!(skills[0].path, "skills/deploy/SKILL.md");
    assert!(skills[0].agents.is_empty());
}

#[test]
fn sheets_come_back_sorted_so_the_cached_prefix_does_not_move() {
    let workspace = TempDir::new().unwrap();
    for name in ["zeta", "alpha", "mu"] {
        write_skill(workspace.path(), name, &sheet("A sheet.", "body"));
    }

    let names: Vec<&str> = read_skills(workspace.path())
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>()
        .iter()
        .map(String::as_str)
        .map(|name| match name {
            "alpha" => "alpha",
            "mu" => "mu",
            _ => "zeta",
        })
        .collect();
    assert_eq!(names, vec!["alpha", "mu", "zeta"]);
}

#[test]
fn a_sheet_with_no_description_is_not_advertised() {
    // An index line reading "**deploy**: " teaches the model that the skill is
    // about nothing.
    let workspace = TempDir::new().unwrap();
    write_skill(
        workspace.path(),
        "deploy",
        "---\nname: deploy\n---\n\nBody.",
    );
    write_skill(workspace.path(), "test", &sheet("How to test.", "Body."));

    let skills = read_skills(workspace.path());
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "test");
}

#[test]
fn a_directory_with_no_readable_sheet_costs_only_itself() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join(SKILLS_DIRNAME).join("empty")).unwrap();
    write_skill(workspace.path(), "real", &sheet("A real one.", "Body."));

    let skills = read_skills(workspace.path());
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "real");
}

#[test]
fn a_file_directly_under_skills_is_not_a_skill() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join(SKILLS_DIRNAME)).unwrap();
    fs::write(
        workspace.path().join(SKILLS_DIRNAME).join("README.md"),
        "not a skill",
    )
    .unwrap();

    assert!(read_skills(workspace.path()).is_empty());
}

#[test]
fn a_long_description_is_cut_to_one_index_line() {
    let workspace = TempDir::new().unwrap();
    let long = "x".repeat(MAX_DESCRIPTION_CHARS + 50);
    write_skill(workspace.path(), "deploy", &sheet(&long, "Body."));

    let skills = read_skills(workspace.path());
    assert_eq!(skills[0].description.chars().count(), MAX_DESCRIPTION_CHARS);
    assert!(skills[0].description.ends_with('…'));
}

#[test]
fn a_wrapped_description_is_collapsed_to_one_line() {
    let workspace = TempDir::new().unwrap();
    write_skill(
        workspace.path(),
        "deploy",
        "---\ndescription:   How   to\tship\n---\n\nBody.",
    );

    assert_eq!(read_skills(workspace.path())[0].description, "How to ship");
}

#[test]
fn a_long_body_is_bounded_and_says_where_the_rest_is() {
    // A body the model opens lands in the transcript and is re-sent on every
    // later iteration of that turn at full price.
    let workspace = TempDir::new().unwrap();
    let long = "y".repeat(SKILL_MAX_BYTES + 500);
    write_skill(workspace.path(), "deploy", &sheet("A sheet.", &long));

    let body = &read_skills(workspace.path())[0].body;
    assert!(body.len() < SKILL_MAX_BYTES + 100);
    assert!(body.contains("[Truncated — read SKILL.md for the rest.]"));
}

#[test]
fn a_body_cut_mid_character_stays_valid_text() {
    let workspace = TempDir::new().unwrap();
    // Three-byte characters, so the budget lands inside one.
    let body = "€".repeat(SKILL_MAX_BYTES);
    write_skill(workspace.path(), "deploy", &sheet("A sheet.", &body));

    let read = &read_skills(workspace.path())[0].body;
    assert!(read.starts_with('€'));
    assert!(read.contains("Truncated"));
}

#[test]
fn more_directories_than_the_cap_meet_a_wall() {
    let workspace = TempDir::new().unwrap();
    for n in 0..MAX_SKILLS + 5 {
        write_skill(
            workspace.path(),
            &format!("skill-{n:03}"),
            &sheet("A.", "B"),
        );
    }

    // The index costs a line per skill on every request.
    assert_eq!(read_skills(workspace.path()).len(), MAX_SKILLS);
}

// Scope

#[test]
fn an_agents_line_narrows_a_sheet_to_the_agents_it_names() {
    assert_eq!(
        parse_skill_agents(Some("coder, writer"), "s"),
        vec!["coder".to_owned(), "writer".to_owned()]
    );
    // The YAML flow form is the other thing a person writes.
    assert_eq!(
        parse_skill_agents(Some("[coder, \"writer\"]"), "s"),
        vec!["coder".to_owned(), "writer".to_owned()]
    );
    // Case-folded, de-duplicated, and quotes stripped per item.
    assert_eq!(
        parse_skill_agents(Some("'Coder', coder"), "s"),
        vec!["coder".to_owned()]
    );
    assert_eq!(parse_skill_agents(None, "s"), Vec::<String>::new());
}

#[test]
fn it_fails_open_when_the_line_names_no_usable_id() {
    // A skill is prose, not a capability: showing a sheet too widely is prompt
    // cost a person can see, and hiding one from everybody is a sheet that
    // silently stopped working with nothing to find.
    assert!(parse_skill_agents(Some(""), "s").is_empty());
    assert!(parse_skill_agents(Some("   "), "s").is_empty());
    assert!(parse_skill_agents(Some("NOT AN ID!, %%%"), "s").is_empty());
    // A usable id beside an unusable one keeps the usable one.
    assert_eq!(
        parse_skill_agents(Some("coder, !!!"), "s"),
        vec!["coder".to_owned()]
    );
}

#[test]
fn a_catalogue_shows_an_agent_its_own_sheets_and_the_unscoped_ones() {
    let skills = vec![
        Skill {
            name: "everyone".to_owned(),
            description: "a".to_owned(),
            body: String::new(),
            path: "skills/everyone/SKILL.md".to_owned(),
            agents: Vec::new(),
        },
        Skill {
            name: "coder-only".to_owned(),
            description: "b".to_owned(),
            body: String::new(),
            path: "skills/coder-only/SKILL.md".to_owned(),
            agents: vec!["coder".to_owned()],
        },
    ];

    let coder = skills_for_agent(&skills, "Coder");
    assert_eq!(coder.len(), 2);
    let writer = skills_for_agent(&skills, "writer");
    assert_eq!(writer.len(), 1);
    assert_eq!(writer[0].name, "everyone");
}

#[test]
fn the_agents_line_is_read_off_the_file() {
    let workspace = TempDir::new().unwrap();
    write_skill(
        workspace.path(),
        "deploy",
        "---\ndescription: Ship it.\nagents: coder\n---\n\nBody.",
    );

    let skills = read_skills(workspace.path());
    assert_eq!(skills[0].agents, vec!["coder".to_owned()]);
    assert!(skills_for_agent(&skills, "writer").is_empty());
}
