//! Step 2 unit tests: isolated, isomorphic `@virtual` stack frames,
//! factual `[virtual ...]` receipts, Stack-Frame GC, and true `Promise.all` concurrency.

use super::{
    TestWorkspace, all_messages_text, assert_single_run_js_tool, assert_valid_tool_pairing,
    assistant_messages, completion, init_task_row, last_tool_output, run_js, tool_call_count,
};
use crate::server::agent_service::{self, AgentServiceState};
use crate::runtime::single_agent::run_task;
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
                    "@virtual must not expose process capabilities: {sys}"
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
                    // Sync and async process access both fail without dispatch.
                    0 => run_js("process.exec_command({ cmd: \"printf denied\" });"),
                    1 => {
                        let output = last_tool_output(&body);
                        assert!(output.contains("COMPILE_ERROR") || output.contains("ReferenceError") || output.contains("RUNTIME ERROR"), "process must not reach host dispatch: {output}");
                        run_js("await process.promises.get_project_process({});")
                    }
                    2 => {
                        let output = last_tool_output(&body);
                        assert!(output.contains("COMPILE_ERROR") || output.contains("ReferenceError") || output.contains("RUNTIME ERROR"), "process must not reach host dispatch: {output}");
                        run_js("/** @virtual nested */\nasync function sub() {}")
                    }
                    // Attempt 3: nested @virtual is rejected, then ordinary file work continues.
                    3 => {
                        assert!(last_tool_output(&body).contains("VIRTUAL_LAYER_LIMIT"), "{user}");
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
    for name in ["exec_command", "get_project_process"] {
        assert_eq!(tool_call_count(ws.path(), "step2-iso", name), 0, "leaf dispatched {name}");
    }
}

#[tokio::test]
async fn step2_virtual_worker_inherits_ban_execution_for_process_tools() {
    let ws = TestWorkspace::new("cac-v2-step2-virtual-ban-execution");
    init_task_row(ws.path(), "step2-virtual-ban-execution");
    let main_seq = Arc::new(AtomicUsize::new(0));
    let leaf_seq = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let main_seq = main_seq.clone();
            let leaf_seq = leaf_seq.clone();
            async move {
                assert_single_run_js_tool(&body);
                let sys = body["messages"][0]["content"].as_str().unwrap();
                let is_virtual = sys.contains("isolated single-layer `@virtual`");
                if is_virtual {
                    let leaf_round = leaf_seq.fetch_add(1, Ordering::SeqCst);
                    assert!(!sys.contains("declare namespace process"), "parent ban_execution must remove process from the child surface: {sys}");
                    if leaf_round == 0 {
                        return Json(run_js("process.exec_command({ cmd: \"printf denied\" });"));
                    }
                    let user = all_messages_text(&body);
                    assert!(
                        user.contains("COMPILE_ERROR")
                            || user.contains("ReferenceError")
                            || user.contains("RUNTIME ERROR"),
                        "the denied process call must be rejected inside the child: {user}"
                    );
                    return Json(run_js("return { denied: true, summary: \"execution boundary inherited\" };"));
                }
                let index = main_seq.fetch_add(1, Ordering::SeqCst);
                Json(match index {
                    0 => run_js("const result = await host.delegate({ task: \"Run a process capability probe\" }); return { result };"),
                    1 => {
                        let output = last_tool_output(&body);
                        assert!(output.contains("execution boundary inherited"), "child must inherit execution denial and finish: {output}");
                        completion("子 worker 继承了父级执行权限边界。")
                    }
                    other => panic!("unexpected main round {other}"),
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_task(
            state,
            "step2-virtual-ban-execution".into(),
            "fake".into(),
            "Delegate a process probe while execution is denied".into(),
            4,
            CancellationToken::new(),
            1,
            true,
        ),
    )
    .await
    .expect("ban_execution inheritance test timed out")
    .unwrap();
    server.abort();
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
                            tool_out.contains("Ref#1:2|line2_a")
                                && tool_out.contains("Ref#2:2|line2_b")
                                && tool_out.contains("Ref#3:2|line2_c"),
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

#[tokio::test]
async fn step2_host_delegate_parallel_partial_pass_by_handle_and_no_nesting() {
    let ws = TestWorkspace::new("cac-v2-step2-delegate");
    std::fs::write(
        ws.path().join("audit.rs"),
        "// DELEGATE_MARKER\npub fn audit() { value.unwrap(); }\n",
    )
    .unwrap();
    init_task_row(ws.path(), "step2-delegate");

    let barrier = Arc::new(Barrier::new(2));
    let main_seq = Arc::new(AtomicUsize::new(0));
    let leaf_calls = Arc::new(AtomicUsize::new(0));
    let leaf_calls_ref = leaf_calls.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let barrier = barrier.clone();
            let main_seq = main_seq.clone();
            let leaf_calls = leaf_calls_ref.clone();
            async move {
                assert_single_run_js_tool(&body);
                let sys = body["messages"][0]["content"].as_str().unwrap();
                let user = all_messages_text(&body);
                let is_virtual = sys.contains("isolated single-layer `@virtual`");
                if is_virtual {
                    leaf_calls.fetch_add(1, Ordering::SeqCst);
                    assert!(!sys.contains("function delegate"), "leaf declarations must omit host.delegate: {sys}");
                    assert!(user.contains("delegate-a") || user.contains("delegate-b"), "delegate task must reach the child: {user}");
                    assert!(user.contains("audit.rs"), "ResourceHandle context must reach the child: {user}");
                    barrier.wait().await;
                    return Json(if user.contains("delegate-a") {
                        run_js(
                            "const line = file.slice(1, 1);\nreturn { name, marker: line, tools, summary: `${name}: checked ${file.path}` };",
                        )
                    } else {
                        run_js("return { name: \"delegate-b\", summary: \"delegate-b checked audit.rs\" };")
                    });
                }
                let index = main_seq.fetch_add(1, Ordering::SeqCst);
                Json(match index {
                    0 => run_js(
                        "const file = io.read({ path: \"audit.rs\" });\n\
                         async function inspect(name) {\n\
                           if (name === \"fast\") return { mode: \"js-fast\", summary: \"deterministic precheck\" };\n\
                           if (name === \"delegate-a\") return await host.delegate({ name, task: \"Inspect audit.rs and return structured evidence.\", context: { name, file, tools: \"target-files\" } });\n\
                           return await host.delegate({ name, task: \"Inspect audit.rs independently as delegate-b and return structured evidence.\", context: { name, file, tools: \"target-files\" } });\n\
                         }\n\
                         const [fast, a, b, check] = await Promise.all([\n\
                           inspect(\"fast\"),\n\
                           inspect(\"delegate-a\"),\n\
                           inspect(\"delegate-b\"),\n\
                           io.promises.workspace_info()\n\
                         ]);\n\
                         return { fast, a, b, check };",
                    ),
                    1 => {
                        let output = last_tool_output(&body);
                        assert!(output.contains("delegate-a") && output.contains("delegate-b"), "both delegate receipts must return: {output}");
                        assert!(output.contains("target-files"), "ordinary context key `tools` must be bound in the child: {output}");
                        assert!(output.contains("workspace_root"), "mixed structured promise must return: {output}");
                        completion("host.delegate 并发、部分实现快路径和句柄传递通过。")
                    }
                    other => panic!("unexpected main round {other}"),
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        run_task(
            state,
            "step2-delegate".into(),
            "fake".into(),
            "Audit audit.rs with two concurrent host.delegate workers and a structured workspace query".into(),
            6,
            CancellationToken::new(),
            1,
            false,
        ),
    )
    .await
    .expect("host.delegate barrier timed out")
    .unwrap();
    server.abort();

    assert_eq!(leaf_calls.load(Ordering::SeqCst), 2, "two delegate frames must be in flight");
    assert_eq!(tool_call_count(ws.path(), "step2-delegate", "read_file"), 1, "children must reuse the parent handle");
    assert_eq!(
        assistant_messages(ws.path(), "step2-delegate"),
        vec!["host.delegate 并发、部分实现快路径和句柄传递通过。"]
    );
}

#[tokio::test]
async fn step2_host_delegate_context_collision_keys_are_preserved_without_overwriting_hosts() {
    let ws = TestWorkspace::new("cac-v2-step2-delegate-context-collisions");
    std::fs::write(ws.path().join("audit.rs"), "fn audit_marker() {}\n").unwrap();
    init_task_row(ws.path(), "step2-delegate-context-collisions");
    let main_seq = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let main_seq = main_seq.clone();
            async move {
                assert_single_run_js_tool(&body);
                let sys = body["messages"][0]["content"].as_str().unwrap();
                let is_virtual = sys.contains("isolated single-layer `@virtual`");
                if is_virtual {
                    assert!(all_messages_text(&body).contains("audit.rs"), "delegate context must reach child: {body}");
                    return Json(run_js(
                        "const direct = lib.slice(1, 1);\n\
                         const viaContext = context.lib.slice(1, 1);\n\
                         const read = io.read({ path: \"audit.rs\" });\n\
                         return {\n\
                           host: context.host,\n\
                           io: context.io,\n\
                           returned: context[\"return\"],\n\
                           tools,\n\
                           contextTools: context.tools,\n\
                           direct,\n\
                           viaContext,\n\
                           read: read.content,\n\
                           ioStillHostLibrary: typeof io.read === \"function\",\n\
                           summary: \"collision context preserved\"\n\
                         };",
                    ));
                }
                let index = main_seq.fetch_add(1, Ordering::SeqCst);
                Json(match index {
                    0 => run_js(
                        "const file = io.read({ path: \"audit.rs\" });\n\
                         const delegated = await host.delegate({\n\
                           name: \"collision-check\",\n\
                           task: \"Read the supplied handle and verify every context field.\",\n\
                           context: { host: \"host-value\", io: \"io-value\", return: \"return-value\", tools: \"tools-value\", lib: file }\n\
                         });\n\
                         const emptyTask = await host.delegate({ task: \"   \" }).catch(error => ({ code: error.code, message: error.message }));\n\
                         const malformed = await host.delegate({ name: \"missing-task\" }).catch(error => ({ code: error.code, message: error.message }));\n\
                         return { delegated, emptyTask, malformed };",
                    ),
                    1 => {
                        let output = last_tool_output(&body);
                        assert!(output.contains("host-value") && output.contains("io-value") && output.contains("return-value"), "collision keys must remain readable through context: {output}");
                        assert!(output.contains("tools-value") && output.contains("audit_marker"), "safe and handle context values must survive: {output}");
                        assert!(output.contains("ioStillHostLibrary") && output.contains("true"), "context.io must not overwrite io: {output}");
                        assert!(output.contains("INVALID_DELEGATE"), "empty task and missing task must remain invalid: {output}");
                        completion("host.delegate context 冲突键已安全归并，宿主库和句柄均可读取。")
                    }
                    other => panic!("unexpected main round {other}"),
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_task(
            state,
            "step2-delegate-context-collisions".into(),
            "fake".into(),
            "Preserve colliding host.delegate context keys".into(),
            4,
            CancellationToken::new(),
            1,
            false,
        ),
    )
    .await
    .expect("delegate context collision test timed out")
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn step2_host_delegate_has_no_artificial_20_step_cap() {
    let ws = TestWorkspace::new("cac-v2-step2-delegate-unbounded");
    init_task_row(ws.path(), "step2-delegate-unbounded");
    let main_seq = Arc::new(AtomicUsize::new(0));
    let leaf_steps = Arc::new(AtomicUsize::new(0));
    let leaf_steps_ref = leaf_steps.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let main_seq = main_seq.clone();
            let leaf_steps = leaf_steps_ref.clone();
            async move {
                assert_single_run_js_tool(&body);
                let sys = body["messages"][0]["content"].as_str().unwrap();
                let is_virtual = sys.contains("isolated single-layer `@virtual`");
                if is_virtual {
                    let step = leaf_steps.fetch_add(1, Ordering::SeqCst) + 1;
                    let user = all_messages_text(&body);
                    assert!(user.contains("target-files"), "worker context must remain available: {user}");
                    assert!(
                        !user.contains("VIRTUAL WORKER BUDGET"),
                        "unbounded worker must not inject step-limit countdowns: {user}"
                    );
                    let code = if step < 22 {
                        "const observation = tools; observation;"
                    } else {
                        "return { ok: true, tools, steps: 22, summary: `finished after ${tools}` };"
                    };
                    return Json(run_js(code));
                }
                let index = main_seq.fetch_add(1, Ordering::SeqCst);
                Json(match index {
                    0 => run_js(
                        "const result = await host.delegate({ name: \"deep_check\", task: \"Inspect the supplied target list and return structured evidence.\", context: { tools: \"target-files\" } });\nreturn { result };",
                    ),
                    1 => {
                        let output = last_tool_output(&body);
                        assert!(output.contains("target-files"), "final worker result must survive past 20 steps: {output}");
                        assert!(!output.contains("VIRTUAL_MAX_ROUNDS"), "worker must not fail at 20 steps: {output}");
                        completion("子 worker 不受 20 步硬上限限制，正常交付最终结果。")
                    }
                    other => panic!("unexpected main round {other}"),
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = agent_service::tests::test_state(ws.path(), listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        run_task(
            state,
            "step2-delegate-unbounded".into(),
            "fake".into(),
            "Delegate a multi-step check without an artificial 20-step ceiling".into(),
            4,
            CancellationToken::new(),
            1,
            false,
        ),
    )
    .await
    .expect("delegate unbounded test timed out")
    .unwrap();
    server.abort();
    assert_eq!(leaf_steps.load(Ordering::SeqCst), 22, "worker should run past 20 steps and finish cleanly");
}
