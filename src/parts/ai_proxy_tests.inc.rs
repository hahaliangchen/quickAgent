    use super::*;

    #[test]
    fn keyless_local_model_saves_with_unconfigured_search_and_without_credentials_file() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        let path = root.join("providers.yml");
        write_yaml_atomic(&path, &json!([
            {"id":"llm-pi-ai","config":{"providers":{
                "deepseek":{"baseURL":"https://api.deepseek.com/v1","api":"openai-completions","models":[{"id":"deepseek-flash"}]}
            }}},
            {"id":"agent-default-model","config":{"provider":"deepseek","model":"deepseek-flash"}}
        ])).unwrap();
        write_yaml_atomic(&generation_settings_path(&path), &json!({"web_search_provider":"deepseek"})).unwrap();
        let view = settings_view(&load_config(&path).unwrap(), disk_revision(&path).unwrap());
        let mut providers = view["providers"].as_array().unwrap().clone();
        providers.push(json!({"id":"qwen-local","url":"http://127.0.0.1:12888/v1","api_type":"openai-completions","models":["customer-qwen3-vl-32b"]}));
        let edit = json!({"revision":view["revision"],"providers":providers,
            "web_search_provider":"deepseek","orchestrator_provider":"qwen-local","orchestrator_model":"customer-qwen3-vl-32b"});
        save_settings(&path, serde_json::from_value(edit).unwrap()).unwrap().unwrap();
        let saved = load_config(&path).unwrap();
        assert_eq!(saved.orchestrator_provider.as_deref(), Some("qwen-local"));
        assert!(saved.providers["qwen-local"].api_key.is_empty());
        assert!(saved.providers["qwen-local"].api_key_ref.is_none());
        assert!(!credentials_path(&path).exists(), "no dummy key or credentials file is required");
        assert!(web_search_credentials(&path).unwrap_err().to_string().contains("WEB_NOT_CONFIGURED"));
        // Search stays unavailable until configured, but further model edits still save.
        let view = settings_view(&saved, disk_revision(&path).unwrap());
        let edit = json!({"revision":view["revision"],"providers":view["providers"],
            "web_search_provider":"", "orchestrator_provider":"qwen-local","orchestrator_model":"customer-qwen3-vl-32b"});
        save_settings(&path, serde_json::from_value(edit).unwrap()).unwrap().unwrap();
        assert!(load_config(&path).unwrap().web_search_provider.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn web_search_credentials_are_explicit_independent_and_preserved_by_other_settings_edits() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        let path = root.join("profiles/web/cordis.patch.yml");
        write_yaml_atomic(&path,&json!([
            {"id":"llm-pi-ai","config":{"providers":{
                "main":{"baseURL":"https://main.example/v1","api":"openai-completions","apiKeyEnv":"MAIN_KEY","models":[{"id":"main-model"}]},
                "search-a":{"baseURL":"https://api.deepseek.com/v1","api":"openai-completions","apiKeyEnv":"SEARCH_A_KEY","models":[{"id":"deepseek-flash"}]},
                "search-b":{"baseURL":"https://api.deepseek.com/v1","api":"openai-completions","apiKeyEnv":"SEARCH_B_KEY","models":[{"id":"deepseek-flash"}]}
            }}},
            {"id":"agent-default-model","config":{"provider":"main","model":"main-model"}}
        ])).unwrap();
        write_yaml_atomic(&credentials_path(&path),&json!({"version":1,"refs":{"MAIN_KEY":"main-secret","SEARCH_A_KEY":"a-secret","SEARCH_B_KEY":"b-secret"},"records":{}})).unwrap();
        assert!(web_search_credentials(&path).unwrap_err().to_string().contains("WEB_NOT_CONFIGURED"));
        // The AI is told before it plans around web, not after the first call fails.
        assert!(crate::builtin_plugins::web::setup_needed(&path).unwrap().contains("专用模型 → 联网搜索 → 搜索凭据"));
        let initial=load_config(&path).unwrap();
        let view=settings_view(&initial,disk_revision(&path).unwrap());
        assert!(!view.to_string().contains("a-secret"));
        let edit=|selection:Option<&str>,main:&str,model:&str| {
            let latest=settings_view(&load_config(&path).unwrap(),disk_revision(&path).unwrap());
            let mut value=json!({"revision":latest["revision"],"providers":latest["providers"],"orchestrator_provider":main,"orchestrator_model":model});
            if let Some(id)=selection { value["web_search_provider"]=json!(id); }
            serde_json::from_value::<SettingsEdit>(value).unwrap()
        };
        save_settings(&path,edit(Some("search-b"),"main","main-model")).unwrap().unwrap();
        assert_eq!(web_search_credentials(&path).unwrap(),("search-b".into(),"b-secret".into()));
        assert!(crate::builtin_plugins::web::setup_needed(&path).is_none());
        assert_eq!(load_config(&path).unwrap().orchestrator_provider.as_deref(),Some("main"));
        // Switching the conversation's provider must not switch the search key.
        save_settings(&path,edit(None,"search-a","deepseek-flash")).unwrap().unwrap();
        assert_eq!(web_search_credentials(&path).unwrap().0,"search-b");
        assert!(save_settings(&path,edit(Some("main"),"search-a","deepseek-flash")).unwrap_err().to_string().contains("WEB_PROVIDER_UNSUPPORTED"));
        assert_eq!(web_search_credentials(&path).unwrap().0,"search-b");
        save_settings(&path,edit(Some(""),"main","main-model")).unwrap().unwrap();
        assert!(web_search_credentials(&path).unwrap_err().to_string().contains("WEB_NOT_CONFIGURED"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configuration_lives_in_the_apps_own_directory() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| move |name: &str| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| std::ffi::OsString::from(value));
        assert_eq!(config_dir_from(vars(&[("HOME", "/home/u")]), false), std::path::PathBuf::from("/home/u/.config/grow"));
        assert_eq!(config_dir_from(vars(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", "/x")]), false), std::path::PathBuf::from("/x/grow"));
        assert_eq!(config_dir_from(vars(&[("HOME", "/home/u"), ("GROW_CONFIG_DIR", "/custom")]), false), std::path::PathBuf::from("/custom"));
        assert_eq!(config_dir_from(vars(&[("APPDATA", "C:/Users/u/AppData/Roaming")]), true), std::path::PathBuf::from("C:/Users/u/AppData/Roaming").join("grow"));
        assert_eq!(config_dir_from(vars(&[("HOME", "/home/u"), ("DSH_HOME", "/home/u/.dsh")]), false), std::path::PathBuf::from("/home/u/.config/grow"));
        assert_eq!(credentials_path(std::path::Path::new("/c/grow/providers.yml")), std::path::PathBuf::from("/c/grow/credentials.yaml"));
    }

    #[test]
    fn a_dsh_configuration_is_copied_once_and_left_untouched() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        let home = root.join("dsh");
        let target = root.join("grow");
        std::fs::create_dir_all(home.join("profiles/web")).unwrap();
        std::fs::write(home.join("profiles/web/cordis.patch.yml"), "- id: llm-pi-ai\n").unwrap();
        std::fs::write(home.join("profiles/web/agent-generation.yml"), "bash_path: /bin/bash\n").unwrap();
        std::fs::write(home.join("profiles/web/skill-packages.json"), "[]").unwrap();
        std::fs::write(home.join(".credentials.yaml"), "version: 1\n").unwrap();
        std::fs::write(home.join("settings.yaml"), "dsh only").unwrap();
        assert_eq!(import_dsh_config_from(&target, &home).unwrap(), Some(home.clone()));
        for (name, content) in [("providers.yml", "- id: llm-pi-ai\n"), ("agent-generation.yml", "bash_path: /bin/bash\n"), ("skill-packages.json", "[]"), ("credentials.yaml", "version: 1\n")] {
            assert_eq!(std::fs::read_to_string(target.join(name)).unwrap(), content, "{name}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(target.join(name)).unwrap().permissions().mode() & 0o777, 0o600, "{name}");
            }
        }
        assert!(!target.join("settings.yaml").exists() && !target.join(".env").exists());
        assert!(std::fs::read_dir(&target).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
        std::fs::write(target.join("providers.yml"), "- id: changed\n").unwrap();
        std::fs::write(home.join("profiles/web/cordis.patch.yml"), "- id: newer dsh\n").unwrap();
        assert_eq!(import_dsh_config_from(&target, &home).unwrap(), None, "imported only once");
        assert_eq!(std::fs::read_to_string(target.join("providers.yml")).unwrap(), "- id: changed\n");
        assert_eq!(std::fs::read_to_string(home.join(".credentials.yaml")).unwrap(), "version: 1\n", "DSH files stay as they were");
        assert_eq!(import_dsh_config_from(&root.join("empty"), &root.join("no-dsh")).unwrap(), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn retired_role_settings_are_not_executed_or_exposed_and_saved_files_survive() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        let path = root.join("profiles/web/cordis.patch.yml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_yaml_atomic(&path,&json!([
            {"id":"llm-pi-ai","config":{"providers":{"test":{"baseURL":"http://localhost:1/v1","api":"openai-completions","models":[{"id":"m"}]}}}},
            {"id":"agent-default-model","config":{"provider":"test","model":"m"}}
        ])).unwrap();
        write_yaml_atomic(
            &credentials_path(&path),
            &json!({"version":1,"refs":{},"records":{}}),
        )
        .unwrap();
        let retired = path.with_file_name("codex-workspace-observer.yml");
        let original = "enabled: true\nprovider: no-longer-installed\nmodel: old-model\n";
        std::fs::write(&retired, original).unwrap();
        let preserved = json!({"provider":"also-retired","model":"old-context-model"});
        write_yaml_atomic(
            &generation_settings_path(&path),
            &json!({"context_agent":preserved,"unrecognized_field":{"keep":true}}),
        )
        .unwrap();
        let loaded = load_config(&path).unwrap();
        let catalog = catalog_view(&loaded);
        assert!(catalog["defaults"].get("observer").is_none());
        assert!(catalog["defaults"].get("context_agent").is_none());
        let edit:SettingsEdit=serde_json::from_value(json!({"revision":disk_revision(&path).unwrap(),"providers":[{"id":"test","url":"http://localhost:1/v1","models":["m"]}],"orchestrator_provider":"test","orchestrator_model":"m"})).unwrap();
        save_settings(&path, edit).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&retired).unwrap(), original);
        let saved = read_yaml(&generation_settings_path(&path), json!({})).unwrap();
        assert_eq!(saved["context_agent"], preserved);
        assert_eq!(saved["unrecognized_field"], json!({"keep":true}));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn image_probe_evidence_is_persisted_and_bound_to_the_saved_route() {
        let root = std::env::temp_dir().join(format!(
            "codex-image-config-{}",
            crate::visual_artifacts::new_id()
        ));
        let path = root.join("profiles/web/cordis.patch.yml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut patch = json!([{"id":"llm-pi-ai","config":{"providers":{"test":{"baseURL":"https://first.example/v1","api":"openai-completions","models":[{"id":"m"}]}}}},
            {"id":"agent-default-model","config":{"provider":"test","model":"m"}}]);
        write_yaml_atomic(&path, &patch).unwrap();
        let generation = generation_settings_path(&path);
        write_yaml_atomic(&generation, &json!({"custom_setting":"retain me"})).unwrap();
        let report = json!({"status":"verified","capability":"supported","request":{"contains_image_content":true},
            "route_fingerprint":crate::visual_probe::route_fingerprint("https://first.example/v1","m"),"response_body":"image-only challenge passed"});
        let original = disk_revision(&path).unwrap();
        let (_, revision) = persist_image_probe(&path, &original, "test", "m", &report).unwrap();
        assert_ne!(revision, original);
        assert_eq!(
            read_yaml(&generation, json!({})).unwrap()["custom_setting"],
            "retain me"
        );
        let config = load_config(&path).unwrap();
        let capability = &config.model_capabilities["test"]["m"];
        assert_eq!(
            capability.image_input,
            crate::visual_artifacts::ImageCapability::Supported
        );
        assert_eq!(capability.image_probe.as_ref().unwrap(), &report);
        assert!(
            persist_image_probe(&path, &original, "test", "m", &report).is_err(),
            "stale revisions cannot overwrite settings"
        );
        patch[0]["config"]["providers"]["test"]["api"] = json!("anthropic-messages");
        write_yaml_atomic(&path, &patch).unwrap();
        assert_eq!(
            load_config(&path).unwrap().model_capabilities["test"]["m"].image_input,
            crate::visual_artifacts::ImageCapability::Unknown,
            "a Chat Completions probe cannot verify another protocol"
        );
        patch[0]["config"]["providers"]["test"]["api"] = json!("openai-completions");
        patch[0]["config"]["providers"]["test"]["baseURL"] = json!("https://second.example/v1");
        write_yaml_atomic(&path, &patch).unwrap();
        assert_eq!(
            catalog_view(&load_config(&path).unwrap())["models"][0]["verification"]["image_input"],
            "unverified"
        );
        assert_eq!(
            load_config(&path).unwrap().model_capabilities["test"]["m"].image_input,
            crate::visual_artifacts::ImageCapability::Unknown
        );
        let edit:SettingsEdit=serde_json::from_value(json!({"revision":disk_revision(&path).unwrap(),"providers":[{"id":"test","url":"https://second.example/v1","api_type":"openai-completions","models":["other"],
            "model_capabilities":{"other":{"image_input":"supported","image_probe":report}}}]})).unwrap();
        assert!(
            save_settings(&path, edit)
                .unwrap_err()
                .to_string()
                .contains("verify image input"),
            "UI payloads cannot manufacture support evidence"
        );
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn pwsh_settings_preserve_unchanged_without_probe_and_allow_clear() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        let path = root.join("profiles/web/cordis.patch.yml");
        let selected = root.join("not-installed/pwsh.exe");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_yaml_atomic(&path, &json!([])).unwrap();
        write_yaml_atomic(&credentials_path(&path), &json!({"version":1,"refs":{},"records":{}})).unwrap();
        write_yaml_atomic(&generation_settings_path(&path), &json!({"pwsh_path":selected,"other":{"keep":true}})).unwrap();
        let mut payload = json!({"revision":disk_revision(&path).unwrap(),"providers":[],"orchestrator_provider":null,"orchestrator_model":null});
        let (saved, revision) = save_settings(&path, serde_json::from_value(payload.clone()).unwrap()).unwrap().unwrap();
        assert_eq!(saved.pwsh_path.as_ref(), Some(&selected));
        // Re-saving an unchanged executable must not launch it, even when absent.
        payload["pwsh_path"] = json!(selected);
        payload["revision"] = json!(revision);
        let (saved, revision) = save_settings(&path, serde_json::from_value(payload.clone()).unwrap()).unwrap().unwrap();
        assert_eq!(settings_view(&saved, revision.clone())["pwsh_path"], json!(selected));
        payload["pwsh_path"] = json!("");
        payload["revision"] = json!(revision);
        save_settings(&path, serde_json::from_value(payload.clone()).unwrap()).unwrap().unwrap();
        assert!(load_config(&path).unwrap().pwsh_path.is_none());
        payload["pwsh_path"] = json!("relative/pwsh.exe");
        payload["revision"] = json!(disk_revision(&path).unwrap());
        assert!(save_settings(&path, serde_json::from_value(payload).unwrap()).unwrap_err().to_string().contains("absolute"));
        assert_eq!(read_yaml(&generation_settings_path(&path), json!({})).unwrap()["other"]["keep"], true);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bash_settings_roundtrip_preserve_omitted_clear_and_reject_invalid() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        let path = root.join("profiles/web/cordis.patch.yml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_yaml_atomic(&path, &json!([])).unwrap();
        write_yaml_atomic(&credentials_path(&path), &json!({"version":1,"refs":{},"records":{}})).unwrap();
        write_yaml_atomic(&generation_settings_path(&path), &json!({"unrelated_setting":{"keep":true}})).unwrap();
        let bash = if cfg!(windows) { root.join("Git with spaces/bin/bash.exe") } else { root.join("bash with spaces") };
        std::fs::create_dir_all(bash.parent().unwrap()).unwrap();
        std::fs::write(&bash, b"validation fixture, never executed").unwrap();
        #[cfg(windows)]
        {
            let inner = root.join("Git with spaces/usr/bin/bash.exe");
            std::fs::create_dir_all(inner.parent().unwrap()).unwrap();
            std::fs::write(inner, b"inner bash fixture").unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bash, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let bash = crate::shell_environment::validate_bash_path(bash.to_str().unwrap()).unwrap().unwrap();
        let mut payload = json!({"revision":disk_revision(&path).unwrap(),"providers":[],"orchestrator_provider":null,"orchestrator_model":null,"bash_path":bash});
        let (saved, revision) = save_settings(&path, serde_json::from_value(payload.clone()).unwrap()).unwrap().unwrap();
        assert_eq!(saved.bash_path.as_ref(), Some(&bash));
        assert_eq!(settings_view(&saved, revision.clone())["bash_path"], json!(bash));
        assert_eq!(crate::shell_environment::read_bash_path(&path).unwrap(), Some(bash.clone()));
        assert_eq!(read_yaml(&generation_settings_path(&path), json!({})).unwrap()["unrelated_setting"]["keep"], true);
        // Omitted fields from other settings clients preserve this local choice.
        payload.as_object_mut().unwrap().remove("bash_path");
        payload["revision"] = json!(revision);
        save_settings(&path, serde_json::from_value(payload.clone()).unwrap()).unwrap().unwrap();
        assert_eq!(load_config(&path).unwrap().bash_path, Some(bash));
        let original = std::fs::read(generation_settings_path(&path)).unwrap();
        payload["revision"] = json!(disk_revision(&path).unwrap());
        payload["bash_path"] = json!("relative/not-bash");
        assert!(save_settings(&path, serde_json::from_value(payload.clone()).unwrap()).unwrap_err().to_string().contains("absolute"));
        assert_eq!(std::fs::read(generation_settings_path(&path)).unwrap(), original);
        // Empty input restores discovery and changes the existing revision fence.
        let stale = payload["revision"].clone();
        payload["bash_path"] = json!("");
        save_settings(&path, serde_json::from_value(payload.clone()).unwrap()).unwrap().unwrap();
        assert!(load_config(&path).unwrap().bash_path.is_none());
        assert!(crate::shell_environment::read_bash_path(&path).unwrap().is_none());
        payload["revision"] = stale;
        assert!(save_settings(&path, serde_json::from_value(payload).unwrap()).unwrap().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dedicated_model_settings_roundtrip_preserve_omitted_and_clear_explicitly() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        let path = root.join("profiles/web/cordis.patch.yml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_yaml_atomic(&path,&json!([{"id":"llm-pi-ai","config":{"providers":{"test":{"baseURL":"http://localhost:1/v1","api":"openai-completions","models":[{"id":"m","maxTokens":65536}],"contextRequestOptions":{"m":{"chat_template_kwargs":{"enable_thinking":false}}}}}}},{"id":"agent-default-model","config":{"provider":"test","model":"m"}}])).unwrap();
        write_yaml_atomic(
            &credentials_path(&path),
            &json!({"version":1,"refs":{},"records":{}}),
        )
        .unwrap();
        let mut payload = json!({"revision":disk_revision(&path).unwrap(),"providers":[{"id":"test","url":"http://localhost:1/v1","models":["m","gpt-6.1-sol"]}],"orchestrator_provider":"test","orchestrator_model":"m","visual_tool":{"model":"gpt-6.1-sol","reasoning_effort":"medium"},"delegate_agent":{"model":"m","reasoning_effort":"medium"}});
        payload["plugin_models"] =
            json!({"ppt-master":[{"provider":"test","model":"gpt-6.1-sol"}]});
        payload["plugin_generation"] =
            json!({"ppt-master":{"reasoning_effort":"low","visual_reasoning_effort":"low"}});
        save_settings(&path, serde_json::from_value(payload.clone()).unwrap())
            .unwrap()
            .unwrap();
        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded.delegate_agent.model.as_deref(), Some("m"));
        assert_eq!(
            loaded.delegate_agent.reasoning_effort.as_deref(),
            Some("medium")
        );
        assert_eq!(
            loaded.plugin_generation["ppt-master"]
                .reasoning_effort
                .as_deref(),
            Some("low")
        );
        let scoped = restricted_config(&loaded, "ppt-master").unwrap();
        assert_eq!(scoped.visual_tool.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(
            loaded.visual_tool.reasoning_effort.as_deref(),
            Some("medium")
        );
        let mut unrestricted = loaded.clone();
        unrestricted.plugin_models.clear();
        assert_eq!(
            restricted_config(&unrestricted, "ppt-master")
                .unwrap()
                .reasoning_effort
                .as_deref(),
            Some("low")
        );
        assert_eq!(
            restricted_config(&unrestricted, "unrelated")
                .unwrap()
                .reasoning_effort,
            loaded.reasoning_effort
        );
        assert_eq!(
            catalog_view(&scoped)["defaults"]["plugin_generation"]["ppt-master"]["reasoning_effort"],
            "low"
        );
        assert_eq!(
            loaded.providers["test"].extra["contextRequestOptions"]["m"]["chat_template_kwargs"]["enable_thinking"],
            false
        );
        assert_eq!(
            plugin_catalog_view(&loaded, "ppt-master")["models"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let mut invalid = payload.clone();
        invalid["revision"] = json!(disk_revision(&path).unwrap());
        invalid["plugin_models"]["ppt-master"][0]["model"] = json!("unregistered");
        assert!(
            save_settings(&path, serde_json::from_value(invalid).unwrap())
                .unwrap_err()
                .to_string()
                .contains("未在可用模型目录中登记")
        );
        let mut invalid = payload.clone();
        invalid["revision"] = json!(disk_revision(&path).unwrap());
        invalid["providers"][0]["models"] = json!(["m"]);
        assert!(
            save_settings(&path, serde_json::from_value(invalid).unwrap())
                .unwrap_err()
                .to_string()
                .contains("not in provider")
        );
        let mut invalid = payload.clone();
        invalid["revision"] = json!(disk_revision(&path).unwrap());
        invalid["visual_tool"]["provider"] = json!("missing");
        assert!(
            save_settings(&path, serde_json::from_value(invalid).unwrap())
                .unwrap_err()
                .to_string()
                .contains("provider is unavailable")
        );
        // Saving settings keeps the model entry, so its declared limit survives.
        assert_eq!(loaded.providers["test"].max_output_tokens["m"], 65536);
        assert_eq!(
            loaded.visual_tool.reasoning_effort.as_deref(),
            Some("medium")
        );
        payload["revision"] = json!(disk_revision(&path).unwrap());
        payload.as_object_mut().unwrap().remove("visual_tool");
        payload.as_object_mut().unwrap().remove("plugin_models");
        save_settings(&path, serde_json::from_value(payload.clone()).unwrap())
            .unwrap()
            .unwrap();
        payload["revision"] = json!(disk_revision(&path).unwrap());
        payload["visual_tool"] = json!({});
        save_settings(&path, serde_json::from_value(payload).unwrap())
            .unwrap()
            .unwrap();
        let cleared = load_config(&path).unwrap();
        assert_eq!(
            plugin_catalog_view(&cleared, "ppt-master")["models"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(cleared.visual_tool, Default::default());
        assert_eq!(cleared.orchestrator_model.as_deref(), Some("m"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn model_entry_declared_levels_reach_the_catalog_and_the_settings_view() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        let path = root.join("profiles/web/cordis.patch.yml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_yaml_atomic(&path, &json!([
            {"id":"llm-pi-ai","config":{"providers":{
                "deepseek":{"baseURL":"https://api.deepseek.com/v1","api":"openai-completions","models":[
                    {"id":"deepseek-flash","contextWindow":1048576,"maxTokens":393216,
                     "reasoningEfforts":{"low":"low","high":"high","max":"max","false":null,"minimal":"minimal"},
                     "cost":{"longContextThreshold":272000}}
                ]},
                "subapi":{"baseURL":"http://relay.example/v1","api":"openai-completions","models":[
                    {"id":"gpt-6.1-sol","contextWindow":1050000,"maxTokens":128000,
                     "reasoningEfforts":{"none":"none","low":"low","medium":"medium","high":"high","xhigh":"xhigh","max":"max"}}
                ]}
            }}},
            {"id":"agent-default-model","config":{"provider":"deepseek","model":"deepseek-flash"}}
        ])).unwrap();
        write_yaml_atomic(
            &credentials_path(&path),
            &json!({"version":1,"refs":{},"records":{}}),
        )
        .unwrap();
        write_yaml_atomic(
            &generation_settings_path(&path),
            &json!({"model_capabilities":{"deepseek":{"deepseek-flash":{"reasoning_efforts":[],"image_input":"unknown"}}}}),
        )
        .unwrap();
        let loaded = load_config(&path).unwrap();
        let mut state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        apply_agent_config(&mut state, &loaded);
        assert_eq!(crate::model_settings::long_context_threshold(&state, "deepseek-flash"), Some(272_000));
        // An entry that names its levels is selectable without repeating them;
        // `false` and `minimal` fold onto levels the host can save.
        assert_eq!(
            loaded.model_capabilities["deepseek"]["deepseek-flash"].reasoning_efforts,
            vec!["none", "low", "high", "max"]
        );
        let view = catalog_view(&loaded);
        assert_eq!(view["models"][0]["context_window"], 1_048_576);
        assert_eq!(view["models"][0]["capabilities"]["reasoning_efforts"], json!(["none","low","high","max"]));
        assert_eq!(view["models"][1]["capabilities"]["reasoning_efforts"], json!(["none","low","medium","high","xhigh","max"]));
        assert_eq!(view["models"][1]["max_output_tokens"], 128_000);
        // Generation settings still win when they name their own levels.
        let mut generation = read_yaml(&generation_settings_path(&path), json!({})).unwrap();
        generation["model_capabilities"]["deepseek"]["deepseek-flash"]["reasoning_efforts"] =
            json!(["high"]);
        write_yaml_atomic(&generation_settings_path(&path), &generation).unwrap();
        let narrowed = load_config(&path).unwrap();
        assert_eq!(
            narrowed.model_capabilities["deepseek"]["deepseek-flash"].reasoning_efforts,
            vec!["high"]
        );
        let settings = settings_view(&loaded, "rev".into());
        let deepseek = settings["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "deepseek")
            .unwrap();
        assert_eq!(
            deepseek["model_capabilities"]["deepseek-flash"]["reasoning_efforts"],
            json!(["none","low","high","max"])
        );
        // A level list written by hand in the generation settings folds too,
        // otherwise the screen would offer a level its own save rejects.
        let mut aliased = read_yaml(&generation_settings_path(&path), json!({})).unwrap();
        aliased["model_capabilities"]["deepseek"]["deepseek-flash"]["reasoning_efforts"] =
            json!(["minimal", "nano", "false"]);
        write_yaml_atomic(&generation_settings_path(&path), &aliased).unwrap();
        let folded = load_config(&path).unwrap();
        assert_eq!(
            folded.model_capabilities["deepseek"]["deepseek-flash"].reasoning_efforts,
            vec!["none", "low"]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn model_catalog_projects_registered_models_and_keeps_credentials_private() {
        let mut config: AiProxyConfig = serde_json::from_value(json!({
            "orchestrator_provider":"route", "orchestrator_model":"m",
            "visual_tool":{"provider":"route","model":"m"},
            "providers":{"route":{"url":"https://private.example/v1","api_key":"secret-value","api_key_ref":"PRIVATE_ENV","model_map":{"m":"upstream-m"},"api_type":"openai-completions","displayName":"My route"}},
            "model_capabilities":{"route":{"m":{"reasoning_efforts":["low","medium"],"image_input":"unknown","image_probe":{"status":"unverified","response_body":"private-response"}}}}
        })).unwrap();
        config
            .providers
            .get_mut("route")
            .unwrap()
            .model_metadata
            .insert(
                "m".into(),
                json!({"name":"Visible model","contextWindow":500000}),
            );
        config
            .providers
            .get_mut("route")
            .unwrap()
            .max_output_tokens
            .insert("m".into(), 65536);
        let view = catalog_view(&config);
        assert_eq!(view["models"].as_array().unwrap().len(), 1);
        assert_eq!(view["models"][0]["name"], "Visible model");
        assert_eq!(view["models"][0]["context_window"], 500000);
        assert_eq!(view["models"][0]["max_output_tokens"], 65536);
        let root = std::env::temp_dir().join(format!(
            "catalog-budget-{}",
            crate::agent_service::uuid_like()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut state =
            crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        apply_agent_config(&mut state, &config);
        // The model entry's declared window is what a request may use; only a
        // model that declares nothing falls back to the conservative default.
        assert_eq!(
            crate::model_settings::context_window(&state, "upstream-m"),
            500_000
        );
        assert_eq!(
            crate::model_settings::context_window(&state, "m"),
            500_000
        );
        assert_eq!(
            crate::model_settings::context_window(&state, "undeclared"),
            crate::context_window::MAX_TOKENS
        );
        let _ = std::fs::remove_dir_all(root);
        assert_eq!(view["defaults"]["visual_tool"]["model"], "m");
        let encoded = view.to_string();
        for private in [
            "private.example",
            "secret-value",
            "PRIVATE_ENV",
            "private-response",
            "upstream-m",
        ] {
            assert!(!encoded.contains(private));
        }
        config.providers.get_mut("route").unwrap().model_map.clear();
        assert!(
            catalog_view(&config)["models"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
