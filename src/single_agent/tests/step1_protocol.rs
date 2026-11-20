//! Step 1 unit tests: pure-JS + `return <<<` protocol, `.d.ts` Rust proxy,
//! three-tier `// =>` + `EPHEMERAL_PEEK`, anti-cheating slice lock, per-turn fresh VM,
//! and First-Trap Truncation termination rule.

use super::{
    TestWorkspace, assert_single_run_js_tool, assistant_messages, completion, init_task_row,
    run_js, run_js_with_note, spawn_mock_provider, split_stable_prefix, tool_call_count,
};
use crate::single_agent::run_task;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn step1_analyze_code_pure_js_dts_rust_proxy_peek_projection_and_return_heredoc() {
    let ws = TestWorkspace::new("cac-v2-step1");
    std::fs::create_dir_all(ws.path().join("src")).unwrap();

    // Create a ~120-line realistic Rust source file (> 2KB so it lands in Tier 2 EPHEMERAL_PEEK!)
    let mut auth_rs = String::new();
    for i in 1..=60 {
        auth_rs.push_str(&format!(
            "// padding comment line {i:03} keeping file realistic for auth module context\n"
        ));
    }
    auth_rs.push_str(
        "pub fn verify_token(token: &str) -> bool {\n\
         \x20\x20\x20\x20// BUG: decode_without_verify ignores expiration and signature!\n\
         \x20\x20\x20\x20decode_without_verify(token).is_ok()\n\
         }\n",
    );
    for i in 65..=120 {
        auth_rs.push_str(&format!(
            "// trailing helper comment line {i:03} for auth module verification\n"
        ));
    }
    assert!(
        auth_rs.chars().count() > 3_500 && auth_rs.chars().count() < 20_000,
        "fixture should land squarely in Tier 2 (2KB..24KB), got {} chars",
        auth_rs.chars().count()
    );
    std::fs::write(ws.path().join("src/auth.rs"), &auth_rs).unwrap();
    init_task_row(ws.path(), "step1-analyze");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        0 => {
            // 1. Verify ONLY the single `run_js` shell tool (ZERO business tool schemas) & presence of `.d.ts` declarations!
            assert_single_run_js_tool(&body);
            let sys = body["messages"][0]["content"].as_str().unwrap();
            assert!(sys.contains("declare namespace io"), "system prompt must contain .d.ts declarations: {sys}");
            assert!(sys.contains("function read("), "io.read .d.ts signature must be present");

            // Emit commentary text alongside `run_js`: commentary must stay cleanly isolated from JS code!
            run_js_with_note(
                "我会先读取 src/auth.rs 检查鉴权实现。",
                "const authFile = io.read({ path: \"src/auth.rs\" });",
            )
        }
        1 => {
            // 2. Verify <= 64KB file prints inline permanently with line numbers (NO EPHEMERAL_PEEK!).
            let user_msg = body["messages"][1]["content"].as_str().unwrap();
            assert!(
                !user_msg.contains("EPHEMERAL_PEEK"),
                "EPHEMERAL_PEEK must be removed: {user_msg}"
            );
            assert!(
                user_msg.contains("authFile (File \"src/auth.rs\", Ref#1")
                    && user_msg.contains("61: pub fn verify_token")
                    && user_msg.contains("63:     decode_without_verify(token).is_ok()"),
                "<= 64KB file must appear permanently inline with line numbers: {user_msg}"
            );

            // Model projects ONLY lines 61..64 via JS from live heap (0 disk re-reads!)
            run_js("const verifyFn = authFile.content.split(\"\\n\").slice(60, 64).join(\"\\n\");")
        }
        2 => {
            // 3. Verify BOTH earlier file and new slice remain permanently visible (zero amnesia, pure Append-Only!)
            let user_msg = body["messages"][1]["content"].as_str().unwrap();
            assert!(
                !user_msg.contains("EPHEMERAL_PEEK"),
                "EPHEMERAL_PEEK must not appear: {user_msg}"
            );
            assert!(
                user_msg.contains("verifyFn:") && user_msg.contains("decode_without_verify(token).is_ok()"),
                "pinned 4-line slice must live permanently in `// =>`: {user_msg}"
            );

            // Emit final Markdown reply directly (no `run_js` call, no `return <<<` needed!)
            // Example ```js block inside Markdown must NOT be executed as JS!
            completion(
                "分析完成：`src/auth.rs` 第 61-64 行的 `verify_token` 使用了 `decode_without_verify`，跳过了签名和过期校验。\n\n示例修复思路（展示代码，不应执行）：\n```js\nio.write({ path: \"MUST_NOT_EXECUTE.txt\", content: \"boom\" });\n```",
            )
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "step1-analyze".into(),
        "fake".into(),
        "分析 src/auth.rs 里的鉴权漏洞".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 3);

    // Verify 100% byte-exact KV-cache prefix growth across all 3 requests!
    let sys0 = reqs[0]["messages"][0]["content"].as_str().unwrap();
    for (i, req) in reqs.iter().enumerate() {
        assert_eq!(req["messages"][0]["content"].as_str().unwrap(), sys0, "system prompt must be 100% static at req #{i}");
        assert_single_run_js_tool(req);
    }
    let p0 = split_stable_prefix(reqs[0]["messages"][1]["content"].as_str().unwrap());
    let p1 = split_stable_prefix(reqs[1]["messages"][1]["content"].as_str().unwrap());
    let p2 = split_stable_prefix(reqs[2]["messages"][1]["content"].as_str().unwrap());
    assert!(p1.starts_with(p0), "step 1 stable prefix must strictly extend step 0");
    assert!(p2.starts_with(p1), "step 2 stable prefix must strictly extend step 1");

    // `read_file` called exactly ONCE (step 1 sliced from live JS heap memory!)
    assert_eq!(tool_call_count(ws.path(), "step1-analyze", "read_file"), 1);
    // Example JS inside `return <<<` MUST NOT execute!
    assert!(!ws.path().join("MUST_NOT_EXECUTE.txt").exists());
    assert_eq!(tool_call_count(ws.path(), "step1-analyze", "write_file"), 0);

    let replies = assistant_messages(ws.path(), "step1-analyze");
    assert_eq!(replies.len(), 1);
    assert!(replies[0].contains("decode_without_verify"));
    assert!(replies[0].contains("```js"));
}

#[tokio::test]
async fn step1_anti_cheating_slice_lock_and_per_turn_fresh_vm() {
    let ws = TestWorkspace::new("cac-v2-step1");
    let big = "line_data_0123456789_padding_for_64kb_cap\n".repeat(1_700); // ~71,400 chars (> 64KB INLINE_LIMIT_CHARS)
    std::fs::write(ws.path().join("big.txt"), &big).unwrap();
    init_task_row(ws.path(), "step1-lock");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        // Turn 1, step 1: read big.txt (> 64KB)
        0 => run_js("const f = io.read({ path: \"big.txt\" });"),
        // Turn 1, step 2: try to dump > 64KB slice -> must stay capped (> 64KB not inlined)
        1 => run_js("const cheatAll = f.content.slice(0, 68000);"),
        // Turn 1, step 3: verify 64KB cap kept >64KB slice out of permanent inline, then finish via native JS `return`!
        2 => {
            let user_msg = body["messages"][1]["content"].as_str().unwrap();
            let stable = split_stable_prefix(user_msg);
            assert!(
                stable.contains("> 64KB not inlined"),
                "64KB cap must prevent >64KB slice from dumping verbatim into permanent trace: {stable}"
            );
            run_js("return \"Turn 1 done cleanly.\";")
        }
        // Turn 2, step 1: try referencing Turn 1's `f` -> MUST fail compile (fresh VM!)
        3 => {
            let msgs = body["messages"].as_array().unwrap();
            assert_eq!(msgs.len(), 4, "[system, user#1, assistant#1, user#2]");
            assert_eq!(msgs[2], json!({"role": "assistant", "content": "Turn 1 done cleanly."}));
            assert!(!msgs[1]["content"].as_str().unwrap().contains("EPHEMERAL_PEEK"));
            run_js("const leaked = f.content;")
        }
        // Turn 2, step 2: verify compile error reported in EPHEMERAL_PEEK, redeclare `const f` cleanly!
        4 => {
            let user2 = body["messages"][3]["content"].as_str().unwrap();
            assert!(user2.contains("COMPILE_ERROR"), "Turn 2 must reject Turn 1 variable: {user2}");
            run_js("const f = \"fresh-turn-2\";\nreturn `Rebound cleanly: ${f}`;")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state.clone(),
        "step1-lock".into(),
        "fake".into(),
        "Read big.txt".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();

    run_task(
        state,
        "step1-lock".into(),
        "fake".into(),
        "Start fresh turn 2".into(),
        10,
        CancellationToken::new(),
        2,
        false,
    )
    .await
    .unwrap();
    server.abort();

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 5);
    // Cross-turn KV cache: Turn 2's user#1 must be byte-identical to Turn 1's final stable prefix!
    let t1_final_stable = split_stable_prefix(reqs[2]["messages"][1]["content"].as_str().unwrap());
    let t2_user1 = reqs[3]["messages"][1]["content"].as_str().unwrap();
    assert!(t2_user1.starts_with(t1_final_stable));

    let replies = assistant_messages(ws.path(), "step1-lock");
    assert_eq!(replies, vec!["Turn 1 done cleanly.", "Rebound cleanly: fresh-turn-2"]);
}

#[tokio::test]
async fn step1_termination_rule_external_tools_continue_pure_memory_projection_finishes() {
    let ws = TestWorkspace::new("cac-v2-step1");
    std::fs::write(ws.path().join("sample.rs"), "pub fn bad() { unsafe_call(); }\n").unwrap();
    init_task_row(ws.path(), "step1-term");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        // Round 0: external tool call (`io.read`) + premature JS `return`
        // Because `external_tool_calls > 0`, host MUST execute `io.read`, discard
        // the premature return, and continue to Round 1!
        0 => run_js(
            "const f = io.read({ path: \"sample.rs\" });\nreturn \"PREMATURE_MUST_BE_DISCARDED\";",
        ),
        // Round 1: pure in-memory JS slicing (`external_tool_calls == 0`) + explicit `return`
        // Host MUST execute slice into permanent `// =>` AND immediately finish the turn!
        1 => {
            let user_msg = body["messages"][1]["content"].as_str().unwrap();
            assert!(
                user_msg.contains("unsafe_call()"),
                "round 1 must see real file content from round 0: {user_msg}"
            );
            assert!(
                user_msg.contains("inspect the real output below before returning"),
                "host note must explain discarded premature return: {user_msg}"
            );
            run_js(
                "const pinned = f.content.trim();\nreturn \"Verified: `sample.rs` calls `unsafe_call()`.\";",
            )
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "step1-term".into(),
        "fake".into(),
        "Check sample.rs".into(),
        5,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(captured.lock().unwrap().len(), 2);
    assert_eq!(tool_call_count(ws.path(), "step1-term", "read_file"), 1);
    let replies = assistant_messages(ws.path(), "step1-term");
    assert_eq!(replies, vec!["Verified: `sample.rs` calls `unsafe_call()`."]);
}
