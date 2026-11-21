//! Step 1 unit tests: pure-JS + `return <<<` protocol, `.d.ts` Rust proxy,
//! three-tier `// =>` + `EPHEMERAL_PEEK`, anti-cheating slice lock, per-turn fresh VM,
//! and First-Trap Truncation termination rule.

use super::{
    TestWorkspace, assert_prefix_immutable, assert_single_run_js_tool, assert_valid_tool_pairing,
    assistant_messages, completion, init_task_row, last_tool_output, run_js, run_js_with_note,
    spawn_mock_provider, tool_call_count,
};
use crate::agent_service::{self, AgentProviderRoute};
use crate::single_agent::run_task;
use axum::{Json, Router, body::Body, http::header, response::Response, routing::post};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
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
            // 2. Verify native multi-turn layout: [system, user, assistant(run_js), tool(output)]
            assert_valid_tool_pairing(&body);
            let msgs = body["messages"].as_array().unwrap();
            assert_eq!(msgs.len(), 4, "[system, user, assistant#1, tool#1]: {msgs:?}");
            assert_eq!(msgs[1]["role"], "user");
            assert_eq!(msgs[2]["role"], "assistant");
            assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "run_js");
            assert_eq!(msgs[3]["role"], "tool");
            let tool_out = last_tool_output(&body);
            assert!(
                !tool_out.contains("EPHEMERAL_PEEK"),
                "EPHEMERAL_PEEK must be removed: {tool_out}"
            );
            assert!(
                tool_out.contains("authFile (File \"src/auth.rs\", Ref#1")
                    && tool_out.contains("61|pub fn verify_token")
                    && tool_out.contains("63|    decode_without_verify(token).is_ok()"),
                "<= 64KB file must appear permanently inline with line numbers: {tool_out}"
            );

            // Model projects ONLY lines 61..64 via JS from live heap (0 disk re-reads!)
            run_js("const verifyFn = authFile.content.split(\"\\n\").slice(60, 64).join(\"\\n\");")
        }
        2 => {
            // 3. Verify BOTH earlier tool output and new slice remain visible in multi-turn history!
            assert_valid_tool_pairing(&body);
            let msgs = body["messages"].as_array().unwrap();
            assert_eq!(msgs.len(), 6, "[system, user, asst#1, tool#1, asst#2, tool#2]");
            let tool2 = last_tool_output(&body);
            assert!(
                tool2.contains("verifyFn:") && tool2.contains("decode_without_verify(token).is_ok()"),
                "pinned 4-line slice must appear in tool#2: {tool2}"
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
    for req in reqs.iter() {
        assert_single_run_js_tool(req);
        assert_valid_tool_pairing(req);
    }
    assert_prefix_immutable(&reqs[0], &reqs[1]);
    assert_prefix_immutable(&reqs[1], &reqs[2]);

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
        // Turn 1, step 3: verify 64KB cap kept >64KB slice out of tool output, then finish via native JS `return`!
        2 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("> 64KB not inlined"),
                "64KB cap must prevent >64KB slice from dumping verbatim into tool output: {tool_out}"
            );
            run_js("return \"Turn 1 done cleanly.\";")
        }
        // Turn 2, step 1: try referencing Turn 1's `f` -> MUST fail compile (fresh VM!)
        3 => {
            assert_valid_tool_pairing(&body);
            let msgs = body["messages"].as_array().unwrap();
            // [system, user#1, asst1, tool1, asst2, tool2, asst3, tool3, final_asst#1, user#2]
            assert_eq!(msgs.len(), 10, "expected full multi-turn Turn 1 + Turn 2 user: {msgs:?}");
            assert_eq!(msgs[8], json!({"role": "assistant", "content": "Turn 1 done cleanly."}));
            assert_eq!(msgs[9], json!({"role": "user", "content": "Start fresh turn 2"}));
            run_js("const leaked = f.content;")
        }
        // Turn 2, step 2: verify compile error returned as matching tool message, redeclare `const f` cleanly!
        4 => {
            assert_valid_tool_pairing(&body);
            let tool_out = last_tool_output(&body);
            assert!(tool_out.contains("COMPILE_ERROR"), "Turn 2 must reject Turn 1 variable: {tool_out}");
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
    // Cross-step & cross-turn KV cache: every request strictly extends the exact prior messages!
    for pair in reqs.windows(2) {
        assert_prefix_immutable(&pair[0], &pair[1]);
    }

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
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("unsafe_call()"),
                "round 1 must see real file content from round 0: {tool_out}"
            );
            assert!(
                tool_out.contains("inspect the real output below before returning"),
                "host note must explain discarded premature return: {tool_out}"
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

#[tokio::test]
async fn step1_final_statement_only_display_rule() {
    let ws = TestWorkspace::new("cac-v2-step1-final-only");
    std::fs::write(ws.path().join("a.txt"), "ALPHA_SINGLE_STMT\n").unwrap();
    std::fs::write(
        ws.path().join("b.txt"),
        "INTERMEDIATE_LINE_1_SHOULD_STAY_SILENT\nPICKED_LINE_2_FINAL\n",
    )
    .unwrap();
    init_task_row(ws.path(), "step1-final");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        // Step 0: single statement -> displays `text`!
        0 => run_js("const text = io.read({ path: \"a.txt\" });"),
        // Step 1: multi-line statements -> intermediate `raw` stays silent in VM, ONLY final `picked` displays!
        1 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("ALPHA_SINGLE_STMT"),
                "single-statement step must display its variable: {tool_out}"
            );
            run_js(
                "const raw = io.read({ path: \"b.txt\" });\n\
                 const picked = raw.slice(2, 2);",
            )
        }
        // Step 2: multi-source consumed by final expression -> ONLY `$_` displays (`n1`/`n2` silent)!
        2 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("PICKED_LINE_2_FINAL") && !tool_out.contains("INTERMEDIATE_LINE_1_SHOULD_STAY_SILENT"),
                "derived pipeline must show ONLY sink `picked` and suppress consumed `raw`: {tool_out}"
            );
            run_js(
                "const n1 = \"SILENT_A\"; const n2 = \"SILENT_B\"; ({ finalVal: n1 + \"+\" + n2 })",
            )
        }
        // Step 3: two independent unconsumed bindings -> BOTH display!
        3 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("SILENT_A+SILENT_B")
                    && !tool_out.contains("// => n1")
                    && !tool_out.contains("// => n2"),
                "consumed intermediates must stay silent when folded into final expr: {tool_out}"
            );
            run_js(
                "const fromA = text.head(1);\n\
                 const fromRaw = raw.head(1);",
            )
        }
        // Step 4: multiple bare query expressions (e.g. `a.slice(...); b.slice(...)`) -> BOTH display!
        4 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("fromA =")
                    && tool_out.contains("ALPHA_SINGLE_STMT")
                    && tool_out.contains("fromRaw =")
                    && tool_out.contains("INTERMEDIATE_LINE_1_SHOULD_STAY_SILENT"),
                "independent unconsumed bindings must BOTH display and silent `raw` must stay live: {tool_out}"
            );
            run_js("text.head(1); raw.slice(2, 2);")
        }
        // Step 5: `console.log(hits)` consumes `hits` -> ONLY console shows, no duplicate `// => hits`!
        5 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("$_1 =")
                    && tool_out.contains("ALPHA_SINGLE_STMT")
                    && tool_out.contains("$_2 =")
                    && tool_out.contains("PICKED_LINE_2_FINAL"),
                "multiple bare query expressions must ALL display ($_1, $_2): {tool_out}"
            );
            run_js("const logged = raw.slice(1, 1); console.log(\"LOG:\" + logged);")
        }
        6 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("LOG:1|INTERMEDIATE_LINE_1_SHOULD_STAY_SILENT")
                    && !tool_out.contains("// => logged"),
                "variable consumed by console.log must NOT duplicate outside console: {tool_out}"
            );
            completion("Dataflow Sink 汇节点展示规则全部验证通过。")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "step1-final".into(),
        "fake".into(),
        "Test dataflow sink display".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    let reqs = captured.lock().unwrap();
    for pair in reqs.windows(2) {
        assert_prefix_immutable(&pair[0], &pair[1]);
    }
    if std::env::var_os("PRINT_TEST_CONTEXT").is_some() {
        eprintln!(
            "=== CHAT COMPLETIONS MULTI-TURN CONTEXT (final step) ===\n{}",
            serde_json::to_string_pretty(reqs.last().unwrap()).unwrap()
        );
    }

    assert_eq!(
        assistant_messages(ws.path(), "step1-final"),
        vec!["Dataflow Sink 汇节点展示规则全部验证通过。"]
    );
}

#[tokio::test]
async fn step1_stream_transport_retry_resets_partial_delta_before_success() {
    let ws = TestWorkspace::new("cac-v2-step1-stream-retry");
    std::fs::write(ws.path().join("README.md"), "retry fixture\n").unwrap();
    init_task_row(ws.path(), "step1-stream-retry");

    let seq = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(_body): Json<Value>| {
            let idx = seq.fetch_add(1, Ordering::SeqCst);
            async move {
                if idx == 0 {
                    let chunks = vec![
                        Ok::<_, std::convert::Infallible>(
                            "data: {\"choices\":[{\"delta\":{\"content\":\"半截旧文本\"}}]}\n\n"
                                .as_bytes()
                                .to_vec(),
                        ),
                        Ok::<_, std::convert::Infallible>(
                            "data: {\"error\":{\"message\":\"upstream_http2_stream_error\"}}\n\n"
                                .as_bytes()
                                .to_vec(),
                        ),
                    ];
                    Response::builder()
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from_stream(futures::stream::iter(chunks)))
                        .unwrap()
                } else {
                    Response::builder()
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&completion(
                            "流式重试成功，最终结果只出现一次。",
                        )).unwrap()))
                        .unwrap()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    run_task(
        state,
        "step1-stream-retry".into(),
        "fake".into(),
        "验证流式请求可恢复".into(),
        3,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        assistant_messages(ws.path(), "step1-stream-retry"),
        vec!["流式重试成功，最终结果只出现一次。"]
    );
    let conn = agent_service::open_db(ws.path()).unwrap();
    let reset_count: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_task_events WHERE task_id=?1 AND kind='assistant/delta' AND json_extract(data,'$.reset')=1",
            ["step1-stream-retry"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reset_count, 1, "a partial stream retry must reset its open slot");
}

#[tokio::test]
async fn step1_responses_api_replays_encrypted_reasoning_items() {
    let ws = TestWorkspace::new("cac-v2-step1-responses");
    std::fs::write(ws.path().join("hello.txt"), "RESPONSES_OK_LINE_1\n").unwrap();
    init_task_row(ws.path(), "step1-resp");

    let seq = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let cap_ref = captured.clone();
    let captured_session_ids = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let sid_ref = captured_session_ids.clone();

    let app = Router::new().route(
        "/v1/responses",
        post(move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
            let idx = seq.fetch_add(1, Ordering::SeqCst);
            sid_ref.lock().unwrap().push(
                headers
                    .get("session_id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
            );
            cap_ref.lock().unwrap().push(body);
            async move {
                let sse = match idx {
                    0 => concat!(
                        "event: response.output_item.done\n",
                        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_step1\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Inspect hello.txt\"}],\"encrypted_content\":\"ENC_BLOB_STEP_1\"}}\n\n",
                        "event: response.output_item.done\n",
                        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_resp_1\",\"name\":\"run_js\",\"arguments\":\"{\\\"code\\\":\\\"const f = io.read({ path: \\\\\\\"hello.txt\\\\\\\" });\\\"}\"}}\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1000,\"output_tokens\":40,\"total_tokens\":1040}}}\n\n",
                    ),
                    1 => concat!(
                        "event: response.output_item.done\n",
                        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_step2\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Done\"}],\"encrypted_content\":\"ENC_BLOB_STEP_2\"}}\n\n",
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Responses API 加密推理链回放验证成功。\"}\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1200,\"input_tokens_details\":{\"cached_tokens\":1140},\"output_tokens\":20,\"output_tokens_details\":{\"reasoning_tokens\":12},\"total_tokens\":1220}}}\n\n",
                    ),
                    other => panic!("unexpected responses round {other}"),
                };
                Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from(sse))
                    .unwrap()
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut state = agent_service::tests::test_state(ws.path(), addr);
    state.provider_url = format!("http://{addr}/v1");
    state.provider_routes.insert(
        state.provider_name.clone(),
        AgentProviderRoute {
            responses: true,
            messages: false,
            messages_capabilities: Default::default(),
            url: state.provider_url.clone(),
            api_key: String::new(),
            models: Default::default(),
            max_output_tokens: Default::default(),
            context_windows: Default::default(),
                long_context_thresholds: Default::default(),
                model_costs: Default::default(),
        },
    );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    run_task(
        state,
        "step1-resp".into(),
        "fake".into(),
        "用 Responses API 读取 hello.txt".into(),
        5,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    // Verify standard `session_id` HTTP header matches `prompt_cache_key` across all steps!
    let sids = captured_session_ids.lock().unwrap();
    assert_eq!(sids.len(), 2);
    let expected_pck = reqs[0]["prompt_cache_key"].as_str().unwrap();
    assert_eq!(sids[0].as_deref(), Some(expected_pck));
    assert_eq!(sids[1].as_deref(), Some(expected_pck));
    let r1 = &reqs[1];
    assert_eq!(r1["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(r1["store"], json!(false));
    let input = r1["input"].as_array().unwrap();
    assert_eq!(input.len(), 4, "[user, reasoning(encrypted), function_call, function_call_output]: {input:?}");
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(input[1]["encrypted_content"], "ENC_BLOB_STEP_1");
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_resp_1");
    assert_eq!(input[2]["name"], "run_js");
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_resp_1");
    assert!(
        input[3]["output"].as_str().unwrap().contains("RESPONSES_OK_LINE_1"),
        "function_call_output must contain file read output: {}",
        input[3]
    );
    // Strict byte-identical prefix: input[0] of req1 == input[0] of req0!
    assert_eq!(reqs[0]["input"][0], r1["input"][0]);
    assert_eq!(reqs[0]["instructions"], r1["instructions"]);

    if std::env::var_os("PRINT_TEST_CONTEXT").is_some() {
        eprintln!(
            "=== RESPONSES API MULTI-TURN CONTEXT (step 2 with encrypted reasoning) ===\n{}",
            serde_json::to_string_pretty(r1).unwrap()
        );
    }

    assert_eq!(
        assistant_messages(ws.path(), "step1-resp"),
        vec!["Responses API 加密推理链回放验证成功。"]
    );
    // Verify exact provider KV-cache & reasoning token telemetry was persisted in context/usage!
    let conn = agent_service::open_db(ws.path()).unwrap();
    let usage_row: String = conn
        .query_row(
            "SELECT data FROM agent_task_events WHERE task_id='step1-resp' AND kind='context/usage' AND json_extract(data,'$.source')='provider_usage' ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let usage_json: Value = serde_json::from_str(&usage_row).unwrap();
    assert_eq!(usage_json["estimated_input_tokens"], json!(1200));
    assert_eq!(usage_json["cached_tokens"], json!(1140));
    assert_eq!(usage_json["cache_hit_pct"], json!(95.0));
    assert_eq!(usage_json["output_tokens"], json!(20));
    assert_eq!(usage_json["reasoning_tokens"], json!(12));
}

#[tokio::test]
async fn step1_deepseek_messages_records_real_cache_and_cost_in_event_and_log() {
    let ws = TestWorkspace::new("cac-v2-deepseek-pricing");
    let task = "step1-deepseek";
    init_task_row(ws.path(), task);
    async fn reply(Json(body): Json<Value>) -> Response {
        assert_eq!(body["model"], "deepseek-flash");
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert_eq!(body["tools"][0]["name"], "run_js");
        assert!(body["tools"][0].get("defer_loading").is_none());
        let sse = [
            json!({"type":"message_start","message":{"id":"msg_test","usage":{"input_tokens":500,"cache_read_input_tokens":9500,"cache_creation_input_tokens":1000}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Done."}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20}}),
            json!({"type":"message_stop"}),
        ].into_iter().map(|record| format!("event: {}\ndata: {record}\n\n", record["type"].as_str().unwrap())).collect::<String>();
        Response::builder().header(header::CONTENT_TYPE, "text/event-stream").body(Body::from(sse)).unwrap()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/v1/messages", post(reply))).await.unwrap();
    });
    let mut state = agent_service::tests::test_state(ws.path(), addr);
    state.provider_name = "deepseek-self".into();
    state.provider_url = format!("http://{addr}/v1");
    state.provider_routes.insert(state.provider_name.clone(), AgentProviderRoute {
        responses: false,
        messages: true,
        messages_capabilities: [("deepseek-flash".into(), crate::anthropic_messages::Capabilities {
            tool_update: true, system_prompt_update: true,
        })].into_iter().collect(),
        url: state.provider_url.clone(), api_key: String::new(), models: Default::default(),
        max_output_tokens: Default::default(), context_windows: Default::default(),
        long_context_thresholds: Default::default(),
        model_costs: [("deepseek-flash".into(), serde_json::from_value(json!({
            "input":1.0,"output":2.0,"cacheRead":0.02,"currency":"CNY"
        })).unwrap())].into_iter().collect(),
    });
    run_task(state, task.into(), "deepseek-flash".into(), "test usage".into(), 5, CancellationToken::new(), 1, false).await.unwrap();
    server.abort();
    let conn = agent_service::open_db(ws.path()).unwrap();
    let event: String = conn.query_row(
        "SELECT data FROM agent_task_events WHERE task_id=?1 AND kind='context/usage' AND json_extract(data,'$.source')='provider_usage'",
        [task], |row| row.get(0),
    ).unwrap();
    let event: Value = serde_json::from_str(&event).unwrap();
    let log = std::fs::read_dir(ws.path().join("logs")).unwrap().flatten()
        .filter(|file| file.file_name().to_string_lossy().starts_with("model-requests-"))
        .flat_map(|file| std::fs::read_to_string(file.path()).unwrap().lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()).collect::<Vec<_>>())
        .find(|record| record["stage"] == "completed" && record["identity"]["task_id"] == task).unwrap();
    for (key, expected) in [
        ("estimated_input_tokens", json!(11_000)), ("uncached_input_tokens", json!(500)),
        ("cached_tokens", json!(9500)), ("cache_write_tokens", json!(1000)),
        ("cache_hit_pct", json!(86.4)), ("output_tokens", json!(20)), ("reasoning_tokens", json!(0)),
        ("cost_currency", json!("CNY")), ("pricing_tier", json!("standard")),
        ("model", json!("deepseek-flash")), ("provider", json!("deepseek-self")),
    ] {
        assert_eq!(event[key], expected, "event {key}");
        assert_eq!(log["detail"][key], expected, "transport {key}");
    }
    assert!((event["cost_amount"].as_f64().unwrap() - 0.00173).abs() < 1e-12);
    assert_eq!(log["detail"]["cost_amount"], event["cost_amount"]);
}

#[tokio::test]
async fn step1_append_only_live_handle_note_progressive_64kb_array_and_console_4kb_cap() {
    let ws = TestWorkspace::new("cac-v2-step1-progressive");
    // Create 3 files (~30KB each -> combined 90KB > 64KB step budget)
    let mk = |tag: &str| {
        (1..=450)
            .map(|i| format!("{tag}_line_{i:03}: payload_with_quotes_\"ok\"_padding_0123456789\n"))
            .collect::<String>()
    };
    std::fs::write(ws.path().join("f1.txt"), mk("FILE1")).unwrap();
    std::fs::write(ws.path().join("f2.txt"), mk("FILE2")).unwrap();
    std::fs::write(ws.path().join("f3.txt"), mk("FILE3")).unwrap();
    init_task_row(ws.path(), "step1-progressive");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        // Turn 1, Step 1: read all 3 files silently (consumed by slice array) and return ~88KB array.
        0 => run_js(
            "const a = io.read({ path: \"f1.txt\" });\n\
             const b = io.read({ path: \"f2.txt\" });\n\
             const c = io.read({ path: \"f3.txt\" });\n\
             [a.slice(1, 450), b.slice(1, 450), c.slice(1, 450)];",
        ),
        // Turn 1, Step 2: verify progressive 64KB unescaped array + append-only live handles note!
        1 => {
            let tool_out = last_tool_output(&body);
            // 1) Must NOT collapse all 88KB into a 978-byte LARGE Array INDEX!
            assert!(
                !tool_out.contains("LARGE Array("),
                "multiline string array must fill progressively up to 64KB, not collapse all-or-nothing: {tool_out}"
            );
            // 2) Must print FILE1 and FILE2 cleanly unescaped (no JSON `\"ok\"` escaping!)
            assert!(
                tool_out.contains("FILE1_line_001: payload_with_quotes_\"ok\"")
                    && tool_out.contains("FILE2_line_001: payload_with_quotes_\"ok\""),
                "first ~64KB items must print verbatim and unescaped: {}",
                &tool_out[..tool_out.len().min(600)]
            );
            // 3) Overflow item must be summarized cleanly with budget notice
            assert!(
                tool_out.contains("64KB step budget reached"),
                "overflow tail must explain 64KB step budget cleanly"
            );
            // 4) Silent new handles must be recorded in a compact append-only 1-line note!
            assert!(
                tool_out.contains("// [live handles added:")
                    && tool_out.contains("a=Ref#1(\"f1.txt\",450L)")
                    && tool_out.contains("c=Ref#3(\"f3.txt\",450L)"),
                "silent new handles must be recorded append-only: {}",
                &tool_out[tool_out.len().saturating_sub(400)..]
            );

            // Now test console.log hard 4KB cap (no new handles -> no repeated handle note!)
            run_js("console.log(\"Z\".repeat(12000));")
        }
        // Turn 1, Step 3: verify console.log capped at 4KB AND no duplicate live handles note!
        2 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("console truncated at 4KB (12000 chars total)")
                    && tool_out.contains("do NOT use console.log for bulk output"),
                "console.log must be strictly capped at 4KB: {tool_out}"
            );
            assert!(
                !tool_out.contains("// [live handles added:"),
                "must NOT repeat already-recorded handles on later steps: {tool_out}"
            );
            completion("Turn 1 complete.")
        }
        // Turn 2, Step 1: strict append-only across turns!
        3 => completion("Turn 2 clean."),
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state.clone(),
        "step1-progressive".into(),
        "fake".into(),
        "Turn 1 task".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();

    run_task(
        state,
        "step1-progressive".into(),
        "fake".into(),
        "Turn 2 task".into(),
        10,
        CancellationToken::new(),
        2,
        false,
    )
    .await
    .unwrap();
    server.abort();

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 4);
    // Strict 100% byte-identical Append-Only KV-cache prefix across ALL steps and turns!
    for pair in reqs.windows(2) {
        assert_prefix_immutable(&pair[0], &pair[1]);
    }
}

#[tokio::test]
async fn step1_loop_guard_infinite_loop_notfound_hint_parse_caret_and_ref_dedup() {
    let ws = TestWorkspace::new("cac-v2-step1-ergonomics");
    std::fs::create_dir_all(ws.path().join("src/context_doc")).unwrap();
    std::fs::write(
        ws.path().join("src/context_doc/mod.rs"),
        "pub fn hello() -> &'static str { \"ok\" }\n",
    )
    .unwrap();
    init_task_row(ws.path(), "step1-ergo");

    let (state, _captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        // Step 0: guess `src/context_doc.rs` -> should hint `src/context_doc/mod.rs`!
        0 => run_js("const missing = io.read({ path: \"src/context_doc.rs\" });"),
        // Step 1: verify directory module hint, then trigger a bracket syntax typo -> should show caret `^`!
        1 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("found directory module: src/context_doc/mod.rs"),
                "NotFound must suggest directory module mod.rs: {tool_out}"
            );
            run_js("({ a: 1 })})")
        }
        // Step 2: verify snippet + `^` caret, then run a heavy finite loop + Ref# alias check!
        2 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("TS_PROGRAM 1:") && tool_out.contains("^"),
                "parse error must include source snippet and caret `^`: {tool_out}"
            );
            run_js(
                "const modFile = io.read({ path: \"src/context_doc/mod.rs\" });\n\
                 let sum = 0;\n\
                 for (let i = 0; i < 5000; i++) sum += i;\n\
                 sum",
            )
        }
        // Step 3: verify finite loop succeeded, bind an alias to `modFile` (same Ref#1) -> must NOT duplicate live handles note,
        // and test React/Vue-style infinite-loop detection (`while(true){}` caught even inside try/catch)!
        3 => {
            let tool_out = last_tool_output(&body);
            assert!(tool_out.contains("12497500"), "finite loop must complete cleanly: {tool_out}");
            run_js("const aliasFile = modFile; try { while (true) {} } catch (e) { 'swallowed'; }")
        }
        // Step 4: verify INFINITE_LOOP_DETECTED fired swiftly AND aliasFile did NOT leak into `live handles added`!
        4 => {
            let tool_out = last_tool_output(&body);
            assert!(
                tool_out.contains("INFINITE_LOOP_DETECTED") && tool_out.contains("state_cycle"),
                "stuck while(true){{}} must be caught by state-cycle detector: {tool_out}"
            );
            assert!(
                !tool_out.contains("aliasFile=Ref#"),
                "alias of existing Ref# must NOT leak into live handles added: {tool_out}"
            );
            completion("Ergonomics & infinite-loop guard verified.")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "step1-ergo".into(),
        "fake".into(),
        "Verify ergonomics and infinite-loop guard".into(),
        10,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        assistant_messages(ws.path(), "step1-ergo"),
        vec!["Ergonomics & infinite-loop guard verified."]
    );
}
