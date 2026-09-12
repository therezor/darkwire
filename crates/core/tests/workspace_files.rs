//! MIME lookup and the text read.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::fs;
use std::path::PathBuf;

use ghostai_core::workspace_files::{
    DEFAULT_MIME_TYPE, MAX_TEXT_BYTES, WorkspaceText, mime_type_for, read_text,
};
use tempfile::TempDir;

/// Writes `contents` into a fresh directory and returns its path and size.
fn file(contents: &[u8]) -> (TempDir, PathBuf, u64) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample");
    fs::write(&path, contents).unwrap();
    (dir, path, contents.len() as u64)
}

mod mime_type {
    use super::*;

    #[test]
    fn maps_known_extensions_case_insensitively() {
        assert_eq!(mime_type_for("photo.PNG"), "image/png");
        assert_eq!(mime_type_for("notes.md"), "text/markdown; charset=utf-8");
        assert_eq!(
            mime_type_for("data.json"),
            "application/json; charset=utf-8"
        );
        assert_eq!(mime_type_for("dir.v2/clip.webm"), "video/webm");
    }

    #[test]
    fn falls_back_for_unknown_types() {
        // A type this table does not know downloads rather than executes.
        for path in [
            "archive.7z",
            "no-extension",
            "script.sh",
            ".bashrc",
            "trailing.",
        ] {
            assert_eq!(mime_type_for(path), DEFAULT_MIME_TYPE, "{path}");
        }
    }

    #[test]
    fn does_not_claim_source_files_are_text() {
        // The property everything downstream depends on: this table cannot be
        // asked whether a file is text. `read_text` answers that, from the bytes.
        for path in ["module.ts", "script.py", "config.yaml"] {
            assert_eq!(mime_type_for(path), DEFAULT_MIME_TYPE, "{path}");
        }
    }
}

mod read {
    use super::*;

    #[test]
    fn reads_a_whole_small_file() {
        let (_dir, path, size) = file(b"date,amount\n2026-01-01,12\n");
        assert_eq!(
            read_text(&path, size).unwrap(),
            Some(WorkspaceText {
                content: "date,amount\n2026-01-01,12\n".to_owned(),
                truncated: false,
            })
        );
    }

    #[test]
    fn reads_a_source_file_the_mime_table_calls_a_binary() {
        let (_dir, path, size) = file(b"def main():\n    return 1\n");
        assert!(
            read_text(&path, size)
                .unwrap()
                .unwrap()
                .content
                .contains("def main()")
        );
    }

    #[test]
    fn returns_nothing_for_bytes_holding_a_nul() {
        let (_dir, path, size) = file(&[0x89, 0x50, 0x4e, 0x47, 0x00, 0x1a]);
        assert_eq!(read_text(&path, size).unwrap(), None);
    }

    #[test]
    fn reads_a_prefix_and_says_so_past_the_cap() {
        let cap = usize::try_from(MAX_TEXT_BYTES).unwrap();
        let (_dir, path, size) = file("x".repeat(cap + 10).as_bytes());
        let text = read_text(&path, size).unwrap().unwrap();
        assert!(text.truncated);
        assert_eq!(text.content.len(), cap);
    }

    #[test]
    fn decodes_lossily_when_the_cap_splits_a_character() {
        let cap = usize::try_from(MAX_TEXT_BYTES).unwrap();
        let mut bytes = "x".repeat(cap - 1).into_bytes();
        bytes.extend("é".as_bytes());
        let (_dir, path, size) = file(&bytes);
        let text = read_text(&path, size).unwrap().unwrap();
        assert!(text.truncated);
        assert!(text.content.ends_with('\u{FFFD}'));
    }

    #[test]
    fn handles_an_empty_file() {
        let (_dir, path, _) = file(b"");
        assert_eq!(
            read_text(&path, 0).unwrap(),
            Some(WorkspaceText {
                content: String::new(),
                truncated: false,
            })
        );
    }

    #[test]
    fn fails_on_a_file_that_is_not_there() {
        let dir = tempfile::tempdir().unwrap();
        let error = read_text(&dir.path().join("missing"), 10).unwrap_err();
        assert_eq!(error.kind, ghostai_core::ErrorKind::NotFound);
    }
}
