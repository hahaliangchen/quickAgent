    use super::*;
    async fn record_return(
        root: &Path,
        context: &crate::media::visual_artifacts::VisualContext,
        name: &str,
        result: &Value,
    ) {
        crate::server::agent_service::open_db(root).unwrap().execute("INSERT OR IGNORE INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES (?1,'reconnect fixture','test','running',0,0)",[context.task_id()]).unwrap();
        let call_id = crate::media::visual_artifacts::new_id();
        crate::server::agent_service::emit(root,context.task_id(),"tool/call",json!({"turn":context.identity["request_id"],"name":name,"callId":call_id,"arguments":"{}"})).await.unwrap();
        crate::server::agent_service::emit(root,context.task_id(),"tool/result",json!({"turn":context.identity["request_id"],"meta":{"result":result},
            "message":{"role":"tool","toolCallId":call_id,"content":[{"type":"text","text":result.to_string()}]}})).await.unwrap();
    }
    async fn forget_runtime(root: &Path, task: &str) {
        let page = sessions().lock().await.remove(&session_key(root, task));
        if let Some(page) = page {
            if let Some(session) = page.lock().await.as_mut() {
                // Model the surviving visible browser after the host loses its handles.
                // Child cannot change kill_on_drop after spawning. Forget only the
                // test's process handle; the reattached CDP connection closes Chrome.
                if let Some(child) = session.child.take() {
                    std::mem::forget(child);
                }
            }
        }
    }

    #[tokio::test]
    async fn partial_connection_keeps_original_mode_and_monotonic_counters() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "browser-metadata-{}",
                crate::media::visual_artifacts::new_id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let context = crate::media::visual_artifacts::VisualContext::mcp(&root);
        let original = json!({"browser_session_id":"original","page_id":"page","websocket_url":"ws://127.0.0.1:1234/devtools/page/page",
            "display_mode":"visible_window","page_epoch":6,"capture_seq":2});
        record_return(
            &root,
            &context,
            "browser_open",
            &json!({"browser_connection":original}),
        )
        .await;
        let mut incomplete = original.clone();
        incomplete["display_mode"] = json!("headless");
        incomplete["page_epoch"] = json!(0);
        incomplete["capture_seq"] = json!(0);
        record_return(
            &root,
            &context,
            "browser_read",
            &json!({"browser_connection":incomplete}),
        )
        .await;
        let partial = json!({"browser_session_id":"original","page_id":"page"});
        for input in [&partial, &incomplete] {
            let restored = restore_connection_record(&root, context.task_id(), input).unwrap();
            for field in ["display_mode", "page_epoch", "capture_seq", "websocket_url"] {
                assert_eq!(restored[field], original[field]);
            }
        }
        assert!(
            restore_connection_record(
                &root,
                context.task_id(),
                &json!({"browser_session_id":"other","page_id":"page"})
            )
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn resumes_original_chrome_from_conversation_without_navigation_or_upload() {
        if browser_path().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "browser-resume-{}",
            crate::media::visual_artifacts::new_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let mut context = crate::media::visual_artifacts::VisualContext::mcp(&root);
        context.identity["request_id"] = json!(1);
        let file = root.join("page.html");
        std::fs::write(&file,r#"<html><body>
            <input id="file" type="file"><span id="file-name"></span><span id="page-indicator" class="page-info">Slide 1 / 2</span>
            <button onclick="window.flips++;document.querySelector('#page-indicator').textContent='Slide 2 / 2'">下一页</button>
            <script>window.flips=0;window.uploads=0;document.querySelector('#file').addEventListener('change',e=>{window.uploads++;document.querySelector('#file-name').textContent=e.target.files[0].name})</script>
            </body></html>"#).unwrap();
        std::fs::write(root.join("保留 文件.pptx"), b"fixture").unwrap();
        let opened = execute_scoped(
            &workspace,
            &context,
            "browser_open",
            &json!({"url":reqwest::Url::from_file_path(&file).unwrap().to_string()}),
        )
        .await
        .unwrap();
        record_return(&root, &context, "browser_open", &opened).await;
        let uploaded = execute_scoped(
            &workspace,
            &context,
            "browser_upload",
            &json!({"path":"保留 文件.pptx","selector":"#file"}),
        )
        .await
        .unwrap();
        record_return(&root, &context, "browser_upload", &uploaded).await;
        let flipped = execute_scoped(
            &workspace,
            &context,
            "browser_click",
            &json!({"text":"下一页"}),
        )
        .await
        .unwrap();
        record_return(&root, &context, "browser_click", &flipped).await;
        let expression = "({flips:window.flips,uploads:window.uploads,file:document.querySelector('#file').files[0].name,origin:performance.timeOrigin,page:document.querySelector('#page-indicator').textContent})";
        let before = inspect_test_page(&root, &context, expression)
            .await
            .unwrap();
        forget_runtime(&root, context.task_id()).await;
        context.identity["request_id"] = json!(2);
        let resumed = execute_scoped(&workspace, &context, "browser_read", &json!({}))
            .await
            .unwrap();
        assert!(resumed.get("error_code").is_none(), "{resumed}");
        assert_eq!(
            resumed["browser_connection"]["websocket_url"],
            opened["browser_connection"]["websocket_url"]
        );
        assert_eq!(resumed["page"]["page_id"], opened["page"]["page_id"]);
        assert_eq!(resumed["page"]["identity"]["request_id"], 2);
        assert_eq!(
            inspect_test_page(&root, &context, expression)
                .await
                .unwrap(),
            before,
            "reconnection must preserve uploaded File, page state and navigation origin"
        );
        // The agent recovers the original connection through its history tool.
        let websocket = opened["browser_connection"]["websocket_url"]
            .as_str()
            .unwrap();
        let history = crate::context::session_history::read(
            &root,
            context.task_id(),
            &json!({"query":websocket,"role":"tool"}),
        )
        .await
        .unwrap();
        assert!(
            history["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["content"].as_str().unwrap().contains(websocket)),
            "{history}"
        );
        // The same returned record also works explicitly without a live cache.
        forget_runtime(&root, context.task_id()).await;
        context.identity["request_id"] = json!(3);
        let partial = json!({"browser_session_id":resumed["browser_connection"]["browser_session_id"],"page_id":resumed["browser_connection"]["page_id"]});
        let explicit = execute_scoped(
            &workspace,
            &context,
            "browser_read",
            &json!({"browser_connection":partial}),
        )
        .await
        .unwrap();
        assert_eq!(explicit["page"]["page_id"], opened["page"]["page_id"]);
        assert_eq!(
            explicit["browser_connection"]["display_mode"],
            opened["browser_connection"]["display_mode"]
        );
        assert_eq!(
            explicit["browser_connection"]["page_epoch"],
            resumed["browser_connection"]["page_epoch"]
        );
        let captured = execute_scoped(
            &workspace,
            &context,
            "browser_screenshot",
            &json!({"browser_connection":partial}),
        )
        .await
        .unwrap();
        record_return(&root, &context, "browser_screenshot", &captured).await;
        let read_again = execute_scoped(
            &workspace,
            &context,
            "browser_read",
            &json!({"browser_connection":partial}),
        )
        .await
        .unwrap();
        assert_eq!(
            read_again["browser_connection"]["capture_seq"],
            captured["browser_connection"]["capture_seq"]
        );
        assert_eq!(
            inspect_test_page(&root, &context, expression)
                .await
                .unwrap(),
            before
        );
        let closed = execute_scoped(&workspace, &context, "browser_close", &json!({}))
            .await
            .unwrap();
        assert_eq!(closed["closed"], true, "{closed}");
        record_return(&root, &context, "browser_close", &closed).await;
        let after = execute_scoped(&workspace, &context, "browser_read", &json!({}))
            .await
            .unwrap();
        assert_eq!(
            after["error_code"], "browser_session_missing",
            "closing must not resurrect an older record"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn legacy_conversation_page_identity_reconnects_its_exact_existing_chrome() {
        if browser_path().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "browser-legacy-resume-{}",
            crate::media::visual_artifacts::new_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let context = crate::media::visual_artifacts::VisualContext::mcp(&root);
        let file = root.join("page.html");
        std::fs::write(&file, "<html><body>legacy page</body></html>").unwrap();
        let opened = execute_scoped(
            &workspace,
            &context,
            "browser_open",
            &json!({"url":reqwest::Url::from_file_path(&file).unwrap().to_string()}),
        )
        .await
        .unwrap();
        let legacy = json!({"page":opened["page"],"display_mode":opened["display_mode"]});
        record_return(&root, &context, "browser_open", &legacy).await;
        forget_runtime(&root, context.task_id()).await;
        let resumed = execute_scoped(&workspace, &context, "browser_read", &json!({}))
            .await
            .unwrap();
        assert_eq!(
            resumed["browser_connection"]["websocket_url"],
            opened["browser_connection"]["websocket_url"]
        );
        assert_eq!(resumed["page"]["page_id"], opened["page"]["page_id"]);
        assert_eq!(
            execute_scoped(&workspace, &context, "browser_close", &json!({}))
                .await
                .unwrap()["closed"],
            true
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn unreachable_recorded_connection_reports_failure_without_replacement() {
        let root = std::env::temp_dir().join(format!(
            "browser-reconnect-failure-{}",
            crate::media::visual_artifacts::new_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let context = crate::media::visual_artifacts::VisualContext::mcp(&root);
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let record = json!({"browser_session_id":"recorded-session","page_id":"recorded-page","websocket_url":format!("ws://127.0.0.1:{port}/devtools/page/recorded-page"),"display_mode":"visible_window"});
        record_return(
            &root,
            &context,
            "browser_read",
            &json!({"browser_connection":record}),
        )
        .await;
        let result = execute_scoped(&workspace, &context, "browser_read", &json!({}))
            .await
            .unwrap();
        assert_eq!(result["error_code"], "browser_reconnect_failed");
        assert_eq!(result["browser_connection"], record);
        assert!(result["reason"].as_str().unwrap().contains("unreachable"));
        assert!(
            sessions()
                .lock()
                .await
                .get(&session_key(&root, context.task_id()))
                .unwrap()
                .lock()
                .await
                .is_none()
        );
        let wrong = json!({"browser_session_id":"recorded-session","page_id":"another-page","websocket_url":record["websocket_url"],"display_mode":"visible_window"});
        let rejected = execute_scoped(
            &workspace,
            &context,
            "browser_read",
            &json!({"browser_connection":wrong}),
        )
        .await
        .unwrap();
        assert_eq!(rejected["error_code"], "browser_reconnect_failed");
        assert!(
            rejected["reason"]
                .as_str()
                .unwrap()
                .contains("recorded page ID")
        );
        let _ = close_session(&root, context.task_id()).await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browser_discovery_honors_explicit_paths_and_skips_non_executables() {
        let root = std::env::temp_dir().join(format!(
            "browser-discovery-{}",
            crate::media::visual_artifacts::new_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let absent = root.join("missing");
        let directory = root.join("directory");
        std::fs::create_dir(&directory).unwrap();
        let browser = root.join("Chrome with spaces");
        std::fs::write(&browser, "fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(find_browser(None, std::slice::from_ref(&browser)).is_none());
            std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let candidates = vec![absent.clone(), directory, browser.clone()];
        assert_eq!(find_browser(None, &candidates), Some(browser.clone()));
        assert_eq!(find_browser(Some(browser.as_os_str()), &[]), Some(browser));
        assert!(find_browser(Some(absent.as_os_str()), &candidates).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn page_change_during_capture_is_recorded() {
        let before = json!({"url":"http://localhost/","dom":"old"});
        let after = json!({"url":"http://localhost/","dom":"new"});
        let observation = capture_observation(&before, &after);
        assert_eq!(observation["before"]["url"], before["url"]);
        assert_eq!(observation["after"]["url"], after["url"]);
        assert_ne!(
            observation["before"]["dom_hash"],
            observation["after"]["dom_hash"]
        );
        let unchanged = capture_observation(&before, &before);
        assert_eq!(unchanged["before"], unchanged["after"]);
    }

    #[test]
    fn recognizes_stale_cdp_dom_node_errors() {
        for message in [
            "Could not find node with given id",
            "No node with given id found",
            "Node with given id is not valid",
        ] {
            assert!(is_stale_node_error(&anyhow::anyhow!(message)), "{message}");
        }
        assert!(!is_stale_node_error(&anyhow::anyhow!(
            "Invalid selector: input[type=file"
        )));
    }

    #[test]
    fn converts_extended_windows_file_paths_to_regular_cdp_paths() {
        if cfg!(windows) {
            assert_eq!(
                cdp_file_path(Path::new(r"\\?\D:\企业 项目\演示.pptx")).unwrap(),
                r"D:\企业 项目\演示.pptx"
            );
            assert_eq!(
                cdp_file_path(Path::new(r"\\?\UNC\server\share\演示.pptx")).unwrap(),
                r"\\server\share\演示.pptx"
            );
        }
    }

    #[tokio::test]
    async fn native_dialog_is_reported_and_only_an_explicit_key_handles_it() {
        if browser_path().is_none() {
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "browser-dialog-{}",
                crate::media::visual_artifacts::new_id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("dialog.html");
        std::fs::write(&file,"<html><body><button onclick=\"setTimeout(()=>alert('Original load failure: font service unavailable'),50)\">Load fixture</button><p>Existing page</p></body></html>").unwrap();
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let opened = execute(
            &workspace,
            "browser_open",
            &json!({"url":reqwest::Url::from_file_path(&file).unwrap().to_string()}),
        )
        .await
        .unwrap();
        execute(&workspace, "browser_click", &json!({"text":"Load fixture"}))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            execute(&workspace, "browser_read", &json!({})),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["error_code"], "browser_dialog_open");
        assert_eq!(
            result["javascript_dialog"]["message"],
            "Original load failure: font service unavailable"
        );
        let untouched = execute(&workspace, "browser_press_key", &json!({"key":"Tab"}))
            .await
            .unwrap();
        assert_eq!(untouched["error_code"], "browser_dialog_open");
        let handled = execute(&workspace, "browser_press_key", &json!({"key":"Enter"}))
            .await
            .unwrap();
        assert_eq!(handled["dialog_handled"], true);
        assert_eq!(handled["javascript_dialog"], result["javascript_dialog"]);
        execute(&workspace, "browser_click", &json!({"text":"Load fixture"}))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let result = execute(&workspace, "browser_read", &json!({}))
            .await
            .unwrap();
        // Restore an old receipt after the original connection has explicitly
        // dismissed it; stale dialog evidence must not count as a new success.
        let dismissed = execute(&workspace, "browser_press_key", &json!({"key":"Escape"}))
            .await
            .unwrap();
        assert_eq!(dismissed["dialog_handled"], true);
        assert_eq!(dismissed["accepted"], false);
        let mut resumed = reconnect(&result["browser_connection"]).await.unwrap();
        assert_eq!(
            resumed.signature["javascript_dialog"]["message"],
            result["javascript_dialog"]["message"]
        );
        let task_id = crate::media::visual_artifacts::VisualContext::mcp(workspace.root())
            .task_id()
            .to_owned();
        let session = sessions()
            .lock()
            .await
            .get(&session_key(workspace.root(), &task_id))
            .cloned()
            .unwrap();
        {
            let mut guard = session.lock().await;
            resumed.child = guard.as_mut().unwrap().child.take();
            *guard = Some(resumed);
        }
        let handled = execute(&workspace, "browser_press_key", &json!({"key":"Escape"}))
            .await
            .unwrap();
        assert_eq!(handled["error_code"], "browser_dialog_unavailable");
        assert_eq!(handled["javascript_dialog"], result["javascript_dialog"]);
        assert!(handled.get("dialog_handled").is_none());
        let page = execute(
            &workspace,
            "browser_read",
            &json!({"expect_text":"Existing page"}),
        )
        .await
        .unwrap();
        assert_eq!(page["matched"], true);
        assert_eq!(
            page["browser_connection"]["page_id"],
            opened["browser_connection"]["page_id"]
        );
        cleanup(workspace.root(), &task_id).await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn reading_or_interacting_without_a_session_never_launches_a_browser() {
        let root = std::env::temp_dir().join(format!(
            "browser-no-implicit-session-{}",
            crate::media::visual_artifacts::new_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let context = crate::media::visual_artifacts::VisualContext::mcp(&root);
        for name in [
            "browser_read",
            "browser_wait",
            "browser_diagnostics",
            "browser_click",
            "browser_press_key",
            "browser_upload",
            "browser_screenshot",
        ] {
            let result = execute_scoped(&workspace, &context, name, &json!({}))
                .await
                .unwrap();
            assert_eq!(
                result["error_code"], "browser_session_missing",
                "{name}: {result}"
            );
            assert_eq!(result["ok"], false);
            let page = sessions()
                .lock()
                .await
                .get(&session_key(&root, context.task_id()))
                .cloned()
                .unwrap();
            assert!(
                page.lock().await.is_none(),
                "{name} must not create a browser process"
            );
        }
        assert!(
            execute_scoped(
                &workspace,
                &context,
                "browser_open",
                &json!({"visible":true})
            )
            .await
            .is_err()
        );
        assert_eq!(
            execute_scoped(&workspace, &context, "browser_close", &json!({}))
                .await
                .unwrap()["closed"],
            false
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn opens_a_local_page_and_matches_visible_text() {
        if browser_path().is_none() {
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "browser-slide-{}",
                crate::media::visual_artifacts::new_id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("codex-browser-slide.html");
        std::fs::write(&file, "<html><body><h1>Slide 1</h1><div id='slide-list'><div class='slide-item'>Slide 1</div></div><div id='page-indicator' class='page-info'>1 / 1</div><div class='loading' hidden>Loading</div></body></html>").unwrap();
        let url = reqwest::Url::from_file_path(&file).unwrap().to_string();
        let workspace = crate::tools::Workspace::new(file.parent().unwrap()).unwrap();
        let opened = execute(&workspace, "browser_open", &json!({"url":url}))
            .await
            .unwrap();
        assert_eq!(opened["title"].as_str().unwrap_or(""), "");
        assert_eq!(opened["display_mode"], "headless");
        assert_eq!(opened["page"]["display_mode"], opened["display_mode"]);
        let page = execute(
            &workspace,
            "browser_read",
            &json!({"expect_text":"Slide 1"}),
        )
        .await
        .unwrap();
        assert_eq!(page["matched"], true, "{page}");
        assert!(page.get("document_loaded").is_none());
        assert!(page.get("slide_count").is_none());
        let waited = execute(
            &workspace,
            "browser_wait",
            &json!({"selector":"#page-indicator","text":"1 / 1","timeout_ms":3000}),
        )
        .await
        .unwrap();
        assert_eq!(waited["status"], "matched", "{waited}");
        let timeout_result = execute(
            &workspace,
            "browser_wait",
            &json!({"text":"never appears","timeout_ms":100}),
        )
        .await
        .unwrap();
        assert_eq!(timeout_result["status"], "timeout");
        assert_eq!(timeout_result["ok"], false);
        assert_eq!(timeout_result["matched"], false);
        let screenshot = execute(&workspace, "browser_screenshot", &json!({}))
            .await
            .unwrap();
        assert_eq!(screenshot["display_mode"], "headless");
        assert_eq!(screenshot["page"]["page_id"], opened["page"]["page_id"]);
        assert_eq!(screenshot["visual_artifact"]["display_mode"], "headless");
        let task_id = crate::media::visual_artifacts::VisualContext::mcp(workspace.root())
            .task_id()
            .to_owned();
        cleanup_after_finish(workspace.root(), &task_id, "completed").await;
        assert!(
            !sessions()
                .lock()
                .await
                .contains_key(&session_key(workspace.root(), &task_id))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn completed_visible_browser_session_is_retained_until_explicit_close() {
        if browser_path().is_none() {
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "browser-lifecycle-{}",
                crate::media::visual_artifacts::new_id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let context = crate::media::visual_artifacts::VisualContext::mcp(workspace.root());
        let html = root.join("page.html");
        std::fs::write(&html, "<html><body>browser lifecycle</body></html>").unwrap();
        let url = reqwest::Url::from_file_path(&html).unwrap().to_string();
        execute_scoped(&workspace, &context, "browser_open", &json!({"url":url}))
            .await
            .unwrap();
        let key = session_key(workspace.root(), context.task_id());
        let page = sessions().lock().await.get(&key).cloned().unwrap();
        {
            let mut guard = page.lock().await;
            guard.as_mut().unwrap().visible = true;
        }
        let before = page.lock().await.as_ref().unwrap().owner.clone();
        cleanup_after_finish(workspace.root(), context.task_id(), "completed").await;
        assert!(sessions().lock().await.contains_key(&key));
        assert_eq!(page.lock().await.as_ref().unwrap().owner, before);
        let closed = execute_scoped(&workspace, &context, "browser_close", &json!({}))
            .await
            .unwrap();
        assert_eq!(closed["closed"], true);
        assert_eq!(closed["status"], "closed");
        assert!(!sessions().lock().await.contains_key(&key));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_keeps_dom_node_on_one_connection_and_checks_change_with_unicode_path() {
        if browser_path().is_none() {
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "browser-upload-{}",
                crate::media::visual_artifacts::new_id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let html = root.join("upload.html");
        let sample = root.join("演示 文件.pptx");
        std::fs::write(&html,"<html><body><input id='pptx' type='file' style='display:none' accept='.pptx'><p id='event'></p><script>document.querySelector('#pptx').addEventListener('change',e=>document.querySelector('#event').textContent='changed:'+e.target.files[0].name)</script></body></html>").unwrap();
        std::fs::write(&sample, b"temporary pptx fixture").unwrap();
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let context = crate::media::visual_artifacts::VisualContext::mcp(workspace.root());
        let url = reqwest::Url::from_file_path(&html).unwrap().to_string();
        execute_scoped(&workspace, &context, "browser_open", &json!({"url":url}))
            .await
            .unwrap();
        let assigned = execute_scoped(
            &workspace,
            &context,
            "browser_upload",
            &json!({"path":"演示 文件.pptx","selector":"#pptx"}),
        )
        .await
        .unwrap();
        assert_eq!(assigned["status"], "file_assigned", "{assigned}");
        assert_eq!(assigned["file"]["name"], "演示 文件.pptx");
        assert_eq!(assigned["file"]["files_length"], 1);
        assert_eq!(
            assigned["input"]["change_event_received"], true,
            "{assigned}"
        );
        assert_eq!(assigned["application_result"], "not_checked");
        let observed = execute_scoped(
            &workspace,
            &context,
            "browser_wait",
            &json!({"text":"changed:演示 文件.pptx","timeout_ms":3000}),
        )
        .await
        .unwrap();
        assert_eq!(observed["status"], "matched", "{observed}");
        let outside = execute_scoped(
            &workspace,
            &context,
            "browser_upload",
            &json!({"path":"../outside.pptx"}),
        )
        .await
        .unwrap();
        assert_eq!(outside["error_code"], "file_unavailable", "{outside}");
        cleanup(workspace.root(), context.task_id()).await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_reports_multiple_inputs_and_diagnostics_capture_console_and_http_errors() {
        if browser_path().is_none() {
            return;
        }
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut request = vec![0u8; 8192];
                    let n = stream.read(&mut request).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&request[..n]);
                    let path = request.split_whitespace().nth(1).unwrap_or("/");
                    let (status, content_type, body) = if path == "/" {
                        (
                            "200 OK",
                            "text/html; charset=utf-8",
                            r#"<html><head><style>@font-face{font-family:missing;src:url('/missing-font.woff2')}body{font-family:missing}</style></head><body><input id='a' type='file'><input id='b' type='file'><p id='event'></p><button aria-label='Next slide' onclick="document.querySelector('#count').textContent=Number(document.querySelector('#count').textContent)+1">&gt;</button><span id='count'>0</span><script>document.querySelectorAll('input[type=file]').forEach(x=>x.addEventListener('change',e=>document.querySelector('#event').textContent='changed:'+e.target.files[0].name));console.error('fixture-console-error');fetch('/api-failure');fetch('/api/fonts?family=demo')</script></body></html>"#,
                        )
                    } else if path == "/api-failure" {
                        ("503 Service Unavailable", "application/json", "{}")
                    } else if path.starts_with("/api/fonts") {
                        ("200 OK", "application/json", r#"{"fonts":["demo"]}"#)
                    } else if path == "/missing-font.woff2" {
                        ("404 Not Found", "text/plain", "missing")
                    } else {
                        ("404 Not Found", "text/plain", "missing")
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "browser-diagnostics-{}",
                crate::media::visual_artifacts::new_id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("sample.pptx"), b"temporary fixture").unwrap();
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let context = crate::media::visual_artifacts::VisualContext::mcp(workspace.root());
        let opened = execute_scoped(
            &workspace,
            &context,
            "browser_open",
            &json!({"url":format!("http://{address}/")}),
        )
        .await
        .unwrap();
        assert_eq!(opened["display_mode"], "headless");
        let probe = inspect_test_page(
            &root,
            &context,
            "({has:!!window.__codexBrowserDiagnostics})",
        )
        .await
        .unwrap();
        assert_eq!(
            probe["has"], true,
            "navigation diagnostics were not installed"
        );
        let ambiguous = execute_scoped(
            &workspace,
            &context,
            "browser_upload",
            &json!({"path":"sample.pptx"}),
        )
        .await
        .unwrap();
        assert_eq!(
            ambiguous["error_code"], "multiple_file_inputs",
            "{ambiguous}"
        );
        assert_eq!(
            ambiguous["details"]["candidates"].as_array().unwrap().len(),
            2
        );
        let assigned = execute_scoped(
            &workspace,
            &context,
            "browser_upload",
            &json!({"path":"sample.pptx","selector":"#a"}),
        )
        .await
        .unwrap();
        assert_eq!(assigned["status"], "file_assigned", "{assigned}");
        let fonts = execute_scoped(
            &workspace,
            &context,
            "browser_wait",
            &json!({"font_status":"loaded","timeout_ms":5000}),
        )
        .await
        .unwrap();
        assert_eq!(fonts["status"], "matched", "{fonts}");
        let diagnostics = execute_scoped(&workspace, &context, "browser_diagnostics", &json!({}))
            .await
            .unwrap();
        assert!(
            diagnostics["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "console_error"),
            "{diagnostics}"
        );
        assert!(
            diagnostics["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "http_error" && item["status"] == 503),
            "{diagnostics}"
        );
        assert!(
            diagnostics["font_and_failed_resources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["url"]
                    .as_str()
                    .unwrap_or("")
                    .contains("missing-font.woff2")),
            "{diagnostics}"
        );
        assert!(
            diagnostics["font_faces"]
                .as_array()
                .unwrap()
                .iter()
                .any(|face| face["family"] == "missing" && face["status"] == "error"),
            "{diagnostics}"
        );
        let fonts_api = execute_scoped(
            &workspace,
            &context,
            "browser_diagnostics",
            &json!({"url_contains":"/api/fonts","category":"fetch"}),
        )
        .await
        .unwrap();
        let request = fonts_api["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["source"] == "fetch")
            .unwrap_or_else(|| panic!("{fonts_api}"));
        assert_eq!(request["status"], 200);
        assert_eq!(request["method"], "GET");
        assert!(request["time"].is_u64());
        assert!(
            !request["url"].as_str().unwrap().contains("family="),
            "query strings stay out of the record"
        );
        assert!(fonts_api["empty_reason"].is_null());
        assert!(fonts_api["request_coverage"]["not_visible"].is_array());
        let none = execute_scoped(
            &workspace,
            &context,
            "browser_diagnostics",
            &json!({"url_contains":"/never-requested"}),
        )
        .await
        .unwrap();
        assert_eq!(none["requests"], json!([]));
        assert!(
            none["empty_reason"]
                .as_str()
                .unwrap()
                .contains("does not prove"),
            "{none}"
        );
        let read = execute_scoped(&workspace, &context, "browser_read", &json!({}))
            .await
            .unwrap();
        let label = read["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|control| control["tag"] == "button")
            .unwrap()["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            label, "Next slide",
            "browser_read lists the accessible label"
        );
        let clicked = execute_scoped(
            &workspace,
            &context,
            "browser_click",
            &json!({"text":"next slide","times":3}),
        )
        .await
        .unwrap();
        assert_eq!(clicked["match"], "exact_label");
        assert_eq!(clicked["times"], 3);
        let count = inspect_test_page(
            &root,
            &context,
            "({n:document.querySelector('#count').textContent})",
        )
        .await
        .unwrap();
        assert_eq!(count["n"], "3");
        let missing = execute_scoped(
            &workspace,
            &context,
            "browser_click",
            &json!({"text":"Export"}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            missing.contains("Next slide"),
            "the failure lists the visible labels: {missing}"
        );
        cleanup(workspace.root(), context.task_id()).await;
        server.abort();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn live_editor_demo_shows_a_slide() {
        let Ok(url) = std::env::var("PPTX_URL") else {
            return;
        };
        if browser_path().is_none() {
            return;
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = crate::tools::Workspace::new(root).unwrap();
        let opened = execute(&workspace, "browser_open", &json!({"url":url}))
            .await
            .unwrap();
        assert!(
            opened["text"].as_str().unwrap_or("").contains("载入示例"),
            "{opened}"
        );
        execute(
            &workspace,
            "browser_click",
            &json!({"selector":"#btn-welcome-demo"}),
        )
        .await
        .unwrap();
        let mut page = json!({});
        for _ in 0..40 {
            page = execute(
                &workspace,
                "browser_read",
                &json!({"expect_text":"Slide 1"}),
            )
            .await
            .unwrap();
            if page["matched"] == true {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let shot = execute(&workspace, "browser_screenshot", &json!({}))
            .await
            .unwrap();
        assert_eq!(page["matched"], true, "page={page} screenshot={shot}");
        cleanup(
            workspace.root(),
            crate::media::visual_artifacts::VisualContext::mcp(root).task_id(),
        )
        .await;
    }

    #[test]
    fn viewport_payload_and_limits() {
        let (metrics, media, result) = viewport_options(&json!({"width":390,"height":844,"color_scheme":"dark"})).unwrap();
        assert_eq!(metrics, json!({"width":390,"height":844,"deviceScaleFactor":1.0,"mobile":false}));
        assert_eq!(media, json!({"features":[{"name":"prefers-color-scheme","value":"dark"}]}));
        assert_eq!(result["width"], 390);
        assert!(viewport_options(&json!({"width":0,"height":844})).is_err());
        assert!(viewport_options(&json!({"width":390,"height":844,"color_scheme":"sepia"})).is_err());
    }

    #[tokio::test]
    async fn responsive_card_capture_and_input() {
        if browser_path().is_none() {
            eprintln!("Skipping browser fixture: no Chrome/Edge executable");
            return;
        }
        let root = std::env::temp_dir().join(format!("browser-card-{}", crate::media::visual_artifacts::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = crate::tools::Workspace::new(&root).unwrap();
        let path = root.join("card.html");
        std::fs::write(&path, r#"<!doctype html><meta charset=utf-8><style>
            body{margin:20px;min-height:2000px;background:#f6f4ee;color:#243233;font:16px system-ui}
            article{box-sizing:border-box;width:300px;height:164px;padding:16px;background:#fff;border:1px solid #c3cbc8}
            @media(prefers-color-scheme:dark){body{background:#111719;color:#e4eeeb}article{background:#253234;border-color:#627b75}}
            </style><article id=card><h2>Responsive card</h2><form><input id=entry><output></output></form></article>
            <script>entry.addEventListener('input',()=>document.querySelector('output').textContent=entry.value);</script>"#).unwrap();
        let context = crate::media::visual_artifacts::VisualContext::mcp(&root);
        execute_scoped(&workspace,&context,"browser_open",&json!({"url":reqwest::Url::from_file_path(path).unwrap().to_string()})).await.unwrap();
        let filled = execute_scoped(&workspace,&context,"browser_fill",&json!({"selector":"#entry","value":"ready"})).await.unwrap();
        assert_eq!(filled["length"],5);
        let scrolled = execute_scoped(&workspace,&context,"browser_scroll",&json!({"y":100})).await.unwrap();
        assert_eq!(scrolled["scroll_y"],100);
        for scheme in ["light","dark"] {
            execute_scoped(&workspace,&context,"browser_set_viewport",&json!({"width":390,"height":844,"color_scheme":scheme})).await.unwrap();
            let info = inspect_test_page(&root,&context,"({width:innerWidth,dark:matchMedia('(prefers-color-scheme: dark)').matches,text:document.querySelector('output').textContent})").await.unwrap();
            assert_eq!(info,json!({"width":390,"dark":scheme == "dark","text":"ready"}));
            let shot = execute_scoped(&workspace,&context,"browser_screenshot",&json!({"selector":"#card","padding":4})).await.unwrap();
            assert_eq!(shot["clip"],json!({"x":16,"y":16,"width":308,"height":172,"scale":1}));
            let id = shot["artifact_id"].as_str().unwrap();
            let (metadata, bytes) = crate::media::visual_artifacts::read(&root,context.task_id(),id).unwrap();
            assert_eq!(metadata["artifact_id"], id);
            if std::env::var_os("CODEX_BROWSER_CARD_ARTIFACTS").is_some() {
                let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("phase1a-card-{scheme}.png"));
                std::fs::write(path, bytes).unwrap();
            }
        }
        cleanup(&root,context.task_id()).await;
        std::fs::remove_dir_all(root).unwrap();
    }
