//! Step 2 unit tests: isolated, isomorphic `@virtual` stack frames,
//! factual `[virtual ...]` receipts, Stack-Frame GC, and true `Promise.all` concurrency.

use super::{
    TestWorkspace, all_messages_text, assert_single_run_js_tool, assert_valid_tool_pairing,
    assistant_messages, completion, init_task_row, last_tool_output, run_js,
};
use crate::agent_service::{self, AgentServiceState};
use crate::single_agent::run_task;
use axum::{Json, Router, routing::post};
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn step2_virtual_frame_strict_isolation_security_gc_and_factual_receipt() {
    let ws = TestWorkspace::new("cac-v2-step2");
    std::fs::write(
        ws.path().join("mod_a.rs"),
        "// SECRET_INNER_FILE_CONTENT_ONLY_CHILD_SEES\npub fn danger() { val.unwrap(); }\n",
    )
    .unwrap();
    init_task_row(ws.path(), "step2-iso");

    let main_seq = Arc::new(AtomicUsize::new(0));
    let leaf_seq = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let cap_ref = captured.clone();

    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let main_seq = main_seq.clone();
            let leaf_seq = leaf_seq.clone();
            let cap_ref = cap_ref.clone();
            async move {
                cap_ref.lock().unwrap().push(body.clone());
                assert_single_run_js_tool(&body);
                let sys = body["messages"][0]["content"].as_str().unwrap();
                let is_virtual = sys.contains("isolated single-layer `@virtual`");

                if !is_virtual {
                    let m_idx = main_seq.fetch_add(1, Ordering::SeqCst);
                    return Json(match m_idx {
                        0 => run_js(
                            "const outerSecret = \"OUTER_HISTORY_MUST_NOT_LEAK\";\n\
                             const policy = \"NO_UNWRAP\";\n\
                             /** @virtual\n\
                              * @uses policy\n\
                              * Audit target file against policy; return { ok: boolean, summary: string }.\n\
                              */\n\
                             async function auditFile(target) {}\n\
                             const verdict = await auditFile(\"mod_a.rs\");",
                        ),
                        1 => {
                            assert_valid_tool_pairing(&body);
                            let full = all_messages_text(&body);
                            // Stack-Frame GC: raw file content read inside child MUST NOT leak to parent!
                            assert!(
                                !full.contains("SECRET_INNER_FILE_CONTENT_ONLY_CHILD_SEES"),
                                "child's intermediate file read must be GC'd on return: {full}"
                            );
                            // Factual receipt + structured return value MUST be visible in parent tool output!
                            let tool_out = last_tool_output(&body);
                            assert!(
                                tool_out.contains("[virtual auditFile(\"mod_a.rs\")")
                                    && tool_out.contains("read: mod_a.rs")
                                    && tool_out.contains("found unwrap() violating NO_UNWRAP"),
                                "parent must see factual virtual receipt & summary: {tool_out}"
                            );
                            completion("审计完成：mod_a.rs 违反 NO_UNWRAP 规则。")
                        }
                        other => panic!("unexpected main round {other}"),
                    });
                }

                // Inside isolated `@virtual` child frame!
                assert_valid_tool_pairing(&body);
                let user = all_messages_text(&body);
                // 1. Strict isolation assertions:
                assert!(
                    !sys.contains("declare namespace process"),
                    "@virtual .d.ts must NOT expose process/shell: {sys}"
                );
                assert!(
                    !sys.contains("executeTask"),
                    "@virtual must NOT inherit MAIN_PROMPT: {sys}"
                );
                assert!(
                    !user.contains("OUTER_HISTORY_MUST_NOT_LEAK"),
                    "@virtual must NOT see uncaptured outer history/variables: {user}"
                );
                assert!(
                    user.contains("async function auditFile(target)")
                        && user.contains("mod_a.rs")
                        && user.contains("NO_UNWRAP"),
                    "@virtual must see its signature, param, and @uses capture: {user}"
                );

                let l_idx = leaf_seq.fetch_add(1, Ordering::SeqCst);
                Json(match l_idx {
                    // Attempt 0: verify shell call is blocked!
                    0 => run_js("process.run_readonly({ cmd: \"echo hacked\" });"),
                    // Attempt 1: verify nested @virtual is blocked with VIRTUAL_LAYER_LIMIT!
                    1 => {
                        assert!(last_tool_output(&body).contains("COMPILE_ERROR"), "shell must be rejected: {user}");
                        run_js("/** @virtual nested */\nasync function sub() {}")
                    }
                    // Attempt 2: verify plain text outside `run_js` is rejected inside @virtual!
                    2 => {
                        assert!(last_tool_output(&body).contains("VIRTUAL_LAYER_LIMIT"), "{user}");
                        completion("Not allowed in virtual")
                    }
                    // Attempt 3: legitimately read `target` via Rust proxy!
                    3 => {
                        assert!(user.contains("VIRTUAL_TOOL_REQUIRED"), "{user}");
                        run_js("const src = io.read({ path: target }).content;")
                    }
                    // Attempt 4: inspect read file and return structured JSON with summary!
                    4 => {
                        assert!(last_tool_output(&body).contains("SECRET_INNER_FILE_CONTENT_ONLY_CHILD_SEES"), "{user}");
                        run_js(
                            "const bad = src.includes(\"unwrap()\");\n\
                             return { ok: !bad, summary: `found unwrap() violating ${policy}` };",
                        )
                    }
                    other => panic!("unexpected leaf round {other}"),
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    run_task(
        state,
        "step2-iso".into(),
        "fake".into(),
        "Audit mod_a.rs with isolated @virtual".into(),
        6,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        assistant_messages(ws.path(), "step2-iso"),
        vec!["审计完成：mod_a.rs 违反 NO_UNWRAP 规则。"]
    );
    // Verify frontend collapsible `@virtual` card events were emitted cleanly.
    let conn = agent_service::open_db(ws.path()).unwrap();
    let node_done: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_task_events WHERE task_id='step2-iso' AND kind='task/node_done'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(node_done, 1);
}

#[tokio::test]
async fn step2_virtual_promise_all_true_concurrency_file_edit_and_try_catch() {
    let ws = TestWorkspace::new("cac-v2-step2");
    std::fs::write(ws.path().join("a.txt"), "old_a").unwrap();
    std::fs::write(ws.path().join("b.txt"), "old_b").unwrap();
    std::fs::write(ws.path().join("c.txt"), "old_c").unwrap();
    init_task_row(ws.path(), "step2-par");

    // A Barrier(3) inside the mock provider PROVES all 3 Promise.all @virtual
    // frames are executing concurrently in flight (serial execution would deadlock!).
    let barrier = Arc::new(Barrier::new(3));
    let main_seq = Arc::new(AtomicUsize::new(0));

    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let barrier = barrier.clone();
            let main_seq = main_seq.clone();
            async move {
                let sys = body["messages"][0]["content"].as_str().unwrap();
                let is_virtual = sys.contains("isolated single-layer `@virtual`");
                let user = all_messages_text(&body);

                if !is_virtual {
                    let idx = main_seq.fetch_add(1, Ordering::SeqCst);
                    return Json(match idx {
                        0 => run_js(
                            "/** @virtual Edit file to uppercase and return { file, summary }. */\n\
                             async function patchFile(file) {}\n\
                             /** @virtual Always fail with $task_error to test try/catch. */\n\
                             async function failingCheck() {}\n\
                             const patched = await Promise.all([\"a.txt\", \"b.txt\", \"c.txt\"].map(f => patchFile(f)));\n\
                             let caught = \"\";\n\
                             try { await failingCheck(); } catch (e) { caught = String(e.message || e); }",
                        ),
                        1 => {
                            let tool_out = last_tool_output(&body);
                            assert!(
                                tool_out.contains("edited: a.txt")
                                    && tool_out.contains("edited: b.txt")
                                    && tool_out.contains("edited: c.txt"),
                                "parent receipt must show edited files for all 3 concurrent virtual calls: {tool_out}"
                            );
                            assert!(
                                tool_out.contains("DELIBERATE_LEAF_ERROR"),
                                "parent try/catch must capture $task_error cleanly: {tool_out}"
                            );
                            completion("并发处理 3 个文件完成，并正确捕获了虚函数异常。")
                        }
                        other => panic!("unexpected main round {other}"),
                    });
                }

                if user.contains("async function failingCheck()") {
                    return Json(run_js(
                        "return { \"$task_error\": \"DELIBERATE_LEAF_ERROR\" };",
                    ));
                }

                // Inside one of the 3 concurrent `patchFile` frames:
                // Synchronize at the 3-way barrier to prove real concurrency!
                barrier.wait().await;
                Json(run_js(
                    "const cur = io.read({ path: file }).content;\n\
                     io.write({ path: file, content: cur.toUpperCase() });\n\
                     return { file, summary: `uppercased ${file}` };",
                ))
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
            "step2-par".into(),
            "fake".into(),
            "Concurrently patch a.txt, b.txt, c.txt".into(),
            6,
            CancellationToken::new(),
            1,
            false,
        ),
    )
    .await
    .expect("Promise.all barrier timed out — virtual calls were not concurrent!")
    .unwrap();
    server.abort();

    assert_eq!(std::fs::read_to_string(ws.path().join("a.txt")).unwrap(), "OLD_A");
    assert_eq!(std::fs::read_to_string(ws.path().join("b.txt")).unwrap(), "OLD_B");
    assert_eq!(std::fs::read_to_string(ws.path().join("c.txt")).unwrap(), "OLD_C");
    assert_eq!(
        assistant_messages(ws.path(), "step2-par"),
        vec!["并发处理 3 个文件完成，并正确捕获了虚函数异常。"]
    );
}

#[tokio::test]
async fn step2_partial_virtual_fast_path_fallthrough_isolation_and_parallel() {
    let ws = TestWorkspace::new("cac-v2-step2-partial");
    std::fs::write(ws.path().join("clean.rs"), "pub fn safe() -> i32 { 42 }\n").unwrap();
    std::fs::write(
        ws.path().join("bad1.rs"),
        "pub fn one() {\n    // DANGER: unwrap_one\n}\n",
    )
    .unwrap();
    std::fs::write(
        ws.path().join("bad2.rs"),
        "pub fn two() {\n    // DANGER: unwrap_two\n}\n",
    )
    .unwrap();
    init_task_row(ws.path(), "step2-partial");

    // Only 2 files (`bad1.rs` and `bad2.rs`) fall through to AI; `clean.rs` returns
    // early in pure JS with 0 LLM calls. Barrier(2) proves both fall-throughs run concurrently!
    let barrier = Arc::new(Barrier::new(2));
    let main_seq = Arc::new(AtomicUsize::new(0));
    let leaf_calls = Arc::new(AtomicUsize::new(0));
    let leaf_calls_in = leaf_calls.clone();

    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let barrier = barrier.clone();
            let main_seq = main_seq.clone();
            let leaf_calls = leaf_calls_in.clone();
            async move {
                assert_single_run_js_tool(&body);
                let sys = body["messages"][0]["content"].as_str().unwrap();
                let is_virtual = sys.contains("isolated single-layer `@virtual`");
                let user = all_messages_text(&body);

                if !is_virtual {
                    let idx = main_seq.fetch_add(1, Ordering::SeqCst);
                    return Json(match idx {
                        0 => run_js(
                            "const outerSecret = \"OUTER_UNCAPTURED_SECRET\";\n\
                             const policy = \"NO_DANGER\";\n\
                             /** @virtual\n\
                              * @uses policy\n\
                              * Review pre-filtered DANGER matches and return { file, mode, snippet, policy, summary }.\n\
                              */\n\
                             async function auditFile(path) {\n\
                               const f = io.read({ path });\n\
                               const hits = f.grep(\"DANGER\");\n\
                               if (hits === \"(no matches)\") return { file: path, mode: \"js-fast\", snippet: \"\", policy, summary: `${path}: clean` };\n\
                               // Fall through to isolated AI frame with live `f` and `hits`!\n\
                             }\n\
                             const results = await Promise.all([\"clean.rs\", \"bad1.rs\", \"bad2.rs\"].map(auditFile));",
                        ),
                        1 => {
                            let tool_out = last_tool_output(&body);
                            assert!(
                                tool_out.contains("\"mode\":\"js-fast\"")
                                    && tool_out.contains("bad1.rs")
                                    && tool_out.contains("bad2.rs")
                                    && tool_out.contains("unwrap_one")
                                    && tool_out.contains("unwrap_two"),
                                "parent must receive both JS fast-path and concurrent AI fall-through results: {tool_out}"
                            );
                            completion("部分实现虚函数测试通过：clean.rs 走 JS 快路径，bad1.rs 与 bad2.rs 并发进入隔离 AI 子帧。")
                        }
                        other => panic!("unexpected main round {other}"),
                    });
                }

                // Inside isolated partial `@virtual` child frame!
                leaf_calls.fetch_add(1, Ordering::SeqCst);
                assert!(
                    !user.contains("OUTER_UNCAPTURED_SECRET"),
                    "unrelated outer executeTask() variable must NOT leak into partial @virtual frame: {user}"
                );
                assert!(
                    !user.contains("clean.rs"),
                    "fast-path clean.rs must NEVER invoke an AI child frame: {user}"
                );
                assert!(
                    user.contains("precomputed by JS prefix")
                        && user.contains("NO_DANGER")
                        && user.contains("hits"),
                    "child frame must see @uses policy and precomputed JS prefix locals: {user}"
                );

                // Synchronize both fall-through child frames to prove real concurrency!
                barrier.wait().await;

                // Use precomputed `f` (ResourceHandle with `.slice()`!) and `hits` directly without re-reading!
                Json(run_js(
                    "const snippet = f.slice(2, 2);\n\
                     return { file: path, mode: \"ai-fallthrough\", snippet, policy, summary: `${path}: ${hits.trim()}` };",
                ))
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
            "step2-partial".into(),
            "fake".into(),
            "Audit clean.rs, bad1.rs, bad2.rs with partial @virtual".into(),
            6,
            CancellationToken::new(),
            1,
            false,
        ),
    )
    .await
    .expect("Partial @virtual Promise.all barrier timed out — fall-throughs were not concurrent!")
    .unwrap();
    server.abort();

    assert_eq!(
        leaf_calls.load(Ordering::SeqCst),
        2,
        "expected exactly 2 AI child calls (clean.rs must take pure JS fast path)"
    );
    assert_eq!(
        assistant_messages(ws.path(), "step2-partial"),
        vec!["部分实现虚函数测试通过：clean.rs 走 JS 快路径，bad1.rs 与 bad2.rs 并发进入隔离 AI 子帧。"]
    );
}

#[tokio::test]
async fn step2_promise_all_parallel_reads_via_io_promises() {
    let ws = TestWorkspace::new("cac-v2-step2-iop");
    std::fs::write(ws.path().join("p1.txt"), "line1_a\nline2_a\n").unwrap();
    std::fs::write(ws.path().join("p2.txt"), "line1_b\nline2_b\n").unwrap();
    std::fs::write(ws.path().join("p3.txt"), "line1_c\nline2_c\n").unwrap();
    init_task_row(ws.path(), "step2-iop");

    let main_seq = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let main_seq = main_seq.clone();
            async move {
                assert_single_run_js_tool(&body);
                let sys = body["messages"][0]["content"].as_str().unwrap();
                assert!(
                    sys.contains("Promise.all") && sys.contains("io.promises"),
                    "system prompt and .d.ts must remind AI about Promise.all and io.promises"
                );
                let idx = main_seq.fetch_add(1, Ordering::SeqCst);
                Json(match idx {
                    0 => run_js(
                        "const files = await Promise.all([\"p1.txt\", \"p2.txt\", \"p3.txt\"].map(path => io.promises.read({ path })));\n\
                         const summary = files.map(f => `${f.__ref}:${f.slice(2, 2)}`);",
                    ),
                    1 => {
                        let tool_out = last_tool_output(&body);
                        assert!(
                            tool_out.contains("Ref#1:2: line2_a")
                                && tool_out.contains("Ref#2:2: line2_b")
                                && tool_out.contains("Ref#3:2: line2_c"),
                            "concurrent io.promises Promise.all must preserve order, refs, and ResourceHandle methods: {tool_out}"
                        );
                        completion("并发读取 3 个文件成功。")
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
        state,
        "step2-iop".into(),
        "fake".into(),
        "Read p1, p2, p3 with Promise.all".into(),
        4,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        assistant_messages(ws.path(), "step2-iop"),
        vec!["并发读取 3 个文件成功。"]
    );
}
