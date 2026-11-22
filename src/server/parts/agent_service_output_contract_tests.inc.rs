    use super::*;

    /// The declared return contracts must describe what the tools return.
    #[tokio::test]
    async fn file_tool_results_match_their_declared_contracts() {
        let root = std::env::temp_dir().join(format!("output-contracts-{}", uuid_like()));
        std::fs::create_dir_all(root.join("docs")).unwrap();
        // Longer than one default page, so pagination fields are exercised.
        std::fs::write(root.join("docs/long.md"), "设计文档的一行内容。\n".repeat(4000)).unwrap();
        let workspace = crate::tools::Workspace::new(root.clone()).unwrap();
        let tools = local_tools(&workspace, None, false);
        let schema = |name: &str| tools.iter().find(|t| t["function"]["name"] == name).unwrap()["function"]["outputSchema"].clone();
        let calls = [
            ("list_dir", json!({"path":"docs"})),
            ("read_file", json!({"path":"docs/long.md"})),
            ("read_file_lines", json!({"path":"docs/long.md","start_line":1,"end_line":3})),
            ("read_file_chunk", json!({"path":"docs/long.md","max_bytes":512})),
            ("search_text", json!({"query":"设计","path":"docs","max_matches":2})),
            ("write_file", json!({"path":"docs/new.md","content":"x"})),
            ("edit_file", json!({"path":"docs/new.md","edits":[{"old_text":"x","new_text":"y"}]})),
            ("replace_range", json!({"path":"docs/new.md","start_line":1,"end_line":1,"replacement":"z"})),
        ];
        let mut first_page = Value::Null;
        for (name, arguments) in calls {
            let mut arguments = arguments;
            if name != "write_file" && arguments["path"] == "docs/new.md" {
                let hash = crate::server::mcp::call_tool(&workspace, json!({"name":"read_file","arguments":{"path":"docs/new.md"}})).await.unwrap();
                let hash: Value = serde_json::from_str(hash["content"][0]["text"].as_str().unwrap()).unwrap();
                arguments["expected_code_hash"] = hash["code_hash"].clone();
            }
            let response = crate::server::mcp::call_tool(&workspace, json!({"name":name,"arguments":arguments})).await.unwrap();
            let result: Value = serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            let validator = jsonschema::validator_for(&schema(name)).unwrap();
            let errors: Vec<_> = validator.iter_errors(&result).map(|e| e.to_string()).collect();
            assert!(errors.is_empty(), "{name}: {errors:?}\n{result}");
            if name == "read_file" { first_page = result; }
        }
        assert_eq!(first_page["complete"], false);
        assert!(first_page["next_start_line"].is_u64());
        let libraries = crate::runtime::agent_program::libraries::catalog(&tools, &json!([]));
        // Other libraries have a read too, such as browser.read.
        let io_read = crate::runtime::agent_program::declarations::selected(&libraries, &json!({"methods":["io.read"]})).unwrap();
        let declarations = crate::runtime::agent_program::declarations::headers(&io_read, &tools);
        let read = declarations.lines().find(|line| line.contains("function read(")).unwrap();
        assert!(read.contains("next_start_line") && !read.ends_with("): unknown;"), "{read}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_chat_keeps_its_own_permission_level_and_older_chats_ask_first() {
        let root = std::env::temp_dir().join(format!("agent-permission-level-{}", uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let conn = open_db(&root).unwrap();
        // Chats saved before the level was stored have no value at all.
        conn.execute_batch("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('older','','m','completed',0,0),('first','','m','draft',0,0),('second','','m','draft',0,0);").unwrap();
        assert_eq!(
            permission_mode_for(&conn, "older").unwrap(),
            PermissionMode::Ask,
            "an older chat asks first, never full access"
        );

        // Choosing a level saves it for that chat alone.
        save_permission_mode(&conn, "first", PermissionMode::FullAccess).unwrap();
        save_permission_mode(&conn, "second", PermissionMode::ReadOnly).unwrap();
        assert_eq!(
            permission_mode_for(&conn, "first").unwrap(),
            PermissionMode::FullAccess
        );
        assert_eq!(
            permission_mode_for(&conn, "second").unwrap(),
            PermissionMode::ReadOnly
        );
        assert_eq!(
            permission_mode_for(&conn, "older").unwrap(),
            PermissionMode::Ask,
            "another chat's choice must not leak into this one"
        );

        // The sidebar, the task and a prompt read the value stored with the chat.
        let task = crate::context::session_browser::decorate(&conn, &root, json!({"task_id":"first"})).unwrap();
        assert_eq!(task["permission_mode"], "full_access");
        let older = crate::context::session_browser::decorate(&conn, &root, json!({"task_id":"older"})).unwrap();
        assert_eq!(older["permission_mode"], "ask");

        // Switching replaces the same chat's choice rather than adding another.
        save_permission_mode(&conn, "first", PermissionMode::AutoReview).unwrap();
        assert_eq!(
            permission_mode_for(&conn, "first").unwrap(),
            PermissionMode::AutoReview
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM agent_tasks", [], |r| r.get::<_, i64>(0)).unwrap(),
            3
        );

        // A value another build wrote can only lower access: it reads as asking first.
        conn.execute("UPDATE agent_tasks SET permission_mode='time_travel' WHERE id IN ('second','older')", []).unwrap();
        assert_eq!(
            permission_mode_for(&conn, "second").unwrap(),
            PermissionMode::Ask
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_task_update_carries_the_permission_level_it_is_given() {
        let request: UpdateTaskRequest =
            serde_json::from_value(json!({"permission_mode":"workspace_write"})).unwrap();
        assert_eq!(request.permission_mode, Some(PermissionMode::WorkspaceWrite));
        // Model-only updates leave the chat's level untouched.
        let model_only: UpdateTaskRequest = serde_json::from_value(json!({"model":"m"})).unwrap();
        assert_eq!(model_only.permission_mode, None);
    }

