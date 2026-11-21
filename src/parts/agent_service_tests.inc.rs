    use super::*;

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
            model_client: crate::model_transport::client().unwrap(),
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
        conn.execute_batch(
            "CREATE TABLE agent_request_contexts(task_id TEXT,request_json TEXT);
            CREATE TABLE agent_context_debug(task_id TEXT,enabled INTEGER);",
        )
        .unwrap();
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
                "INSERT INTO agent_request_contexts VALUES (?1,'raw request')",
                [status],
            )
            .unwrap();
            conn.execute("INSERT INTO agent_context_debug VALUES (?1,1)", [status])
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
