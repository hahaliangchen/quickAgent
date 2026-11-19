//! Step 4 unit tests:
//! 1. Large result (> 24KB) durable `asset_id` archival + cross-turn `session.read_asset(...)`
//! 2. Cross-session process code reuse via `require("batch-files")` & project skill modules
//! 3. `host.ask_user({ question })` graceful turn handoff (`NEEDS_USER`)
//! 4. `host_folder` turn `.js` archival + `crate::rewind::rewind` round-trip verification

use super::{TestWorkspace, assistant_messages, completion, extract_asset_id, init_task_row};
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

    // Create a > 28KB file so `io.read` automatically archives it to durable `asset_id`!
    let mut huge = String::from("//! HUGE_ARCHIVE_HEADER_TOKEN_98765\n");
    for i in 2..=380 {
        huge.push_str(&format!(
            "// huge archival padding line {i:04}: testing durable cross-turn context_assets persistence\n"
        ));
    }
    huge.push_str("pub fn buried_symbol() -> u32 { 42 }\n");
    assert!(huge.chars().count() > 28_000);
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
                    0 => completion("const big = io.read({ path: \"src/huge.rs\" });"),
                    // Turn 1, step 2: verify `asset: asset-...` is recorded in permanent `// =>`, then finish Turn 1 (destroys VM!)
                    1 => {
                        let user1 = body["messages"][1]["content"].as_str().unwrap();
                        let aid = extract_asset_id(user1).unwrap_or_else(|| {
                            panic!("expected `asset: asset-...` in Turn 1 permanent trace: {user1}")
                        });
                        assert!(aid.starts_with("asset-"), "unexpected asset id: {aid}");
                        *asset_ref.lock().unwrap() = Some(aid);
                        completion("return <<<\nTurn 1 已归档大文件。")
                    }
                    // Turn 2, step 1 (brand new VM!): read back from durable asset store via `session.read_asset`!
                    2 => {
                        let msgs = body["messages"].as_array().unwrap();
                        assert_eq!(msgs.len(), 4, "[system, user#1, assistant#1, user#2]");
                        let frozen_user1 = msgs[1]["content"].as_str().unwrap();
                        let aid = asset_ref.lock().unwrap().clone().unwrap();
                        assert!(
                            frozen_user1.contains(&format!("asset: {aid}")),
                            "frozen Turn 1 trace must preserve asset_id for cross-turn recall: {frozen_user1}"
                        );
                        completion(&format!(
                            "const page = session.read_asset({{ asset_id: {aid:?}, char_offset: 0, max_chars: 600 }});\n\
                             const recovered = page.content.includes(\"HUGE_ARCHIVE_HEADER_TOKEN_98765\");"
                        ))
                    }
                    // Turn 2, step 2: verify recovered flag is true and finish!
                    3 => {
                        let user2 = body["messages"][3]["content"].as_str().unwrap();
                        assert!(
                            user2.contains("recovered = true"),
                            "session.read_asset must recover archived Turn 1 payload across turns: {user2}"
                        );
                        completion("return <<<\n跨轮通过 session.read_asset 成功取回归档内容。")
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
                let user = body["messages"][1]["content"].as_str().unwrap();
                Json(match idx {
                    0 => completion(
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
                        completion("return <<<\n跨会话复用库 `batch-files` 与项目技能模块 `audit-kit` 并发调用成功。")
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
                    0 => completion(
                        "const cfg = JSON.parse(io.read({ path: \"deploy.json\" }).content);\n\
                         if (cfg.targets.length > 1) {\n\
                           host.ask_user({ question: \"请确认要部署到 staging 还是 prod？\" });\n\
                         }",
                    ),
                    // Turn 2: user replied "staging", verify Turn 1 trace paused cleanly, finish Turn 2!
                    1 => {
                        let msgs = body["messages"].as_array().unwrap();
                        assert_eq!(msgs.len(), 4);
                        assert!(
                            msgs[1]["content"]
                                .as_str()
                                .unwrap()
                                .contains("[paused for user input via host.ask_user]"),
                            "Turn 1 trace must record clean ask_user pause: {}",
                            msgs[1]["content"]
                        );
                        assert_eq!(
                            msgs[2]["content"].as_str().unwrap(),
                            "请确认要部署到 staging 还是 prod？"
                        );
                        completion("return <<<\n收到，已按你的确认选择 `staging` 目标。")
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
                    0 => completion("io.write({ path: \"state.txt\", content: \"v1\" });"),
                    1 => completion("return <<<\nTurn 1 wrote v1."),
                    // Turn 2 (first attempt): write v2
                    2 => completion("io.write({ path: \"state.txt\", content: \"v2\" });"),
                    3 => completion("return <<<\nTurn 2 wrote v2."),
                    // Turn 2 (after rewind to before turn 2): verify Turn 2 was dropped and file is back to v1!
                    4 => {
                        let msgs = body["messages"].as_array().unwrap();
                        assert_eq!(msgs.len(), 4, "rewound state must have only Turn 1 + new Turn 2");
                        assert_eq!(msgs[2]["content"].as_str().unwrap(), "Turn 1 wrote v1.");
                        completion("const cur = io.read({ path: \"state.txt\" }).content;")
                    }
                    5 => {
                        let user2 = body["messages"][3]["content"].as_str().unwrap();
                        assert!(
                            user2.contains("cur = \"v1\""),
                            "rewound disk file must be back at v1: {user2}"
                        );
                        completion("return <<<\n回退后重新执行 Turn 2 成功，确认文件已恢复为 v1。")
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
