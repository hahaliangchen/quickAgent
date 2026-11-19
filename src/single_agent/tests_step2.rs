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

fn tool_call_count(root: &Path, task_id: &str, name: &str) -> usize {
    let conn = agent_service::open_db(root).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM agent_task_events WHERE task_id=?1 AND kind='tool/call' AND json_extract(data,'$.name')=?2",
        params![task_id, name],
        |r| r.get::<_, usize>(0),
    )
    .unwrap()
}

/// Live real-model integration test for Step 2 (opt-in via `--ignored`):
/// Concurrent `@virtual` + `Promise.all` security audit of 3 >2KB Rust files,
/// verifying strict isolation, Stack-Frame GC, factual `[virtual ...]` receipts,
/// and printing human-readable parent + child contexts.
///
/// Env overrides:
/// - `LIVE_PROVIDER` / `LIVE_MODEL` / `LIVE_EFFORT` (outer frame)
/// - `LIVE_DELEGATE_PROVIDER` / `LIVE_DELEGATE_MODEL` / `LIVE_DELEGATE_EFFORT` (optional child frame)
#[tokio::test]
#[ignore]
async fn live_step2_virtual_parallel_audit() {
    let ws = TestWorkspace::new();
    std::fs::create_dir_all(ws.path().join("src")).unwrap();

    let pad = |tag: &str, n: usize| -> String {
        (1..=n)
            .map(|i| format!("// {tag} padding note {i:03}: standard internal module commentary\n"))
            .collect()
    };

    let auth_rs = format!(
        "//! JWT & session validator.\n// SENTINEL_ONLY_CHILD_AUTH_SEES\n{}\
         pub fn verify_token(token: &str, secret: &str, now_ts: u64) -> Option<String> {{\n\
         \x20\x20\x20\x20let parts: Vec<&str> = token.split('.').collect();\n\
         \x20\x20\x20\x20if parts.len() != 3 {{ return None; }}\n\
         \x20\x20\x20\x20// Accepts alg=none without checking signature!\n\
         \x20\x20\x20\x20if parts[0].contains(\"none\") {{ return Some(parts[1].to_string()); }}\n\
         \x20\x20\x20\x20let expected = format!(\"{{}}{{}}{{}}\", parts[0], parts[1], secret);\n\
         \x20\x20\x20\x20if parts[2] == expected {{ let _ = now_ts; return Some(parts[1].to_string()); }}\n\
         \x20\x20\x20\x20None\n\
         }}\n{}",
        pad("auth", 30),
        pad("auth-tail", 20),
    );

    let db_rs = format!(
        "//! User query helper.\n// SENTINEL_ONLY_CHILD_DB_SEES\n{}\
         pub fn build_user_query(username: &str, limit_raw: &str) -> (String, usize) {{\n\
         \x20\x20\x20\x20// Raw string interpolation -> SQL injection!\n\
         \x20\x20\x20\x20let sql = format!(\"SELECT id, email FROM users WHERE name = '{{}}'\", username);\n\
         \x20\x20\x20\x20// Unchecked unwrap on untrusted user input -> DoS panic!\n\
         \x20\x20\x20\x20let limit: usize = limit_raw.parse().unwrap();\n\
         \x20\x20\x20\x20(sql, limit)\n\
         }}\n{}",
        pad("db", 30),
        pad("db-tail", 20),
    );

    let cache_rs = format!(
        "//! Safe bounded in-memory TTL cache.\n// SENTINEL_ONLY_CHILD_CACHE_SEES\n\
         use std::collections::HashMap;\n\
         use std::sync::RwLock;\n\
         use std::time::{{Duration, Instant}};\n{}\
         pub struct SafeCache {{\n\
         \x20\x20\x20\x20inner: RwLock<HashMap<String, (String, Instant)>>,\n\
         \x20\x20\x20\x20max_entries: usize,\n\
         }}\n\
         impl SafeCache {{\n\
         \x20\x20\x20\x20pub fn new(max_entries: usize) -> Self {{\n\
         \x20\x20\x20\x20\x20\x20\x20\x20Self {{ inner: RwLock::new(HashMap::new()), max_entries }}\n\
         \x20\x20\x20\x20}}\n\
         \x20\x20\x20\x20pub fn get(&self, key: &str) -> Option<String> {{\n\
         \x20\x20\x20\x20\x20\x20\x20\x20let guard = self.inner.read().ok()?;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20let (val, expires_at) = guard.get(key)?;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20if Instant::now() < *expires_at {{ Some(val.clone()) }} else {{ None }}\n\
         \x20\x20\x20\x20}}\n\
         \x20\x20\x20\x20pub fn insert(&self, key: String, val: String, ttl: Duration) -> bool {{\n\
         \x20\x20\x20\x20\x20\x20\x20\x20let Ok(mut guard) = self.inner.write() else {{ return false; }};\n\
         \x20\x20\x20\x20\x20\x20\x20\x20if guard.len() >= self.max_entries && !guard.contains_key(&key) {{ return false; }}\n\
         \x20\x20\x20\x20\x20\x20\x20\x20guard.insert(key, (val, Instant::now() + ttl));\n\
         \x20\x20\x20\x20\x20\x20\x20\x20true\n\
         \x20\x20\x20\x20}}\n\
         }}\n{}",
        pad("cache", 30),
        pad("cache-tail", 20),
    );

    std::fs::write(ws.path().join("src/auth.rs"), &auth_rs).unwrap();
    std::fs::write(ws.path().join("src/db.rs"), &db_rs).unwrap();
    std::fs::write(ws.path().join("src/cache.rs"), &cache_rs).unwrap();

    let config = crate::ai_proxy::load_config(&crate::ai_proxy::config_path())
        .expect("failed to load ~/.config/grow/providers.yml");
    let mut state = agent_service::tests::test_state(ws.path(), "127.0.0.1:0".parse().unwrap());
    crate::ai_proxy::apply_agent_config(&mut state, &config);

    if let Ok(prov) = std::env::var("LIVE_PROVIDER")
        && let Some(route) = state.provider_routes.get(&prov).cloned()
    {
        state.provider_name = prov;
        state.provider_url = route.url;
        state.api_key = route.api_key;
        state.model_map = route.models;
    }
    let model = std::env::var("LIVE_MODEL").unwrap_or_else(|_| state.default_model.clone());
    let effort = std::env::var("LIVE_EFFORT").unwrap_or_else(|_| "low".into());
    state.reasoning_effort = Some(effort.clone());

    // Explicitly control delegate routing so tests don't accidentally hit an unintended default.
    let del_prov = std::env::var("LIVE_DELEGATE_PROVIDER").ok();
    let del_model = std::env::var("LIVE_DELEGATE_MODEL").ok();
    let del_effort = std::env::var("LIVE_DELEGATE_EFFORT").ok();
    if del_prov.is_some() || del_model.is_some() {
        state.delegate_agent_settings = crate::model_settings::ModelSettings {
            provider: del_prov,
            model: del_model,
            reasoning_effort: del_effort.or_else(|| Some(effort.clone())),
            ..Default::default()
        };
    } else {
        state.delegate_agent_settings = Default::default();
    }

    println!(
        "=== LIVE STEP2 TEST ===\nouter: provider={} model={} effort={:?}\ndelegate: provider={:?} model={:?}\nfiles: auth.rs={}c db.rs={}c cache.rs={}c",
        state.provider_name,
        model,
        state.reasoning_effort,
        state.delegate_agent_settings.provider,
        state.delegate_agent_settings.model,
        auth_rs.chars().count(),
        db_rs.chars().count(),
        cache_rs.chars().count(),
    );

    let task_id = "live-step2-virtual-audit";
    init_task_row(ws.path(), task_id);

    let cancel = CancellationToken::new();
    let cancel_watchdog = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(240)).await;
        cancel_watchdog.cancel();
    });

    let started = std::time::Instant::now();
    let run_res = run_task(
        state,
        task_id.into(),
        model,
        "请对 src/auth.rs、src/db.rs、src/cache.rs 这三个文件做并发安全审计（只读分析，不要修改任何文件）。请定义一个 `@virtual` 异步函数并用 `Promise.all` 并发审查这三个文件，每个子函数返回结构化结果和简洁 `summary`，最后汇总成一份清晰的安全审计报告给用户。".into(),
        6,
        cancel,
        1,
        false,
    )
    .await;
    let elapsed = started.elapsed();
    println!("\n=== run_task result ({elapsed:.2?}): {run_res:?} ===");

    let conn = agent_service::open_db(ws.path()).unwrap();

    // Dump all recorded model requests (both `agent` and `virtual`) for inspection!
    let mut virtual_req_count = 0usize;
    let mut virtual_frames = std::collections::BTreeSet::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT id, actor, step, node_id, metadata, request_json FROM agent_request_contexts WHERE task_id=?1 ORDER BY id",
    ) {
        let rows: Vec<(i64, String, i64, String, String, String)> = stmt
            .query_map([task_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();

        println!("\n=== RECORDED REQUEST CONTEXTS ({} total) ===", rows.len());
        for (id, actor, step, node_id, meta_raw, req_raw) in &rows {
            let meta: Value = serde_json::from_str(meta_raw).unwrap_or_default();
            let req: Value = serde_json::from_str(req_raw).unwrap_or_default();
            let scope = meta["context_scope"].as_str().unwrap_or("");
            let sys = req["messages"][0]["content"].as_str().unwrap_or("");
            let user = req["messages"][1]["content"].as_str().unwrap_or("");
            let has_tools = req.get("tools").is_some();
            let has_process_dts = sys.contains("declare namespace process");
            println!(
                "\n--- [req #{id}] actor={actor} step={step} node={node_id} scope={scope} tools={has_tools} has_process_dts={has_process_dts} ---"
            );
            assert!(!has_tools, "request #{id} must have zero JSON tools");
            if actor == "virtual" {
                virtual_req_count += 1;
                virtual_frames.insert(scope.to_owned());
                assert!(
                    !has_process_dts,
                    "virtual frame #{id} MUST NOT receive process.* .d.ts!"
                );
                // Print full user trace for virtual frames (concise single-frame view)
                println!("{user}");
            } else {
                println!("{user}");
            }
        }
    }

    if let Ok(raw_win) = conn.query_row(
        "SELECT state FROM agent_runtime_windows WHERE task_id=?1 AND scope='main'",
        [task_id],
        |r| r.get::<_, String>(0),
    ) {
        let win: super::Window = serde_json::from_str(&raw_win).unwrap();
        let wire = win.context_doc.render_conversation_messages();
        let final_user = wire[0]["content"].as_str().unwrap_or("");
        println!("\n=== FINAL OUTER USER #1 TRACE ===\n{final_user}");
        if let Some(a) = wire.get(1) {
            println!("\n=== FINAL ASSISTANT #1 ===\n{}", a["content"].as_str().unwrap_or(""));
        }

        // Stack-Frame GC assertion: raw file sentinels read ONLY inside `@virtual`
        // frames MUST NEVER appear in the outer main trace!
        assert!(
            !final_user.contains("SENTINEL_ONLY_CHILD_AUTH_SEES")
                && !final_user.contains("SENTINEL_ONLY_CHILD_DB_SEES")
                && !final_user.contains("SENTINEL_ONLY_CHILD_CACHE_SEES"),
            "child raw file contents leaked into outer trace!"
        );
        assert!(
            final_user.contains("[virtual "),
            "outer trace must contain factual `[virtual ...]` receipts!"
        );
    }

    run_res.expect("live step2 run_task failed");
    assert_eq!(tool_call_count(ws.path(), task_id, "write_file"), 0);
    assert_eq!(tool_call_count(ws.path(), task_id, "edit_file"), 0);
    assert!(
        virtual_frames.len() >= 3,
        "expected >= 3 distinct @virtual frames, got {} (total virtual reqs: {})",
        virtual_frames.len(),
        virtual_req_count
    );
    let replies = assistant_messages(ws.path(), task_id);
    assert_eq!(replies.len(), 1);
    let reply = &replies[0];
    assert!(reply.contains("auth.rs") && reply.contains("db.rs") && reply.contains("cache.rs"));
}
