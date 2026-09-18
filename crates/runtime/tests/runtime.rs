//! The composition root: config in, a running agent out.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use common::{CountingProviders, Install, configured, err};
use darkwire_core::ErrorKind;
use darkwire_mcp::testkit::{FakeServer, echo_tool};
use darkwire_protocol::{DEFAULT_AGENT_ID, ToolSource};
use darkwire_runtime::agents::AgentWarningCode;
use darkwire_runtime::{McpChoice, RuntimeOptions, VaultChoice, create_runtime};
use serde_json::{Value, json};

fn patch(value: Value) -> Value {
    value
}

mod construction {
    use super::*;

    #[test]
    fn wires_a_loop_from_the_config_file() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        assert!(runtime.configured());
        assert!(runtime.agent_loop().is_some());
        assert_eq!(runtime.model(), "llama3");
        assert_eq!(runtime.instance().unwrap().id, "ollama");
        assert_eq!(runtime.spec().unwrap().id, "ollama");
        assert!(runtime.require_loop().is_ok());
    }

    #[test]
    fn names_the_config_file_it_read() {
        let install = Install::with(&configured("llama3"));
        assert_eq!(install.runtime().unwrap().file(), install.config_file());
    }

    #[test]
    fn starts_with_no_tools_at_all_when_asked() {
        let install = Install::with(&configured("llama3"));
        let runtime = create_runtime(RuntimeOptions {
            tools: false,
            ..install.options()
        })
        .unwrap();
        assert_eq!(runtime.tools().size(), 0);
        // Still configured: no tools is not no model.
        assert!(runtime.configured());
    }

    #[test]
    fn registers_the_built_ins_by_default() {
        let runtime = Install::with(&configured("llama3")).runtime().unwrap();
        assert!(runtime.tools().has("read"));
        assert!(runtime.tools().has("exec"));
    }

    #[test]
    fn builds_unconfigured_when_nothing_names_a_provider_and_refuses_only_the_turn() {
        // A fresh machine: `darkwire serve` has to come up and serve the settings
        // UI that fixes this.
        let install = Install::bare();
        let runtime = install.runtime().unwrap();
        assert!(!runtime.configured());
        assert!(runtime.agent_loop().is_none());
        assert_eq!(runtime.model(), "");
        assert!(runtime.instance().is_none());
        assert!(runtime.spec().is_none());
        assert!(!runtime.has_credential());
        // Everything that does not need a model still works.
        assert!(runtime.tools().has("read"));
        assert!(runtime.jail().root().exists());
        assert_eq!(runtime.workspaces().list().unwrap().len(), 1);
        assert!(runtime.store().get_session("nobody").unwrap().is_none());

        let error = err(runtime.require_loop());
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(
            error.message.contains("No provider could be resolved"),
            "{}",
            error.message
        );
        assert!(error.message.contains("darkwire init"), "{}", error.message);
    }

    #[test]
    fn reports_a_provider_with_no_model_as_unconfigured_naming_the_provider() {
        let install = Install::with(&json!({
            "providers": {"ollama": {"type": "ollama"}},
            "agents": {"list": {"default": {"provider": "ollama"}}},
        }));
        let runtime = install.runtime().unwrap();
        assert!(!runtime.configured());
        // The instance resolved; only the model is missing, and the refusal says
        // which half it is.
        assert_eq!(runtime.instance().unwrap().id, "ollama");
        let error = err(runtime.require_loop());
        assert!(
            error.message.contains("No model configured for"),
            "{}",
            error.message
        );
        // And the warning surfaces it where the operator is looking.
        assert!(
            runtime
                .config_warnings()
                .iter()
                .any(|warning| warning.code == AgentWarningCode::NoModel)
        );
    }

    #[test]
    fn takes_an_exported_api_key_as_the_operator_naming_a_provider() {
        // `OPENAI_API_KEY=… darkwire chat` should not need a config file. What it
        // will not do is fall back to *some* provider.
        let install = Install::with(&json!({"agents": {"list": {"default": {"model": "gpt-4o"}}}}));
        let runtime = create_runtime(RuntimeOptions {
            env: Some(HashMap::from([(
                "OPENAI_API_KEY".to_owned(),
                "sk-exported".to_owned(),
            )])),
            ..install.options()
        })
        .unwrap();
        assert!(runtime.configured());
        assert_eq!(runtime.instance().unwrap().id, "openai");
        assert!(runtime.has_credential());
    }

    #[test]
    fn resolves_one_of_two_instances_of_the_same_provider_type() {
        let install = Install::with(&json!({
            "providers": {
                "laptop": {"type": "ollama", "apiBase": "http://laptop:11434/v1"},
                "gpu": {"type": "ollama", "apiBase": "http://gpu:11434/v1"},
            },
            "agents": {"list": {"default": {"model": "m", "provider": "gpu"}}},
        }));
        let runtime = install.runtime().unwrap();
        assert_eq!(runtime.instance().unwrap().id, "gpu");
        assert_eq!(
            runtime.instance().unwrap().config.api_base.as_deref(),
            Some("http://gpu:11434/v1")
        );
    }

    #[test]
    fn resolves_the_workspaces_folder_from_the_config_relative_to_the_root() {
        let install = Install::with(&json!({"workspaces": "projects"}));
        let runtime = create_runtime(install.options_without_a_workspaces_folder()).unwrap();
        assert_eq!(
            runtime.paths().workspaces_dir,
            install.root.join("projects")
        );
    }

    #[test]
    fn lets_an_explicit_workspaces_folder_win_over_the_config() {
        let install = Install::with(&json!({"workspaces": "projects"}));
        let elsewhere = install.temp.path().join("elsewhere");
        let runtime = create_runtime(RuntimeOptions {
            workspaces: Some(elsewhere.to_string_lossy().into_owned()),
            ..install.options()
        })
        .unwrap();
        assert!(runtime.paths().workspaces_dir.ends_with("elsewhere"));
    }

    #[test]
    fn refuses_settings_that_cannot_be_built_at_all() {
        // An unbuildable agent is refused outright rather than surviving as a
        // warning: an egress rule that is not a CIDR was never going to work.
        let install = Install::with(&json!({"agents": {"list": {"net": {
            "environment": {"network": {"mode": "allowlist", "allow": ["nope"]}},
        }}}}));
        assert_eq!(err(install.runtime()).kind, ErrorKind::Config);
    }

    #[test]
    fn describes_itself_without_naming_a_credential() {
        let runtime = Install::with(&configured("llama3")).runtime().unwrap();
        let shown = format!("{runtime:?}");
        assert!(shown.contains("WireRuntime"), "{shown}");
        assert!(shown.contains("llama3"), "{shown}");
    }

    #[test]
    fn options_describe_their_seams_without_their_contents() {
        let shown = format!("{:?}", RuntimeOptions::default());
        assert!(shown.contains("VaultChoice::Default"), "{shown}");
        assert!(shown.contains("McpChoice::Default"), "{shown}");
    }
}

mod shared_connection {
    use super::*;

    #[test]
    fn leaves_a_borrowed_connection_usable_after_the_runtime_goes() {
        let install = Install::with(&configured("llama3"));
        {
            let runtime = install.runtime().unwrap();
            runtime
                .store()
                .ensure_session(
                    "chat",
                    darkwire_core::session_store::CreateSession::default(),
                )
                .unwrap();
        }
        // The auth store and the scheduler live in the same file: whoever opened
        // the connection decides when the last reference goes.
        assert!(install.database.column_names("sessions").is_ok());
    }

    #[test]
    fn opens_its_own_file_when_no_connection_is_borrowed() {
        let install = Install::with(&configured("llama3"));
        let runtime = create_runtime(RuntimeOptions {
            database: None,
            ..install.options()
        })
        .unwrap();
        runtime
            .store()
            .ensure_session(
                "chat",
                darkwire_core::session_store::CreateSession::default(),
            )
            .unwrap();
        assert!(runtime.paths().db_file.exists());
    }
}

mod reconfigure {
    use super::*;

    #[test]
    fn moves_the_model_and_rebuilds_the_loop_keeping_the_store() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        let store = Arc::clone(runtime.store());
        let registry = Arc::clone(runtime.tools());
        let steering = Arc::clone(runtime.steering());

        let next = runtime
            .reconfigure(&patch(
                json!({"agents": {"list": {"default": {"model": "qwen"}}}}),
            ))
            .unwrap();
        assert_eq!(next.agents.list["default"].settings.model, "qwen");
        assert_eq!(runtime.model(), "qwen");
        // Owned things survive; derived things are rebuilt.
        assert!(Arc::ptr_eq(&store, runtime.store()));
        assert!(Arc::ptr_eq(&registry, runtime.tools()));
        assert!(Arc::ptr_eq(&steering, runtime.steering()));
    }

    #[test]
    fn becomes_configured_when_a_reconfigure_supplies_what_was_missing() {
        let install = Install::bare();
        let runtime = install.runtime().unwrap();
        assert!(!runtime.configured());
        runtime
            .reconfigure(&patch(json!({
                "providers": {"ollama": {"type": "ollama"}},
                "agents": {"list": {"default": {"model": "m", "provider": "ollama"}}},
            })))
            .unwrap();
        assert!(runtime.configured());
    }

    #[test]
    fn goes_back_to_unconfigured_when_the_model_is_cleared_out_of_the_config() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        runtime
            .reconfigure(&patch(
                json!({"agents": {"list": {"default": {"provider": "ollama"}}}}),
            ))
            .unwrap();
        assert!(!runtime.configured());
        assert!(
            err(runtime.require_loop())
                .message
                .contains("No model configured")
        );
    }

    #[test]
    fn leaves_a_construction_time_override_in_place() {
        // `darkwire chat --model x` is a statement about this process: a settings
        // save from a browser must not silently move the terminal session.
        let install = Install::with(&configured("llama3"));
        let runtime = create_runtime(RuntimeOptions {
            model: Some("pinned".to_owned()),
            provider: Some("ollama".to_owned()),
            ..install.options()
        })
        .unwrap();
        assert_eq!(runtime.model(), "pinned");
        runtime
            .reconfigure(&patch(
                json!({"agents": {"list": {"default": {"model": "qwen"}}}}),
            ))
            .unwrap();
        assert_eq!(runtime.model(), "pinned");
    }

    #[test]
    fn re_registers_the_built_ins_so_a_disabled_scheduler_drops_automation() {
        // A tool that can only answer "this installation has no scheduler" costs
        // a turn to learn what its absence would have said for free.
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        assert!(runtime.tools().has("automation"));
        runtime
            .reconfigure(&patch(json!({"scheduler": {"enabled": false}})))
            .unwrap();
        assert!(!runtime.tools().has("automation"));
    }

    #[test]
    fn keeps_registrations_that_are_not_built_ins() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        runtime
            .tools()
            .register(common::tool("mcp_files_read"), ToolSource::Mcp)
            .unwrap();
        runtime
            .reconfigure(&patch(json!({"scheduler": {"enabled": false}})))
            .unwrap();
        // Exact by source: an MCP server is one connection however many saves
        // happen.
        assert!(runtime.tools().has("mcp_files_read"));
    }

    #[test]
    fn applies_a_new_tool_timeout_to_the_live_registry() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        assert_eq!(runtime.tools().timeout_ms(), 0);
        runtime
            .reconfigure(&patch(json!({"agents": {"list": {"default": {
                "model": "llama3", "provider": "ollama", "toolTimeoutMs": 9000,
            }}}})))
            .unwrap();
        assert_eq!(runtime.tools().timeout_ms(), 9000);
    }

    #[test]
    fn moves_the_jail_when_the_workspaces_folder_moves_and_reuses_it_when_it_does_not() {
        let mut config = configured("llama3");
        config["workspaces"] = json!("first");
        let install = Install::with(&config);
        // The patch has to be what names the folder, so the construction-time
        // override is left off: it wins over a patch, deliberately.
        let runtime = create_runtime(install.options_without_a_workspaces_folder()).unwrap();
        let before = runtime.jail();
        runtime
            .reconfigure(&patch(json!({"agents": {"list": {"default": {
                "model": "qwen", "provider": "ollama",
            }}}})))
            .unwrap();
        // A jail canonicalises and creates its root, so keeping the cache when
        // nothing moved saves that work on every workspace already in use.
        assert!(Arc::ptr_eq(&before, &runtime.jail()));

        runtime
            .reconfigure(&patch(json!({"workspaces": "elsewhere"})))
            .unwrap();
        assert!(!Arc::ptr_eq(&before, &runtime.jail()));
        // The default's own folder, one level inside the tree that moved.
        assert!(runtime.jail().root().ends_with("elsewhere/default"));
    }

    #[test]
    fn reuses_the_cached_adapter_when_nothing_about_the_connection_changed() {
        let install = Install::with(&configured("llama3"));
        let providers = CountingProviders::new(8);
        let runtime = create_runtime(RuntimeOptions {
            providers: Some(Arc::clone(&providers.cache)),
            ..install.options()
        })
        .unwrap();
        assert_eq!(providers.built(), 1);
        runtime
            .reconfigure(&patch(json!({"server": {"port": 4567}})))
            .unwrap();
        // A settings panel saves a panel at a time; a new client per keystroke
        // would leak a connection pool and pay a fresh handshake on every turn.
        assert_eq!(providers.built(), 1);
    }

    #[tokio::test]
    async fn leaves_an_injected_cache_alone_on_close() {
        let install = Install::with(&configured("llama3"));
        let providers = CountingProviders::new(8);
        let runtime = create_runtime(RuntimeOptions {
            providers: Some(Arc::clone(&providers.cache)),
            ..install.options()
        })
        .unwrap();
        // An injected cache outlives this runtime, so `close` leaves it alone:
        // closing adapters the caller shares between runtimes is not ours to do.
        runtime.close().await;
        assert_eq!(providers.cache.size(), 1);
    }

    #[tokio::test]
    async fn closes_a_cache_it_opened_itself() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        assert!(runtime.configured());
        runtime.close().await;
        // The cache is private, so what is observable is the other half of the
        // contract: `close` releases the adapters it opened and leaves the
        // database alone, because the handle is shared with whoever passed it in.
        assert!(runtime.store().get_session("nothing").unwrap().is_none());
    }

    #[test]
    fn changes_nothing_when_the_patch_cannot_be_built() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        let error = err(
            runtime.reconfigure(&patch(json!({"agents": {"list": {"net": {
                "environment": {"network": {"mode": "allowlist", "allow": ["nope"]}},
            }}}}))),
        );
        assert_eq!(error.kind, ErrorKind::Config);
        // All-or-nothing: the runtime is still serving what it was serving.
        assert_eq!(runtime.model(), "llama3");
        assert!(runtime.configured());
        assert!(!runtime.config().agents.list.contains_key("net"));
    }

    #[test]
    fn rejects_a_patch_the_schema_refuses_without_touching_the_runtime() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        let error = err(runtime.reconfigure(&patch(
            json!({"agents": {"list": {"default": {"temperature": 9}}}}),
        )));
        assert_eq!(error.kind, ErrorKind::Config);
        assert_eq!(runtime.model(), "llama3");
    }

    #[test]
    fn refuses_a_patch_that_introduces_an_id_nothing_could_use() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        let error = err(runtime.reconfigure(&patch(json!({"agents": {"list": {"Bad Id": {}}}}))));
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(!runtime.config().agents.list.contains_key("Bad Id"));
    }

    #[test]
    fn applies_a_typed_patch_through_the_same_path() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        let typed: darkwire_protocol::ConfigPatch =
            serde_json::from_value(json!({"server": {"port": 4567}})).unwrap();
        let next = runtime.apply_patch(&typed).unwrap();
        assert_eq!(next.server.port, 4567);
        assert_eq!(runtime.config().server.port, 4567);
    }

    #[test]
    fn picks_up_a_credential_saved_since_the_runtime_was_built() {
        // Re-read on every build rather than cached: a key saved in the settings
        // UI has to be usable on the next turn.
        let install = Install::with(&json!({
            "providers": {"openai": {"type": "openai"}},
            "agents": {"list": {"default": {"model": "gpt-4o", "provider": "openai"}}},
        }));
        let mut env = HashMap::new();
        let runtime = create_runtime(RuntimeOptions {
            env: Some(env.clone()),
            ..install.options()
        })
        .unwrap();
        assert!(!runtime.has_credential());

        env.insert("OPENAI_API_KEY".to_owned(), "sk-new".to_owned());
        let after = create_runtime(RuntimeOptions {
            env: Some(env),
            ..install.options()
        })
        .unwrap();
        assert!(after.has_credential());
    }
}

mod reload {
    use super::*;

    #[test]
    fn picks_up_an_edit_made_to_the_file_since_the_runtime_was_built() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        install.write_config(&configured("qwen"));
        let next = runtime.reload().unwrap();
        assert_eq!(next.agents.list["default"].settings.model, "qwen");
        assert_eq!(runtime.model(), "qwen");
    }

    #[test]
    fn takes_the_file_whole_so_a_hand_reverted_field_actually_reverts() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        runtime
            .reconfigure(&patch(json!({"server": {"port": 9999}})))
            .unwrap();
        assert_eq!(runtime.config().server.port, 9999);
        // The whole file, not a merge over what is in memory: a value that is no
        // longer written anywhere has to actually come back.
        runtime.reload().unwrap();
        assert_eq!(runtime.config().server.port, 3000);
    }

    #[test]
    fn re_registers_the_built_ins_so_a_tool_switched_off_in_the_file_disappears() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        let mut edited = configured("llama3");
        edited["scheduler"] = json!({"enabled": false});
        install.write_config(&edited);
        runtime.reload().unwrap();
        assert!(!runtime.tools().has("automation"));
    }

    #[test]
    fn keeps_the_store_and_the_steering_queue_so_a_turn_in_flight_is_not_disturbed() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        let store = Arc::clone(runtime.store());
        let steering = Arc::clone(runtime.steering());
        install.write_config(&configured("qwen"));
        runtime.reload().unwrap();
        assert!(Arc::ptr_eq(&store, runtime.store()));
        assert!(Arc::ptr_eq(&steering, runtime.steering()));
    }

    #[test]
    fn changes_nothing_when_the_file_cannot_be_built() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        install.write_config(&json!({"agents": {"list": {"net": {
            "environment": {"network": {"mode": "allowlist", "allow": ["nope"]}},
        }}}}));
        assert_eq!(err(runtime.reload()).kind, ErrorKind::Config);
        assert_eq!(runtime.model(), "llama3");
        assert!(runtime.configured());
    }

    #[test]
    fn refuses_a_file_that_is_not_valid_settings() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        std::fs::write(install.config_file(), "{not json").unwrap();
        assert_eq!(err(runtime.reload()).kind, ErrorKind::Config);
        assert_eq!(runtime.model(), "llama3");
    }

    #[test]
    fn leaves_a_construction_time_override_in_place() {
        let install = Install::with(&configured("llama3"));
        let runtime = create_runtime(RuntimeOptions {
            model: Some("pinned".to_owned()),
            ..install.options()
        })
        .unwrap();
        install.write_config(&configured("qwen"));
        runtime.reload().unwrap();
        assert_eq!(runtime.model(), "pinned");
    }
}

mod multiple_agents {
    use super::*;

    fn tree() -> Value {
        json!({
            "providers": {"ollama": {"type": "ollama"}},
            "agents": {"list": {
                "default": {"model": "llama3", "provider": "ollama"},
                "reviewer": {"model": "qwen", "provider": "ollama", "label": "Reviewer"},
                "off": {"model": "m", "provider": "ollama", "enabled": false},
            }},
        })
    }

    #[test]
    fn lists_the_default_agent_on_an_install_that_named_none() {
        let runtime = Install::with(&configured("llama3")).runtime().unwrap();
        let agents = runtime.agents();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, DEFAULT_AGENT_ID);
    }

    #[test]
    fn gives_a_named_agent_its_own_loop_on_its_own_model() {
        let install = Install::with(&tree());
        let providers = CountingProviders::new(8);
        let runtime = create_runtime(RuntimeOptions {
            providers: Some(Arc::clone(&providers.cache)),
            ..install.options()
        })
        .unwrap();
        // Only the default is built at boot: an install with six agents and one
        // in use should not open six provider connections.
        assert_eq!(providers.built(), 1);
        assert!(runtime.loop_for(Some("reviewer")).unwrap().is_some());
        assert_eq!(providers.built(), 2);
    }

    #[test]
    fn builds_a_named_agent_once_and_reuses_it() {
        let install = Install::with(&tree());
        let providers = CountingProviders::new(8);
        let runtime = create_runtime(RuntimeOptions {
            providers: Some(Arc::clone(&providers.cache)),
            ..install.options()
        })
        .unwrap();
        runtime.loop_for(Some("reviewer")).unwrap();
        let after = providers.built();
        runtime.loop_for(Some("reviewer")).unwrap();
        assert_eq!(providers.built(), after);
    }

    #[test]
    fn treats_none_empty_and_default_as_the_same_agent() {
        let runtime = Install::with(&tree()).runtime().unwrap();
        for id in [None, Some(""), Some(DEFAULT_AGENT_ID)] {
            assert!(runtime.loop_for(id).unwrap().is_some(), "{id:?}");
        }
    }

    #[test]
    fn refuses_an_id_that_names_nothing_runnable() {
        let runtime = Install::with(&tree()).runtime().unwrap();
        assert_eq!(
            err(runtime.loop_for(Some("ghost"))).kind,
            ErrorKind::NotFound
        );
        assert_eq!(
            err(runtime.require_loop_for(Some("ghost"))).kind,
            ErrorKind::NotFound
        );
    }

    #[test]
    fn hides_a_disabled_agent_from_the_list_and_from_resolution() {
        let runtime = Install::with(&tree()).runtime().unwrap();
        let ids: Vec<String> = runtime.agents().into_iter().map(|agent| agent.id).collect();
        assert_eq!(ids, vec!["default", "reviewer"]);
        assert_eq!(err(runtime.loop_for(Some("off"))).kind, ErrorKind::NotFound);
    }

    #[test]
    fn shares_one_workspace_between_agents() {
        // Bound, so the temporary root outlives the assertion on it.
        let install = Install::with(&tree());
        let runtime = install.runtime().unwrap();
        // The working folder is root-level: an agent has no `workspace` field at
        // all, which is what makes this unambiguous.
        assert!(runtime.agents().iter().all(|agent| agent.id != "workspace"));
        assert!(runtime.jail().root().exists());
    }

    #[test]
    fn narrows_one_agents_tools_without_touching_the_shared_registry() {
        let mut tree = tree();
        tree["agents"]["list"]["reviewer"]["tools"] = json!({"read": "allow"});
        let runtime = Install::with(&tree).runtime().unwrap();
        runtime.loop_for(Some("reviewer")).unwrap();
        // A view of the one shared registry, not a registry of its own.
        assert!(runtime.tools().has("exec"));
    }

    #[test]
    fn drops_cached_loops_on_a_reconfigure_so_a_settings_save_takes_effect() {
        let install = Install::with(&tree());
        let providers = CountingProviders::new(8);
        let runtime = create_runtime(RuntimeOptions {
            providers: Some(Arc::clone(&providers.cache)),
            ..install.options()
        })
        .unwrap();
        runtime.loop_for(Some("reviewer")).unwrap();
        runtime
            .reconfigure(&patch(json!({"agents": {"list": {"reviewer": {
                "model": "phi", "provider": "ollama",
            }}}})))
            .unwrap();
        runtime.loop_for(Some("reviewer")).unwrap();
        // Every loop in the old cache was derived from the settings that just
        // changed, so a fresh adapter for the new model is the point.
        assert!(providers.built() >= 3);
    }

    #[test]
    fn adds_an_agent_added_by_a_patch() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        runtime
            .reconfigure(&patch(json!({"agents": {"list": {"writer": {
                "model": "m", "provider": "ollama",
            }}}})))
            .unwrap();
        assert!(runtime.loop_for(Some("writer")).unwrap().is_some());
    }

    #[test]
    fn applies_a_process_wide_model_pin_to_every_agent() {
        // An agent that quietly ignored `--model` would be the more surprising
        // rule.
        let install = Install::with(&tree());
        let runtime = create_runtime(RuntimeOptions {
            model: Some("pinned".to_owned()),
            ..install.options()
        })
        .unwrap();
        let (_, model) = runtime
            .provider_for(Some("reviewer"), None)
            .unwrap()
            .unwrap();
        assert_eq!(model, "pinned");
    }

    #[test]
    fn leaves_the_runtime_serving_when_a_patch_adds_an_unbuildable_agent() {
        let install = Install::with(&tree());
        let runtime = install.runtime().unwrap();
        let error = err(
            runtime.reconfigure(&patch(json!({"agents": {"list": {"net": {
                "environment": {"network": {"mode": "allowlist", "allow": ["nope"]}},
            }}}}))),
        );
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(runtime.loop_for(Some("reviewer")).unwrap().is_some());
    }
}

mod provider_for {
    use super::*;

    #[test]
    fn answers_with_the_agents_own_endpoint_and_model() {
        let runtime = Install::with(&configured("llama3")).runtime().unwrap();
        let (provider, model) = runtime.provider_for(None, None).unwrap().unwrap();
        assert_eq!(provider.id(), "ollama");
        assert_eq!(model, "llama3");
    }

    #[test]
    fn lets_a_caller_override_the_model_for_one_request() {
        // How a heartbeat gets to run on a cheaper model than the agent's.
        let runtime = Install::with(&configured("llama3")).runtime().unwrap();
        let (_, model) = runtime.provider_for(None, Some("tiny")).unwrap().unwrap();
        assert_eq!(model, "tiny");
        // An empty override is not an override.
        let (_, unchanged) = runtime.provider_for(None, Some("")).unwrap().unwrap();
        assert_eq!(unchanged, "llama3");
    }

    #[test]
    fn answers_with_nothing_on_an_unconfigured_install() {
        let runtime = Install::bare().runtime().unwrap();
        assert!(runtime.provider_for(None, None).unwrap().is_none());
    }

    #[test]
    fn refuses_an_id_that_names_nothing() {
        let runtime = Install::with(&configured("llama3")).runtime().unwrap();
        assert_eq!(
            err(runtime.provider_for(Some("ghost"), None)).kind,
            ErrorKind::NotFound
        );
    }
}

mod agent_references_surviving_a_delete {
    use super::*;

    fn delegating() -> Value {
        json!({
            "providers": {"ollama": {"type": "ollama"}},
            "agents": {"list": {
                "default": {"model": "m", "provider": "ollama"},
                "researcher": {"model": "m", "provider": "ollama"},
                "main": {
                    "model": "m", "provider": "ollama",
                    "subagents": [{"id": "researcher"}],
                },
            }},
        })
    }

    #[test]
    fn deletes_an_agent_another_one_delegates_to_instead_of_reporting_a_fault() {
        let install = Install::with(&delegating());
        let runtime = install.runtime().unwrap();
        // The delete used to leave a ref pointing at nothing, the rebuild failed,
        // and the operator got a fault and a file that had not changed.
        let next = runtime
            .reconfigure(&patch(json!({"agents": {"list": {"researcher": null}}})))
            .unwrap();
        assert!(!next.agents.list.contains_key("researcher"));
        // The returned config is what a caller persists, so the file is written
        // already healed.
        assert!(next.agents.list["main"].subagents.is_empty());
    }

    #[test]
    fn leaves_the_delegation_alone_when_the_target_is_only_switched_off() {
        let install = Install::with(&delegating());
        let runtime = install.runtime().unwrap();
        let next = runtime
            .reconfigure(&patch(json!({"agents": {"list": {"researcher": {
                "model": "m", "provider": "ollama", "enabled": false,
            }}}})))
            .unwrap();
        // Switching an agent off is the reversible half of deleting it.
        assert_eq!(next.agents.list["main"].subagents.len(), 1);
        assert!(
            runtime
                .config_warnings()
                .iter()
                .any(|warning| warning.code == AgentWarningCode::DisabledSubagent)
        );
    }

    #[test]
    fn starts_on_a_config_whose_delegation_names_an_agent_that_is_not_there() {
        // One hand-edited line must not stop the server from starting at all.
        let install = Install::with(&json!({
            "providers": {"ollama": {"type": "ollama"}},
            "agents": {"list": {
                "default": {"model": "m", "provider": "ollama"},
                "main": {"model": "m", "provider": "ollama", "subagents": [{"id": "ghost"}]},
            }},
        }));
        let runtime = install.runtime().unwrap();
        assert!(runtime.configured());
        assert!(
            runtime
                .config_warnings()
                .iter()
                .any(|warning| warning.code == AgentWarningCode::MissingSubagent)
        );
    }

    #[test]
    fn reports_a_tool_prompt_override_naming_a_tool_the_agent_does_not_have() {
        let install = Install::with(&json!({
            "providers": {"ollama": {"type": "ollama"}},
            "agents": {"list": {"default": {
                "model": "m", "provider": "ollama",
                "toolPrompts": {"nmap": {"description": "scan"}},
            }}},
        }));
        let runtime = install.runtime().unwrap();
        let warning = runtime
            .config_warnings()
            .into_iter()
            .find(|warning| warning.code == AgentWarningCode::UnknownToolPrompt)
            .unwrap();
        assert_eq!(warning.subject.as_deref(), Some("nmap"));
    }

    #[test]
    fn reports_the_same_warning_after_a_reload_without_rewriting_the_file() {
        let tree = json!({
            "providers": {"ollama": {"type": "ollama"}},
            "agents": {"list": {
                "default": {"model": "m", "provider": "ollama"},
                "main": {"model": "m", "provider": "ollama", "subagents": [{"id": "ghost"}]},
            }},
        });
        let install = Install::with(&tree);
        let runtime = install.runtime().unwrap();
        let before = runtime.config_warnings().len();
        runtime.reload().unwrap();
        assert_eq!(runtime.config_warnings().len(), before);
        // Nothing wrote the file: healing belongs to a save, not to a read.
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(install.config_file()).unwrap()).unwrap();
        assert_eq!(
            on_disk["agents"]["list"]["main"]["subagents"][0]["id"],
            "ghost"
        );
    }

    #[test]
    fn still_lets_an_odd_id_already_on_disk_be_deleted() {
        let install = Install::with(&json!({
            "providers": {"ollama": {"type": "ollama"}},
            "agents": {"list": {
                "default": {"model": "m", "provider": "ollama"},
                "Bad Id": {"model": "m"},
            }},
        }));
        let runtime = install.runtime().unwrap();
        assert!(
            runtime
                .config_warnings()
                .iter()
                .any(|warning| warning.code == AgentWarningCode::IllegalAgentId)
        );
        let next = runtime
            .reconfigure(&patch(json!({"agents": {"list": {"Bad Id": null}}})))
            .unwrap();
        assert!(!next.agents.list.contains_key("Bad Id"));
    }

    #[test]
    fn resets_the_default_agent_rather_than_erroring_when_its_entry_is_deleted() {
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        let next = runtime
            .reconfigure(&patch(json!({"agents": {"list": {"default": null}}})))
            .unwrap();
        // `default` is an agent, not the absence of one: it comes back complete
        // and unconfigured rather than leaving the install with none.
        assert!(!next.agents.list.contains_key("default"));
        assert_eq!(runtime.agents().len(), 1);
        assert_eq!(runtime.agents()[0].id, DEFAULT_AGENT_ID);
        assert!(!runtime.configured());
    }
}

mod workspaces {
    use super::*;

    #[test]
    fn resolves_a_jail_per_workspace_and_forgets_one_on_request() {
        let runtime = Install::with(&configured("llama3")).runtime().unwrap();
        let jails = runtime.jails();
        let research = jails.for_workspace("research");
        assert!(research.root().ends_with("research"));
        assert!(Arc::ptr_eq(&research, &jails.for_workspace("research")));

        // The folder move behind a workspace rename: an entry keyed on an id
        // whose directory has just been renamed away holds a path that is no
        // longer there.
        runtime.evict_workspace("research");
        assert!(!Arc::ptr_eq(
            &research,
            &runtime.jails().for_workspace("research")
        ));
    }

    #[test]
    fn the_default_jail_is_the_file_routes_fallback() {
        let runtime = Install::with(&configured("llama3")).runtime().unwrap();
        assert!(Arc::ptr_eq(
            &runtime.jail(),
            &runtime.jails().default_jail()
        ));
    }
}

mod mcp {
    use super::*;

    fn with_server(install: &Install, server: &FakeServer) -> Arc<darkwire_runtime::WireRuntime> {
        create_runtime(RuntimeOptions {
            mcp: McpChoice::Connector {
                connect: server.connector(),
                backoff: None,
                callback_port: None,
            },
            ..install.options()
        })
        .unwrap()
    }

    fn one_server() -> Value {
        let mut tree = configured("llama3");
        tree["tools"] = json!({"mcpServers": {"files": {"command": "srv"}}});
        tree
    }

    #[tokio::test]
    async fn registers_a_configured_servers_tools_into_the_shared_registry() {
        let install = Install::with(&one_server());
        let server = FakeServer::new(vec![echo_tool()]);
        let runtime = with_server(&install, &server);
        assert!(
            common::eventually(Duration::from_secs(5), || runtime
                .tools()
                .names()
                .iter()
                .any(|name| name.starts_with("mcp_files_")))
            .await,
            "tools: {:?}",
            runtime.tools().names()
        );
        assert_eq!(runtime.mcp_servers().len(), 1);
    }

    #[tokio::test]
    async fn keeps_the_built_ins_when_it_registers_them_beside_mcp_tools() {
        let install = Install::with(&one_server());
        let server = FakeServer::new(vec![echo_tool()]);
        let runtime = with_server(&install, &server);
        assert!(
            common::eventually(Duration::from_secs(5), || runtime
                .tools()
                .names()
                .iter()
                .any(|name| name.starts_with("mcp_files_")))
            .await
        );
        runtime
            .reconfigure(&patch(json!({"server": {"port": 4567}})))
            .unwrap();
        assert!(runtime.tools().has("read"));
        assert!(
            runtime
                .tools()
                .names()
                .iter()
                .any(|name| name.starts_with("mcp_files_"))
        );
    }

    #[tokio::test]
    async fn does_not_fail_a_save_because_a_server_is_unreachable() {
        // Reconcile is synchronous and infallible: an unreachable server becomes
        // a status row, not a save the operator loses.
        let install = Install::with(&one_server());
        let server = FakeServer::new(vec![echo_tool()]);
        server.fail_connects(darkwire_core::WireError::new(ErrorKind::Network, "down"));
        let runtime = with_server(&install, &server);
        assert!(runtime.configured());
        assert!(
            runtime
                .reconfigure(&patch(json!({"server": {"port": 4567}})))
                .is_ok()
        );
    }

    #[test]
    fn registers_nothing_at_all_when_the_client_is_switched_off() {
        let install = Install::with(&one_server());
        // `mcp: Off` is the default in the harness, so this proves the registry
        // holds only built-ins.
        let runtime = install.runtime().unwrap();
        assert!(runtime.mcp_servers().is_empty());
        assert!(
            !runtime
                .tools()
                .names()
                .iter()
                .any(|name| name.starts_with("mcp_"))
        );
    }
}

mod credential_tie_break {
    use super::*;

    fn two_instances() -> Value {
        json!({
            "providers": {
                "openai": {"type": "openai"},
                "ollama": {"type": "ollama"},
            },
            "agents": {"list": {"default": {"model": "gpt-4o", "provider": "auto"}}},
        })
    }

    #[test]
    fn an_exported_variable_breaks_the_auto_tie() {
        let install = Install::with(&two_instances());
        let runtime = create_runtime(RuntimeOptions {
            env: Some(HashMap::from([(
                "OPENAI_API_KEY".to_owned(),
                "sk-exported".to_owned(),
            )])),
            ..install.options()
        })
        .unwrap();
        assert_eq!(runtime.instance().unwrap().id, "openai");
    }

    #[test]
    fn an_instance_with_no_entry_at_all_holds_no_credential() {
        // Resolution is choosing between endpoints: a lookup for something the
        // config does not name is `false`, never a failure.
        let install = Install::with(&configured("llama3"));
        let runtime = install.runtime().unwrap();
        assert!(runtime.configured());
        assert!(!runtime.has_credential());
    }
}

#[tokio::test]
async fn close_stops_what_it_owns_and_leaves_a_borrowed_connection() {
    let install = Install::with(&configured("llama3"));
    let runtime = install.runtime().unwrap();
    runtime.close().await;
    // The database is a shared handle: whoever opened it decides when the last
    // reference goes, which is the same contract a borrowed connection had.
    assert!(install.database.column_names("sessions").is_ok());
}

#[test]
fn an_explicit_no_vault_never_reaches_a_keychain() {
    // The seam that makes every other test in this file safe to run.
    let install = Install::with(&configured("llama3"));
    let runtime = create_runtime(RuntimeOptions {
        vault: VaultChoice::None,
        ..install.options()
    })
    .unwrap();
    assert!(!runtime.paths().key_file.exists());
    assert!(!runtime.paths().vault_file.exists());
}
