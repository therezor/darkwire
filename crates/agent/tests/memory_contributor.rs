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
use darkwire_core::memory::{Memory, save_memory};
use tempfile::TempDir;

fn memory(key: &str, title: &str) -> Memory {
    Memory {
        key: key.to_owned(),
        title: title.to_owned(),
        content: format!("# {title}\n\nThe content, which is not in the section."),
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
fn a_line_is_the_key_then_the_title_and_nothing_else() {
    let section = render_memory_section(
        &[
            memory("units", "Prefers metric"),
            memory("deploy-target", "Ships to fly.io"),
        ],
        None,
    );

    assert!(section.starts_with("## Memory"));
    assert!(section.contains("units: Prefers metric"));
    assert!(section.contains("deploy-target: Ships to fly.io"));
    // The key is the whole address the tool takes, so the line spends nothing
    // on a path, a bullet or a kind label.
    assert!(!section.contains("memory/"));
    assert!(!section.contains("- `"));
    // The one thing the data cannot say for itself. Every other rule is in
    // the tool description, which is paid for once rather than every request.
    assert!(section.contains("Do not guess a key."));
    assert!(!section.contains("Saving"));
    // The contents are not here.
    assert!(!section.contains("which is not in the section"));
}

#[test]
fn an_operator_may_reword_the_section_or_delete_it() {
    let memories = [memory("units", "Prefers metric")];

    let reworded = render_memory_section(&memories, Some("# Notes ({{count}})\n\n{{index}}"));
    assert!(reworded.starts_with("# Notes (1)"));
    assert!(reworded.contains("units: Prefers metric"));

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
        "units",
        "# Prefers metric\n\nThey said so.",
    )
    .expect("a saved memory");

    let section = contributor
        .static_section(&context)
        .await
        .expect("a section");
    assert!(section.contains("units: Prefers metric"));
    assert!(!section.contains("They said so."));
    // Nothing about memory varies per message, so there is no runtime half.
    assert_eq!(
        contributor.runtime_section(&darkwire_agent::prompt::RuntimePromptContext::default()),
        None
    );
}

#[tokio::test]
async fn a_deleted_template_places_nothing_even_with_memories_to_show() {
    let workspace = TempDir::new().unwrap();
    save_memory(workspace.path(), "units", "# Prefers metric").expect("a saved memory");

    let contributor = MemoryContributor::new().with_template(" ");
    assert_eq!(
        contributor.static_section(&context(workspace.path())).await,
        None
    );
}
