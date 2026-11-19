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

pub(super) fn extract_asset_id(text: &str) -> Option<String> {
    let marker = "asset: ";
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .unwrap_or(rest.len());
    Some(rest[..end].to_owned())
}
