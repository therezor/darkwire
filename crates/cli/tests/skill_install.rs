//! Copying skill sheets out of a catalogue and into a workspace.
//!
//! Every bound is asserted through the constant rather than a literal, so
//! raising one moves the test with it — and every case that trips a bound
//! asserts the *sentence*, because the sentence is the whole product of a
//! module that never refuses.

// `allow-unwrap-in-tests` covers a `#[test]` body; the fixture helpers beside
// them are ordinary functions, and a helper that returned `Result` would make
// every case carry a `?` for a temporary directory that cannot fail in a way
// the case could act on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture helpers in an integration test"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ghostai::skill_install::{
    MAX_SKILL_FILE_BYTES, MAX_SKILL_FILES, SkillInstallRequest, WrittenSheet, install_skills,
    skills_target_dir,
};
use ghostai_core::GhostPaths;
use ghostai_core::paths::ResolveGhostPaths;
use tempfile::TempDir;

/// One sheet to write into a fixture catalogue.
#[derive(Default)]
struct Sheet {
    /// The frontmatter `agents:` line, when it has one.
    agents: Option<&'static str>,
    /// Extra files beside `SKILL.md`, by relative path.
    extras: Vec<(String, String)>,
}

/// A catalogue's `skills/` holding the named sheets.
fn catalogue(root: &Path, sheets: Vec<(&str, Sheet)>) -> PathBuf {
    let dir = root.join("skills");
    for (name, sheet) in sheets {
        let sheet_dir = dir.join(name);
        std::fs::create_dir_all(&sheet_dir).unwrap();
        let scope = sheet
            .agents
            .map(|agents| format!("agents: {agents}\n"))
            .unwrap_or_default();
        std::fs::write(
            sheet_dir.join("SKILL.md"),
            format!("---\ndescription: What {name} does.\n{scope}---\n\nBody of {name}.\n"),
        )
        .unwrap();
        for (file, contents) in sheet.extras {
            let path = sheet_dir.join(&file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }
    dir
}

fn target(root: &Path) -> PathBuf {
    root.join("workspace").join("skills")
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

fn written(name: &str, files: usize) -> WrittenSheet {
    WrittenSheet {
        name: name.to_owned(),
        files,
    }
}

#[test]
fn copies_a_sheet_and_its_attachments_byte_for_byte() {
    let root = TempDir::new().unwrap();
    let skills = catalogue(
        root.path(),
        vec![(
            "code-review",
            Sheet {
                extras: vec![("checklist.md".to_owned(), "- Read it.\n".to_owned())],
                ..Sheet::default()
            },
        )],
    );
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert_eq!(result.written, vec![written("code-review", 2)]);
    assert_eq!(
        std::fs::read_to_string(target_dir.join("code-review").join("SKILL.md")).unwrap(),
        std::fs::read_to_string(skills.join("code-review").join("SKILL.md")).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(target_dir.join("code-review").join("checklist.md")).unwrap(),
        "- Read it.\n"
    );
}

#[test]
fn does_nothing_at_all_when_the_preset_names_no_sheets() {
    let root = TempDir::new().unwrap();
    let skills = catalogue(root.path(), Vec::new());
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &[],
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert!(result.written.is_empty());
    assert!(!target_dir.exists());
}

#[test]
fn does_nothing_when_there_is_nowhere_to_write() {
    let root = TempDir::new().unwrap();
    let skills = catalogue(root.path(), vec![("code-review", Sheet::default())]);

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: None,
        force: false,
    });

    assert_eq!(
        result,
        ghostai::skill_install::SkillInstallResult::default()
    );
}

#[test]
fn reports_a_sheet_this_catalogue_does_not_carry_and_installs_the_rest() {
    // A warning rather than a refusal: an agent with one fewer index line still runs.
    let root = TempDir::new().unwrap();
    let skills = catalogue(root.path(), vec![("code-review", Sheet::default())]);
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review", "ghost-ops"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert_eq!(result.missing, vec!["ghost-ops"]);
    assert_eq!(
        result
            .written
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["code-review"]
    );
}

#[test]
fn reports_every_sheet_as_missing_when_there_is_no_catalogue_at_all() {
    // The ordinary case for `ghostai agent install` with an operator's own
    // preset on a box that has never fetched a catalogue.
    let root = TempDir::new().unwrap();
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review"]),
        catalogue_skills_dir: None,
        target_dir: Some(&target_dir),
        force: false,
    });

    assert_eq!(result.missing, vec!["code-review"]);
    assert!(result.written.is_empty());
}

#[test]
fn treats_a_directory_with_no_skill_file_as_absent() {
    // A half-checkout. Copying it would report a sheet installed that the
    // reader then silently skips, with nothing anywhere saying why.
    let root = TempDir::new().unwrap();
    let skills = root.path().join("skills");
    std::fs::create_dir_all(skills.join("code-review")).unwrap();
    std::fs::write(skills.join("code-review").join("notes.md"), "Nothing.").unwrap();
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert_eq!(result.missing, vec!["code-review"]);
}

#[test]
fn leaves_a_sheet_the_workspace_already_has_alone() {
    let root = TempDir::new().unwrap();
    let skills = catalogue(root.path(), vec![("code-review", Sheet::default())]);
    let target_dir = target(root.path());
    std::fs::create_dir_all(target_dir.join("code-review")).unwrap();
    std::fs::write(target_dir.join("code-review").join("SKILL.md"), "Mine.\n").unwrap();

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert_eq!(result.kept, vec!["code-review"]);
    assert!(result.written.is_empty());
    assert_eq!(
        std::fs::read_to_string(target_dir.join("code-review").join("SKILL.md")).unwrap(),
        "Mine.\n"
    );
}

#[test]
fn overwrites_with_force_without_deleting_what_it_did_not_bring() {
    // File by file rather than a wipe: removing an operator's own file from
    // inside a sheet directory is a larger claim than `--force` makes.
    let root = TempDir::new().unwrap();
    let skills = catalogue(root.path(), vec![("code-review", Sheet::default())]);
    let target_dir = target(root.path());
    std::fs::create_dir_all(target_dir.join("code-review")).unwrap();
    std::fs::write(target_dir.join("code-review").join("SKILL.md"), "Mine.\n").unwrap();
    std::fs::write(
        target_dir.join("code-review").join("notes.md"),
        "Keep me.\n",
    )
    .unwrap();

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: true,
    });

    assert_eq!(result.written, vec![written("code-review", 1)]);
    assert!(
        std::fs::read_to_string(target_dir.join("code-review").join("SKILL.md"))
            .unwrap()
            .contains("Body of code-review.")
    );
    assert_eq!(
        std::fs::read_to_string(target_dir.join("code-review").join("notes.md")).unwrap(),
        "Keep me.\n"
    );
}

#[test]
#[cfg(unix)]
fn skips_a_symlink_inside_a_sheet_rather_than_following_it() {
    let root = TempDir::new().unwrap();
    let skills = catalogue(root.path(), vec![("code-review", Sheet::default())]);
    let secret = root.path().join("secret.txt");
    std::fs::write(&secret, "not yours").unwrap();
    std::os::unix::fs::symlink(&secret, skills.join("code-review").join("link.txt")).unwrap();
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert_eq!(result.written, vec![written("code-review", 1)]);
    assert_eq!(
        result.warnings,
        vec!["skill \"code-review\" contains a symlink (link.txt), which was skipped"]
    );
}

#[test]
#[cfg(unix)]
fn treats_a_symlinked_sheet_directory_as_absent() {
    let root = TempDir::new().unwrap();
    let skills = catalogue(root.path(), Vec::new());
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("SKILL.md"), "---\ndescription: X.\n---\n\nB.").unwrap();
    std::fs::create_dir_all(&skills).unwrap();
    std::os::unix::fs::symlink(&outside, skills.join("escaped")).unwrap();
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["escaped"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert_eq!(result.missing, vec!["escaped"]);
}

#[test]
fn skips_a_name_that_is_not_a_directory_name() {
    // Defence in depth: the parser refuses one of these, and this is the second
    // place the rule holds — the one where breaking it would be a path outside
    // the workspace rather than a missing index line.
    let root = TempDir::new().unwrap();
    let skills = catalogue(root.path(), vec![("code-review", Sheet::default())]);
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["../escape"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert!(result.written.is_empty());
    assert!(result.missing.is_empty());
    assert_eq!(
        result.warnings,
        vec!["skill \"../escape\" is not a usable name, so it was skipped"]
    );
    assert!(!root.path().join("workspace").join("escape").exists());
}

#[test]
fn stops_a_sheet_that_holds_more_files_than_the_cap() {
    let root = TempDir::new().unwrap();
    let extras = (0..=MAX_SKILL_FILES)
        .map(|index| (format!("note-{index}.md"), "x".to_owned()))
        .collect();
    let skills = catalogue(
        root.path(),
        vec![(
            "code-review",
            Sheet {
                extras,
                ..Sheet::default()
            },
        )],
    );
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert!(result.written.is_empty());
    assert!(
        result.warnings[0].contains("more than 64 files"),
        "{:?}",
        result.warnings
    );
}

#[test]
fn skips_one_oversized_file_and_keeps_the_rest_of_the_sheet() {
    let root = TempDir::new().unwrap();
    let huge = "x".repeat(usize::try_from(MAX_SKILL_FILE_BYTES).unwrap() + 1);
    let skills = catalogue(
        root.path(),
        vec![(
            "code-review",
            Sheet {
                extras: vec![
                    ("huge.bin".to_owned(), huge),
                    ("notes.md".to_owned(), "small".to_owned()),
                ],
                ..Sheet::default()
            },
        )],
    );
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    // The sheet still installs — one file is dropped, not the page.
    assert_eq!(result.written, vec![written("code-review", 2)]);
    assert!(!target_dir.join("code-review").join("huge.bin").exists());
    assert!(target_dir.join("code-review").join("notes.md").exists());
    assert!(
        result.warnings[0].contains("a file over 1024 KB"),
        "{:?}",
        result.warnings
    );
}

#[test]
fn stops_a_sheet_that_nests_deeper_than_the_cap() {
    let root = TempDir::new().unwrap();
    let skills = catalogue(
        root.path(),
        vec![(
            "code-review",
            Sheet {
                extras: vec![("a/b/c/d/e/deep.md".to_owned(), "too far".to_owned())],
                ..Sheet::default()
            },
        )],
    );
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["code-review"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert!(result.written.is_empty());
    assert!(
        result.warnings[0].contains("nests deeper than 4 levels"),
        "{:?}",
        result.warnings
    );
}

#[test]
fn stops_when_the_sheets_together_come_to_more_than_the_total() {
    // The budget is shared across every sheet in one install, so this is the
    // one bound a single sheet cannot trip on its own.
    let root = TempDir::new().unwrap();
    let megabyte = "x".repeat(usize::try_from(MAX_SKILL_FILE_BYTES).unwrap());
    let extras = (0..9)
        .map(|index| (format!("part-{index}.bin"), megabyte.clone()))
        .collect();
    let skills = catalogue(
        root.path(),
        vec![(
            "big",
            Sheet {
                extras,
                ..Sheet::default()
            },
        )],
    );
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["big"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert!(result.written.is_empty());
    assert!(
        result.warnings[0].contains("more than 8 MB"),
        "{:?}",
        result.warnings
    );
}

#[test]
fn warns_when_a_sheet_is_scoped_away_from_the_agent_that_brought_it() {
    let root = TempDir::new().unwrap();
    let skills = catalogue(
        root.path(),
        vec![(
            "triage",
            Sheet {
                agents: Some("team-lead"),
                ..Sheet::default()
            },
        )],
    );
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["triage"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert_eq!(result.written.len(), 1);
    assert_eq!(
        result.warnings,
        vec!["skill \"triage\" is scoped to team-lead, so the coder agent will not see it"]
    );
}

#[test]
fn says_nothing_when_a_sheet_names_this_agent_or_names_none() {
    // An absent `agents:` means every agent, which includes this one. Asserted
    // so that "it did not warn" reads as a decision rather than an oversight.
    let root = TempDir::new().unwrap();
    let skills = catalogue(
        root.path(),
        vec![
            ("shared", Sheet::default()),
            (
                "mine",
                Sheet {
                    agents: Some("writer, coder"),
                    ..Sheet::default()
                },
            ),
        ],
    );
    let target_dir = target(root.path());

    let result = install_skills(&SkillInstallRequest {
        preset_id: "coder",
        names: &names(&["shared", "mine"]),
        catalogue_skills_dir: Some(&skills),
        target_dir: Some(&target_dir),
        force: false,
    });

    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

fn paths(home: &Path) -> GhostPaths {
    GhostPaths::resolve(ResolveGhostPaths {
        root: Some(home.to_string_lossy().into_owned()),
        workspace: None,
        env: Some(HashMap::new()),
        home: Some(home.to_path_buf()),
    })
    .unwrap()
}

#[test]
fn the_target_is_the_default_workspace_and_creates_nothing_while_resolving() {
    // Every install path calls this before it knows whether any preset names a
    // sheet. Creating a directory here would make `<root>/workspace` on every
    // `ghostai agent install`, and would turn an unwritable root into a
    // missing-file error thrown over whatever the real problem was.
    let root = TempDir::new().unwrap();
    let resolved = paths(root.path());

    assert_eq!(
        skills_target_dir(&resolved, "default").unwrap(),
        resolved.workspace.join("skills")
    );
    assert!(!resolved.workspace.exists());
}

#[test]
fn the_target_is_a_named_workspace_when_it_exists() {
    let root = TempDir::new().unwrap();
    let resolved = paths(root.path());
    std::fs::create_dir_all(resolved.workspace.join("acme")).unwrap();

    assert_eq!(
        skills_target_dir(&resolved, "acme").unwrap(),
        resolved.workspace.join("acme").join("skills")
    );
}

#[test]
fn refuses_a_named_workspace_that_does_not_exist() {
    // `workspace_dir_for` validates the shape of an id and joins; the registry
    // is in SQLite and it never asks. Without this, a typo would create a tree
    // no UI ever lists and nothing ever reads.
    let root = TempDir::new().unwrap();

    let error = skills_target_dir(&paths(root.path()), "typo").unwrap_err();

    assert!(
        error.message.contains("no typo workspace"),
        "{}",
        error.message
    );
}

#[test]
fn refuses_an_id_that_is_not_a_workspace_id_at_all() {
    let root = TempDir::new().unwrap();

    let error = skills_target_dir(&paths(root.path()), "../escape").unwrap_err();

    assert!(
        error.message.contains("Not a workspace id"),
        "{}",
        error.message
    );
}
