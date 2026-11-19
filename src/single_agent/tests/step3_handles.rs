//! Step 3 unit tests: Lazy Resource Handles (`.slice()` / multi-range `.slice([[s,e],...])` /
//! `.grep()` / `.outline()` / `.head()` / `.tail()`), Demand Paging on `> 24KB` files,
//! Pass-by-Handle zero outer expansion + `@virtual` Auto-Dereference, `EPHEMERAL_PEEK`
//! cross-binding deduplication, and same-step inline alias folding.

use super::{TestWorkspace, assistant_messages, completion, init_task_row, tool_call_count};
use crate::agent_service::{self, AgentServiceState};
use crate::single_agent::run_task;
use axum::{Json, Router, routing::post};
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn step3_lazy_handle_demand_paging_slice_multirange_grep_outline_head_tail() {
    let ws = TestWorkspace::new("cac-v2-step3");
    std::fs::create_dir_all(ws.path().join("src")).unwrap();

    // Build a > 28KB file (> 24KB PEEK_FULL_LIMIT_CHARS -> Tier 3 LARGE INDEX!)
    let mut big_rs = String::from(
        "//! Big core engine header line 1.\n\
         //! Big core engine header line 2.\n\
         pub struct EngineCore { pub id: u64 }\n",
    );
    for i in 4..=259 {
        big_rs.push_str(&format!(
            "// filler engine documentation line {i:04}: keeping source file realistically large for demand paging\n"
        ));
    }
    big_rs.push_str(
        "pub fn critical_handler(input: &str) -> bool {\n\
         \x20\x20\x20\x20// SECRET_BUG_MARKER: skips signature check!\n\
         \x20\x20\x20\x20input.starts_with(\"admin\")\n\
         }\n",
    );
    for i in 264..=420 {
        big_rs.push_str(&format!(
            "// trailing engine commentary line {i:04}: additional module documentation notes for testing\n"
        ));
    }
    big_rs.push_str("pub fn shutdown_engine() {}\n");
    assert!(
        big_rs.chars().count() > 28_000,
        "fixture must exceed 24KB Tier-3 threshold, got {} chars",
        big_rs.chars().count()
    );
    std::fs::write(ws.path().join("src/big_engine.rs"), &big_rs).unwrap();
    init_task_row(ws.path(), "step3-paging");

    let seq = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let seq = seq.clone();
            async move {
                let idx = seq.fetch_add(1, Ordering::SeqCst);
                let sys = body["messages"][0]["content"].as_str().unwrap();
                assert!(
                    sys.contains("interface ResourceHandle")
                        && sys.contains("outline(): string")
                        && sys.contains("grep(pattern"),
                    ".d.ts must advertise ResourceHandle methods: {sys}"
                );
                let user = body["messages"][1]["content"].as_str().unwrap();
                Json(match idx {
                    0 => completion("const big = io.read({ path: \"src/big_engine.rs\" });"),
                    1 => {
                        // Tier 3: compact Ref#1 handle in permanent trace + LARGE INDEX (not full 28KB!)
                        assert!(
                            user.contains("[File(\"src/big_engine.rs\") | Ref#1")
                                && user.contains(".slice() .grep() .outline()"),
                            "expected ResourceHandle signature with Ref#1: {user}"
                        );
                        assert!(
                            user.contains("LARGE INDEX")
                                && user.contains("L260: pub fn critical_handler"),
                            "expected Tier-3 structural symbol index in EPHEMERAL_PEEK: {user}"
                        );
                        assert!(
                            !user.contains("filler engine documentation line 0150"),
                            "Tier-3 large file must NOT dump middle filler lines into prompt: {user}"
                        );
                        // Use all 5 built-in zero-I/O handle methods + verify plain objects still reject `.slice()`!
                        completion(
                            "const sym = big.outline();\n\
                             const hits = big.grep(/secret_bug_marker/i, 1);\n\
                             const multi = big.slice([[1, 2], [260, 263]]);\n\
                             const top2 = big.head(2);\n\
                             const bot1 = big.tail(1);\n\
                             let plainThrew = false;\n\
                             try { ({ a: 1 }).slice(1, 2); } catch (e) { plainThrew = true; }",
                        )
                    }
                    2 => {
                        assert!(
                            !user.contains("EPHEMERAL_PEEK"),
                            "small handle projections must all fit inline and wipe EPHEMERAL_PEEK: {user}"
                        );
                        assert!(
                            user.contains("260: pub fn critical_handler")
                                && user.contains("261:     // SECRET_BUG_MARKER")
                                && user.contains("skipped 257 lines")
                                && user.contains("pub fn shutdown_engine()")
                                && user.contains("plainThrew = true"),
                            "expected outline/grep/multi-slice/head/tail/TypeError check results: {user}"
                        );
                        completion("return <<<\n已通过惰性句柄按需分页定位第 260-263 行的 `critical_handler` 漏洞。")
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
        "step3-paging".into(),
        "fake".into(),
        "Inspect src/big_engine.rs".into(),
        6,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    // Only ONE disk read happened — all `.outline()`/`.grep()`/`.slice()`/`.head()`/`.tail()` ran in VM!
    assert_eq!(tool_call_count(ws.path(), "step3-paging", "read_file"), 1);
    assert_eq!(
        assistant_messages(ws.path(), "step3-paging"),
        vec!["已通过惰性句柄按需分页定位第 260-263 行的 `critical_handler` 漏洞。"]
    );
}

#[tokio::test]
async fn step3_pass_by_handle_zero_outer_expansion_child_auto_deref_and_alias_dedup() {
    let ws = TestWorkspace::new("cac-v2-step3");
    std::fs::create_dir_all(ws.path().join("src")).unwrap();

    let pad = |tag: &str| -> String {
        (1..=45)
            .map(|i| format!("// {tag} filler comment {i:03} for realistic file length\n"))
            .collect()
    };
    let a_rs = format!(
        "//! Module A\n// SECRET_RAW_A_ONLY_CHILD_SEES\n{}pub fn alpha() {{}}\n",
        pad("alpha")
    );
    let b_rs = format!(
        "//! Module B\n// SECRET_RAW_B_ONLY_CHILD_SEES\n{}pub fn beta() {{}}\n",
        pad("beta")
    );
    std::fs::write(ws.path().join("src/a.rs"), &a_rs).unwrap();
    std::fs::write(ws.path().join("src/b.rs"), &b_rs).unwrap();
    init_task_row(ws.path(), "step3-pbh");

    let main_seq = Arc::new(AtomicUsize::new(0));
    let seen_children = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_children_ref = seen_children.clone();

    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let main_seq = main_seq.clone();
            let seen_children_ref = seen_children_ref.clone();
            async move {
                let sys = body["messages"][0]["content"].as_str().unwrap();
                let user = body["messages"][1]["content"].as_str().unwrap();
                let is_virtual = sys.contains("isolated single-layer `@virtual`");

                if !is_virtual {
                    let idx = main_seq.fetch_add(1, Ordering::SeqCst);
                    return Json(match idx {
                        0 => completion(
                            "const files = [\n\
                               io.read({ path: \"src/a.rs\" }),\n\
                               io.read({ path: \"src/b.rs\" }),\n\
                             ];\n\
                             const a = files[0];\n\
                             const b = files[1];\n\
                             /** @virtual Audit the passed file handle; return { path, markerLine, summary }. */\n\
                             async function reviewHandle(h) {}\n\
                             const reports = await Promise.all(files.map(f => reviewHandle(f)));\n\
                             const reportsAlias = reports;\n\
                             JSON.stringify(reports);",
                        ),
                        1 => {
                            // 1. Pass-by-Handle: ZERO raw file expansion in outer prompt!
                            assert!(
                                !user.contains("SECRET_RAW_A_ONLY_CHILD_SEES")
                                    && !user.contains("SECRET_RAW_B_ONLY_CHILD_SEES"),
                                "delegated handles MUST NOT expand raw file content in outer EPHEMERAL_PEEK: {user}"
                            );
                            assert!(
                                !user.contains("EPHEMERAL_PEEK"),
                                "all delegated handles suppressed -> outer EPHEMERAL_PEEK should be empty: {user}"
                            );
                            // 2. Compact handle array signature + clean Ref# arg previews in [virtual ...] receipts!
                            assert!(
                                user.contains("files = [Array(2) of Handles: Ref#1(\"src/a.rs\"), Ref#2(\"src/b.rs\")]"),
                                "expected compact handle array signature: {user}"
                            );
                            assert!(
                                user.contains("[virtual reviewHandle(Ref#1(\"src/a.rs\"))")
                                    && user.contains("[virtual reviewHandle(Ref#2(\"src/b.rs\"))"),
                                "expected clean Ref# handle previews in virtual receipts: {user}"
                            );
                            // 3. Same-step duplicate inline alias folding!
                            assert!(
                                user.contains("reportsAlias = (same as reports)")
                                    && user.contains("$_ = (same as reports)"),
                                "expected duplicate inline aliases to fold cleanly: {user}"
                            );
                            // 4. Demand Paging in outer scope on an already-delegated handle (zero extra disk I/O!)
                            completion("const spot = a.grep(\"SECRET_RAW_A\", 0);")
                        }
                        2 => {
                            assert!(
                                user.contains("2: // SECRET_RAW_A_ONLY_CHILD_SEES"),
                                "demand-paged .grep() must materialize only the requested line on demand: {user}"
                            );
                            completion("return <<<\nPass-by-Handle 零拷贝透传与按需分页全部验证通过。")
                        }
                        other => panic!("unexpected main round {other}"),
                    });
                }

                // Inside `@virtual reviewHandle(h)`: verify Auto-Dereference!
                // Child MUST receive clean line-numbered source code in Step 1 EPHEMERAL_PEEK (NOT escaped JSON!)
                assert!(
                    user.contains("EPHEMERAL_PEEK")
                        && user.contains("1: //! Module ")
                        && !user.contains("\"content\": \"//!"),
                    "virtual child must auto-dereference handle into clean line-numbered view: {user}"
                );
                seen_children_ref.lock().unwrap().push(user.to_owned());
                // Child uses handle's built-in `.grep()` directly on `h`!
                Json(completion(
                    "const hit = h.grep(\"SECRET_RAW\", 0);\n\
                     return { path: h.path, markerLine: hit, summary: `audited ${h.path} cleanly with detailed findings and verified no critical security defects` };",
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

    run_task(
        state,
        "step3-pbh".into(),
        "fake".into(),
        "Audit src/a.rs and src/b.rs via Pass-by-Handle".into(),
        6,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(seen_children.lock().unwrap().len(), 2);
    // Only 2 disk reads total (outer `a` and `b`); zero disk re-reads for child auto-deref or outer `.grep()`!
    assert_eq!(tool_call_count(ws.path(), "step3-pbh", "read_file"), 2);
    assert_eq!(
        assistant_messages(ws.path(), "step3-pbh"),
        vec!["Pass-by-Handle 零拷贝透传与按需分页全部验证通过。"]
    );
}

#[tokio::test]
async fn step3_ephemeral_peek_deduplicates_aliases_in_same_step() {
    let ws = TestWorkspace::new("cac-v2-step3");
    let content: String = (1..=60)
        .map(|i| format!("// UNIQUE_LINE_MARKER_{i:03} padding text to exceed 2KB inline threshold\n"))
        .collect();
    std::fs::write(ws.path().join("dup.rs"), &content).unwrap();
    init_task_row(ws.path(), "step3-dedup");

    let seq = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let seq = seq.clone();
            async move {
                let idx = seq.fetch_add(1, Ordering::SeqCst);
                let user = body["messages"][1]["content"].as_str().unwrap();
                Json(match idx {
                    0 => completion(
                        "const f = io.read({ path: \"dup.rs\" });\n\
                         const arr = [f];\n\
                         const map = { primary: f.content };",
                    ),
                    1 => {
                        let count = user.matches("UNIQUE_LINE_MARKER_001").count();
                        assert_eq!(
                            count, 1,
                            "same file referenced by `f`, `arr`, and `map` MUST appear AT MOST ONCE in EPHEMERAL_PEEK (got {count}): {user}"
                        );
                        completion("return <<<\nDeduplicated cleanly.")
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
        "step3-dedup".into(),
        "fake".into(),
        "Read dup.rs with aliases".into(),
        5,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();
}
