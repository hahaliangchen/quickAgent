//! Opt-in (`#[ignore]`) live real-model integration tests for Context-as-Code v2.
//!
//! Run with:
//! `LIVE_PROVIDER=subapi LIVE_MODEL=gpt-6.1-terra LIVE_EFFORT=low cargo test --bin codex-workspace-mcp single_agent::tests::live_models:: -- --ignored --nocapture`

use super::{TestWorkspace, assistant_messages, init_task_row, tool_call_count};
use crate::agent_service;
use crate::single_agent::{Window, run_task};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Live real-model smoke test for Step 1 (opt-in via `--ignored`).
/// Loads `~/.config/grow/providers.yml` (override with `LIVE_PROVIDER` / `LIVE_MODEL`),
/// runs a real code-analysis task on a ~140-line Rust file, and prints the exact
/// trace, tool proxy calls, rejections (if any), and final Markdown reply.
#[tokio::test]
#[ignore]
async fn live_step1_analyze_real_code_file() {
    let ws = TestWorkspace::new("cac-v2-live1");
    std::fs::create_dir_all(ws.path().join("src")).unwrap();

    let mut auth_rs = String::from(
        "//! Lightweight JWT & session token validator.\n\
         use std::collections::HashMap;\n\n\
         pub struct Claims {\n\
         \x20\x20\x20\x20pub sub: String,\n\
         \x20\x20\x20\x20pub role: String,\n\
         \x20\x20\x20\x20pub exp: u64,\n\
         }\n\n",
    );
    for i in 1..=55 {
        auth_rs.push_str(&format!(
            "// helper documentation line {i:03}: standard middleware pipeline notes for auth service\n"
        ));
    }
    auth_rs.push_str(
        "pub fn verify_token(token: &str, secret: &str, now_ts: u64) -> Option<Claims> {\n\
         \x20\x20\x20\x20let parts: Vec<&str> = token.split('.').collect();\n\
         \x20\x20\x20\x20if parts.len() != 3 {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20return None;\n\
         \x20\x20\x20\x20}\n\
         \x20\x20\x20\x20// BUG 1: ignores HMAC signature when header says alg=none\n\
         \x20\x20\x20\x20if parts[0].contains(\"none\") {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20return decode_claims_unchecked(parts[1]);\n\
         \x20\x20\x20\x20}\n\
         \x20\x20\x20\x20// BUG 2: non-constant-time comparison & skips `claims.exp < now_ts` check\n\
         \x20\x20\x20\x20let expected = compute_hmac(parts[0], parts[1], secret);\n\
         \x20\x20\x20\x20if parts[2] == expected {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20let _ = now_ts;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20return decode_claims_unchecked(parts[1]);\n\
         \x20\x20\x20\x20}\n\
         \x20\x20\x20\x20None\n\
         }\n\n",
    );
    for i in 1..=50 {
        auth_rs.push_str(&format!(
            "// audit note {i:03}: internal helper stub for token serialization and logging\n"
        ));
    }
    auth_rs.push_str(
        "fn compute_hmac(_h: &str, _p: &str, _s: &str) -> String { String::new() }\n\
         fn decode_claims_unchecked(_p: &str) -> Option<Claims> { None }\n",
    );
    std::fs::write(ws.path().join("src/auth.rs"), &auth_rs).unwrap();

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
    state.reasoning_effort = Some(
        std::env::var("LIVE_EFFORT").unwrap_or_else(|_| "low".into()),
    );
    println!(
        "=== LIVE STEP1 TEST ===\nprovider={} url={} model={} effort={:?} auth.rs={} chars",
        state.provider_name,
        state.provider_url,
        model,
        state.reasoning_effort,
        auth_rs.chars().count()
    );

    let task_id = "live-step1-auth";
    init_task_row(ws.path(), task_id);

    let cancel = CancellationToken::new();
    let cancel_watchdog = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(240)).await;
        cancel_watchdog.cancel();
    });

    let run_res = run_task(
        state,
        task_id.into(),
        model,
        "请分析 src/auth.rs 中的鉴权逻辑，找出所有安全漏洞，并给出修复示例（只展示修复代码，不要修改文件）。".into(),
        5,
        cancel,
        1,
        false,
    )
    .await;
    println!("\n=== run_task result: {run_res:?} ===");

    let conn = agent_service::open_db(ws.path()).unwrap();
    let mut stmt = conn
        .prepare("SELECT seq, kind, data FROM agent_task_events WHERE task_id=?1 ORDER BY seq")
        .unwrap();
    let events: Vec<(i64, String, String)> = stmt
        .query_map([task_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();

    println!("\n=== NON-DELTA EVENTS ===");
    for (seq, kind, data) in &events {
        if kind == "assistant/delta" {
            continue;
        }
        let preview: String = data.chars().take(700).collect();
        println!("  #{seq} [{kind}] {preview}");
    }

    if let Ok(raw_win) = conn.query_row(
        "SELECT state FROM agent_runtime_windows WHERE task_id=?1 AND scope='main'",
        [task_id],
        |r| r.get::<_, String>(0),
    ) {
        let win: Window = serde_json::from_str(&raw_win).unwrap();
        let wire = win.context_doc.render_conversation_messages();
        println!("\n=== USER #1 TRACE ===\n{}", wire[0]["content"].as_str().unwrap_or(""));
        if let Some(a) = wire.get(1) {
            println!("\n=== FINAL ASSISTANT #1 ===\n{}", a["content"].as_str().unwrap_or(""));
        }
    }

    run_res.expect("live run_task failed");
    assert_eq!(tool_call_count(ws.path(), task_id, "write_file"), 0, "must not write files");
    assert_eq!(tool_call_count(ws.path(), task_id, "edit_file"), 0, "must not edit files");
    assert!(!assistant_messages(ws.path(), task_id).is_empty(), "must produce final assistant reply");
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
    let ws = TestWorkspace::new("cac-v2-live2");
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

    let model_outer = model.clone();
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
            let req_model = req["model"].as_str().unwrap_or("");
            let sys = req["messages"][0]["content"].as_str().unwrap_or("");
            let user = req["messages"][1]["content"].as_str().unwrap_or("");
            let has_tools = req.get("tools").is_some();
            let has_process_dts = sys.contains("declare namespace process");
            println!(
                "\n--- [req #{id}] actor={actor} model={req_model} step={step} node={node_id} scope={scope} tools={has_tools} has_process_dts={has_process_dts} ---"
            );
            assert!(!has_tools, "request #{id} must have zero JSON tools");
            if actor == "virtual" {
                virtual_req_count += 1;
                virtual_frames.insert(scope.to_owned());
                assert!(
                    !has_process_dts,
                    "virtual frame #{id} MUST NOT receive process.* .d.ts!"
                );
                if let Ok(expected_del) = std::env::var("LIVE_DELEGATE_MODEL") {
                    assert!(
                        !req_model.is_empty() && req_model != model_outer,
                        "expected virtual frame #{id} to route to delegate model ({expected_del}), got {req_model}"
                    );
                }
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
        let win: Window = serde_json::from_str(&raw_win).unwrap();
        let wire = win.context_doc.render_conversation_messages();
        let final_user = wire[0]["content"].as_str().unwrap_or("");
        println!("\n=== FINAL OUTER USER #1 TRACE ===\n{final_user}");
        if let Some(a) = wire.get(1) {
            println!("\n=== FINAL ASSISTANT #1 ===\n{}", a["content"].as_str().unwrap_or(""));
        }

        // Stack-Frame GC assertion: raw full file bodies read ONLY inside `@virtual`
        // frames MUST NEVER dump into the outer main trace! (Check middle padding lines
        // rather than line 2, since a child may legitimately quote a 5-line head snippet.)
        assert!(
            !final_user.contains("auth padding note 025")
                && !final_user.contains("db padding note 025")
                && !final_user.contains("cache padding note 025"),
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
