    use super::*;

    #[test]
    fn read_file_contract_distinguishes_admission_and_return_limits() {
        let definitions = tool_definitions();
        let definition = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "read_file")
            .unwrap();
        let description = definition["description"].as_str().unwrap();
        assert!(description.contains("file-size admission limit"), "{description}");
        assert!(description.contains("rejected; it never truncates"), "{description}");
        assert!(description.contains("use max_chars and normally omit max_bytes"), "{description}");
        assert!(definition["inputSchema"]["properties"]["max_bytes"]["description"]
            .as_str().unwrap().contains("rejected; it is not truncated"));
        assert!(definition["inputSchema"]["properties"]["max_chars"]["description"]
            .as_str().unwrap().contains("limits the returned text"));
    }

    struct TestMcpSession {
        id: String,
        skills: Arc<tokio::sync::Mutex<SkillActivation>>,
    }
    impl TestMcpSession {
        async fn new(workspace: &Arc<Workspace>) -> Self {
            let (headers, response) = rpc(workspace, None, "initialize", json!({})).await;
            assert!(response["error"].is_null(), "{response}");
            let id = headers["mcp-session-id"].to_str().unwrap().to_owned();
            let skills = sessions().lock().unwrap().get(&id).unwrap().skills.clone();
            Self { id, skills }
        }
        fn headers(&self) -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert("mcp-session-id", self.id.parse().unwrap());
            headers
        }
        async fn call(&self, workspace: &Arc<Workspace>, name: &str, args: Value) -> Value {
            let (_, response) = rpc(workspace, Some(&self.id), "tools/call",
                json!({"name":name,"arguments":args})).await;
            assert!(response["error"].is_null(), "{response}");
            response["result"]["structuredContent"].clone()
        }
    }
    impl Drop for TestMcpSession {
        fn drop(&mut self) {
            sessions().lock().unwrap().remove(&self.id);
        }
    }

    async fn rpc(
        workspace: &Arc<Workspace>,
        token: Option<&str>,
        method: &str,
        params: Value,
    ) -> (HeaderMap, Value) {
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            headers.insert("mcp-session-id", token.parse().unwrap());
        }
        let response = handle_mcp(
            State(workspace.clone()),
            headers,
            Json(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})),
        )
        .await;
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (headers, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn shell_requires_scope_and_stateless_listing_hides_it() {
        let workspace = Arc::new(Workspace::new(std::env::current_dir().unwrap()).unwrap());
        let (_, listed) = rpc(&workspace, None, "tools/list", json!({})).await;
        assert!(listed["result"]["tools"].as_array().unwrap().iter()
            .all(|tool| !crate::tools::shell_process::is_tool(tool["name"].as_str().unwrap())));
        for name in crate::tools::shell_process::TOOLS {
            let params = json!({"name":name,"arguments":{"cmd":"echo must-not-run","session_id":"unowned"}});
            let error = call_tool(&workspace, params.clone()).await.unwrap_err();
            assert!(error.to_string().contains("scoped MCP session"), "{error:#}");
            let (_, response) = rpc(&workspace, None, "tools/call", params).await;
            assert!(response["error"]["message"].as_str().unwrap()
                .contains("initialize an MCP session"), "{response}");
        }
    }

    #[tokio::test]
    async fn scoped_shell_definitions_follow_environment_availability() {
        let workspace = Arc::new(Workspace::new(std::env::current_dir().unwrap()).unwrap());
        let session = TestMcpSession::new(&workspace).await;
        let available = crate::tools::shell_environment::resolve().is_ok();
        let (_, listed) = rpc(&workspace, Some(&session.id), "tools/list", json!({})).await;
        for name in crate::tools::shell_process::TOOLS {
            assert_eq!(listed["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == *name), available);
        }
    }

    #[test]
    fn native_registry_always_contains_shell_schemas() {
        let definitions = tool_definitions();
        let definitions = definitions.as_array().unwrap();
        for name in crate::tools::shell_process::TOOLS {
            let schemas = definitions.iter().filter(|tool| tool["name"] == *name).collect::<Vec<_>>();
            assert_eq!(schemas.len(), 1, "{name} must be registered exactly once");
            assert_eq!(schemas[0]["inputSchema"]["type"], "object");
            assert!(!schemas[0]["inputSchema"]["required"].as_array().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn shell_scope_cancels_on_delete_expiration_and_activation_drop() {
        let workspace = Arc::new(Workspace::new(std::env::current_dir().unwrap()).unwrap());
        let first = TestMcpSession::new(&workspace).await;
        let second = TestMcpSession::new(&workspace).await;
        let first_cancel = first.skills.lock().await.shell.cancel.clone();
        let second_cancel = second.skills.lock().await.shell.cancel.clone();
        assert_ne!(first.skills.lock().await.shell.task, second.skills.lock().await.shell.task);
        // Deleting must cancel even with an in-flight RPC holding activation.
        let mut held = first.skills.lock().await;
        assert_eq!(handle_mcp_delete(first.headers()).await.status(), StatusCode::NO_CONTENT);
        assert!(first_cancel.is_cancelled());
        assert!(!second_cancel.is_cancelled());
        assert!(held.shell.context(&workspace)
            .await.err().unwrap().to_string().contains("closed"));
        drop(held);
        // Windows Instant cannot predate boot. Inject expiration instead of
        // subtracting two hours (which panics on a freshly restarted machine).
        expire_session_with_timeout(second.id.clone(), second_cancel.clone(),std::time::Duration::ZERO).await;
        assert!(second_cancel.is_cancelled());
        assert!(!sessions().lock().unwrap().contains_key(&second.id));
        let activation = SkillActivation::default();
        let cancel = activation.shell.cancel.clone();
        drop(activation);
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn scoped_shell_exec_write_kill_and_assets_stay_out_of_ui_sessions() {
        if let Err(error) = crate::tools::shell_environment::resolve() {
            eprintln!("skipped: shell environment unavailable: {error:#}");
            return;
        }
        let root = std::env::temp_dir().join(format!("mcp-shell-{}", crate::server::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = Arc::new(Workspace::new(&root).unwrap());
        let first = TestMcpSession::new(&workspace).await;
        let second = TestMcpSession::new(&workspace).await;
        let (_, listed) = rpc(&workspace, Some(&first.id), "tools/list", json!({})).await;
        for name in crate::tools::shell_process::TOOLS {
            assert!(listed["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == *name));
        }
        let read_command = if cfg!(windows) {
            r#"$line = [Console]::ReadLine(); Write-Output "received:$line"; Start-Sleep -Seconds 120"#
        } else {
            "read -r line; printf 'received:%s\\n' \"$line\"; sleep 120 & wait"
        };
        let started = first.call(&workspace, "exec_command", json!({
            "cmd":read_command,
            "yield_time_ms":0,
            "stdin":true
        })).await;
        let id = started["session_id"].as_str().unwrap();
        assert_eq!(started["running"], true);
        for name in ["write_stdin", "kill_session"] {
            let (_, rejected) = rpc(&workspace, Some(&second.id), "tools/call",
                json!({"name":name,"arguments":{"session_id":id,"chars":"must-not-send\n"}})).await;
            assert!(rejected["error"]["message"].as_str().unwrap().contains("SHELL_SESSION_SCOPE_DENIED"), "{rejected}");
        }
        let (_, busy) = rpc(&workspace, Some(&second.id), "tools/call",
            json!({"name":"exec_command","arguments":{"cmd":"echo must-not-run"}})).await;
        assert!(busy["error"]["message"].as_str().unwrap().contains("SHELL_BUSY"), "{busy}");
        let coexist_command = if cfg!(windows) { "Write-Output coexist" } else { "printf coexist" };
        let alongside = first.call(&workspace, "exec_command",
            json!({"cmd":coexist_command,"yield_time_ms":10000})).await;
        assert_eq!(alongside["running"], false, "{alongside}");
        assert_eq!(alongside["stdout"].as_str().unwrap().trim_end(), "coexist");
        assert!(alongside["capture_error"].is_null(), "{alongside}");
        let mut written = first.call(&workspace, "write_stdin",
            json!({"session_id":id,"chars":"hello\n","yield_time_ms":1000})).await;
        let mut stdout = written["stdout"].as_str().unwrap().to_owned();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !stdout.contains("received:hello") {
                written = first.call(&workspace, "write_stdin",
                    json!({"session_id":id,"yield_time_ms":100})).await;
                stdout.push_str(written["stdout"].as_str().unwrap());
            }
        }).await.unwrap();
        let killed = first.call(&workspace, "kill_session", json!({"session_id":id})).await;
        assert_eq!(killed["running"], false, "{killed}");
        assert_eq!(killed["termination_reason"], "requested_stop");
        assert!(killed["capture_error"].is_null(), "{killed}");

        let output_command = if cfg!(windows) {
            "[Console]::Out.Write(('0' * 5000)); exit 7"
        } else {
            "printf '%05000d' 0; exit 7"
        };
        let mut output = first.call(&workspace, "exec_command",
            json!({"cmd":output_command,"max_output_tokens":128})).await;
        let mut saved = output["output_asset"].is_object();
        let id = output["session_id"].as_str().unwrap().to_owned();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while output["running"] == true {
                output = first.call(&workspace, "write_stdin",
                    json!({"session_id":id,"yield_time_ms":100,"max_output_tokens":128})).await;
                saved |= output["output_asset"].is_object();
            }
        }).await.unwrap();
        assert_eq!(output["exit_code"], 7);
        assert!(saved, "large output must have a durable asset");
        let page = first.call(&workspace, "write_stdin",
            json!({"session_id":id,"stream":"stdout","char_offset":0,"max_output_tokens":128})).await;
        assert_eq!(page["total_chars"], 5000);

        let state = first.skills.lock().await.shell.state.clone().unwrap();
        assert!(matches!(state.permission_mode, crate::server::agent_service::PermissionMode::FullAccess));
        let response = crate::server::agent_service::list_tasks(State(state),
            axum::extract::Query(crate::server::agent_service::ListTasksQuery { limit: None })).await.into_response();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let ui: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(ui["tasks"], json!([]), "{ui}");
        let task = first.skills.lock().await.shell.task.clone();
        let conn = crate::server::agent_service::open_db(&root).unwrap();
        let events: i64 = conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id=?1 AND kind IN ('workspace/changes','shell/output')",
            [&task], |row| row.get(0)).unwrap();
        assert!(events >= 3);
        drop(conn);
        drop(first);
        drop(second);
        // The shared DB writer can retain its SQLite handle until its idle timeout on Windows.
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn kill_rpc_interrupts_stdin_larger_than_pipe_without_deleting_session() {
        if let Err(error) = crate::tools::shell_environment::resolve() {
            eprintln!("skipped: shell environment unavailable: {error:#}");
            return;
        }
        let root = std::env::temp_dir().join(format!("mcp-shell-blocked-stdin-{}", crate::server::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = Arc::new(Workspace::new(&root).unwrap());
        let session = TestMcpSession::new(&workspace).await;
        let cmd = if cfg!(windows) {
            "Write-Output ready; Start-Sleep -Seconds 120"
        } else {
            "printf 'ready\\n'; sleep 120"
        };
        let mut started = session.call(&workspace, "exec_command", json!({"cmd":cmd,"yield_time_ms":100,"stdin":true})).await;
        let id = started["session_id"].as_str().unwrap().to_owned();
        let mut stdout = started["stdout"].as_str().unwrap().to_owned();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !stdout.contains("ready") {
                assert_eq!(started["running"], true, "{started}");
                started = session.call(&workspace, "write_stdin",
                    json!({"session_id":id,"yield_time_ms":100})).await;
                stdout.push_str(started["stdout"].as_str().unwrap());
            }
        }).await.unwrap();
        // Far beyond the pipe buffer. yield=0 means a pending RPC is blocked
        // delivering stdin, rather than simply waiting for command completion.
        let mut write = Box::pin(rpc(&workspace, Some(&session.id), "tools/call", json!({
            "name":"write_stdin", "arguments":{
                "session_id":id,"chars":"x".repeat(4 * 1024 * 1024),"yield_time_ms":0
            }
        })));
        assert!(tokio::time::timeout(std::time::Duration::from_millis(300), write.as_mut()).await.is_err(),
            "stdin must remain blocked until the separate kill RPC");
        let (killed, (_, written)) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(
                session.call(&workspace, "kill_session", json!({"session_id":id})),
                write.as_mut()
            )
        }).await.expect("kill RPC must interrupt blocked stdin without DELETE");
        drop(write);
        assert_eq!(killed["running"], false, "{killed}");
        assert_eq!(killed["termination_reason"], "requested_stop", "{killed}");
        assert!(written["error"]["message"].is_string(), "blocked write should fail after kill: {written}");
        assert!(sessions().lock().unwrap().contains_key(&session.id));
        assert!(!session.skills.lock().await.shell.cancel.is_cancelled());
        let next = session.call(&workspace, "exec_command", json!({
            "cmd":if cfg!(windows) { "Write-Output recovered" } else { "printf recovered" },
            "yield_time_ms":10000
        })).await;
        assert_eq!(next["running"], false, "{next}");
        assert_eq!(next["stdout"].as_str().unwrap().trim_end(), "recovered");
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn deleting_shell_session_stops_live_process_tree_and_retained_scope() {
        if let Err(error) = crate::tools::shell_environment::resolve() {
            eprintln!("skipped: shell environment unavailable: {error:#}");
            return;
        }
        let root = std::env::temp_dir().join(format!("mcp-shell-cancel-{}", crate::server::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = Arc::new(Workspace::new(&root).unwrap());
        let session = TestMcpSession::new(&workspace).await;
        let tree_command = if cfg!(windows) {
            "$child = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120' -WindowStyle Hidden -PassThru; Write-Output 'child-started'; $child.WaitForExit()"
        } else {
            "sleep 120 & printf 'child-started\\n'; wait"
        };
        let mut started = session.call(&workspace, "exec_command",
            json!({"cmd":tree_command,"yield_time_ms":1000})).await;
        let id = started["session_id"].as_str().unwrap().to_owned();
        let mut stdout = started["stdout"].as_str().unwrap().to_owned();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !stdout.contains("child-started") {
                assert_eq!(started["running"], true, "{started}");
                started = session.call(&workspace, "write_stdin",
                    json!({"session_id":id,"yield_time_ms":100})).await;
                stdout.push_str(started["stdout"].as_str().unwrap());
            }
        }).await.unwrap();
        let task = session.skills.lock().await.shell.task.clone();
        let state = session.skills.lock().await.shell.state.clone().unwrap();
        assert_eq!(handle_mcp_delete(session.headers()).await.status(), StatusCode::NO_CONTENT);
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let result = crate::tools::shell_process::execute(&state, &workspace, &task, 1, None, "write_stdin",
                    &json!({"session_id":started["session_id"],"yield_time_ms":100}),
                    &tokio_util::sync::CancellationToken::new(), false).await.unwrap();
                if result["running"] == false { break result; }
            }
        }).await.unwrap();
        assert!(crate::tools::shell_process::active(&root).await.is_none());
        assert_eq!(result["running"], false, "{result}");
        assert_eq!(result["termination_reason"], "task_cancelled", "{result}");
        assert!(result["capture_error"].is_null(), "{result}");
        let error = session.skills.lock().await.shell.context(&workspace).await.err().unwrap();
        assert!(error.to_string().contains("closed"));
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn mcp_retires_code_index_tools_and_resources() {
        let workspace = Arc::new(Workspace::new(std::env::current_dir().unwrap()).unwrap());
        let (_, listed) = rpc(&workspace, None, "tools/list", json!({})).await;
        let names = listed["result"]["tools"].as_array().unwrap().iter()
            .filter_map(|tool| tool["name"].as_str()).collect::<Vec<_>>();
        assert!(names.contains(&"search_text") && names.contains(&"read_file_lines"));
        assert!(RETIRED_CODE_INDEX_TOOLS.iter().all(|name| !names.contains(name)));
        let (_, result) = rpc(&workspace, None, "tools/call",
            json!({"name":"search_code_map","arguments":{"query":"main"}})).await;
        assert!(result["error"]["message"].as_str().unwrap().contains("TOOL_UNAVAILABLE"), "{result}");
        let (_, result) = rpc(&workspace, None, "resources/read",
            json!({"uri":"mcp://codex-workspace-mcp/ast/status"})).await;
        assert!(result["error"]["message"].is_string(), "{result}");
    }

    #[tokio::test]
    async fn mcp_skill_sessions_isolate_activation_and_invalidate_reloaded_packages() {
        let root =
            std::env::temp_dir().join(format!("mcp-skill-{}", crate::server::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("code-flow.json"),
            r#"{"version":1,"features":[]}"#,
        )
        .unwrap();
        let workspace = Arc::new(Workspace::new(&root).unwrap());
        let (runtime, catalog) = crate::server::skill_packages::test_runtime();
        crate::server::skill_packages::bind_root(&catalog);
        let (headers, init) = rpc(&workspace, None, "initialize", json!({})).await;
        assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], true);
        let first = headers["mcp-session-id"].to_str().unwrap().to_owned();
        let (headers, _) = rpc(&workspace, None, "initialize", json!({})).await;
        let second = headers["mcp-session-id"].to_str().unwrap().to_owned();
        let has_flow = |packet: &Value| {
            packet["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "read_code_flow")
        };
        assert!(!has_flow(
            &rpc(&workspace, Some(&first), "tools/list", json!({}))
                .await
                .1
        ));
        let mut changed = sessions()
            .lock()
            .unwrap()
            .get(&first)
            .unwrap()
            .changed
            .subscribe();
        let packet = rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"read_skill","arguments":{"name":"code-flow"}}),
        )
        .await
        .1;
        assert!(packet["error"].is_null());
        assert!(changed.has_changed().unwrap());
        changed.borrow_and_update();
        assert!(has_flow(
            &rpc(&workspace, Some(&first), "tools/list", json!({}))
                .await
                .1
        ));
        assert!(!has_flow(
            &rpc(&workspace, Some(&second), "tools/list", json!({}))
                .await
                .1
        ));
        assert!(!has_flow(
            &rpc(&workspace, None, "tools/list", json!({})).await.1
        ));
        let rejected = rpc(
            &workspace,
            Some(&second),
            "tools/call",
            json!({"name":"read_code_flow"}),
        )
        .await
        .1;
        assert!(
            rejected["error"]["message"]
                .as_str()
                .unwrap()
                .contains("TOOL_UNAVAILABLE")
        );
        let result = rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"read_code_flow"}),
        )
        .await
        .1;
        assert!(result["result"]["structuredContent"]["features"].is_array());
        crate::server::skill_packages::reload(&runtime, &catalog.packages, "code-flow").unwrap();
        assert!(!has_flow(
            &rpc(&workspace, Some(&first), "tools/list", json!({}))
                .await
                .1
        ));
        rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"read_skill","arguments":{"name":"code-flow"}}),
        )
        .await;
        rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"unload_skill","arguments":{"name":"code-flow"}}),
        )
        .await;
        assert!(!has_flow(
            &rpc(&workspace, Some(&first), "tools/list", json!({}))
                .await
                .1
        ));
        rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"read_skill","arguments":{"name":"code-flow"}}),
        )
        .await;
        rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"reset_skill_activation"}),
        )
        .await;
        assert!(!has_flow(
            &rpc(&workspace, Some(&first), "tools/list", json!({}))
                .await
                .1
        ));
        let mut headers = HeaderMap::new();
        headers.insert("mcp-session-id", first.parse().unwrap());
        assert_eq!(
            handle_mcp_delete(headers.clone()).await.status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            handle_mcp_get(headers).await.status(),
            StatusCode::NOT_FOUND
        );
        sessions().lock().unwrap().remove(&second);
        runtime.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn legacy_run_command_is_disabled_and_native_tool_is_advertised() {
        let root = std::env::current_dir().unwrap();
        let workspace = Workspace::new(&root).unwrap();
        let result = call_tool(
            &workspace,
            json!({"name":"run_command","arguments":{"command":"echo should-not-run"}}),
        )
        .await
        .unwrap();
        assert_eq!(result["structuredContent"]["outcome"], "disabled");
        assert!(
            result["structuredContent"]["error"]
                .as_str()
                .unwrap()
                .contains("run_program")
        );
        let definitions = tool_definitions();
        let names = definitions
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"run_program"));
        assert!(!names.contains(&"run_command"));
    }
