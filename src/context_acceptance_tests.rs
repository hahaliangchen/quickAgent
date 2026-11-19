//! End-to-end Acceptance Matrix tests for Context-as-Code + Dynamic Call Stack + Per-Turn Fresh VM.
//!
//! Every test here drives the real `single_agent::run_task` loop against an Axum
//! HTTP model endpoint, real workspace files, and SQLite `agent_task_events`.

use crate::{
    agent_service::{self, AgentServiceState},
    model_stream,
    single_agent::run_task,
};
use axum::{Json, Router, routing::post};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

struct TestWorkspace(PathBuf);

impl TestWorkspace {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cac-e2e-{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn chat_completion(content: &str, finish_reason: &str) -> Value {
    json!({
        "choices": [{
            "finish_reason": finish_reason,
            "message": {
                "role": "assistant",
                "content": content,
            }
        }]
    })
}

fn init_task_row(root: &Path, task_id: &str) {
    let conn = agent_service::open_db(root).unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES (?1,'','fake','running',0,0)",
        params![task_id],
    )
    .unwrap();
}

async fn spawn_mock_provider(
    root: &Path,
    handler: impl Fn(usize, Value) -> Value + Send + Sync + 'static,
) -> (AgentServiceState, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_ref = captured.clone();
    let seq = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(handler);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let idx = seq.fetch_add(1, Ordering::SeqCst);
            captured_ref.lock().unwrap().push(body.clone());
            let resp = handler(idx, body);
            async move { Json(resp) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = agent_service::tests::test_state(root, listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (state, captured, server)
}

fn tool_call_count(root: &Path, task_id: &str, tool_name: &str) -> i64 {
    let conn = agent_service::open_db(root).unwrap();
    conn.query_row(
        "SELECT count(*) FROM agent_task_events WHERE task_id=?1 AND kind='tool/call' AND json_extract(data,'$.name')=?2",
        params![task_id, tool_name],
        |r| r.get(0),
    )
    .unwrap()
}

fn total_tool_calls(root: &Path, task_id: &str) -> i64 {
    let conn = agent_service::open_db(root).unwrap();
    conn.query_row(
        "SELECT count(*) FROM agent_task_events WHERE task_id=?1 AND kind='tool/call'",
        params![task_id],
        |r| r.get(0),
    )
    .unwrap()
}

fn surfaced_assistant_messages(root: &Path, task_id: &str) -> Vec<String> {
    let conn = agent_service::open_db(root).unwrap();
    conn.prepare(
        "SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE task_id=?1 AND kind='assistant/message' ORDER BY seq",
    )
    .unwrap()
    .query_map(params![task_id], |r| r.get::<_, String>(0))
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn stable_user_prefix_from_wire(user_content: &str) -> &str {
    user_content
        .split("\n\n// === Active Execution Stack")
        .next()
        .unwrap_or(user_content)
}

#[tokio::test]
async fn acceptance_01_live_binding_reused_across_segments_without_reread() {
    let ws = TestWorkspace::new();
    std::fs::write(ws.path().join("notes.txt"), "alpha\nbeta\ngamma").unwrap();
    init_task_row(ws.path(), "t1");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        0 => chat_completion(
            "<run>\nconst evidence = io.read({ path: \"notes.txt\" });\n</run>",
            "stop",
        ),
        1 => {
            let text = body["messages"].to_string();
            assert!(text.contains("evidence"), "c1 binding must appear in overlay: {text}");
            assert!(text.contains("alpha"), "inline preview must show small read: {text}");
            chat_completion(
                "<run>\nconst lineCount = evidence.content.split(\"\\n\").length;\nlineCount\n</run>",
                "stop",
            )
        }
        2 => {
            let user_text = body["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap();
            assert!(user_text.contains("\"lineCount\""), "c2 binding must appear in last_step_bindings: {user_text}");
            assert!(user_text.contains("\"older_locals\""), "unchanged c1 binding must compact into older_locals: {user_text}");
            chat_completion("Line count is 3.", "stop")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "t1".into(),
        "fake".into(),
        "Count lines in notes.txt".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(captured.lock().unwrap().len(), 3);
    assert_eq!(tool_call_count(ws.path(), "t1", "read_file"), 1, "notes.txt must be read once");
    assert_eq!(tool_call_count(ws.path(), "t1", "read_asset"), 0, "no read_asset call needed");
    assert_eq!(surfaced_assistant_messages(ws.path(), "t1"), vec!["Line count is 3."]);
}

#[tokio::test]
async fn acceptance_02_medium_40kb_value_sliced_in_js_without_reread() {
    let ws = TestWorkspace::new();
    let mut big = "a".repeat(25_000);
    big.push_str("SECRET_NEEDLE_42");
    big.push_str(&"z".repeat(20_000));
    std::fs::write(ws.path().join("large.log"), &big).unwrap();
    init_task_row(ws.path(), "t2");

    let (state, _captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        0 => chat_completion(
            "<run>\nconst raw = io.read({ path: \"large.log\", max_chars: 60000 }).content;\n</run>",
            "stop",
        ),
        1 => {
            let user_msg = body["messages"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["content"]
                .as_str()
                .unwrap();
            assert!(
                user_msg.len() < 24_000,
                "45KB binding must be summarized in overlay (<16KB tier cap), got {} bytes",
                user_msg.len()
            );
            assert!(
                !user_msg.contains("SECRET_NEEDLE_42"),
                "middle of 45KB string is omitted from head/tail overlay preview"
            );
            chat_completion(
                "<run>\nconst exact = raw.slice(25000, 25016);\nexact\n</run>",
                "stop",
            )
        }
        2 => {
            let text = body["messages"].to_string();
            assert!(
                text.contains("SECRET_NEEDLE_42"),
                "JS slice on live heap binding must extract exact middle substring without rereading: {text}"
            );
            chat_completion("Extracted SECRET_NEEDLE_42.", "stop")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "t2".into(),
        "fake".into(),
        "Extract needle at offset 25000".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(tool_call_count(ws.path(), "t2", "read_file"), 1);
    assert_eq!(tool_call_count(ws.path(), "t2", "read_asset"), 0);
}

#[tokio::test]
async fn acceptance_03_crash_restart_recovery_resumes_saved_window_without_replaying_effects() {
    let ws = TestWorkspace::new();
    init_task_row(ws.path(), "t3");

    // Simulate turn stopping at max_rounds=1 right after c1 writes a file & binds state,
    // then verify the persisted SQLite Window deserializes with live heap bindings intact
    // and resuming the same active turn does NOT replay c1's write!
    let (state, _captured, server1) = spawn_mock_provider(ws.path(), |idx, _| match idx {
        0 => chat_completion(
            "<run>\nio.write({ path: \"once.txt\", content: \"written-once\" });\nconst savedCounter = 77;\n</run>",
            "stop",
        ),
        _ => panic!("max_rounds=1 must stop before second model round"),
    })
    .await;

    run_task(
        state,
        "t3".into(),
        "fake".into(),
        "Write once and remember counter".into(),
        1,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server1.abort();
    assert_eq!(tool_call_count(ws.path(), "t3", "write_file"), 1);

    // Re-open task in the SAME turn=1 (crash/restart continuation) and read `savedCounter`!
    init_task_row(ws.path(), "t3");
    let conn = agent_service::open_db(ws.path()).unwrap();
    conn.execute("UPDATE agent_tasks SET status='running' WHERE id='t3'", []).unwrap();
    drop(conn);

    let (state2, _captured2, server2) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        0 => {
            let text = body["messages"].to_string();
            assert!(text.contains("savedCounter"), "restored SQLite window must preserve c1 binding: {text}");
            chat_completion("<run>\nconst nextVal = savedCounter + 1;\nnextVal\n</run>", "stop")
        }
        1 => {
            let text = body["messages"].to_string();
            assert!(text.contains("78"), "live heap binding survived SQLite roundtrip: {text}");
            chat_completion("Recovered 78.", "stop")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state2,
        "t3".into(),
        "fake".into(),
        "Write once and remember counter".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server2.abort();

    assert_eq!(
        tool_call_count(ws.path(), "t3", "write_file"),
        1,
        "c1 write_file must NOT replay after restart recovery"
    );
}

#[tokio::test]
async fn acceptance_04_ordinary_js_function_ten_iterations_zero_extra_llm_rounds() {
    let ws = TestWorkspace::new();
    for i in 0..10 {
        std::fs::write(ws.path().join(format!("item_{i}.txt")), format!("{i}")).unwrap();
    }
    init_task_row(ws.path(), "t4");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        0 => chat_completion(
            "<run>\nfunction sumTen() {\n  let acc = 0;\n  for (let i = 0; i < 10; i++) {\n    acc += Number(io.read({ path: `item_${i}.txt` }).content.trim());\n  }\n  return acc;\n}\nconst total = sumTen();\n</run>",
            "stop",
        ),
        1 => {
            let text = body["messages"].to_string();
            assert!(text.contains("45"), "sum of 0..9 must equal 45: {text}");
            chat_completion("Total is 45.", "stop")
        }
        other => panic!("unexpected LLM round {other}"),
    })
    .await;

    run_task(
        state,
        "t4".into(),
        "fake".into(),
        "Sum item_0..item_9".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        captured.lock().unwrap().len(),
        2,
        "10-iteration ordinary JS function must complete in 0 extra LLM calls (only initial <run> + final reply)"
    );
    assert_eq!(tool_call_count(ws.path(), "t4", "read_file"), 10);
}

#[tokio::test]
async fn acceptance_05_virtual_function_call_stack_multi_step_and_ephemeral_cleanup() {
    let ws = TestWorkspace::new();
    std::fs::write(ws.path().join("mod_a.rs"), "pub fn run() { val.unwrap(); }\n").unwrap();
    init_task_row(ws.path(), "t5");

    let main_prefix_snapshot = Arc::new(Mutex::new(String::new()));
    let snap = main_prefix_snapshot.clone();

    let (state, _captured, server) = spawn_mock_provider(ws.path(), move |idx, body| match idx {
        0 => chat_completion(
            "<run>\nconst policy = \"FORBID_UNWRAP\";\n/** @virtual\n * @uses policy\n * Check target against policy and return { clean: boolean, tag: string }.\n */\nasync function inspectModule(target) {\n  // Read target and judge against policy\n}\nconst verdict = await inspectModule(\"mod_a.rs\");\n</run>",
            "stop",
        ),
        1 => {
            // Inside @virtual child frame (round 1): verify prefix identity + boundary + security!
            let msgs = body["messages"].as_array().unwrap();
            let user_text = msgs.last().unwrap()["content"].as_str().unwrap();
            assert!(user_text.contains("VIRTUAL CALL BOUNDARY"), "{user_text}");
            assert!(user_text.contains("FORBID_UNWRAP"), "captured @uses binding must appear: {user_text}");
            assert!(user_text.contains("mod_a.rs"), "parameter binding must appear: {user_text}");
            *snap.lock().unwrap() = stable_user_prefix_from_wire(user_text).to_owned();
            // Verify shell is forbidden inside @virtual!
            chat_completion("<run>\nprocess.run_readonly({ cmd: \"echo nope\" });\n</run>", "stop")
        }
        2 => {
            let user_text = body["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap();
            assert!(
                user_text.contains("SOURCE_REJECTED") || user_text.contains("process is not defined"),
                "@virtual frame must reject shell access: {user_text}"
            );
            // Verify bare Markdown reply is rejected inside @virtual!
            chat_completion("It contains unwrap().", "stop")
        }
        3 => {
            let user_text = body["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap();
            assert!(
                user_text.contains("VIRTUAL_RETURN_REQUIRED"),
                "@virtual frame must reject bare Markdown prose: {user_text}"
            );
            // Execute sub-segment s1 (read file into child local)
            chat_completion("<run>\nconst src = io.read({ path: target }).content;\n</run>", "stop")
        }
        4 => {
            let user_text = body["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap();
            assert!(user_text.contains("val.unwrap()"), "child frame sees its own s1 read: {user_text}");
            // Execute sub-segment s2 (explicit return!)
            chat_completion(
                "<run>\nreturn { clean: !src.includes(\"unwrap()\"), tag: policy };\n</run>",
                "stop",
            )
        }
        5 => {
            // Back in Main! Verify child stack & mod_a.rs raw content vanished completely,
            // while Main's prefix stayed 100% byte-identical!
            let msgs = body["messages"].as_array().unwrap();
            let user_text = msgs.last().unwrap()["content"].as_str().unwrap();
            assert_eq!(
                stable_user_prefix_from_wire(user_text),
                snap.lock().unwrap().as_str(),
                "Main prefix must remain 100% byte-identical before, during, and after @virtual call!"
            );
            assert!(
                !user_text.contains("VIRTUAL CALL BOUNDARY") && !user_text.contains("val.unwrap()"),
                "child stack and raw file body must be completely cleared on return: {user_text}"
            );
            assert!(
                user_text.contains("FORBID_UNWRAP") && user_text.contains("clean: false"),
                "Main receives structured return value in verdict: {user_text}"
            );
            chat_completion("Module failed FORBID_UNWRAP check.", "stop")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "t5".into(),
        "fake".into(),
        "Audit mod_a.rs".into(),
        12,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(surfaced_assistant_messages(ws.path(), "t5"), vec!["Module failed FORBID_UNWRAP check."]);
}

#[tokio::test]
async fn acceptance_06_and_07_intra_turn_and_cross_turn_byte_exact_prefix_stability() {
    let ws = TestWorkspace::new();
    std::fs::write(ws.path().join("a.txt"), "hello").unwrap();
    init_task_row(ws.path(), "t6");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, _| match idx {
        // Turn 1: 3 segments + reply
        0 => chat_completion("<run>\nconst a = io.read({ path: \"a.txt\" }).content;\n</run>", "stop"),
        1 => chat_completion("<run>\nconst b = a.toUpperCase();\n</run>", "stop"),
        2 => chat_completion("<run>\nconst c = `${b}!`;\n</run>", "stop"),
        3 => chat_completion("Turn 1 answer: HELLO!", "stop"),
        // Turn 2: 1 segment + reply
        4 => chat_completion("<run>\nconst t2 = 42;\n</run>", "stop"),
        5 => chat_completion("Turn 2 answer: 42.", "stop"),
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state.clone(),
        "t6".into(),
        "fake".into(),
        "Run three steps".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();

    run_task(
        state,
        "t6".into(),
        "fake".into(),
        "Follow up in turn 2".into(),
        10,
        CancellationToken::new(),
        2,
        false,
    )
    .await
    .unwrap();
    server.abort();

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 6);

    let sys_count = reqs[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .take_while(|m| m["role"] == "system")
        .count();
    let baseline_systems = &reqs[0]["messages"].as_array().unwrap()[..sys_count];
    for (i, req) in reqs.iter().enumerate() {
        assert_eq!(
            &req["messages"].as_array().unwrap()[..sys_count],
            baseline_systems,
            "system prefix must be 100% byte-identical in request #{i}"
        );
    }

    let u0 = stable_user_prefix_from_wire(reqs[0]["messages"][sys_count]["content"].as_str().unwrap());
    let u1 = stable_user_prefix_from_wire(reqs[1]["messages"][sys_count]["content"].as_str().unwrap());
    let u2 = stable_user_prefix_from_wire(reqs[2]["messages"][sys_count]["content"].as_str().unwrap());
    let u3 = stable_user_prefix_from_wire(reqs[3]["messages"][sys_count]["content"].as_str().unwrap());

    assert!(u1.starts_with(u0), "c1 must append monotonically to user #1");
    assert!(u2.starts_with(u1), "c2 must append monotonically to user #1");
    assert!(u3.starts_with(u2), "c3 must append monotonically to user #1");

    // Cross-turn: Turn 2's [user #1] must be 100% byte-identical to Turn 1's settled u3,
    // with ZERO Active Execution Stack left in Turn 1!
    let turn2_user1 = reqs[4]["messages"][sys_count]["content"].as_str().unwrap();
    assert_eq!(
        turn2_user1, u3,
        "Turn 2 user #1 must be 100% byte-identical to Turn 1 settled code prefix for KV-cache hit!"
    );
    assert!(!turn2_user1.contains("Active Execution Stack"));
    assert_eq!(
        reqs[4]["messages"][sys_count + 1],
        json!({"role": "assistant", "content": "Turn 1 answer: HELLO!"})
    );
}

#[tokio::test]
async fn acceptance_08_per_turn_fresh_vm_wipes_old_scope_and_allows_clean_redeclaration() {
    let ws = TestWorkspace::new();
    init_task_row(ws.path(), "t8");

    let (state, _captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        // Turn 1 declares `target` and `helper`
        0 => chat_completion(
            "<run>\nconst target = \"turn1-secret\";\nfunction helper() { return 1; }\n</run>",
            "stop",
        ),
        1 => chat_completion("Turn 1 finished.", "stop"),
        // Turn 2 tries to access `target` without declaring it -> MUST fail compilation!
        2 => chat_completion("<run>\nconst leaked = target;\n</run>", "stop"),
        3 => {
            let text = body["messages"].to_string();
            assert!(
                text.contains("unknown variable target") || text.contains("target is not defined"),
                "Turn 2 VM must be completely fresh (Turn 1 bindings wiped): {text}"
            );
            // Re-declaring `const target` and `function helper` cleanly works!
            chat_completion(
                "<run>\nconst target = \"turn2-fresh\";\nfunction helper() { return 2; }\nconst check = `${target}:${helper()}`;\n</run>",
                "stop",
            )
        }
        4 => {
            let text = body["messages"].to_string();
            assert!(text.contains("turn2-fresh:2"), "{text}");
            chat_completion("Turn 2 finished cleanly.", "stop")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state.clone(),
        "t8".into(),
        "fake".into(),
        "Start turn 1".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();

    run_task(
        state,
        "t8".into(),
        "fake".into(),
        "Start turn 2".into(),
        10,
        CancellationToken::new(),
        2,
        false,
    )
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn acceptance_09_cross_turn_backtracking_copies_readonly_logic_without_replaying_writes() {
    let ws = TestWorkspace::new();
    init_task_row(ws.path(), "t9");

    let (state, _captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        // Turn 1 writes config.json once and defines a read-only parser snippet
        0 => chat_completion(
            "<run>\nio.write({ path: \"config.json\", content: JSON.stringify({ port: 8080, env: \"prod\" }) });\nconst loadPort = () => JSON.parse(io.read({ path: \"config.json\" }).content).port;\nconst p = loadPort();\n</run>",
            "stop",
        ),
        1 => chat_completion("Initialized port 8080.", "stop"),
        // Turn 2 sees Turn 1's code in [user #1] and copies ONLY the read-only `loadPort` snippet
        2 => {
            let text = body["messages"].to_string();
            assert!(
                text.contains("const loadPort = () => JSON.parse"),
                "Turn 2 sees Turn 1 code in [user #1] for backtracking: {text}"
            );
            chat_completion(
                "<run>\nconst loadPort = () => JSON.parse(io.read({ path: \"config.json\" }).content).port;\nconst currentPort = loadPort();\n</run>",
                "stop",
            )
        }
        3 => {
            let text = body["messages"].to_string();
            assert!(text.contains("8080"), "{text}");
            chat_completion("Port is 8080.", "stop")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(state.clone(), "t9".into(), "fake".into(), "Init config".into(), 10, CancellationToken::new(), 1, false).await.unwrap();
    run_task(state, "t9".into(), "fake".into(), "What port was configured?".into(), 10, CancellationToken::new(), 2, false).await.unwrap();
    server.abort();

    assert_eq!(
        tool_call_count(ws.path(), "t9", "write_file"),
        1,
        "Turn 1 write_file must NEVER be replayed in Turn 2"
    );
}

#[tokio::test]
async fn acceptance_10_mixed_run_and_truncated_run_execute_zero_tools() {
    let ws = TestWorkspace::new();
    init_task_row(ws.path(), "t10");

    let (state, _captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        // 1. Mixed prose + <run> -> MIXED_RUN_AND_REPLY
        0 => chat_completion(
            "Let me create that file:\n<run>\nio.write({ path: \"forbidden1.txt\", content: \"bad\" });\n</run>",
            "stop",
        ),
        // 2. Truncated <run> -> OUTPUT_TRUNCATED
        1 => {
            let text = body["messages"].to_string();
            assert!(text.contains("MIXED_RUN_AND_REPLY"), "{text}");
            chat_completion(
                "<run>\nio.write({ path: \"forbidden2.txt\", content: \"bad\" });",
                "length",
            )
        }
        // 3. Valid reply after verifying 0 side effects ran
        2 => {
            let text = body["messages"].to_string();
            assert!(text.contains("OUTPUT_TRUNCATED"), "{text}");
            chat_completion("All safe, nothing written.", "stop")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(state, "t10".into(), "fake".into(), "Test protocol guards".into(), 10, CancellationToken::new(), 1, false).await.unwrap();
    server.abort();

    assert!(!ws.path().join("forbidden1.txt").exists());
    assert!(!ws.path().join("forbidden2.txt").exists());
    assert_eq!(total_tool_calls(ws.path(), "t10"), 0);
}

#[tokio::test]
async fn acceptance_11_partial_failure_recovery_and_tdz_rollback_preserve_prior_bindings_and_effects() {
    let ws = TestWorkspace::new();
    init_task_row(ws.path(), "t11");

    let (state, _captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        0 => chat_completion("<run>\nconst preserved = \"SAFE_VALUE\";\n</run>", "stop"),
        1 => chat_completion(
            "<run>\nio.write({ path: \"once.txt\", content: \"APPLIED\" });\nconst preserved = JSON.parse(\"{broken\");\n</run>",
            "stop",
        ),
        2 => {
            let text = body["messages"].to_string();
            assert!(
                text.contains("io.write") && text.contains("uncaught_error"),
                "overlay must show completed io.write effect + uncaught_error: {text}"
            );
            // Verify `preserved` rolled back from TDZ to c1's "SAFE_VALUE", and do NOT re-run io.write!
            chat_completion(
                "<run>\nconst combined = `${preserved}:${io.read({ path: \"once.txt\" }).content}`;\n</run>",
                "stop",
            )
        }
        3 => {
            let text = body["messages"].to_string();
            assert!(
                text.contains("SAFE_VALUE:APPLIED"),
                "TDZ rollback must preserve c1's initialized binding: {text}"
            );
            chat_completion("Recovered SAFE_VALUE:APPLIED.", "stop")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(state, "t11".into(), "fake".into(), "Test partial failure".into(), 10, CancellationToken::new(), 1, false).await.unwrap();
    server.abort();

    assert_eq!(tool_call_count(ws.path(), "t11", "write_file"), 1, "write_file must execute once");
}

#[tokio::test]
async fn acceptance_12_wire_requests_never_contain_json_tool_schemas_across_routes() {
    let ws = TestWorkspace::new();
    std::fs::write(ws.path().join("doc.txt"), "hello").unwrap();
    init_task_row(ws.path(), "t12");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, _| match idx {
        0 => chat_completion(
            "<run>\n/** @virtual Return {ok:boolean}. */\nasync function ping() {}\nconst r = await ping();\n</run>",
            "stop",
        ),
        1 => chat_completion("<run>\nreturn { ok: true };\n</run>", "stop"),
        2 => chat_completion("Done.", "stop"),
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(state, "t12".into(), "fake".into(), "Check zero tools on wire".into(), 10, CancellationToken::new(), 1, false).await.unwrap();
    server.abort();

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 3);
    for (idx, body) in reqs.iter().enumerate() {
        assert!(body.get("tools").is_none(), "request #{idx} must omit tools: {body}");
        assert!(body.get("tool_choice").is_none(), "request #{idx} must omit tool_choice: {body}");
        assert!(body.get("parallel_tool_calls").is_none(), "request #{idx} must omit parallel_tool_calls: {body}");

        // Also verify Responses API projection keeps zero tool fields!
        let responses = model_stream::responses_request(body);
        assert!(responses.get("tools").is_none(), "responses wire #{idx} must omit tools");
        assert!(responses.get("tool_choice").is_none(), "responses wire #{idx} must omit tool_choice");
        assert!(responses.get("parallel_tool_calls").is_none(), "responses wire #{idx} must omit parallel_tool_calls");
    }
}
