//! Step 4 unit tests:
//! 1. Large result (> 24KB) durable `asset_id` archival + cross-turn `session.read_asset(...)`
//! 2. Cross-session process code reuse via `require("batch-files")` & project skill modules
//! 3. `host.ask_user({ question })` graceful turn handoff (`NEEDS_USER`)
//! 4. `host_folder` turn `.js` archival + `crate::rewind::rewind` round-trip verification

use super::{
    TestWorkspace, all_messages_text, assert_valid_tool_pairing, assistant_messages, completion,
    extract_asset_id, init_task_row, last_tool_output, run_js, spawn_mock_provider,
};
use crate::agent_service::{self, AgentServiceState};
use crate::single_agent::{host_folder, run_task};
use axum::{Json, Router, routing::post};
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn step4_large_result_asset_archival_and_cross_turn_read_asset() {
    let ws = TestWorkspace::new("cac-v2-step4");
    std::fs::create_dir_all(ws.path().join("src")).unwrap();

    // Create a > 70KB file so `io.read` automatically archives it to durable `asset_id`!
    let mut huge = String::from("//! HUGE_ARCHIVE_HEADER_TOKEN_98765\n");
    for i in 2..=820 {
        huge.push_str(&format!(
            "// huge archival padding line {i:04}: testing durable cross-turn context_assets persistence beyond 64kb\n"
        ));
    }
    huge.push_str("pub fn buried_symbol() -> u32 { 42 }\n");
    assert!(huge.chars().count() > 70_000);
    std::fs::write(ws.path().join("src/huge.rs"), &huge).unwrap();
    init_task_row(ws.path(), "step4-asset");

    let captured_asset = Arc::new(Mutex::new(None::<String>));
    let asset_ref = captured_asset.clone();
    let seq = Arc::new(AtomicUsize::new(0));

    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let seq = seq.clone();
            let asset_ref = asset_ref.clone();
            async move {
                let idx = seq.fetch_add(1, Ordering::SeqCst);
                Json(match idx {
                    // Turn 1, step 1: read > 28KB file
                    0 => run_js("const big = io.read({ path: \"src/huge.rs\" });"),
                    // Turn 1, step 2: verify `asset: asset-...` is recorded in tool output, then finish Turn 1 (destroys VM!)
                    1 => {
                        let tool1 = last_tool_output(&body);
                        let aid = extract_asset_id(tool1).unwrap_or_else(|| {
                            panic!("expected `asset: asset-...` in Turn 1 tool output: {tool1}")
                        });
                        assert!(aid.starts_with("asset-"), "unexpected asset id: {aid}");
                        *asset_ref.lock().unwrap() = Some(aid);
                        completion("Turn 1 已归档大文件。")
                    }
                    // Turn 2, step 1 (brand new VM!): read back from durable asset store via `session.read_asset`!
                    2 => {
                        assert_valid_tool_pairing(&body);
                        let msgs = body["messages"].as_array().unwrap();
                        // [system, user#1, asst#1, tool#1, final_asst#1, user#2]
                        assert_eq!(msgs.len(), 6, "expected full multi-turn Turn 1 + Turn 2 user: {msgs:?}");
                        let all_text = all_messages_text(&body);
                        let aid = asset_ref.lock().unwrap().clone().unwrap();
                        assert!(
                            all_text.contains(&format!("asset: {aid}")),
                            "Turn 1 history must preserve asset_id for cross-turn recall: {all_text}"
                        );
                        run_js(&format!(
                            "const page = session.read_asset({{ asset_id: {aid:?}, char_offset: 0, max_chars: 600 }});\n\
                             const recovered = page.content.includes(\"HUGE_ARCHIVE_HEADER_TOKEN_98765\");"
                        ))
                    }
                    // Turn 2, step 2: verify recovered flag is true and finish!
                    3 => {
                        let tool2 = last_tool_output(&body);
                        assert!(
                            tool2.contains("recovered = true"),
                            "session.read_asset must recover archived Turn 1 payload across turns: {tool2}"
                        );
                        completion("跨轮通过 session.read_asset 成功取回归档内容。")
                    }
                    other => panic!("unexpected round {other}"),
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state: AgentServiceState =
        agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    run_task(
        state.clone(),
        "step4-asset".into(),
        "fake".into(),
        "Read src/huge.rs".into(),
        5,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();

    run_task(
        state,
        "step4-asset".into(),
        "fake".into(),
        "Recall the archived Turn 1 header via session.read_asset".into(),
        5,
        CancellationToken::new(),
        2,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        assistant_messages(ws.path(), "step4-asset"),
        vec![
            "Turn 1 已归档大文件。",
            "跨轮通过 session.read_asset 成功取回归档内容。",
        ]
    );
}

#[tokio::test]
async fn step4_cross_session_code_reuse_require_preset_and_project_skill_modules() {
    let ws = TestWorkspace::new("cac-v2-step4");
    std::fs::create_dir_all(ws.path().join("src")).unwrap();
    std::fs::write(ws.path().join("src/one.rs"), "pub fn one() { /* TODO_MARKER_1 */ }\n").unwrap();
    std::fs::write(ws.path().join("src/two.rs"), "pub fn two() { /* TODO_MARKER_2 */ }\n").unwrap();

    // Install a reusable project skill JS module in `.agents/skills/audit-kit/modules/scanMarkers.js`!
    let skill_dir = ws.path().join(".agents/skills/audit-kit");
    let modules_dir = skill_dir.join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: audit-kit\ndescription: Reusable project marker scanner\n---\nUse require(\"audit-kit\").\n",
    )
    .unwrap();
    std::fs::write(
        modules_dir.join("scanMarkers.js"),
        "/** Count occurrences of a marker substring across multiple files. */\n\
         async function scanMarkers(paths, marker) {\n\
           const io = require(\"io\");\n\
           let hits = [];\n\
           for (const p of paths) {\n\
             const f = io.read({ path: p });\n\
             if (f.content.includes(marker)) hits.push(p);\n\
           }\n\
           return { marker, count: hits.length, files: hits };\n\
         }\n",
    )
    .unwrap();
    init_task_row(ws.path(), "step4-reuse");

    let seq = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let seq = seq.clone();
            async move {
                let idx = seq.fetch_add(1, Ordering::SeqCst);
                let sys = body["messages"][0]["content"].as_str().unwrap();
                assert!(
                    sys.contains("require(\"batch-files\")")
                        && sys.contains("require(\"audit-kit\")"),
                    "system prompt must advertise built-in preset & project skill libraries: {sys}"
                );
                let user = last_tool_output(&body);
                Json(match idx {
                    0 => run_js(
                        "const bf = require(\"batch-files\");\n\
                         const ak = require(\"audit-kit/promises\");\n\
                         const many = bf.readMany([\"src/one.rs\", \"src/two.rs\"]);\n\
                         const [scan] = await Promise.all([\n\
                           ak.scanMarkers([\"src/one.rs\", \"src/two.rs\"], \"TODO_MARKER_\"),\n\
                         ]);\n\
                         const toolCatalog = host.tools();\n\
                         const summary = { readCount: many.length, scanCount: scan.count, hasAuditKit: toolCatalog.orchestration_libraries.some(l => l.name === \"audit-kit\") };",
                    ),
                    1 => {
                        assert!(
                            user.contains("[library batch-files.readMany")
                                && user.contains("[library audit-kit.scanMarkers"),
                            "parent trace must include factual [library ...] receipts: {user}"
                        );
                        assert!(
                            user.contains("\"readCount\":2")
                                && user.contains("\"scanCount\":2")
                                && user.contains("\"hasAuditKit\":true"),
                            "both preset and project skill modules must execute cleanly: {user}"
                        );
                        completion("跨会话复用库 `batch-files` 与项目技能模块 `audit-kit` 并发调用成功。")
                    }
                    other => panic!("unexpected round {other}"),
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state: AgentServiceState =
        agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        run_task(
            state,
            "step4-reuse".into(),
            "fake".into(),
            "Reuse batch-files and audit-kit libraries".into(),
            5,
            CancellationToken::new(),
            1,
            false,
        ),
    )
    .await
    .expect("step4-reuse timed out")
    .unwrap();
    server.abort();

    assert_eq!(
        assistant_messages(ws.path(), "step4-reuse"),
        vec!["跨会话复用库 `batch-files` 与项目技能模块 `audit-kit` 并发调用成功。"]
    );
}

#[tokio::test]
async fn step4_host_ask_user_pauses_turn_gracefully_and_resumes_on_next_turn() {
    let ws = TestWorkspace::new("cac-v2-step4");
    std::fs::write(ws.path().join("deploy.json"), "{\"targets\":[\"staging\",\"prod\"]}\n").unwrap();
    init_task_row(ws.path(), "step4-ask");

    let seq = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let seq = seq.clone();
            async move {
                let idx = seq.fetch_add(1, Ordering::SeqCst);
                Json(match idx {
                    // Turn 1: read deploy.json and ask user which target to deploy
                    0 => run_js(
                        "const cfg = JSON.parse(io.read({ path: \"deploy.json\" }).content);\n\
                         if (cfg.targets.length > 1) {\n\
                           host.ask_user({ question: \"请确认要部署到 staging 还是 prod？\" });\n\
                         }",
                    ),
                    // Turn 2: user replied "staging", verify Turn 1 trace paused cleanly, finish Turn 2!
                    1 => {
                        assert_valid_tool_pairing(&body);
                        let msgs = body["messages"].as_array().unwrap();
                        // [system, user#1, asst#1, tool#1(paused), final_asst#1(question), user#2]
                        assert_eq!(msgs.len(), 6, "expected Turn 1 pause + Turn 2 user: {msgs:?}");
                        assert!(
                            msgs[3]["content"]
                                .as_str()
                                .unwrap()
                                .contains("[paused for user input via host.ask_user]"),
                            "Turn 1 tool response must record clean ask_user pause: {}",
                            msgs[3]["content"]
                        );
                        assert_eq!(
                            msgs[4]["content"].as_str().unwrap(),
                            "请确认要部署到 staging 还是 prod？"
                        );
                        completion("收到，已按你的确认选择 `staging` 目标。")
                    }
                    other => panic!("unexpected round {other}"),
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state: AgentServiceState =
        agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    run_task(
        state.clone(),
        "step4-ask".into(),
        "fake".into(),
        "帮我执行部署".into(),
        5,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();

    // Verify durable `agent/needs_user` event was recorded!
    let conn = agent_service::open_db(ws.path()).unwrap();
    let needs_user_count: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_task_events WHERE task_id='step4-ask' AND kind='agent/needs_user'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(needs_user_count, 1);

    run_task(
        state,
        "step4-ask".into(),
        "fake".into(),
        "部署到 staging".into(),
        5,
        CancellationToken::new(),
        2,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        assistant_messages(ws.path(), "step4-ask"),
        vec![
            "请确认要部署到 staging 还是 prod？",
            "收到，已按你的确认选择 `staging` 目标。",
        ]
    );
}

#[tokio::test]
async fn step4_host_folder_turn_js_archive_and_rewind_roundtrip() {
    let ws = TestWorkspace::new("cac-v2-step4");
    std::fs::write(ws.path().join("state.txt"), "v0").unwrap();
    init_task_row(ws.path(), "step4-rewind");

    let seq = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let seq = seq.clone();
            async move {
                let idx = seq.fetch_add(1, Ordering::SeqCst);
                Json(match idx {
                    // Turn 1: write v1
                    0 => run_js("io.write({ path: \"state.txt\", content: \"v1\" });"),
                    1 => completion("Turn 1 wrote v1."),
                    // Turn 2 (first attempt): write v2
                    2 => run_js("io.write({ path: \"state.txt\", content: \"v2\" });"),
                    3 => completion("Turn 2 wrote v2."),
                    // Turn 2 (after rewind to before turn 2): verify Turn 2 was dropped and file is back to v1!
                    4 => {
                        assert_valid_tool_pairing(&body);
                        let msgs = body["messages"].as_array().unwrap();
                        // [system, user#1, asst#1, tool#1, final_asst#1, user#2]
                        assert_eq!(msgs.len(), 6, "rewound state must have only Turn 1 + new Turn 2: {msgs:?}");
                        assert_eq!(msgs[4]["content"].as_str().unwrap(), "Turn 1 wrote v1.");
                        run_js("const cur = io.read({ path: \"state.txt\" }).content;")
                    }
                    5 => {
                        let tool2 = last_tool_output(&body);
                        assert!(
                            tool2.contains("cur = \"v1\""),
                            "rewound disk file must be back at v1: {tool2}"
                        );
                        completion("回退后重新执行 Turn 2 成功，确认文件已恢复为 v1。")
                    }
                    other => panic!("unexpected round {other}"),
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state: AgentServiceState =
        agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    run_task(
        state.clone(),
        "step4-rewind".into(),
        "fake".into(),
        "Update to v1".into(),
        5,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(ws.path().join("state.txt")).unwrap(), "v1");

    run_task(
        state.clone(),
        "step4-rewind".into(),
        "fake".into(),
        "Update to v2".into(),
        5,
        CancellationToken::new(),
        2,
        false,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(ws.path().join("state.txt")).unwrap(), "v2");

    // Verify clean `turn-01.js` and `turn-02.js` archives exist in `host_folder`!
    let chat_dir = host_folder::chat_dir(ws.path(), "step4-rewind").unwrap();
    assert!(
        chat_dir.join("turn-01.js").is_file() && chat_dir.join("turn-02.js").is_file(),
        "expected turn-01.js and turn-02.js archived in {chat_dir:?}"
    );
    let t1_js = std::fs::read_to_string(chat_dir.join("turn-01.js")).unwrap();
    assert!(t1_js.contains("io.write") && t1_js.contains("state.txt"));

    // Verify `run_task` automatically transitioned `agent_tasks.status` to `completed`
    // and emitted `turn/start` + `turn/end` events for the frontend!
    let conn = agent_service::open_db(ws.path()).unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM agent_tasks WHERE id='step4-rewind'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "completed");
    let turn_ends: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_task_events WHERE task_id='step4-rewind' AND kind='turn/end'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(turn_ends, 2);
    drop(conn);

    let rewound = crate::rewind::rewind(ws.path(), "step4-rewind", 2).unwrap();
    assert_eq!(rewound["prompt"].as_str().unwrap(), "Update to v2");
    assert_eq!(
        std::fs::read_to_string(ws.path().join("state.txt")).unwrap(),
        "v1",
        "rewind to before turn 2 must restore state.txt to v1!"
    );

    // Re-run Turn 2 cleanly on top of restored Turn 1 Window!
    run_task(
        state,
        "step4-rewind".into(),
        "fake".into(),
        "Verify rewound state".into(),
        5,
        CancellationToken::new(),
        2,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        assistant_messages(ws.path(), "step4-rewind"),
        vec![
            "Turn 1 wrote v1.",
            "回退后重新执行 Turn 2 成功，确认文件已恢复为 v1。",
        ]
    );
}

#[tokio::test]
async fn step4_multiturn_folding_snapshot_handle_refresh_baselines_and_level2_truncation() {
    let ws = TestWorkspace::new("cac-v2-step4-fold");
    std::fs::write(
        ws.path().join("target.rs"),
        "fn v1_alpha() {}\nfn v1_beta() {}\nfn v1_gamma() {}\n",
    )
    .unwrap();
    init_task_row(ws.path(), "step4-fold");

    let seq = Arc::new(AtomicUsize::new(0));
    let pinned_system = Arc::new(std::sync::Mutex::new(String::new()));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let seq = seq.clone();
            let pinned_system = pinned_system.clone();
            async move {
                let idx = seq.fetch_add(1, Ordering::SeqCst);
                let sys = body["messages"][0]["content"].as_str().unwrap().to_owned();
                {
                    let mut guard = pinned_system.lock().unwrap();
                    if guard.is_empty() {
                        *guard = sys.clone();
                    } else {
                        assert_eq!(*guard, sys, "pinned_system_prompt must remain byte-identical across turns");
                    }
                }
                Json(match idx {
                    // Turn 1: read target.rs (v1)
                    0 => run_js("const f1 = io.read({ path: \"target.rs\" }); f1.slice(1, 2);"),
                    1 => completion("Turn 1 read v1."),
                    // Turn 2: hard_limit triggers Level-1 fold of Turn 1!
                    2 => {
                        assert_valid_tool_pairing(&body);
                        let msgs = body["messages"].as_array().unwrap();
                        // [system, user#1, folded_asst#1, folded_tool#1, final_asst#1, user#2]
                        assert_eq!(msgs.len(), 6, "Turn 1 should be folded into 1 native pair: {msgs:?}");
                        assert!(!msgs[1]["content"].as_str().unwrap().contains("io.read"), "user#1 must not contain folded code");
                        assert_eq!(msgs[2]["role"], "assistant");
                        assert!(msgs[2].get("_reasoning_items").is_none());
                        assert!(msgs[2].get("reasoning_content").is_none());
                        let folded_code = msgs[2]["tool_calls"][0]["function"]["arguments"].as_str().unwrap();
                        assert!(folded_code.contains("io.read") && folded_code.contains("Turn 1 folded"));
                        assert_eq!(msgs[3]["role"], "tool");
                        let manifest: Value = serde_json::from_str(msgs[3]["content"].as_str().unwrap()).unwrap();
                        assert!(manifest["binding_assets"].as_array().is_some_and(|assets| assets.len() <= 16));
                        assert!(manifest.get("live_vm_handles").is_none());
                        let file_asset = manifest["binding_assets"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|e| e["name"] == "f1" && e["kind"] == "file")
                            .expect("f1 file binding must have its own snapshot asset_id");
                        let aid = file_asset["asset_id"].as_str().unwrap();
                        assert!(
                            msgs[5]["content"].as_str().unwrap().contains("Files changed between turns: target.rs"),
                            "Turn 2 user message must announce between-turn external edit: {}",
                            msgs[5]["content"]
                        );
                        run_js(&format!(
                            "const oldSnap = session.snapshot({{ asset_id: {aid:?} }});\n\
                             const oldSlice = oldSnap.slice(1, 2);\n\
                             const fresh = io.read({{ path: \"target.rs\" }});\n\
                             const newSlice = fresh.slice(1, 2);\n\
                             io.edit({{ path: \"target.rs\", edits: [{{ old_text: \"fn v2_beta() {{}}\", new_text: \"fn v3_beta() {{}}\" }}] }});\n\
                             ({{ oldSlice, newSlice }});"
                        ))
                    }
                    3 => {
                        let tool2 = last_tool_output(&body);
                        assert!(
                            tool2.contains("v1_alpha") && tool2.contains("v1_beta"),
                            "session.snapshot().slice() must return frozen Turn 1 bytes after disk edit: {tool2}"
                        );
                        assert!(
                            tool2.contains("v2_alpha") && tool2.contains("v2_beta"),
                            "io.read().slice() must return updated disk bytes: {tool2}"
                        );
                        completion("Turn 2 verified frozen snapshot + edited v3.")
                    }
                    // Turn 3: hard_limit triggers Level-2 aggressive truncation (drops Turn 1, keeps Turn 2 + Turn 3!)
                    4 => {
                        assert_valid_tool_pairing(&body);
                        let msgs = body["messages"].as_array().unwrap();
                        let all_text = all_messages_text(&body);
                        assert!(
                            !all_text.contains("Inspect v1 target"),
                            "Level-2 truncation must drop Turn 1 from active hot context: {msgs:?}"
                        );
                        assert!(
                            all_text.contains("Compare snapshot and edit") && all_text.contains("Turn 3 check"),
                            "Level-2 truncation must retain latest completed Turn 2 + active Turn 3: {msgs:?}"
                        );
                        completion("Turn 3 confirmed Level-2 truncation.")
                    }
                    other => panic!("unexpected round {other}"),
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut state: AgentServiceState =
        agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    state.provider_name = "test".into();
    state.provider_routes.insert(
        "test".into(),
        agent_service::AgentProviderRoute {
            prefix_system_messages: false,
            responses: false,
            messages: false,
            messages_capabilities: Default::default(),
            url: state.provider_url.clone(),
            api_key: String::new(),
            models: [("fake".into(), "fake".into())].into_iter().collect(),
            max_output_tokens: Default::default(),
            context_windows: [("fake".into(), 200_000)].into_iter().collect(),
            long_context_thresholds: [("fake".into(), 100)].into_iter().collect(),
            model_costs: Default::default(),
        },
    );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    run_task(
        state.clone(),
        "step4-fold".into(),
        "fake".into(),
        "Inspect v1 target".into(),
        5,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();

    // User externally modifies target.rs between Turn 1 and Turn 2!
    std::fs::write(
        ws.path().join("target.rs"),
        "fn v2_alpha() {}\nfn v2_beta() {}\nfn v2_gamma() {}\n",
    )
    .unwrap();

    run_task(
        state.clone(),
        "step4-fold".into(),
        "fake".into(),
        "Compare snapshot and edit".into(),
        5,
        CancellationToken::new(),
        2,
        false,
    )
    .await
    .unwrap();
    assert!(
        std::fs::read_to_string(ws.path().join("target.rs"))
            .unwrap()
            .contains("fn v3_beta() {}"),
        "Turn 2 io.edit after external edit must succeed via refresh_baselines"
    );

    run_task(
        state,
        "step4-fold".into(),
        "fake".into(),
        "Turn 3 check".into(),
        5,
        CancellationToken::new(),
        3,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        assistant_messages(ws.path(), "step4-fold"),
        vec![
            "Turn 1 read v1.",
            "Turn 2 verified frozen snapshot + edited v3.",
            "Turn 3 confirmed Level-2 truncation.",
        ]
    );
}

#[tokio::test]
async fn step4_idle_soft_fold_respects_hot_window_and_preserves_history() {
    let ws = TestWorkspace::new("cac-v2-idle-fold");
    init_task_row(ws.path(), "step4-idle-fold");
    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        0 => run_js("const old = 1;"),
        1 => completion("Turn 1 complete."),
        2 => {
            let all_text = all_messages_text(&body);
            assert!(
                all_text.contains("const old = 1;"),
                "hot-cache Turn 2 must keep the unfolded prefix"
            );
            assert!(
                !all_text.contains("Turn 1 folded"),
                "hot-cache Turn 2 must not soft-fold"
            );
            run_js("const hot = 2;")
        }
        3 => completion("Turn 2 complete."),
        4 => {
            assert_valid_tool_pairing(&body);
            let all_text = all_messages_text(&body);
            assert!(
                all_text.contains("Turn 1 complete."),
                "folding must preserve the completed assistant reply"
            );
            let manifest = last_tool_output(&body);
            assert!(
                manifest.contains("frozen_turn_snapshots") && manifest.contains("session.snapshot"),
                "folding must preserve durable snapshot recall metadata: {manifest}"
            );
            completion("Turn 3 observed idle soft-fold.")
        }
        other => panic!("unexpected mock request {other}"),
    })
    .await;

    run_task(
        state.clone(),
        "step4-idle-fold".into(),
        "fake".into(),
        "Turn 1".into(),
        5,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    run_task(
        state.clone(),
        "step4-idle-fold".into(),
        "fake".into(),
        "Turn 2".into(),
        5,
        CancellationToken::new(),
        2,
        false,
    )
    .await
    .unwrap();

    let conn = agent_service::open_db(ws.path()).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT state FROM agent_runtime_windows WHERE task_id=?1 AND scope='main'",
            ["step4-idle-fold"],
            |row| row.get(0),
        )
        .unwrap();
    let mut saved: Value = serde_json::from_str(&raw).unwrap();
    assert!(saved["last_model_request_at_ms"].as_u64().is_some());
    saved["last_model_request_at_ms"] = 0.into();
    conn.execute(
        "UPDATE agent_runtime_windows SET state=?1 WHERE task_id=?2 AND scope='main'",
        rusqlite::params![serde_json::to_string(&saved).unwrap(), "step4-idle-fold"],
    )
    .unwrap();
    drop(conn);

    run_task(
        state,
        "step4-idle-fold".into(),
        "fake".into(),
        "Turn 3".into(),
        5,
        CancellationToken::new(),
        3,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        assistant_messages(ws.path(), "step4-idle-fold"),
        vec![
            "Turn 1 complete.",
            "Turn 2 complete.",
            "Turn 3 observed idle soft-fold.",
        ]
    );
    assert_eq!(captured.lock().unwrap().len(), 5);
}
