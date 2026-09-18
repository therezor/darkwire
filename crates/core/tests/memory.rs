//! The memory store: slugs, titles, reading, the atomic save and delete.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod capture;

use std::fs;
use std::path::Path;

use capture::{Captured, capturing};
use darkwire_core::ErrorKind;
use darkwire_core::logger::LogLevel;
use darkwire_core::memory::{
    MAX_MEMORIES, MAX_MEMORY_TITLE_CHARS, MEMORY_MAX_BYTES, Memory, delete_memory, derive_title,
    index_line, memory_slug, read_memories, read_memory, render_memory, save_memory,
};
use tempfile::TempDir;

fn workspace() -> TempDir {
    tempfile::tempdir().unwrap()
}

/// Writes one file into `memory/` verbatim.
fn install(root: &Path, name: &str, contents: &str) {
    fs::create_dir_all(root.join("memory")).unwrap();
    fs::write(root.join("memory").join(name), contents).unwrap();
}

fn stored(root: &Path, key: &str) -> String {
    fs::read_to_string(root.join("memory").join(format!("{key}.md"))).unwrap()
}

const FIXTURE: &str = "# PostgreSQL-backed sessions\n\nSessions expire after 30 days.";

/// Runs `work` with warnings captured, returning its result and the messages.
fn warnings<T>(work: impl FnOnce() -> T) -> (T, Vec<String>) {
    let sink = Captured::new();
    let result = tracing::subscriber::with_default(capturing(&sink, LogLevel::Warn), work);
    (result, sink.messages())
}

mod slug {
    use super::*;

    #[test]
    fn passes_a_key_that_is_already_one_through() {
        assert_eq!(
            memory_slug("run-full-ci-gate").as_deref(),
            Some("run-full-ci-gate")
        );
    }

    #[test]
    fn slugs_a_key_a_person_would_type() {
        assert_eq!(
            memory_slug("UI Stack Preferences").as_deref(),
            Some("ui-stack-preferences")
        );
    }

    #[test]
    fn cannot_produce_a_key_that_leaves_the_folder() {
        assert_eq!(
            memory_slug("../../etc/passwd").as_deref(),
            Some("etc-passwd")
        );
        assert_eq!(memory_slug("a/b").as_deref(), Some("a-b"));
        assert_eq!(memory_slug(".."), None);
    }

    #[test]
    fn is_none_when_nothing_usable_is_left() {
        assert_eq!(memory_slug("???"), None);
        assert_eq!(memory_slug("   "), None);
    }

    #[test]
    fn does_not_end_a_key_on_the_separator_the_cap_landed_on() {
        let long = format!("{} tail", "a".repeat(63));
        assert_eq!(memory_slug(&long).as_deref(), Some("a".repeat(63).as_str()));
    }
}

mod title {
    use super::*;

    #[test]
    fn takes_the_first_h1() {
        assert_eq!(derive_title("# Use Bun\n\nNot npm.", "pm"), "Use Bun");
    }

    #[test]
    fn prefers_an_h1_to_a_line_above_it() {
        // A memory that opens with a note still gets the heading its author
        // wrote rather than whatever happened to be on line one.
        assert_eq!(
            derive_title("Draft, tidy later\n\n# Use Bun", "pm"),
            "Use Bun"
        );
    }

    #[test]
    fn ignores_a_deeper_heading_as_an_h1() {
        assert_eq!(derive_title("## Sub\n\nbody", "pm"), "Sub");
    }

    #[test]
    fn falls_back_to_the_first_line_with_anything_on_it() {
        assert_eq!(
            derive_title("\n\n  Sessions use Redis.\n", "pm"),
            "Sessions use Redis."
        );
    }

    #[test]
    fn falls_back_to_the_key_when_there_is_nothing_to_read() {
        assert_eq!(derive_title("", "auth-sessions"), "auth-sessions");
        assert_eq!(derive_title("   \n\n", "auth-sessions"), "auth-sessions");
        assert_eq!(derive_title("# ###", "auth-sessions"), "auth-sessions");
    }

    #[test]
    fn collapses_whitespace_so_one_memory_is_one_line() {
        assert_eq!(derive_title("#   Use\tBun   now  ", "pm"), "Use Bun now");
    }

    #[test]
    fn strips_the_markdown_a_title_does_not_need() {
        assert_eq!(derive_title("- **Bun**, not `npm`", "pm"), "Bun, not npm");
        assert_eq!(derive_title("> *Redis* sessions", "pm"), "Redis sessions");
        assert_eq!(derive_title("1. ~~Old~~ new", "pm"), "Old new");
        assert_eq!(
            derive_title("# See [the RFC](https://example.com/rfc)", "pm"),
            "See the RFC"
        );
    }

    #[test]
    fn keeps_underscores_because_a_title_is_often_code() {
        // Stripping `_italic_` would cost `snake_case_names`, and the second is
        // far commoner in a title than the first.
        assert_eq!(
            derive_title("# snake_case_names stay", "pm"),
            "snake_case_names stay"
        );
        assert_eq!(derive_title("# _as written_", "pm"), "_as written_");
    }

    #[test]
    fn cuts_a_long_title_without_splitting_a_character() {
        let title = derive_title(&format!("# {}", "é".repeat(200)), "pm");
        assert_eq!(title.chars().count(), MAX_MEMORY_TITLE_CHARS);
        assert!(title.chars().all(|c| c == 'é'));
    }
}

mod render {
    use super::*;

    #[test]
    fn writes_the_content_and_nothing_else() {
        assert_eq!(render_memory("# Title\n\nBody."), "# Title\n\nBody.\n");
    }

    #[test]
    fn trims_and_ends_on_exactly_one_newline() {
        assert_eq!(render_memory("\n\n  Body.  \n\n\n"), "Body.\n");
    }
}

mod reading {
    use super::*;

    #[test]
    fn a_workspace_with_no_folder_has_no_memories() {
        assert_eq!(read_memories(workspace().path()), Vec::new());
    }

    #[test]
    fn reads_a_file_whole_with_its_derived_title() {
        let root = workspace();
        install(root.path(), "auth-sessions.md", FIXTURE);
        assert_eq!(
            read_memories(root.path()),
            vec![Memory {
                key: "auth-sessions".to_owned(),
                title: "PostgreSQL-backed sessions".to_owned(),
                content: FIXTURE.to_owned(),
            }]
        );
    }

    #[test]
    fn sorts_by_key_so_the_cached_prefix_does_not_move() {
        let root = workspace();
        install(root.path(), "zeta.md", "# Z");
        install(root.path(), "alpha.md", "# A");
        install(root.path(), "mid.md", "# M");
        let keys: Vec<String> = read_memories(root.path())
            .into_iter()
            .map(|memory| memory.key)
            .collect();
        assert_eq!(keys, ["alpha", "mid", "zeta"]);
    }

    #[test]
    fn ignores_everything_that_is_not_a_markdown_file() {
        let root = workspace();
        install(root.path(), "real.md", "# Real");
        install(root.path(), "notes.txt", "not a memory");
        fs::create_dir_all(root.path().join("memory").join("folder.md")).unwrap();
        let keys: Vec<String> = read_memories(root.path())
            .into_iter()
            .map(|memory| memory.key)
            .collect();
        assert_eq!(keys, ["real"]);
    }

    #[test]
    fn skips_a_filename_that_is_not_a_usable_key_and_says_which() {
        // The key is the whole address, so a file no key can name would be a
        // line in every prompt that `read` answers "no memory" for, with no way
        // to remove it from inside the session.
        let root = workspace();
        install(root.path(), "good.md", "# Good");
        install(root.path(), "Auth Sessions.md", "# Hand written");
        install(root.path(), "Ünits.md", "# Also hand written");
        let (memories, logged) = warnings(|| read_memories(root.path()));
        let keys: Vec<&str> = memories.iter().map(|memory| memory.key.as_str()).collect();
        assert_eq!(keys, ["good"]);
        assert_eq!(
            logged
                .iter()
                .filter(|line| line.contains("not a usable key"))
                .count(),
            2
        );
    }

    #[test]
    fn stops_at_the_cap_and_says_so() {
        let root = workspace();
        for index in 0..=MAX_MEMORIES {
            install(root.path(), &format!("m{index:04}.md"), "# One");
        }
        let (memories, logged) = warnings(|| read_memories(root.path()));
        assert_eq!(memories.len(), MAX_MEMORIES);
        assert!(logged.iter().any(|line| line.contains("more memory files")));
    }

    #[test]
    fn reads_at_most_the_byte_cap_of_one_file() {
        let root = workspace();
        install(root.path(), "big.md", &"x".repeat(MEMORY_MAX_BYTES * 2));
        let memories = read_memories(root.path());
        assert_eq!(memories[0].content.len(), MEMORY_MAX_BYTES);
    }

    #[test]
    fn a_file_that_cannot_be_read_costs_that_memory_and_not_the_turn() {
        let root = workspace();
        install(root.path(), "good.md", "# Good");
        // Invalid UTF-8 is the readable-but-not-a-string case.
        fs::write(root.path().join("memory").join("bad.md"), [0xff, 0xfe]).unwrap();
        let (memories, logged) = warnings(|| read_memories(root.path()));
        let keys: Vec<&str> = memories.iter().map(|memory| memory.key.as_str()).collect();
        assert_eq!(keys, ["good"]);
        assert!(logged.iter().any(|line| line.contains("could not be read")));
    }
}

mod reading_one {
    use super::*;

    #[test]
    fn opens_the_file_the_key_names() {
        let root = workspace();
        install(root.path(), "auth-sessions.md", FIXTURE);
        let memory = read_memory(root.path(), "auth-sessions").unwrap();
        assert_eq!(memory.content, FIXTURE);
        assert_eq!(memory.title, "PostgreSQL-backed sessions");
    }

    #[test]
    fn slugs_the_key_it_is_handed() {
        let root = workspace();
        install(root.path(), "auth-sessions.md", FIXTURE);
        assert!(read_memory(root.path(), "Auth Sessions").is_some());
    }

    #[test]
    fn is_none_for_a_key_with_nothing_under_it() {
        let root = workspace();
        install(root.path(), "auth-sessions.md", FIXTURE);
        let (found, _) = warnings(|| read_memory(root.path(), "missing"));
        assert_eq!(found, None);
    }

    #[test]
    fn is_none_for_a_key_that_is_not_usable_as_a_filename() {
        assert_eq!(read_memory(workspace().path(), "???"), None);
    }
}

mod saving {
    use super::*;

    #[test]
    fn writes_the_content_verbatim_under_the_key() {
        let root = workspace();
        let saved = save_memory(root.path(), "auth-sessions", FIXTURE).unwrap();
        assert_eq!(saved.key, "auth-sessions");
        assert!(!saved.replaced);
        assert_eq!(saved.total, 1);
        assert_eq!(stored(root.path(), "auth-sessions"), format!("{FIXTURE}\n"));
    }

    #[test]
    fn creates_the_folder_on_the_first_save() {
        let root = workspace();
        save_memory(root.path(), "first", "# First").unwrap();
        assert!(root.path().join("memory").is_dir());
    }

    #[test]
    fn reports_the_key_it_used_when_it_slugged_one() {
        let root = workspace();
        let saved = save_memory(root.path(), "Auth Sessions", FIXTURE).unwrap();
        assert_eq!(saved.key, "auth-sessions");
    }

    #[test]
    fn keeps_two_differently_keyed_memories_apart() {
        let root = workspace();
        save_memory(root.path(), "alpha", "# A").unwrap();
        let second = save_memory(root.path(), "zeta", "# Z").unwrap();
        assert!(!second.replaced);
        assert_eq!(second.total, 2);
    }

    #[test]
    fn saving_a_key_again_replaces_it_whole() {
        let root = workspace();
        save_memory(root.path(), "auth-sessions", "# Postgres\n\nOld detail.").unwrap();
        let again = save_memory(root.path(), "auth-sessions", "# Redis").unwrap();
        assert!(again.replaced);
        assert_eq!(again.total, 1);
        assert_eq!(stored(root.path(), "auth-sessions"), "# Redis\n");
    }

    #[test]
    fn serialises_concurrent_saves_so_both_land_and_the_count_is_right() {
        // A save reads the folder to count it, so two running together would
        // each report a total that did not know about the other's file.
        let root = workspace();
        std::thread::scope(|scope| {
            let a = scope.spawn(|| save_memory(root.path(), "alpha", "# A"));
            let b = scope.spawn(|| save_memory(root.path(), "zeta", "# Z"));
            a.join().unwrap().unwrap();
            b.join().unwrap().unwrap();
        });
        let keys: Vec<String> = read_memories(root.path())
            .into_iter()
            .map(|memory| memory.key)
            .collect();
        assert_eq!(keys, ["alpha", "zeta"]);
        assert_eq!(save_memory(root.path(), "third", "# T").unwrap().total, 3);
    }

    #[test]
    fn refuses_a_key_that_is_not_usable_as_a_filename() {
        let error = save_memory(workspace().path(), "???", FIXTURE).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(error.message.contains("not usable as a filename"));
    }

    #[test]
    fn surfaces_a_folder_that_cannot_be_created() {
        let root = workspace();
        fs::write(root.path().join("memory"), "a file where the folder goes").unwrap();
        let error = save_memory(root.path(), "auth-sessions", FIXTURE).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Storage);
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let root = workspace();
        save_memory(root.path(), "auth-sessions", FIXTURE).unwrap();
        let names: Vec<String> = fs::read_dir(root.path().join("memory"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["auth-sessions.md"]);
    }
}

mod deleting {
    use super::*;

    #[test]
    fn removes_the_file_the_key_names() {
        let root = workspace();
        save_memory(root.path(), "auth-sessions", FIXTURE).unwrap();
        let removed = delete_memory(root.path(), "auth-sessions").unwrap();
        assert!(removed.existed);
        assert_eq!(removed.total, 0);
        assert!(!root.path().join("memory/auth-sessions.md").exists());
    }

    #[test]
    fn leaves_every_other_memory_alone() {
        let root = workspace();
        save_memory(root.path(), "alpha", "# A").unwrap();
        save_memory(root.path(), "zeta", "# Z").unwrap();
        let removed = delete_memory(root.path(), "alpha").unwrap();
        assert_eq!(removed.total, 1);
        let keys: Vec<String> = read_memories(root.path())
            .into_iter()
            .map(|memory| memory.key)
            .collect();
        assert_eq!(keys, ["zeta"]);
    }

    #[test]
    fn a_key_with_nothing_under_it_is_not_an_error() {
        let root = workspace();
        save_memory(root.path(), "alpha", "# A").unwrap();
        let removed = delete_memory(root.path(), "never-written").unwrap();
        assert!(!removed.existed);
        assert_eq!(removed.total, 1);
    }

    #[test]
    fn deleting_twice_leaves_the_same_state() {
        let root = workspace();
        save_memory(root.path(), "alpha", "# A").unwrap();
        assert!(delete_memory(root.path(), "alpha").unwrap().existed);
        assert!(!delete_memory(root.path(), "alpha").unwrap().existed);
    }

    #[test]
    fn slugs_the_key_it_is_handed() {
        let root = workspace();
        save_memory(root.path(), "auth-sessions", FIXTURE).unwrap();
        assert!(delete_memory(root.path(), "Auth Sessions").unwrap().existed);
    }

    #[test]
    fn refuses_a_key_that_is_not_usable_as_a_filename() {
        let error = delete_memory(workspace().path(), "???").unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
    }

    #[test]
    fn a_save_and_a_delete_together_leave_a_consistent_folder() {
        let root = workspace();
        save_memory(root.path(), "alpha", "# A").unwrap();
        std::thread::scope(|scope| {
            let a = scope.spawn(|| save_memory(root.path(), "zeta", "# Z"));
            let b = scope.spawn(|| delete_memory(root.path(), "alpha"));
            a.join().unwrap().unwrap();
            b.join().unwrap().unwrap();
        });
        let keys: Vec<String> = read_memories(root.path())
            .into_iter()
            .map(|memory| memory.key)
            .collect();
        assert_eq!(keys, ["zeta"]);
    }
}

mod index {
    use super::*;

    #[test]
    fn is_the_key_then_the_title() {
        let root = workspace();
        install(root.path(), "auth-sessions.md", FIXTURE);
        let memories = read_memories(root.path());
        assert_eq!(
            index_line(&memories[0]),
            "auth-sessions: PostgreSQL-backed sessions"
        );
    }
}
