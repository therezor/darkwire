//! Config load, parse and save, and the byte-parity fixtures.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use ghostai_core::config::{
    LoadConfigOptions, load_config, parse_config, render_config, save_config, validation_issues,
};
use ghostai_core::paths::ResolveGhostPaths;
use ghostai_core::{ErrorKind, GhostError};
use ghostai_protocol::Config;
use serde_json::{Value, json};
use tempfile::TempDir;

const ROUNDTRIP: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/config/roundtrip.json"
));

fn temp() -> TempDir {
    tempfile::tempdir().unwrap()
}

fn write_config(root: &Path, value: &str) -> PathBuf {
    let file = root.join("config.yaml");
    fs::write(&file, value).unwrap();
    file
}

fn options(root: &Path) -> LoadConfigOptions {
    LoadConfigOptions {
        paths: ResolveGhostPaths {
            root: Some(root.to_string_lossy().into_owned()),
            env: Some(HashMap::new()),
            home: Some(PathBuf::from("/home/someone-else")),
            workspace: None,
        },
        file: None,
    }
}

fn err(result: Result<impl std::fmt::Debug, GhostError>) -> GhostError {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error,
    }
}

mod parse {
    use super::*;

    #[test]
    fn fills_every_default_from_an_empty_object() {
        let config = parse_config("{}", Path::new("config.yaml")).unwrap();
        assert_eq!(config.agents.list["default"].settings.provider, "auto");
        assert_eq!(config.server.port, 3000);
        assert_eq!(config.tools.approval_timeout_ms, 5 * 60 * 1000);
        assert_eq!(config, Config::default());
    }

    #[test]
    fn names_the_file_and_the_syntax_problem_on_malformed_yaml() {
        let error = err(parse_config(
            "{ \"agents\": ",
            Path::new("/etc/ghost/config.yaml"),
        ));
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(error.message.contains("/etc/ghost/config.yaml"));
        assert!(error.message.contains("not valid YAML"));
        assert_eq!(error.details["file"], "/etc/ghost/config.yaml");
    }

    #[test]
    fn reports_invalid_settings_as_dotted_paths() {
        // The point of the flattening: `agents.list.default.temperature` is a
        // string an operator can search their config file for.
        let text = json!({"agents": {"list": {"default": {"temperature": 9}}}}).to_string();
        let error = err(parse_config(&text, Path::new("config.yaml")));
        assert!(
            error.message.contains("agents.list.default.temperature"),
            "{}",
            error.message
        );
        assert_eq!(error.details["issues"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn labels_a_root_level_type_error_rather_than_emitting_an_empty_path() {
        let error = err(parse_config("[]", Path::new("config.yaml")));
        assert!(error.message.contains("(root)"), "{}", error.message);
    }

    #[test]
    fn names_a_missing_required_field_at_its_own_path() {
        let text =
            json!({"providers": {"ollama": {"apiBase": "http://gpu.lan:11434/v1"}}}).to_string();
        let error = err(parse_config(&text, Path::new("config.yaml")));
        assert!(
            error.message.contains("providers.ollama.type"),
            "{}",
            error.message
        );
    }

    #[test]
    fn names_a_field_of_the_wrong_type() {
        let error = err(parse_config(
            "{ \"server\": { \"port\": \"3000\" } }",
            Path::new("config.yaml"),
        ));
        assert!(error.message.contains("server.port"), "{}", error.message);
    }

    #[test]
    fn has_no_issues_with_the_defaults() {
        assert!(validation_issues(&Config::default()).is_empty());
    }
}

mod load {
    use super::*;

    #[test]
    fn returns_defaults_when_no_file_exists() {
        let root = temp();
        let loaded = load_config(options(root.path())).unwrap();
        assert!(!loaded.from_file);
        assert_eq!(loaded.file, root.path().join("config.yaml"));
        assert_eq!(
            loaded.config.agents.list["default"]
                .settings
                .max_tool_iterations,
            40
        );
    }

    #[test]
    fn reads_the_file_when_there_is_one() {
        let root = temp();
        write_config(
            root.path(),
            &json!({"agents": {"list": {"default": {"model": "qwen3:8b", "provider": "ollama"}}}})
                .to_string(),
        );
        let loaded = load_config(options(root.path())).unwrap();
        assert!(loaded.from_file);
        let default = &loaded.config.agents.list["default"].settings;
        assert_eq!(default.model, "qwen3:8b");
        assert_eq!(default.provider, "ollama");
    }

    #[test]
    fn refuses_a_provider_entry_that_does_not_name_a_type() {
        // There is no migration path: a file written against an older shape is
        // an error that names the key rather than something quietly rewritten.
        let root = temp();
        write_config(
            root.path(),
            &json!({"providers": {"ollama": {"apiBase": "http://gpu.lan:11434/v1"}}}).to_string(),
        );
        let error = err(load_config(options(root.path())));
        assert!(
            error.message.contains("providers.ollama.type"),
            "{}",
            error.message
        );
    }

    #[test]
    fn does_not_write_a_config_file_for_an_install_that_has_none() {
        let root = temp();
        let loaded = load_config(options(root.path())).unwrap();
        assert!(!loaded.from_file);
        assert!(!root.path().join("config.yaml").exists());
    }

    #[test]
    fn keeps_the_workspace_under_the_root_when_the_config_names_none() {
        // A default of the literal `~/.ghostai/workspace` would restate the
        // *default* root, so an install relocated with GHOSTAI_HOME would point
        // the agent's tools back at the home directory it thought it had left.
        let root = temp();
        let loaded = load_config(options(root.path())).unwrap();
        assert_eq!(loaded.config.workspace, "");
        assert_eq!(loaded.paths.workspace, root.path().join("workspace"));
    }

    #[test]
    fn folds_the_config_workspace_into_the_resolved_paths() {
        let root = temp();
        write_config(
            root.path(),
            &json!({"workspace": "projects/alpha"}).to_string(),
        );
        let loaded = load_config(options(root.path())).unwrap();
        // Relative to the root, not the process cwd.
        assert_eq!(loaded.paths.workspace, root.path().join("projects/alpha"));
    }

    #[test]
    fn expands_tilde_in_the_config_workspace_against_the_given_home() {
        let root = temp();
        let home = temp();
        write_config(
            root.path(),
            &json!({"workspace": "~/ghost-work"}).to_string(),
        );
        let mut options = options(root.path());
        options.paths.home = Some(home.path().to_path_buf());
        let loaded = load_config(options).unwrap();
        assert_eq!(loaded.paths.workspace, home.path().join("ghost-work"));
    }

    #[test]
    fn lets_an_explicit_workspace_win_over_the_config_file() {
        let root = temp();
        write_config(
            root.path(),
            &json!({"workspace": "from-config"}).to_string(),
        );
        let mut options = options(root.path());
        options.paths.workspace =
            Some(root.path().join("from-flag").to_string_lossy().into_owned());
        let loaded = load_config(options).unwrap();
        assert_eq!(loaded.paths.workspace, root.path().join("from-flag"));
    }

    #[test]
    fn honours_an_explicit_file_over_the_root_config() {
        let root = temp();
        let elsewhere = temp();
        let other = elsewhere.path().join("elsewhere.json");
        fs::write(&other, json!({"server": {"port": 8080}}).to_string()).unwrap();
        let mut options = options(root.path());
        options.file = Some(other.clone());
        let loaded = load_config(options).unwrap();
        assert_eq!(loaded.file, other);
        assert_eq!(loaded.config.server.port, 8080);
    }

    #[test]
    fn refuses_a_malformed_config_rather_than_falling_back_to_defaults() {
        let root = temp();
        write_config(root.path(), "{ \"server\": { \"port\": \"3000\" } }");
        let error = err(load_config(options(root.path())));
        assert!(error.message.contains("server.port"), "{}", error.message);
    }

    #[test]
    fn surfaces_an_unreadable_file_instead_of_treating_it_as_absent() {
        let root = temp();
        // A directory where the config file should be: readable in principle,
        // not a file in practice, and definitely not "no config here".
        let mut options = options(root.path());
        options.file = Some(root.path().to_path_buf());
        let error = err(load_config(options));
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(
            error.message.contains("could not be read"),
            "{}",
            error.message
        );
    }

    #[test]
    fn treats_a_missing_parent_directory_as_no_config() {
        let root = temp();
        let mut options = options(root.path());
        options.file = Some(root.path().join("missing").join("config.yaml"));
        assert!(!load_config(options).unwrap().from_file);
    }

    #[test]
    fn reads_the_home_variable_when_no_root_is_given() {
        let root = temp();
        write_config(root.path(), &json!({"server": {"port": 4100}}).to_string());
        let options = LoadConfigOptions {
            paths: ResolveGhostPaths {
                root: None,
                workspace: None,
                env: Some(HashMap::from([(
                    "GHOSTAI_HOME".to_owned(),
                    root.path().to_string_lossy().into_owned(),
                )])),
                home: Some(PathBuf::from("/home/someone-else")),
            },
            file: None,
        };
        let loaded = load_config(options).unwrap();
        assert_eq!(loaded.config.server.port, 4100);
        assert_eq!(loaded.paths.root, root.path());
    }
}

mod save {
    use super::*;

    #[test]
    fn round_trips_through_load_config() {
        let root = temp();
        let file = root.path().join("config.yaml");
        let mut config = parse_config("{}", &file).unwrap();
        config.server.port = 4242;
        save_config(&file, &config).unwrap();
        assert_eq!(
            load_config(options(root.path()))
                .unwrap()
                .config
                .server
                .port,
            4242
        );
    }

    #[test]
    fn writes_a_file_a_human_can_read_and_edit() {
        let root = temp();
        let file = root.path().join("config.yaml");
        save_config(&file, &parse_config("{}", &file).unwrap()).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("workspace:"));
        assert!(text.ends_with('\n'));
    }

    #[cfg(unix)]
    #[test]
    fn writes_the_file_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = temp();
        let file = root.path().join("config.yaml");
        save_config(&file, &Config::default()).unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!root.path().join("config.yaml.tmp").exists());
    }

    #[test]
    fn creates_the_directory_it_is_asked_to_write_into() {
        let root = temp();
        let file = root
            .path()
            .join("nested")
            .join("deeper")
            .join("config.yaml");
        save_config(&file, &parse_config("{}", &file).unwrap()).unwrap();
        let mut options = options(root.path());
        options.file = Some(file);
        assert!(load_config(options).unwrap().from_file);
    }

    #[test]
    fn refuses_to_write_settings_the_next_boot_would_reject() {
        let root = temp();
        let file = root.path().join("config.yaml");
        let mut broken = Config::default();
        broken.agents.list["default"].settings.temperature = Some(9.0);
        let error = err(save_config(&file, &broken));
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(
            error.message.contains("Refusing to write"),
            "{}",
            error.message
        );
        assert!(!file.exists());
    }

    #[test]
    fn leaves_the_previous_file_in_place_when_the_write_fails() {
        let root = temp();
        let file = root.path().join("config.yaml");
        save_config(&file, &Config::default()).unwrap();
        let before = fs::read_to_string(&file).unwrap();

        // A directory where the temp file wants to go: the write fails, and the
        // rename that would have replaced the real file never runs.
        fs::create_dir(root.path().join("config.yaml.tmp")).unwrap();
        let mut changed = Config::default();
        changed.server.port = 4242;
        let error = err(save_config(&file, &changed));
        assert!(
            error.message.contains("could not be written"),
            "{}",
            error.message
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), before);
    }
}

mod fixtures {
    use super::*;

    #[test]
    fn defaults_yaml_round_trips_through_the_saved_file() {
        let root = temp();
        let file = root.path().join("config.yaml");
        save_config(&file, &Config::default()).unwrap();
        assert_eq!(
            parse_config(&fs::read_to_string(&file).unwrap(), &file).unwrap(),
            Config::default()
        );
        assert_eq!(
            render_config(&parse_config("{}", &file).unwrap()).unwrap(),
            fs::read_to_string(&file).unwrap()
        );
    }

    #[test]
    fn roundtrip_cases_preserve_the_parsed_configuration() {
        let fixture: Value = serde_json::from_str(ROUNDTRIP).unwrap();
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 10);
        let file = Path::new("config.yaml");
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let input = serde_json::to_string(&case["input"]).unwrap();
            let config = parse_config(&input, file).unwrap();
            assert_eq!(
                parse_config(&render_config(&config).unwrap(), file).unwrap(),
                config,
                "case: {name}"
            );
        }
    }
}
