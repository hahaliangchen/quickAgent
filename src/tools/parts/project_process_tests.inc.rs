    use super::{
        PROCESS_OBSERVATION_TTL_MS, build_process_observation, build_single_process_observation,
        can_reuse_process_observation, external_runtime_path, launch, listener_ownership, node_npm,
        normalize_windows_runtime_path, observation_is_fresh, observation_sample_is_current,
        prefer_exact,
    };
    #[cfg(not(windows))]
    use std::path::Path;
    use std::process::Stdio;

    #[test]
    fn lsof_fields_filter_address_family_port_and_prefer_exact_binding() {
        let output = "p10\nn*:5173\np11\nn127.0.0.1:5173\np12\nn[::1]:5173\np13\nn10.1.2.3:5173\np14\nn127.0.0.1:9999\n";
        assert_eq!(
            super::lsof_listener_pids(output, 5173, "127.0.0.1"),
            vec![11]
        );
        assert_eq!(super::lsof_listener_pids(output, 5173, "::1"), vec![12]);
        assert_eq!(
            super::lsof_listener_pids(output, 5173, "localhost"),
            vec![11, 12]
        );
        assert_eq!(
            super::lsof_listener_pids(output, 5173, "10.1.2.4"),
            vec![10]
        );
        assert!(super::lsof_listener_pids(output, 4321, "localhost").is_empty());
    }

    #[cfg(all(target_os = "linux", target_endian = "little"))]
    #[test]
    fn proc_tcp_tables_decode_native_words_and_ignore_connected_sockets() {
        let v4 =
            "0: 0100007F:1435 00000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 12345";
        assert_eq!(
            super::linux_tcp_listener(v4),
            Some(("127.0.0.1".parse().unwrap(), 5173, 12345))
        );
        let v6 = "0: 00000000000000000000000001000000:1435 00000000000000000000000000000000:0000 0A 0:0 00:0 0 1000 0 12346";
        assert_eq!(
            super::linux_tcp_listener(v6),
            Some(("::1".parse().unwrap(), 5173, 12346))
        );
        let mapped = v6.replace(
            "00000000000000000000000001000000",
            "0000000000000000FFFF00000100007F",
        );
        assert_eq!(
            super::linux_tcp_listener(&mapped).unwrap().0,
            "::ffff:127.0.0.1".parse::<std::net::IpAddr>().unwrap()
        );
        assert!(super::linux_tcp_listener(&v4.replace(" 0A ", " 01 ")).is_none());
        assert!(super::linux_tcp_listener("invalid").is_none());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[tokio::test]
    async fn dual_stack_wildcard_listener_is_found_from_ipv4() {
        let Ok(listener) = tokio::net::TcpListener::bind("[::]:0").await else {
            return;
        };
        let port = listener.local_addr().unwrap().port();
        // Platforms with IPv6-only sockets cannot provide this dual-stack case.
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            return;
        }
        let pids = super::listener_pids(port, "127.0.0.1").unwrap();
        assert!(pids.contains(&std::process::id()), "{pids:?}");
    }

    #[test]
    fn powershell_script_detection_covers_direct_commands_and_configured_paths() {
        assert!(super::script_invokes_powershell(
            r#"powershell.exe -NoProfile -Command "exit 0""#
        ));
        assert!(super::script_invokes_powershell(
            r#"pwsh -File scripts/build.ps1"#
        ));
        assert!(super::powershell_script_shell(
            r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoProfile"#
        ));
        assert!(super::powershell_script_shell(
            r"C:\Program Files\PowerShell\7\pwsh.exe"
        ));
        assert!(super::powershell_script_shell(
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
        ));
        assert!(!super::powershell_script_shell(
            r"C:\Windows\System32\cmd.exe"
        ));
        assert!(!super::script_invokes_powershell("node scripts/build.js"));
    }

    #[tokio::test]
    async fn explicitly_invoked_powershell_package_script_is_rejected_before_launch() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "test-powershell-script-{}-{}",
                std::process::id(),
                super::now()
            ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"bad":"powershell.exe -NoProfile -Command \"exit 0\""}}"#,
        )
        .unwrap();
        let args = serde_json::json!({"project_path":".","script":"bad"});
        let error = launch(
            &root,
            &args,
            false,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("unsupported_script_shell"),
            "{error:#}"
        );
        assert!(error.to_string().contains("bad"), "{error:#}");
        for hook in ["prebad", "postbad"] {
            let mut scripts = serde_json::Map::new();
            scripts.insert(
                "bad".into(),
                serde_json::json!("node -e \"console.log('safe')\""),
            );
            scripts.insert(
                hook.into(),
                serde_json::json!("powershell.exe -NoProfile -Command \"exit 0\""),
            );
            std::fs::write(
                root.join("package.json"),
                serde_json::to_vec(&serde_json::json!({"scripts":scripts})).unwrap(),
            )
            .unwrap();
            let error = launch(
                &root,
                &args,
                false,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains(hook), "{error:#}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn effective_npm_script_shell_is_checked_before_launch() {
        if node_npm().is_err() || std::env::var_os("NPM_CONFIG_SCRIPT_SHELL").is_some() {
            return;
        }
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "test-powershell-shell-{}-{}",
                std::process::id(),
                super::now()
            ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"safe":"node -e \"console.log('must not run')\""}}"#,
        )
        .unwrap();
        std::fs::write(root.join(".npmrc"), "script-shell=powershell.exe\n").unwrap();
        let error = launch(
            &root,
            &serde_json::json!({"project_path":".","script":"safe"}),
            false,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            error.to_string().contains("effective npm script-shell"),
            "{error:#}"
        );
        assert!(error.to_string().contains("PowerShell"), "{error:#}");
    }

    fn fixture_record(
        root: &std::path::Path,
        id: &str,
        project_path: &str,
        args: &[&str],
    ) -> std::sync::Arc<super::Record> {
        let (stop, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let meta = super::Meta {
            process_id: id.to_owned(),
            pid: 1,
            root: root.canonicalize().unwrap(),
            project_path: project_path.to_owned(),
            script: "dev".into(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            operation: "script".into(),
            check_key: format!("npm-start:{project_path}:dev"),
            running: true,
            ready: false,
            listener_owned: false,
            page_ready: false,
            readiness_detail: "pending".into(),
            script_command: "npm run dev".into(),
            node_executable: "node".into(),
            npm_cli: "npm-cli.js".into(),
            ready_url: None,
            ready_port: None,
            started_at: super::now(),
            ended_at: None,
            exit_code: None,
            termination_reason: None,
            background: true,
            sandboxed: false,
        };
        std::sync::Arc::new(super::Record {
            meta: tokio::sync::Mutex::new(meta),
            logs: tokio::sync::Mutex::new(super::Logs::default()),
            persist_guard: tokio::sync::Mutex::new(()),
            dir: root.to_path_buf(),
            stop,
            ended: tokio::sync::Notify::new(),
        })
    }

    #[tokio::test]
    async fn process_snapshot_preserves_error_output_without_diagnosis_or_repair_guidance() {
        let root = std::env::current_dir().unwrap();
        let record = fixture_record(&root, "raw-error", ".", &[]);
        let stderr = "MODULE_NOT_FOUND: cannot find module npm-cli\n  at original.js:17\n";
        {
            let mut meta = record.meta.lock().await;
            meta.running = false;
            meta.exit_code = Some(1);
            meta.termination_reason = Some("exited".into());
            meta.script_command = "concurrently npm run frontend npm run backend".into();
            let mut logs = record.logs.lock().await;
            logs.sequence = 1;
            logs.chunks
                .push_back(serde_json::json!({"seq":1,"stream":"stderr","text":stderr}));
        }
        let result = record.snapshot(&serde_json::json!({})).await;
        assert_eq!(result["stderr"], stderr);
        assert_eq!(result["exit_code"], 1);
        for field in [
            "diagnosis",
            "failure_stage",
            "host_fix_guidance",
            "port_args",
        ] {
            assert!(
                result.get(field).is_none(),
                "host must not interpret process output into {field}"
            );
        }
    }

    #[test]
    fn windows_runtime_paths_normalize_dos_unc_and_reject_device_paths() {
        assert_eq!(
            normalize_windows_runtime_path(r"\\?\C:\Users\Admin\npm-cli.js").unwrap(),
            r"C:\Users\Admin\npm-cli.js"
        );
        assert_eq!(
            normalize_windows_runtime_path(r"\\?\UNC\server\share\npm-cli.js").unwrap(),
            r"\\server\share\npm-cli.js"
        );
        assert_eq!(
            normalize_windows_runtime_path(r"C:\Users\Admin\npm-cli.js").unwrap(),
            r"C:\Users\Admin\npm-cli.js"
        );
        assert!(
            normalize_windows_runtime_path(r"\\?\GLOBALROOT\Device\HarddiskVolume1\npm-cli.js")
                .is_err()
        );
        assert!(normalize_windows_runtime_path(r"\\.\PhysicalDrive0").is_err());
        #[cfg(not(windows))]
        assert_eq!(
            external_runtime_path(Path::new("/tmp/node")).unwrap(),
            std::ffi::OsString::from("/tmp/node")
        );
    }

    #[tokio::test]
    async fn discovered_npm_cli_runs_through_the_external_runtime_path() {
        let Ok(Ok((node, npm))) = tokio::task::spawn_blocking(node_npm).await else {
            return;
        };
        let node = external_runtime_path(&node).unwrap();
        let npm = external_runtime_path(&npm).unwrap();
        let output = tokio::process::Command::new(node)
            .arg(npm)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "npm CLI failed before a project script: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).trim().is_empty(),
            "npm CLI returned no version"
        );
    }

    #[tokio::test]
    async fn run_project_script_enters_the_package_script_through_the_normalized_npm_cli() {
        if node_npm().is_err() {
            return;
        }
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "test-npm-script-{}-{}",
                std::process::id(),
                super::now()
            ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"dev":"node -e \"console.log('npm-script-entered')\""}}"#,
        )
        .unwrap();
        let result = launch(
            &root,
            &serde_json::json!({"project_path":".","script":"dev","timeout_seconds":30}),
            false,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
        let _ = std::fs::remove_dir_all(&root);
        let result = result.unwrap();
        assert_eq!(result["status"], 0, "stderr: {}", result["stderr"]);
        assert!(
            result["stdout"]
                .as_str()
                .unwrap_or("")
                .contains("npm-script-entered"),
            "stdout: {}",
            result["stdout"]
        );
        assert!(result["failure_stage"].is_null());
        assert!(
            result["process_observation"]["input_version"]
                .as_str()
                .is_some()
        );
        assert_eq!(
            result["process_observation"]["scope"]["coverage"],
            "single_process"
        );
        assert_eq!(
            result["process_observation"]["scope"]["operation"],
            "script"
        );
        assert!(!observation_is_fresh(
            &root,
            &serde_json::json!({"project_path":"."}),
            &result["process_observation"]
        ));
    }

    #[test]
    fn scoped_process_observations_expire_on_inputs_host_events_and_ttl() {
        let workspace = std::env::current_dir().unwrap();
        let project_name = format!(
            "test-process-observation-{}-{}",
            std::process::id(),
            super::now()
        );
        let root = workspace.join("target").join(&project_name);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"dev":"node server.js"}}"#,
        )
        .unwrap();
        std::fs::write(root.join("package-lock.json"), r#"{"lockfileVersion":3}"#).unwrap();
        std::fs::write(root.join("npm-shrinkwrap.json"), r#"{"lockfileVersion":3}"#).unwrap();
        let args = serde_json::json!({"project_path":format!("target/{project_name}"),"script":"dev","ready_url":"http://127.0.0.1:4321"});
        let mut sample = build_process_observation(&workspace, &args, &[]).unwrap();
        assert!(observation_sample_is_current(&workspace, &sample));

        let mut old_host = sample.clone();
        old_host["host_instance_id"] = serde_json::json!("previous-host");
        assert!(!observation_sample_is_current(&workspace, &old_host));
        let mut old_events = sample.clone();
        old_events["process_event_generation"] = serde_json::json!(
            sample["process_event_generation"]
                .as_u64()
                .unwrap()
                .saturating_sub(1)
        );
        assert!(!observation_sample_is_current(&workspace, &old_events));
        sample["sampled_at"] =
            serde_json::json!(super::now().saturating_sub(PROCESS_OBSERVATION_TTL_MS + 1));
        assert!(!observation_sample_is_current(&workspace, &sample));

        let changed = build_process_observation(&workspace, &args, &[]).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"dev":"node changed.js"}}"#,
        )
        .unwrap();
        assert!(!observation_sample_is_current(&workspace, &changed));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn process_lists_mark_coverage_preserve_subprojects_and_ignore_log_limits_in_scope() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "test-process-list-{}-{}",
                std::process::id(),
                super::now()
            ));
        for project in ["apps/one", "apps/two"] {
            let directory = root.join(project);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("package.json"),
                r#"{"scripts":{"dev":"node server.js"}}"#,
            )
            .unwrap();
        }
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let canonical = workspace.root().to_path_buf();
        let ids = [
            "project_fixture_one_3001",
            "project_fixture_one_3002",
            "project_fixture_two_4001",
        ];
        {
            let mut records = super::registry().lock().await;
            records.records.insert(
                ids[0].into(),
                fixture_record(&canonical, ids[0], "apps/one", &["--port", "3001"]),
            );
            records.records.insert(
                ids[1].into(),
                fixture_record(&canonical, ids[1], "apps/one", &["--port", "3002"]),
            );
            records.records.insert(
                ids[2].into(),
                fixture_record(&canonical, ids[2], "apps/two", &["--port", "4001"]),
            );
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let workspace_list = super::execute_cancellable(
            &workspace,
            "get_project_process",
            serde_json::json!({}),
            &cancel,
        )
        .await;
        let workspace_script_list = super::execute_cancellable(
            &workspace,
            "get_project_process",
            serde_json::json!({"script":"dev"}),
            &cancel,
        )
        .await;
        let project_list = super::execute_cancellable(
            &workspace,
            "get_project_process",
            serde_json::json!({"project_path":"apps/one"}),
            &cancel,
        )
        .await;
        {
            let mut records = super::registry().lock().await;
            for id in ids {
                records.records.remove(id);
            }
        }

        let workspace_list = workspace_list.unwrap();
        let workspace_script_list = workspace_script_list.unwrap();
        let project_list = project_list.unwrap();
        assert_eq!(workspace_list["processes"].as_array().unwrap().len(), 3);
        assert!(
            workspace_list["processes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|process| process["project_path"] == "apps/two"),
            "workspace listing must retain processes from child projects"
        );
        assert_eq!(
            workspace_list["process_observation"]["scope"]["coverage"],
            "workspace_list"
        );
        assert_eq!(
            workspace_script_list["processes"].as_array().unwrap().len(),
            3,
            "script filtering without project_path remains workspace-wide"
        );
        assert_eq!(
            workspace_script_list["process_observation"]["scope"]["coverage"],
            "workspace_list"
        );
        assert_eq!(
            project_list["processes"].as_array().unwrap().len(),
            2,
            "same-script processes with different args must both remain visible"
        );
        let process_args = project_list["processes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|process| process["args"].to_string())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            process_args.len(),
            2,
            "the list must retain each instance's distinct script args"
        );
        assert_eq!(
            project_list["process_observation"]["scope"]["coverage"],
            "project_list"
        );
        assert!(
            observation_is_fresh(
                &canonical,
                &serde_json::json!({"max_chars":4096}),
                &workspace_list["process_observation"]
            ),
            "max_chars changes log presentation, not process-list identity"
        );
        assert!(observation_is_fresh(
            &canonical,
            &serde_json::json!({"project_path":"apps/one","max_chars":8192}),
            &project_list["process_observation"]
        ));
        let active_probe =
            serde_json::json!({"project_path":"apps/one","ready_url":"http://127.0.0.1:4321"});
        assert!(
            !can_reuse_process_observation(&active_probe),
            "an endpoint request asks for an active readiness probe"
        );

        let first_process = project_list["processes"][0].clone();
        let single = build_single_process_observation(
            &canonical,
            &serde_json::json!({"project_path":"apps/one"}),
            &[super::process_fact(&first_process)],
        )
        .unwrap();
        assert_eq!(single["scope"]["coverage"], "single_process");
        assert!(single["scope"]["process_id"].as_str().is_some());
        assert!(
            !observation_is_fresh(
                &canonical,
                &serde_json::json!({"project_path":"apps/one"}),
                &single
            ),
            "a single process snapshot must not satisfy a complete project list query"
        );
        let install_fact = serde_json::json!({"process_id":"project_fixture_install","project_path":"apps/one","script":"install",
            "args":[],"operation":"install","running":false,"ready":false});
        let install_sample = build_single_process_observation(
            &canonical,
            &serde_json::json!({"project_path":"apps/one"}),
            &[install_fact],
        )
        .unwrap();
        assert_eq!(install_sample["scope"]["operation"], "install");
        assert!(
            !observation_is_fresh(
                &canonical,
                &serde_json::json!({"project_path":"apps/one"}),
                &install_sample
            ),
            "an install result cannot mask the complete project process list"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn wait_seconds_bypasses_pending_snapshot_and_waits_for_delayed_readiness() {
        if node_npm().is_err() {
            return;
        }
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "test-delayed-readiness-{}-{}",
                std::process::id(),
                super::now()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let port_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = port_listener.local_addr().unwrap().port();
        drop(port_listener);
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"dev":"node server.js"}}"#,
        )
        .unwrap();
        std::fs::write(root.join("server.js"),format!(
            "const http=require('http'); setTimeout(()=>http.createServer((req,res)=>res.end('ready')).listen({port},'127.0.0.1'),1500); setInterval(()=>{{}},1000);"
        )).unwrap();
        let launch_args = serde_json::json!({"project_path":".","script":"dev","background":true,
            "ready_url":format!("http://127.0.0.1:{port}/"),"wait_seconds":0});
        let launched = launch(
            &root,
            &launch_args,
            false,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
        let Ok(launched) = launched else {
            let _ = std::fs::remove_dir_all(&root);
            panic!("fixture server did not launch: {launched:?}")
        };
        let process_id = launched["process_id"].as_str().unwrap().to_owned();
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let query_args = serde_json::json!({"project_path":".","script":"dev","wait_seconds":0});
        let pending_result = super::execute_cancellable(
            &workspace,
            "get_project_process",
            query_args,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
        let wait_args = serde_json::json!({"project_path":".","script":"dev","wait_seconds":5});
        let pending_sample = pending_result
            .as_ref()
            .ok()
            .map(|result| result["process_observation"].clone())
            .unwrap_or_default();
        let reuse_rejected = !observation_is_fresh(workspace.root(), &wait_args, &pending_sample);
        let wait_started = tokio::time::Instant::now();
        let ready_result = super::execute_cancellable(
            &workspace,
            "get_project_process",
            wait_args,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
        let waited = wait_started.elapsed();
        let _ = super::execute_cancellable(
            &workspace,
            "stop_project_process",
            serde_json::json!({"process_id":process_id}),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
        super::registry()
            .lock()
            .await
            .records
            .remove(launched["process_id"].as_str().unwrap());
        let _ = std::fs::remove_dir_all(&root);

        let pending = pending_result.unwrap();
        let ready = ready_result.unwrap();
        assert_eq!(
            pending["processes"][0]["ready"], false,
            "fixture must first return a pending snapshot"
        );
        assert_eq!(
            pending["process_observation"]["scope"]["coverage"],
            "project_list"
        );
        assert!(
            reuse_rejected,
            "a waiting query cannot be satisfied by a pending cached snapshot"
        );
        assert!(
            waited >= std::time::Duration::from_millis(700),
            "readiness query returned before delayed listener appeared: {waited:?}"
        );
        assert_eq!(
            ready["processes"][0]["ready"], true,
            "wait_seconds query must probe until delayed readiness succeeds"
        );
    }

    #[tokio::test]
    async fn an_open_port_is_attributed_to_its_listener_not_an_unrelated_pid() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let ownership = listener_ownership(port, "127.0.0.1", 1);
        assert!(!ownership.owned, "{}", ownership.detail);
        // Restricted Windows test runners may deny GetExtendedTcpTable. In that
        // case the production result is conservatively unowned and cannot pass
        // readiness; PID attribution itself requires a privileged runner.
        if ownership.detail.contains("GetExtendedTcpTable failed: 5") {
            return;
        }
        assert!(ownership.detail.contains("pid"), "{}", ownership.detail);
        let other_family = listener_ownership(port, "::1", 1);
        assert!(!other_family.owned, "{}", other_family.detail);
        assert!(
            other_family.detail.contains("nothing is listening"),
            "{}",
            other_family.detail
        );
        assert_eq!(prefer_exact(vec![7], vec![9]), vec![7]);
        assert_eq!(prefer_exact(vec![], vec![9, 9]), vec![9]);
        drop(listener);
        if std::env::var("LIVE_PORT_CHECK").is_ok() {
            let loopback_live = listener_ownership(3000, "127.0.0.1", 1);
            assert!(!loopback_live.owned, "{}", loopback_live.detail);
            assert!(
                loopback_live.detail.contains("codex-workspace-mcp"),
                "{}",
                loopback_live.detail
            );
            assert!(
                !loopback_live.detail.contains("node"),
                "{}",
                loopback_live.detail
            );
            if let Ok(host) = std::env::var("LIVE_PORT_HOST") {
                let editor = listener_ownership(3000, &host, 1);
                assert!(editor.detail.contains("node"), "{}", editor.detail);
                assert!(!editor.owned, "{}", editor.detail);
            }
        }
    }
