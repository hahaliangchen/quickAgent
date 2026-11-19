//! Step 2 verification tests: isolated, isomorphic `@virtual` stack frames,
//! factual `[virtual ...]` receipts, Stack-Frame GC, and true `Promise.all` concurrency.

use super::run_task;
use crate::agent_service::{self, AgentServiceState};
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
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;

struct TestWorkspace(PathBuf);

impl TestWorkspace {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cac-v2-step2-{}", agent_service::uuid_like()));
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

fn completion(content: &str) -> Value {
    json!({
        "choices": [{
            "finish_reason": "stop",
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

fn assistant_messages(root: &Path, task_id: &str) -> Vec<String> {
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

#[tokio::test]
async fn step2_virtual_frame_strict_isolation_security_gc_and_factual_receipt() {
    let ws = TestWorkspace::new();
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
                assert!(body.get("tools").is_none(), "zero JSON schemas everywhere");
                let sys = body["messages"][0]["content"].as_str().unwrap();
                let is_virtual = sys.contains("isolated single-layer `@virtual`");

                if !is_virtual {
                    let m_idx = main_seq.fetch_add(1, Ordering::SeqCst);
                    return Json(match m_idx {
                        0 => completion(
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
                            let user = body["messages"][1]["content"].as_str().unwrap();
                            // Stack-Frame GC: raw file content read inside child MUST NOT leak to parent!
                            assert!(
                                !user.contains("SECRET_INNER_FILE_CONTENT_ONLY_CHILD_SEES"),
                                "child's intermediate file read must be GC'd on return: {user}"
                            );
                            // Factual receipt + structured return value MUST be visible in parent `// =>`!
                            assert!(
                                user.contains("[virtual auditFile(\"mod_a.rs\")")
                                    && user.contains("read: mod_a.rs")
                                    && user.contains("found unwrap() violating NO_UNWRAP"),
                                "parent must see factual virtual receipt & summary: {user}"
                            );
                            completion("return <<<\n审计完成：mod_a.rs 违反 NO_UNWRAP 规则。")
                        }
                        other => panic!("unexpected main round {other}"),
                    });
                }

                // Inside isolated `@virtual` child frame!
                let user = body["messages"][1]["content"].as_str().unwrap();
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
                    0 => completion("process.run_readonly({ cmd: \"echo hacked\" });"),
                    // Attempt 1: verify nested @virtual is blocked with VIRTUAL_LAYER_LIMIT!
                    1 => {
                        assert!(user.contains("COMPILE_ERROR"), "shell must be rejected: {user}");
                        completion("/** @virtual nested */\nasync function sub() {}")
                    }
                    // Attempt 2: verify `return <<<` is rejected inside @virtual!
                    2 => {
                        assert!(user.contains("VIRTUAL_LAYER_LIMIT"), "{user}");
                        completion("return <<<\nNot allowed in virtual")
                    }
                    // Attempt 3: legitimately read `target` via Rust proxy!
                    3 => {
                        assert!(user.contains("VIRTUAL_RETURN_REQUIRED"), "{user}");
                        completion("const src = io.read({ path: target }).content;")
                    }
                    // Attempt 4: inspect read file and return structured JSON with summary!
                    4 => {
                        assert!(user.contains("SECRET_INNER_FILE_CONTENT_ONLY_CHILD_SEES"), "{user}");
                        completion(
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
}

#[tokio::test]
async fn step2_virtual_promise_all_true_concurrency_file_edit_and_try_catch() {
    let ws = TestWorkspace::new();
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
                let user = body["messages"][1]["content"].as_str().unwrap();

                if !is_virtual {
                    let idx = main_seq.fetch_add(1, Ordering::SeqCst);
                    return Json(match idx {
                        0 => completion(
                            "/** @virtual Edit file to uppercase and return { file, summary }. */\n\
                             async function patchFile(file) {}\n\
                             /** @virtual Always fail with $task_error to test try/catch. */\n\
                             async function failingCheck() {}\n\
                             const patched = await Promise.all([\"a.txt\", \"b.txt\", \"c.txt\"].map(f => patchFile(f)));\n\
                             let caught = \"\";\n\
                             try { await failingCheck(); } catch (e) { caught = String(e.message || e); }",
                        ),
                        1 => {
                            assert!(
                                user.contains("edited: a.txt")
                                    && user.contains("edited: b.txt")
                                    && user.contains("edited: c.txt"),
                                "parent receipt must show edited files for all 3 concurrent virtual calls: {user}"
                            );
                            assert!(
                                user.contains("DELIBERATE_LEAF_ERROR"),
                                "parent try/catch must capture $task_error cleanly: {user}"
                            );
                            completion("return <<<\n并发处理 3 个文件完成，并正确捕获了虚函数异常。")
                        }
                        other => panic!("unexpected main round {other}"),
                    });
                }

                if user.contains("async function failingCheck()") {
                    return Json(completion(
                        "return { \"$task_error\": \"DELIBERATE_LEAF_ERROR\" };",
                    ));
                }

                // Inside one of the 3 concurrent `patchFile` frames:
                // Synchronize at the 3-way barrier to prove real concurrency!
                barrier.wait().await;
                Json(completion(
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
