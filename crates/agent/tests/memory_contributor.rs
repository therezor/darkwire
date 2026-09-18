//! Memory as a prompt section: an index, not the contents.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot be written is a failing test either way"
)]

use std::path::Path;

use darkwire_agent::memory_contributor::{MemoryContributor, render_memory_section};
use darkwire_agent::prompt::{ContextContributor, StaticPromptContext};
use darkwire_core::memory::{Memory, MemoryInput, MemoryType, save_memory};
use tempfile::TempDir;

fn memory(name: &str, description: &str, kind: MemoryType) -> Memory {
    Memory {
        name: name.to_owned(),
        description: description.to_owned(),
        memory_type: kind,
        body: String::new(),
        path: format!("memory/{name}.md"),
    }
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
fn an_empty_folder_is_no_section_rather_than_a_paragraph_about_nothing() {
    // Checked before the template is resolved: rendering the built-in would
    // place a paragraph explaining an index that is not there.
    assert_eq!(render_memory_section(&[], None), "");
    assert_eq!(render_memory_section(&[], Some("## Always")), "");
}

#[test]
fn the_path_comes_first_because_that_is_what_read_file_takes() {
    let section = render_memory_section(
        &[
            memory("units", "Prefers metric.", MemoryType::User),
            memory("deploy-target", "Ships to fly.io.", MemoryType::Project),
        ],
        None,
    );

    assert!(section.starts_with("## Memory"));
    assert!(section.contains("- `memory/units.md` (user): Prefers metric."));
    assert!(section.contains("- `memory/deploy-target.md` (project): Ships to fly.io."));
    // The kind is stated because a preference and a pointer to a document are
    // not the same claim.
    assert!(section.contains("(user)"));
    // Two sentences do work: it names `read_file`, and it says a repeated name
    // replaces.
    assert!(section.contains("read_file"));
    assert!(section.contains("replaces"));
    // The bodies are not here.
    assert!(!section.contains("body"));
}

#[test]
fn an_operator_may_reword_the_section_or_delete_it() {
    let memories = [memory("units", "Prefers metric.", MemoryType::User)];

    let reworded = render_memory_section(&memories, Some("# Notes ({{count}})\n\n{{index}}"));
    assert!(reworded.starts_with("# Notes (1)"));
    assert!(reworded.contains("memory/units.md"));

    assert_eq!(render_memory_section(&memories, Some(" ")), "");
    assert!(render_memory_section(&memories, Some("")).starts_with("## Memory"));
}

#[tokio::test]
async fn the_contributor_reads_the_workspace_every_turn() {
    let workspace = TempDir::new().unwrap();
    let contributor = MemoryContributor::new();
    let context = context(workspace.path());

    assert_eq!(contributor.name(), "memory");
    assert_eq!(contributor.static_section(&context).await, None);

    save_memory(
        workspace.path(),
        &MemoryInput {
            name: "units".to_owned(),
            description: "Prefers metric.".to_owned(),
            memory_type: MemoryType::User,
            body: "They said so.".to_owned(),
        },
    )
    .expect("a saved memory");

    let section = contributor
        .static_section(&context)
        .await
        .expect("a section");
    assert!(section.contains("memory/units.md"));
    // Nothing about memory varies per message, so there is no runtime half.
    assert_eq!(
        contributor.runtime_section(&darkwire_agent::prompt::RuntimePromptContext::default()),
        None
    );
}

#[tokio::test]
async fn a_deleted_template_places_nothing_even_with_memories_to_show() {
    let workspace = TempDir::new().unwrap();
    save_memory(
        workspace.path(),
        &MemoryInput {
            name: "units".to_owned(),
            description: "Prefers metric.".to_owned(),
            memory_type: MemoryType::User,
            body: "They said so.".to_owned(),
        },
    )
    .expect("a saved memory");

    let contributor = MemoryContributor::new().with_template(" ");
    assert_eq!(
        contributor.static_section(&context(workspace.path())).await,
        None
    );
}
