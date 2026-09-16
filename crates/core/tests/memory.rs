//! The memory store: slugs, rendering, reading and the atomic save.
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
use darkwire_core::frontmatter::parse_frontmatter;
use darkwire_core::logger::LogLevel;
use darkwire_core::memory::{
    MAX_MEMORIES, MEMORY_MAX_BYTES, MEMORY_TYPES, Memory, MemoryInput, MemoryType, memory_slug,
    read_memories, render_index, render_memory, save_memory,
};
use tempfile::TempDir;

fn workspace() -> TempDir {
    tempfile::tempdir().unwrap()
}

/// Writes one file into `memory/` verbatim, frontmatter and all.
fn install(root: &Path, name: &str, contents: &str) {
    fs::create_dir_all(root.join("memory")).unwrap();
    fs::write(root.join("memory").join(name), contents).unwrap();
}

fn fixture() -> MemoryInput {
    MemoryInput {
        name: "ui-stack-preferences".to_owned(),
        description: "no shadcn/ui; Tailwind in rem, not px".to_owned(),
        memory_type: MemoryType::User,
        body: "The user wants an explicit design token layer.".to_owned(),
    }
}

fn named(name: &str) -> MemoryInput {
    MemoryInput {
        name: name.to_owned(),
        ..fixture()
    }
}

fn index_of(root: &Path) -> String {
    fs::read_to_string(root.join("memory").join("MEMORY.md")).unwrap()
}

/// Runs `work` with warnings captured, returning its result and the messages.
fn warnings<T>(work: impl FnOnce() -> T) -> (T, Vec<String>) {
    let sink = Captured::new();
    let result = tracing::subscriber::with_default(capturing(&sink, LogLevel::Warn), work);
    (result, sink.messages())
}

mod slug {
    use super::*;

    #[test]
    fn passes_a_name_that_is_already_one_through() {
        assert_eq!(
            memory_slug("run-full-ci-gate").as_deref(),
            Some("run-full-ci-gate")
        );
    }

    #[test]
    fn slugs_a_name_a_person_would_type() {
        assert_eq!(
            memory_slug("UI Stack Preferences").as_deref(),
            Some("ui-stack-preferences")
        );
    }

    #[test]
    fn cannot_produce_a_name_that_leaves_the_folder() {
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
    fn refuses_the_index_whatever_case_it_is_asked_for_in() {
        assert_eq!(memory_slug("memory"), None);
        assert_eq!(memory_slug("MEMORY"), None);
    }

    #[test]
    fn does_not_end_a_name_on_the_separator_the_cap_landed_on() {
        let long = format!("{} tail", "a".repeat(63));
        assert_eq!(memory_slug(&long).as_deref(), Some("a".repeat(63).as_str()));
    }
}

mod types {
    use super::*;

    #[test]
    fn spell_and_parse_every_kind() {
        for kind in MEMORY_TYPES {
            assert_eq!(MemoryType::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(MemoryType::parse("nonsense"), None);
    }
}

mod render {
    use super::*;

    #[test]
    fn round_trips_through_the_frontmatter_parser() {
        // The file is written with a *nested* `metadata.type` and read back
        // through a parser that flattens it to a dotted key.
        let parsed = parse_frontmatter(&render_memory(&fixture()));
        assert_eq!(parsed.fields["name"], "ui-stack-preferences");
        assert_eq!(
            parsed.fields["description"],
            "no shadcn/ui; Tailwind in rem, not px"
        );
        assert_eq!(parsed.fields["metadata.type"], "user");
        assert_eq!(
            parsed.body,
            "The user wants an explicit design token layer."
        );
    }

    #[test]
    fn collapses_a_description_that_arrived_on_two_lines() {
        let text = render_memory(&MemoryInput {
            description: "one\n  two".to_owned(),
            ..fixture()
        });
        assert!(text.contains("description: one two"));
    }

    #[test]
    fn the_index_links_each_memory_relatively_with_its_kind() {
        let root = workspace();
        save_memory(root.path(), &fixture()).unwrap();
        assert!(index_of(root.path()).contains(
            "- [ui-stack-preferences](ui-stack-preferences.md) _(user)_ — no shadcn/ui; Tailwind in rem, not px",
        ));
    }

    #[test]
    fn the_index_says_so_when_there_is_nothing() {
        assert!(render_index(&[]).contains("_Nothing recorded yet._"));
    }

    #[test]
    fn the_index_lists_a_memory_by_hand_too() {
        let memory = Memory {
            name: "alpha".to_owned(),
            description: "a".to_owned(),
            memory_type: MemoryType::Reference,
            body: String::new(),
            path: "memory/alpha.md".to_owned(),
        };
        assert!(render_index(&[memory]).contains("- [alpha](alpha.md) _(reference)_ — a"));
    }
}

mod reading {
    use super::*;

    #[test]
    fn is_empty_for_a_workspace_that_has_none() {
        assert!(read_memories(workspace().path()).is_empty());
    }

    #[test]
    fn says_nothing_about_a_workspace_that_simply_has_no_memory() {
        let root = workspace();
        let (_, messages) = warnings(|| read_memories(root.path()));
        assert!(messages.is_empty());
    }

    #[test]
    fn sorts_by_name() {
        let root = workspace();
        save_memory(root.path(), &named("zeta")).unwrap();
        save_memory(root.path(), &named("alpha")).unwrap();
        let names: Vec<String> = read_memories(root.path())
            .into_iter()
            .map(|memory| memory.name)
            .collect();
        assert_eq!(names, ["alpha", "zeta"]);
    }

    #[test]
    fn carries_the_path_the_model_hands_to_read_file() {
        let root = workspace();
        save_memory(root.path(), &named("alpha")).unwrap();
        assert_eq!(read_memories(root.path())[0].path, "memory/alpha.md");
    }

    #[test]
    fn skips_the_generated_index_rather_than_advertising_it() {
        let root = workspace();
        save_memory(root.path(), &fixture()).unwrap();
        let memories = read_memories(root.path());
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].name, "ui-stack-preferences");
    }

    #[test]
    fn skips_a_lowercase_memory_md_left_by_the_old_format_silently() {
        let root = workspace();
        install(
            root.path(),
            "memory.md",
            "Always deploy with `make release`.\n",
        );
        let (memories, messages) = warnings(|| read_memories(root.path()));
        assert!(memories.is_empty());
        assert!(messages.is_empty());
    }

    #[test]
    fn ignores_a_file_that_is_not_markdown() {
        let root = workspace();
        install(root.path(), "notes.txt", "not a memory");
        assert!(read_memories(root.path()).is_empty());
    }

    #[test]
    fn skips_a_memory_with_no_description_and_says_why() {
        let root = workspace();
        install(
            root.path(),
            "broken.md",
            "---\nname: broken\n---\n\nA body.\n",
        );
        let (memories, messages) = warnings(|| read_memories(root.path()));
        assert!(memories.is_empty());
        assert_eq!(messages, ["memory has no description; skipped"]);
    }

    #[test]
    fn reads_an_unrecognised_kind_as_project_and_warns() {
        let root = workspace();
        install(
            root.path(),
            "odd.md",
            "---\ndescription: something\nmetadata:\n  type: nonsense\n---\n\nBody.\n",
        );
        let (memories, messages) = warnings(|| read_memories(root.path()));
        assert_eq!(memories[0].memory_type, MemoryType::Project);
        assert_eq!(
            messages,
            ["memory has an unrecognised metadata.type; read as project"]
        );
    }

    #[test]
    fn reads_a_hand_written_memory_with_no_kind_as_project_silently() {
        let root = workspace();
        install(
            root.path(),
            "handwritten.md",
            "---\ndescription: typed by hand\n---\n\nB.\n",
        );
        let (memories, messages) = warnings(|| read_memories(root.path()));
        assert_eq!(memories[0].memory_type, MemoryType::Project);
        assert!(messages.is_empty());
    }

    #[test]
    fn bounds_what_it_reads_on_a_character_boundary() {
        let root = workspace();
        let head = "---\ndescription: big\n---\n\n";
        install(
            root.path(),
            "big.md",
            &format!("{head}{}", "é".repeat(MEMORY_MAX_BYTES)),
        );
        let memory = read_memories(root.path()).remove(0);
        assert!(memory.body.len() <= MEMORY_MAX_BYTES);
        assert!(memory.body.chars().all(|c| c == 'é'));
    }

    #[test]
    fn bounds_the_description_it_advertises() {
        let root = workspace();
        install(
            root.path(),
            "wordy.md",
            &format!("---\ndescription: {}\n---\n\nB.\n", "d".repeat(300)),
        );
        assert_eq!(read_memories(root.path())[0].description.len(), 200);
    }

    #[test]
    fn caps_how_many_it_advertises_and_says_so() {
        let root = workspace();
        for n in 0..=MAX_MEMORIES {
            install(
                root.path(),
                &format!("m{n:04}.md"),
                "---\ndescription: one\nmetadata:\n  type: project\n---\n\nB.\n",
            );
        }
        let (memories, messages) = warnings(|| read_memories(root.path()));
        assert_eq!(memories.len(), MAX_MEMORIES);
        assert_eq!(
            messages,
            ["more memory files than the cap; the rest are not advertised"]
        );
    }

    #[test]
    fn skips_a_directory_named_like_a_memory() {
        let root = workspace();
        save_memory(root.path(), &named("good")).unwrap();
        fs::create_dir(root.path().join("memory").join("a-directory.md")).unwrap();
        let names: Vec<String> = read_memories(root.path())
            .into_iter()
            .map(|memory| memory.name)
            .collect();
        assert_eq!(names, ["good"]);
    }

    #[cfg(unix)]
    #[test]
    fn costs_one_memory_when_a_file_cannot_be_read_not_the_call() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = workspace();
        save_memory(root.path(), &named("good")).unwrap();
        save_memory(root.path(), &named("sealed")).unwrap();
        let sealed = root.path().join("memory").join("sealed.md");
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_to_string(&sealed).is_ok() {
            // Running as a user permissions do not bind; nothing to test.
            return;
        }

        let (memories, messages) = warnings(|| read_memories(root.path()));
        let names: Vec<&str> = memories.iter().map(|memory| memory.name.as_str()).collect();
        assert_eq!(names, ["good"]);
        assert_eq!(messages, ["memory could not be read"]);
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

mod saving {
    use super::*;

    #[test]
    fn creates_the_folder_and_writes_a_memory_the_reader_can_load_back() {
        let root = workspace();
        let result = save_memory(root.path(), &fixture()).unwrap();
        assert_eq!(result.name, "ui-stack-preferences");
        assert_eq!(result.path, "memory/ui-stack-preferences.md");
        assert!(!result.replaced);
        assert_eq!(result.total, 1);
        let memory = read_memories(root.path()).remove(0);
        assert_eq!(memory.description, "no shadcn/ui; Tailwind in rem, not px");
        assert_eq!(memory.memory_type, MemoryType::User);
    }

    #[test]
    fn replaces_a_memory_of_the_same_name_rather_than_adding_a_second() {
        let root = workspace();
        save_memory(
            root.path(),
            &MemoryInput {
                body: "The old answer.".to_owned(),
                ..fixture()
            },
        )
        .unwrap();
        let result = save_memory(
            root.path(),
            &MemoryInput {
                body: "The new answer.".to_owned(),
                ..fixture()
            },
        )
        .unwrap();
        assert!(result.replaced);
        assert_eq!(result.total, 1);
        assert_eq!(read_memories(root.path())[0].body, "The new answer.");
    }

    #[test]
    fn regenerates_the_index_on_every_save() {
        let root = workspace();
        save_memory(root.path(), &named("alpha")).unwrap();
        save_memory(root.path(), &named("zeta")).unwrap();
        let text = index_of(root.path());
        assert!(text.contains("(alpha.md)"));
        assert!(text.contains("(zeta.md)"));
    }

    #[test]
    fn reports_the_name_it_actually_used() {
        let root = workspace();
        let result = save_memory(root.path(), &named("Build Conventions")).unwrap();
        assert_eq!(result.name, "build-conventions");
        assert_eq!(result.path, "memory/build-conventions.md");
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let root = workspace();
        save_memory(root.path(), &fixture()).unwrap();
        let leftovers: Vec<String> = fs::read_dir(root.path().join("memory"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| Path::new(name).extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn moves_aside_a_memory_md_that_resolves_to_the_index_path() {
        let root = workspace();
        let target = root.path().join("memory").join("MEMORY.md");
        install(
            root.path(),
            "MEMORY.md",
            "Always deploy with `make release`.\n",
        );
        save_memory(root.path(), &fixture()).unwrap();
        let mut aside = target.clone().into_os_string();
        aside.push(".replaced");
        assert_eq!(
            fs::read_to_string(aside).unwrap(),
            "Always deploy with `make release`.\n"
        );
        assert!(
            fs::read_to_string(&target)
                .unwrap()
                .contains("(ui-stack-preferences.md)")
        );
    }

    #[test]
    fn overwrites_an_index_it_wrote_itself_without_moving_it_aside() {
        let root = workspace();
        save_memory(root.path(), &named("alpha")).unwrap();
        save_memory(root.path(), &named("zeta")).unwrap();
        let replaced = fs::read_dir(root.path().join("memory"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| {
                Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext == "replaced")
            })
            .count();
        assert_eq!(replaced, 0);
    }

    #[test]
    fn serialises_concurrent_saves_so_the_index_holds_both() {
        // Each save is a read-modify-write of the *folder*, so two running
        // together would each write an index that did not know about the other.
        let root = workspace();
        std::thread::scope(|scope| {
            let a = scope.spawn(|| save_memory(root.path(), &named("alpha")));
            let b = scope.spawn(|| save_memory(root.path(), &named("zeta")));
            a.join().unwrap().unwrap();
            b.join().unwrap().unwrap();
        });
        let text = index_of(root.path());
        assert!(text.contains("(alpha.md)"));
        assert!(text.contains("(zeta.md)"));
    }

    #[test]
    fn refuses_a_name_that_is_not_usable_as_a_filename() {
        let error = save_memory(workspace().path(), &named("???")).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(error.message.contains("not usable as a filename"));
    }

    #[test]
    fn surfaces_a_folder_that_cannot_be_created() {
        let root = workspace();
        fs::write(root.path().join("memory"), "a file where the folder goes").unwrap();
        let error = save_memory(root.path(), &fixture()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Storage);
    }
}
