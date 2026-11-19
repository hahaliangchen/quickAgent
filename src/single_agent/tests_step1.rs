//! Step 1 end-to-end verification tests for Context-as-Code v2:
//!
//! Proves on a real code-analysis task (real Rust file on disk + real Rust JS VM
//! + real Rust `host_tools` proxy + mock Axum HTTP provider):
//! 1. Zero JSON tool schemas on the wire, replaced by `.d.ts` in `system`.
//! 2. Model emits pure raw JavaScript (no JSON tool_calls, no `<run>` XML tags)
//!    that traps the Rust JS VM and proxies into Rust `io.read` (`read_file`).
//! 3. Three-tier feedback + `EPHEMERAL_PEEK` (阅后即焚):
//!    - Medium 200-line file shows compact signature in permanent `// =>` and
//!      expands with line numbers in `EPHEMERAL_PEEK`.
//!    - Next step slices exact lines in JS memory (0 disk re-reads!), pinning
//!      the ground-truth snippet permanently in `// =>` while `EPHEMERAL_PEEK`
//!      vanishes completely.
//!    - Byte-exact KV-cache prefix holds across every step AND across turns.
//! 4. Anti-cheating lock: slicing `> 2KB` refuses to inline into permanent `// =>`.
//! 5. `return <<<` Markdown final reply containing ```` ```js ```` example code
//!    is never executed as JS and is delivered verbatim to the user.
//! 6. Per-turn fresh VM: Turn 2 cannot reference Turn 1's variables and can
//!    cleanly redeclare the same names.

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
use tokio_util::sync::CancellationToken;

struct TestWorkspace(PathBuf);

impl TestWorkspace {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cac-v2-step1-{}", agent_service::uuid_like()));
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

fn split_stable_prefix(user_content: &str) -> &str {
    user_content
        .split("\n\n// === EPHEMERAL_PEEK")
        .next()
        .unwrap_or(user_content)
}

#[tokio::test]
async fn step1_analyze_code_pure_js_dts_rust_proxy_peek_projection_and_return_heredoc() {
    let ws = TestWorkspace::new();
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
            // 1. Verify ZERO JSON tool schemas & presence of `.d.ts` declarations!
            assert!(body.get("tools").is_none(), "must NOT send JSON tool schemas");
            assert!(body.get("tool_choice").is_none());
            let sys = body["messages"][0]["content"].as_str().unwrap();
            assert!(sys.contains("declare namespace io"), "system prompt must contain .d.ts declarations: {sys}");
            assert!(sys.contains("function read("), "io.read .d.ts signature must be present");

            // Emit pure raw JS (NO JSON, NO <run> tags) that proxies into Rust `read_file`!
            completion("const authFile = io.read({ path: \"src/auth.rs\" });")
        }
        1 => {
            // 2. Verify permanent `// =>` has compact signature while `EPHEMERAL_PEEK` has full numbered file!
            let user_msg = body["messages"][1]["content"].as_str().unwrap();
            let stable = split_stable_prefix(user_msg);
            assert!(
                stable.contains("// => authFile = [live File(\"src/auth.rs\")"),
                "permanent trace must keep compact signature: {stable}"
            );
            assert!(
                !stable.contains("decode_without_verify"),
                "full file body must NOT pollute permanent trace: {stable}"
            );
            assert!(
                user_msg.contains("EPHEMERAL_PEEK")
                    && user_msg.contains("61: pub fn verify_token")
                    && user_msg.contains("63:     decode_without_verify(token).is_ok()"),
                "EPHEMERAL_PEEK must expose full line-numbered file for 1 step: {user_msg}"
            );

            // Model projects ONLY lines 61..64 via JS from live heap (0 disk re-reads!)
            completion("const verifyFn = authFile.content.split(\"\\n\").slice(60, 64).join(\"\\n\");")
        }
        2 => {
            // 3. Verify EPHEMERAL_PEEK vanished completely and ONLY the 4-line slice is pinned in `// =>`!
            let user_msg = body["messages"][1]["content"].as_str().unwrap();
            assert!(
                !user_msg.contains("EPHEMERAL_PEEK"),
                "EPHEMERAL_PEEK must be wiped after pinning a <=2KB slice: {user_msg}"
            );
            assert!(
                !user_msg.contains("padding comment line 001"),
                "old 120-line file content must be completely gone from prompt: {user_msg}"
            );
            assert!(
                user_msg.contains("decode_without_verify(token).is_ok()"),
                "pinned 4-line slice must live permanently in `// =>`: {user_msg}"
            );

            // Emit final Markdown reply containing a ```js example code block via `return <<<`
            // (must NOT be executed as JS!)
            completion(
                "return <<<\n分析完成：`src/auth.rs` 第 61-64 行的 `verify_token` 使用了 `decode_without_verify`，跳过了签名和过期校验。\n\n示例修复思路（展示代码，不应执行）：\n```js\nio.write({ path: \"MUST_NOT_EXECUTE.txt\", content: \"boom\" });\n```",
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
        assert!(req.get("tools").is_none(), "req #{i} must have zero tools");
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
    let ws = TestWorkspace::new();
    let big = "line_data_0123456789\n".repeat(180); // ~3,780 chars (> 2KB INLINE_LIMIT_CHARS)
    std::fs::write(ws.path().join("big.txt"), &big).unwrap();
    init_task_row(ws.path(), "step1-lock");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        // Turn 1, step 1: read big.txt
        0 => completion("const f = io.read({ path: \"big.txt\" });"),
        // Turn 1, step 2: try to cheat by slicing the entire > 2KB content
        1 => completion("const cheatAll = f.content.slice(0, 3500);"),
        // Turn 1, step 3: verify anti-cheating lock kept >2KB out of permanent `// =>`, then finish via native JS `return`!
        2 => {
            let user_msg = body["messages"][1]["content"].as_str().unwrap();
            let stable = split_stable_prefix(user_msg);
            assert!(
                stable.contains("> 2KB not inlined"),
                "anti-cheating lock must prevent >2KB slice from dumping into permanent trace: {stable}"
            );
            completion("return \"Turn 1 done cleanly.\";")
        }
        // Turn 2, step 1: try referencing Turn 1's `f` -> MUST fail compile (fresh VM!)
        3 => {
            let msgs = body["messages"].as_array().unwrap();
            assert_eq!(msgs.len(), 4, "[system, user#1, assistant#1, user#2]");
            assert_eq!(msgs[2], json!({"role": "assistant", "content": "Turn 1 done cleanly."}));
            assert!(!msgs[1]["content"].as_str().unwrap().contains("EPHEMERAL_PEEK"));
            completion("const leaked = f.content;")
        }
        // Turn 2, step 2: verify compile error reported in EPHEMERAL_PEEK, redeclare `const f` cleanly!
        4 => {
            let user2 = body["messages"][3]["content"].as_str().unwrap();
            assert!(user2.contains("COMPILE_ERROR"), "Turn 2 must reject Turn 1 variable: {user2}");
            completion("const f = \"fresh-turn-2\";\nreturn `Rebound cleanly: ${f}`;")
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
    let ws = TestWorkspace::new();
    std::fs::write(ws.path().join("sample.rs"), "pub fn bad() { unsafe_call(); }\n").unwrap();
    init_task_row(ws.path(), "step1-term");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        // Round 0: external tool call (`io.read`) + premature `return <<<`
        // Because `external_tool_calls > 0`, host MUST execute `io.read`, discard
        // the premature reply, and continue to Round 1!
        0 => completion(
            "const f = io.read({ path: \"sample.rs\" });return <<<\nPREMATURE_MUST_BE_DISCARDED\n>>>",
        ),
        // Round 1: pure in-memory JS slicing (`external_tool_calls == 0`) + `return <<<`
        // Host MUST execute slice into permanent `// =>` AND immediately finish the turn!
        1 => {
            let user_msg = body["messages"][1]["content"].as_str().unwrap();
            assert!(
                user_msg.contains("unsafe_call()"),
                "round 1 must see real file content from round 0: {user_msg}"
            );
            assert!(
                user_msg.contains("discarded the premature `return <<<`"),
                "host note must explain discarded premature reply: {user_msg}"
            );
            completion(
                "const pinned = f.content.trim();\nreturn <<<\nVerified: `sample.rs` calls `unsafe_call()`.\n<<<",
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

/// Live real-model smoke test for Step 1 (opt-in via `--ignored`).
/// Loads `~/.config/grow/providers.yml` (override with `LIVE_PROVIDER` / `LIVE_MODEL`),
/// runs a real code-analysis task on a ~140-line Rust file, and prints the exact
/// trace, tool proxy calls, rejections (if any), and final Markdown reply.
#[tokio::test]
#[ignore]
async fn live_step1_analyze_real_code_file() {
    let ws = TestWorkspace::new();
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
        let win: super::Window = serde_json::from_str(&raw_win).unwrap();
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
