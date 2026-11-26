    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn history_reads_during_write_lock_keep_details_and_sse_lossless() {
        use axum::{Router, routing::get};
        use std::time::{Duration, Instant};
        let root = std::env::temp_dir().join(format!("agent-history-http-{}", uuid_like()));
        let mut conn = open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES('task','','test','running',0,0),('other','','test','completed',0,0)", []).unwrap();
        let full = format!("a{}", "中🌊".repeat(4500));
        let large = json!({"actor":"agent","turn":1,"flowNodeId":"flow",
            "message":{"role":"tool","toolCallId":"call","isError":false,"content":[{"type":"text","text":full}]},
            "meta":{"result":{"stdout":full,"status":"done","diff":{"added":2,"patch":full}}}});
        let raw = |kind: &str, data: Value| (kind.to_owned(), data.to_string());
        let entries = [
            raw("assistant/delta", json!({"turn":1,"step":1,"text":"settled"})),
            raw("workspace/changes", json!({"turn":1,"files":[],"total":1})),
            raw("tool/call", json!({"turn":1,"callId":"call","name":"run_program"})),
            raw("tool/result", large.clone()),
            raw("assistant/message", json!({"turn":1,"step":1,"message":{"role":"assistant","content":"committed"}})),
            raw("workspace/changes", json!({"turn":1,"files":[{"path":"kept.txt"}],"total":2})),
            raw("assistant/delta", json!({"turn":1,"step":2,"text":"also settled"})),
            raw("turn/end", json!({"turn":1})),
            raw("assistant/delta", json!({"turn":2,"step":1,"text":"superseded in live turn"})),
            raw("assistant/message", json!({"turn":2,"step":1,"message":{"role":"assistant","content":"step finished"}})),
            raw("assistant/delta", json!({"turn":2,"step":2,"text":"still live"})),
            ("step/start".into(), r#"{ "turn": 2, "step": 3 }"#.into()),
        ];
        for (seq, (kind, data)) in entries.iter().enumerate() {
            conn.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES('task',?1,1,?2,?3)", rusqlite::params![seq,kind,data]).unwrap();
        }
        let state = test_state(&root, "127.0.0.1:1".parse().unwrap());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://{}/agent/tasks", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/agent/tasks/{task_id}/events", get(super::get_events))
            .route("/agent/tasks/{task_id}/events/{seq}", get(super::get_event))
            .route("/agent/tasks/{task_id}/stream", get(super::stream_events))
            .with_state(state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(4)).build().unwrap();
        let history_url = format!("{base}/task/events");

        // Hold the writer until all 50 requests finish. Any schema write would block.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
        tx.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES('task',12,1,'turn/end','{\"turn\":2}')", []).unwrap();
        let results = futures::future::join_all((0..50).map(|_| async {
            let start = Instant::now();
            let response = client.get(&history_url).send().await?.error_for_status()?.text().await?;
            Ok::<_, reqwest::Error>((start.elapsed(), response))
        })).await;
        let (mut timings, bodies): (Vec<_>, Vec<_>) = results.into_iter().collect::<Result<Vec<_>,_>>().unwrap().into_iter().unzip();
        timings.sort_unstable();
        println!("50 history GETs with writer held: p95={:?}", timings[47]);
        assert!(timings[47] < Duration::from_secs(3));
        assert!(bodies.iter().all(|body| body == &bodies[0]));
        assert!(bodies[0].contains(r#"{ "turn": 2, "step": 3 }"#), "ordinary events preserve raw JSON");
        assert!(bodies[0].len() * 8 < entries.iter().map(|(_, data)| data.len()).sum::<usize>());
        let history: Value = serde_json::from_str(&bodies[0]).unwrap();
        let events = history["events"].as_array().unwrap();
        assert_eq!(events.iter().map(|event| event["seq"].as_u64().unwrap()).collect::<Vec<_>>(), [2,3,4,5,7,9,10,11]);
        assert_eq!(events[3]["data"]["files"][0]["path"], "kept.txt");
        assert_eq!(events[6]["data"]["text"], "still live");
        let preview = &events[1]["data"];
        assert_eq!(preview["truncated"], true);
        assert_eq!(preview["full_bytes"], entries[3].1.len());
        assert_eq!(preview["detail_seq"], 3);
        assert_eq!(preview["flowNodeId"], "flow");
        assert_eq!(preview["message"]["toolCallId"], "call");
        assert_eq!(preview["meta"]["result"]["diff"]["added"], 2);
        let prefix = preview["message"]["content"][0]["text"].as_str().unwrap();
        assert!((4092..=4096).contains(&prefix.len()) && full.starts_with(prefix));
        let detail: Value = client.get(format!("{history_url}/3")).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(detail["data"], large);
        assert_eq!(client.get(format!("{base}/other/events/3")).send().await.unwrap().status(), reqwest::StatusCode::NOT_FOUND);

        tx.execute("UPDATE agent_tasks SET status='completed' WHERE id='task'", []).unwrap();
        tx.commit().unwrap();
        let sse = client.get(format!("{base}/task/stream")).send().await.unwrap().error_for_status().unwrap().text().await.unwrap();
        let original: Vec<Value> = sse.lines().filter_map(|line| line.strip_prefix("data: "))
            .map(|data| serde_json::from_str(data).unwrap()).collect();
        assert_eq!(original.len(), entries.len() + 1);
        assert_eq!(original[0]["data"]["text"], "settled");
        assert_eq!(original[3]["data"], large);
        assert_eq!(original.last().unwrap()["seq"], 12);
        server.abort();
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn tool_argument_progress_is_emitted_before_the_call_finishes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let root = std::env::temp_dir().join(format!("agent-tool-progress-{}", uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let conn = open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES('task','','test','running',0,0)", []).unwrap();
        drop(conn);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
            let data = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"write_file","arguments":"{\"content\":\"secret"}}]}}]});
            socket.write_all(format!("data: {data}\n\n").as_bytes()).await.unwrap();
            let _ = wait.await;
            let data = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"}"}}]},"finish_reason":"tool_calls"}]});
            socket.write_all(format!("data: {data}\n\ndata: [DONE]\n\n").as_bytes()).await.unwrap();
        });
        let response = reqwest::get(format!("http://{address}/")).await.unwrap();
        let root_for_task = root.clone();
        let consumer = tokio::spawn(async move {
            consume_model_stream_observed(response, &root_for_task, "task", 1, 1, Some("main"), &CancellationToken::new(), None).await.unwrap()
        });
        let progress = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let conn = open_db(&root).unwrap();
                let value: Option<String> = conn.query_row("SELECT data FROM agent_task_events WHERE task_id='task' AND kind='assistant/delta' ORDER BY seq DESC LIMIT 1", [], |row| row.get(0)).ok();
                if let Some(value) = value { break serde_json::from_str::<Value>(&value).unwrap(); }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.unwrap();
        assert!(!consumer.is_finished(), "the progress must arrive while the provider is still writing");
        assert_eq!(progress["tool_progress"][0]["name"], "write_file");
        assert_eq!(progress["tool_progress"][0]["argument_bytes"], 18);
        assert!(!progress.to_string().contains("secret"));
        release.send(()).unwrap();
        let (message, _) = consumer.await.unwrap();
        assert_eq!(message["tool_calls"][0]["function"]["arguments"], "{\"content\":\"secret\"}");
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    pub(crate) fn test_state(
        root: &std::path::Path,
        address: std::net::SocketAddr,
    ) -> AgentServiceState {
        AgentServiceState {
            workspace: Arc::new(crate::tools::Workspace::new(root.to_path_buf()).unwrap()),
            model_client: crate::model::model_transport::client().unwrap(),
            model_request_slots: Arc::new(tokio::sync::Semaphore::new(3)),
            provider_url: format!("http://{address}/v1"),
            api_key: "fake".into(),
            provider_name: String::new(),
            provider_routes: HashMap::new(),
            default_model: "fake-model".into(),
            model_map: HashMap::new(),
            reasoning_effort: None,
            fast_mode: false,
            permission_mode: PermissionMode::FullAccess,

            visual: Default::default(),
            visual_tool_settings: Default::default(),
            image_generation_settings: Default::default(),
            delegate_agent_settings: Default::default(),
            config: None,
            config_path: None,
            tool_catalog: None,
            plugin_cancel: CancellationToken::new(),
            cancellations: Arc::default(),
        }
    }

    #[test]
    fn terminal_cleanup_preserves_tools_flow_and_messages_after_completion_and_restart() {
        let root = std::env::temp_dir().join(format!("agent-history-retention-{}", uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let mut conn = open_db(&root).unwrap();
        let visible = [
            ("flow/plan", json!({"nodes":[{"id":"probe"}]})),
            (
                "tool/call",
                json!({"callId":"probe_call","name":"http_probe","arguments":"{}"}),
            ),
            (
                "tool/result",
                json!({"toolCallId":"probe_call","message":{"isError":true,"content":[{"type":"text","text":"original failure"}]}}),
            ),
            (
                "assistant/message",
                json!({"message":{"content":[{"type":"tool-call","name":"http_probe"}]}}),
            ),
            (
                "assistant/message",
                json!({"message":{"content":[{"type":"text","text":"final answer with limitation"}]}}),
            ),
            ("worker/progress", json!({"purpose":"Check the service"})),
            (
                "observer/inbox_state",
                json!({"messages":["original advice"]}),
            ),
            (
                "debug/context_request",
                json!({"id":1,"actor":"worker","record_kind":"model_input"}),
            ),
            ("debug/context_end", json!({"id":1})),
        ];
        for status in [
            "completed",
            "failed",
            "cancelled",
            "max_steps",
            "interrupted",
            "running",
        ] {
            conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES (?1,'test','model',?1,0,0)",[status]).unwrap();
            for (kind, data) in &visible {
                append_event(&root, status, kind, data.clone(), None).unwrap();
            }
            append_event(
                &root,
                status,
                "scheduler/state",
                json!({"state":"temporary"}),
                None,
            )
            .unwrap();
            conn.execute(
                "INSERT INTO agent_request_contexts(task_id,actor,turn,step,node_id,metadata,request_json) VALUES (?1,'worker',1,1,'n0','{\"status\":\"ok\"}','{}')",
                [status],
            )
            .unwrap();
            conn.execute("INSERT INTO agent_context_debug(task_id,enabled) VALUES (?1,1)", [status])
                .unwrap();
        }
        cleanup_terminal_task_data(&mut conn, Some("completed"), false).unwrap();
        // Startup cleanup runs over all terminal tasks, including already cleaned tasks.
        cleanup_terminal_task_data(&mut conn, None, false).unwrap();
        for status in [
            "completed",
            "failed",
            "cancelled",
            "max_steps",
            "interrupted",
            "running",
        ] {
            let rows = conn
                .prepare(
                    "SELECT seq,kind,data FROM agent_task_events WHERE task_id=?1 ORDER BY seq",
                )
                .unwrap()
                .query_map([status], |row| {
                    Ok((
                        row.get::<_, usize>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            for (index, (kind, data)) in visible.iter().enumerate() {
                assert_eq!(rows[index].0, index);
                assert_eq!(&rows[index].1, kind);
                assert_eq!(
                    serde_json::from_str::<Value>(&rows[index].2).unwrap(),
                    *data
                );
            }
            assert_eq!(rows.len(), visible.len() + usize::from(status == "running"));
            let contexts: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM agent_request_contexts WHERE task_id=?1",
                    [status],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                contexts, 1,
                "actual inputs survive completion and startup cleanup"
            );
            let settings: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM agent_context_debug WHERE task_id=?1",
                    [status],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(settings, 1);
        }
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn recovery_closes_open_step_and_marks_task_interrupted() {
        let root = std::env::temp_dir().join(format!("agent-recovery-test-{}", uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let task_id = format!("task_{}", uuid_like());
        {
            let conn = open_db(&root).unwrap();
            conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES (?1,'test','fake-model','running',?2,?2)", params![task_id,now()]).unwrap();
        }
        append_event(&root, &task_id, "turn/start", json!({"turn":1}), None).unwrap();
        append_event(
            &root,
            &task_id,
            "step/start",
            json!({"turn":1,"step":1}),
            None,
        )
        .unwrap();

        recover_orphaned_tasks(&root).await.unwrap();

        let conn = open_db(&root).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM agent_tasks WHERE id=?1",
                [&task_id],
                |r| r.get(0),
            )
            .unwrap();
        let kinds: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT kind FROM agent_task_events WHERE task_id=?1 ORDER BY seq")
                .unwrap();
            stmt.query_map([&task_id], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(status, "interrupted");
        assert_eq!(&kinds[2..], ["step/end", "turn/end"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn live_service_lock_prevents_second_instance_from_interrupting_running_task() {
        let root = std::env::temp_dir().join(format!("service-lock-{}", uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let live_lock = try_acquire_service_lock(&root)
            .unwrap()
            .expect("first server acquires exclusive service lock");
        let task_id = "task_live_running".to_string();
        {
            let conn = open_db(&root).unwrap();
            conn.execute(
                "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES (?1,'test','fake-model','running',?2,?2)",
                params![task_id, now()],
            )
            .unwrap();
        }
        append_event(&root, &task_id, "turn/start", json!({"turn":1}), None).unwrap();

        // While the live server holds the lock, a second server opening the same
        // state directory must skip orphan recovery and leave the running task intact.
        let second = acquire_service_lock_and_recover(&root).await.unwrap();
        assert!(second.is_none(), "second server must see existing lock");
        let status_while_locked: String = open_db(&root)
            .unwrap()
            .query_row("SELECT status FROM agent_tasks WHERE id=?1", [&task_id], |r| r.get(0))
            .unwrap();
        assert_eq!(status_while_locked, "running");

        // Once the first server exits (lock dropped), recovery runs normally.
        drop(live_lock);
        let recovered = acquire_service_lock_and_recover(&root).await.unwrap();
        assert!(recovered.is_some(), "lock should be free after first server exits");
        let status_after_drop: String = open_db(&root)
            .unwrap()
            .query_row("SELECT status FROM agent_tasks WHERE id=?1", [&task_id], |r| r.get(0))
            .unwrap();
        assert_eq!(status_after_drop, "interrupted");
        drop(recovered);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workspace_file_http_range_and_task_scope() {
        use axum::{routing::get, Router};
        use reqwest::header;
        let root = std::env::temp_dir().join(format!("file-preview-{}", uuid_like()));
        let workspace = root.join("work");
        std::fs::create_dir_all(workspace.join("docs")).unwrap();
        std::fs::write(root.join("outside.txt"), b"private").unwrap();
        std::fs::write(workspace.join("docs/report.pdf"), b"0123456789").unwrap();
        std::fs::write(workspace.join("empty.txt"), b"").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("outside.txt"), workspace.join("linked.txt")).unwrap();
            std::os::unix::fs::symlink(workspace.join("docs"), workspace.join("linked-dir")).unwrap();
        }
        let db = open_db(&root).unwrap();
        db.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES('task','','test','completed',0,0)", []).unwrap();
        db.execute("INSERT INTO agent_session_meta(task_id,workspace) VALUES('task',?1)", [workspace.to_str().unwrap()]).unwrap();
        drop(db);
        let state = test_state(&root, "127.0.0.1:1".parse().unwrap());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://{}/agent/tasks", listener.local_addr().unwrap());
        let app = Router::new().route("/agent/tasks/{task_id}/workspace-file", get(super::workspace_file)).with_state(state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("{base}/task/workspace-file");
        let full = client.get(&url).query(&[("path", "docs/report.pdf")]).send().await.unwrap();
        assert_eq!(full.status(), 200);
        assert_eq!(full.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(full.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(full.headers()[header::CONTENT_TYPE], "application/pdf");
        assert_eq!(full.headers()[header::CONTENT_LENGTH], "10");
        assert!(full.headers().contains_key(header::ETAG));
        assert_eq!(full.bytes().await.unwrap().as_ref(), b"0123456789");
        for (range, expected, body) in [("bytes=1-3", "bytes 1-3/10", "123"), ("bytes=-3", "bytes 7-9/10", "789"), ("bytes=8-", "bytes 8-9/10", "89")] {
            let response = client.get(&url).query(&[("path", "docs/report.pdf")]).header(header::RANGE, range).send().await.unwrap();
            assert_eq!(response.status(), 206);
            assert_eq!(response.headers()[header::CONTENT_RANGE], expected);
            assert_eq!(response.text().await.unwrap(), body);
        }
        for range in ["bytes=10-", "bytes=-0", "bytes=3-2", "bytes=0-1,3-4", "whatever", "bytes=+1-3"] {
            let response = client.get(&url).query(&[("path", "docs/report.pdf")]).header(header::RANGE, range).send().await.unwrap();
            assert_eq!(response.status(), 416, "{range}");
            assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */10");
        }
        let head = client.head(&url).query(&[("path", "docs/report.pdf")]).header(header::RANGE, "bytes=2-4").send().await.unwrap();
        assert_eq!(head.status(), 206);
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "3");
        assert!(head.bytes().await.unwrap().is_empty());
        let empty = client.get(&url).query(&[("path", "empty.txt")]).header(header::RANGE, "bytes=0-").send().await.unwrap();
        assert_eq!(empty.status(), 416);
        assert_eq!(empty.headers()[header::CONTENT_RANGE], "bytes */0");
        for path in [workspace.join("docs/report.pdf").display().to_string(), format!("file://{}/docs/report.pdf", workspace.display())] {
            assert_eq!(client.get(&url).query(&[("path", path)]).send().await.unwrap().status(), 200);
        }
        let denied = vec!["../outside.txt".to_owned(), "docs/../empty.txt".to_owned(), "docs".to_owned(), root.join("outside.txt").display().to_string()];
        #[cfg(unix)]
        let denied = denied.into_iter().chain(["linked.txt".into(), "linked-dir/report.pdf".into()]);
        for path in denied {
            assert_eq!(client.get(&url).query(&[("path", &path)]).send().await.unwrap().status(), 404, "{path}");
        }
        assert_eq!(client.get(format!("{base}/unknown/workspace-file")).query(&[("path", "outside.txt")]).send().await.unwrap().status(), 404);
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn workspace_file_routes_general_chat_to_unified_chat_assets_folder() {
        use axum::{routing::get, Router};
        let root = std::env::temp_dir().join(format!("chat-assets-preview-{}", uuid_like()));
        let assets = crate::context::session_browser::chat_assets_dir(&root).unwrap();
        std::fs::create_dir_all(assets.join("daily-report-skill")).unwrap();
        std::fs::write(assets.join("daily-report-skill/SKILL.md"), b"# Daily Guide").unwrap();
        std::fs::write(root.join("legacy-note.md"), b"# Legacy").unwrap();
        let db = open_db(&root).unwrap();
        db.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES('chat_new','','test','completed',0,0)", []).unwrap();
        db.execute("INSERT INTO agent_session_meta(task_id,workspace,kind) VALUES('chat_new',?1,'chat')", [assets.to_str().unwrap()]).unwrap();
        db.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES('chat_legacy','','test','completed',0,0)", []).unwrap();
        db.execute("INSERT INTO agent_session_meta(task_id,workspace,kind) VALUES('chat_legacy',?1,'chat')", [root.to_str().unwrap()]).unwrap();
        drop(db);

        assert_eq!(
            crate::context::session_browser::task_workspace(&root, "chat_new").unwrap(),
            assets
        );
        assert_eq!(
            crate::context::session_browser::task_workspace(&root, "chat_legacy").unwrap(),
            assets
        );

        let state = test_state(&root, "127.0.0.1:1".parse().unwrap());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://{}/agent/tasks", listener.local_addr().unwrap());
        let app = Router::new().route("/agent/tasks/{task_id}/workspace-file", get(super::workspace_file)).with_state(state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        for id in ["chat_new", "chat_legacy"] {
            let resp = client
                .get(format!("{base}/{id}/workspace-file"))
                .query(&[("path", "daily-report-skill/SKILL.md")])
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "{id}");
            assert_eq!(resp.text().await.unwrap(), "# Daily Guide");

            // Absolute path inside chat-assets also resolves cleanly.
            let abs_resp = client
                .get(format!("{base}/{id}/workspace-file"))
                .query(&[("path", assets.join("daily-report-skill/SKILL.md").to_str().unwrap())])
                .send()
                .await
                .unwrap();
            assert_eq!(abs_resp.status(), 200, "{id} abs");

            // Traversal and host-state paths remain strictly blocked.
            let denied = client
                .get(format!("{base}/{id}/workspace-file"))
                .query(&[("path", "../.codex-workspace-mcp/codex_state.db")])
                .send()
                .await
                .unwrap();
            assert_eq!(denied.status(), 404, "{id} denied");
        }
        // Pre-migration file under root still remains readable as fallback for chat.
        let legacy = client
            .get(format!("{base}/chat_legacy/workspace-file"))
            .query(&[("path", "legacy-note.md")])
            .send()
            .await
            .unwrap();
        assert_eq!(legacy.status(), 200);
        assert_eq!(legacy.text().await.unwrap(), "# Legacy");

        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn workspace_file_attachment_range_head_and_scoping() {
        use axum::{routing::get, Router};
        use reqwest::header;
        let root = std::env::temp_dir().join(format!("attachment-preview-{}", uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let db = open_db(&root).unwrap();
        db.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES('task','','test','completed',0,0)", []).unwrap();
        let state = test_state(&root, "127.0.0.1:1".parse().unwrap());
        let attachment = crate::media::attachments::store(&state, "task", "sample.txt", b"abcdef").unwrap();
        let receipt: String = db.query_row("SELECT receipt FROM agent_attachments WHERE task_id='task'", [], |r| r.get(0)).unwrap();
        db.execute("UPDATE agent_attachments SET turn=1 WHERE receipt=?1", [&receipt]).unwrap();
        drop(db);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://{}/agent/tasks", listener.local_addr().unwrap());
        let app = Router::new().route("/agent/tasks/{task_id}/attachments/{receipt}", get(crate::media::attachments::download)).with_state(state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("{base}/task/attachments/{receipt}");
        let response = client.get(&url).header(header::RANGE,"bytes=1-3").send().await.unwrap();
        assert_eq!(response.status(),206);
        assert_eq!(response.headers()[header::CONTENT_RANGE],"bytes 1-3/6");
        assert_eq!(response.text().await.unwrap(),"bcd");
        let head = client.head(format!("{base}/task/attachments/{}", attachment["file"]["attachmentId"].as_str().unwrap())).send().await.unwrap();
        assert_eq!(head.status(),200);
        assert_eq!(head.headers()[header::CONTENT_LENGTH],"6");
        assert!(head.bytes().await.unwrap().is_empty());
        assert_eq!(client.get(format!("{base}/wrong/attachments/{receipt}")).send().await.unwrap().status(),404);
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(root).unwrap();
    }
