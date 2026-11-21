//! Shared test harness & modular test suites for Context-as-Code v2.
//!
//! Layout:
//! - `step1_protocol`: pure-JS + `return <<<` protocol, `.d.ts`, 3-tier `// =>`, First-Trap Truncation
//! - `step2_virtual`: isolated `@virtual` frames, `Promise.all` concurrency, Stack-Frame GC, receipts
//! - `step3_handles`: `ResourceHandle` `.slice()`/`.grep()`/`.outline()`, Pass-by-Handle, dedup/folding
//! - `step4_durability`: `asset_id` cross-turn recall, `require()` reuse, `ask_user`, `rewind`
//! - `live_models`: opt-in `#[ignore]` real-model integration tests

mod live_models;
mod step1_protocol;
mod step2_virtual;
mod step3_handles;
mod step4_durability;
mod step5_image_generation;
mod step6_attachments;

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

pub(super) struct TestWorkspace(PathBuf);

impl TestWorkspace {
    pub(super) fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn completion(content: &str) -> Value {
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

pub(super) fn run_js(code: &str) -> Value {
    run_js_with_note("", code)
}

pub(super) fn run_js_with_note(note: &str, code: &str) -> Value {
    json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": if note.is_empty() { Value::Null } else { json!(note) },
                "tool_calls": [{
                    "id": format!("call_{}", agent_service::uuid_like()),
                    "type": "function",
                    "function": {
                        "name": "run_js",
                        "arguments": json!({"code": code}).to_string(),
                    }
                }]
            }
        }]
    })
}

pub(super) fn assert_single_run_js_tool(body: &Value) {
    let tools = body["tools"]
        .as_array()
        .expect("request must carry the single run_js shell tool");
    assert_eq!(tools.len(), 1, "must carry ONLY run_js, zero business tool schemas: {tools:?}");
    assert_eq!(tools[0]["function"]["name"], "run_js");
    assert_eq!(body["parallel_tool_calls"], json!(false));
}

pub(super) fn init_task_row(root: &Path, task_id: &str) {
    let conn = agent_service::open_db(root).unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES (?1,'','fake','running',0,0)",
        params![task_id],
    )
    .unwrap();
}

pub(super) async fn spawn_mock_provider(
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

pub(super) fn tool_call_count(root: &Path, task_id: &str, tool_name: &str) -> usize {
    let conn = agent_service::open_db(root).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM agent_task_events WHERE task_id=?1 AND kind='tool/call' AND json_extract(data,'$.name')=?2",
        params![task_id, tool_name],
        |r| r.get::<_, usize>(0),
    )
    .unwrap()
}

pub(super) fn assistant_messages(root: &Path, task_id: &str) -> Vec<String> {
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

pub(super) fn split_stable_prefix(user_content: &str) -> &str {
    user_content
        .split("\n\n// === EPHEMERAL_PEEK")
        .next()
        .unwrap_or(user_content)
}

/// Concatenate all non-system messages (`user`, `assistant`, `tool`) in a wire request
/// for quick content assertions in tests.
pub(super) fn all_messages_text(body: &Value) -> String {
    let mut parts = Vec::new();
    for msg in body["messages"].as_array().into_iter().flatten().skip(1) {
        if let Some(c) = msg["content"].as_str() {
            parts.push(c.to_owned());
        }
        if let Some(calls) = msg["tool_calls"].as_array() {
            for c in calls {
                if let Some(args) = c["function"]["arguments"].as_str() {
                    parts.push(args.to_owned());
                }
            }
        }
    }
    parts.join("\n")
}

/// Return the `content` of the latest `role == "tool"` message in `body["messages"]`.
pub(super) fn last_tool_output(body: &Value) -> &str {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|m| m["role"] == "tool")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
}

/// Assert that `later["messages"]` strictly extends `earlier["messages"]` with 100%
/// byte-identical earlier elements (proving strict Append-Only KV-cache prefix stability).
pub(super) fn assert_prefix_immutable(earlier: &Value, later: &Value) {
    let a = earlier["messages"].as_array().unwrap();
    let b = later["messages"].as_array().unwrap();
    assert!(
        b.len() >= a.len(),
        "later request shrunk ({} < {})",
        b.len(),
        a.len()
    );
    for (idx, (ma, mb)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(
            ma, mb,
            "message[{idx}] mutated across steps, breaking KV-cache prefix!"
        );
    }
}

/// Assert every `assistant(tool_calls)` message is immediately followed by matching
/// `tool(tool_call_id)` messages with exact ID alignment.
pub(super) fn assert_valid_tool_pairing(body: &Value) {
    let msgs = body["messages"].as_array().unwrap();
    let mut i = 0;
    while i < msgs.len() {
        let role = msgs[i]["role"].as_str().unwrap_or("");
        if role == "assistant"
            && let Some(calls) = msgs[i]["tool_calls"].as_array()
            && !calls.is_empty()
        {
            for (offset, call) in calls.iter().enumerate() {
                let expected_id = call["id"].as_str().expect("tool call id");
                let tool_msg = msgs
                    .get(i + 1 + offset)
                    .unwrap_or_else(|| panic!("missing tool response for {expected_id}: {msgs:?}"));
                assert_eq!(tool_msg["role"], "tool");
                assert_eq!(tool_msg["tool_call_id"], expected_id);
            }
            i += 1 + calls.len();
            continue;
        }
        assert_ne!(role, "tool", "orphan tool message at index {i}: {msgs:?}");
        i += 1;
    }
}

pub(super) fn extract_asset_id(text: &str) -> Option<String> {
    let marker = "asset: ";
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .unwrap_or(rest.len());
    Some(rest[..end].to_owned())
}
