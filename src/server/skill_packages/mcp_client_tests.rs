use super::*;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use tokio_util::task::AbortOnDropHandle;

#[derive(Default)]
struct Peer {
    sessions: AtomicUsize,
    initialized: Mutex<BTreeSet<String>>,
    hang_initialize: AtomicBool,
    hang_call: AtomicBool,
    expire: AtomicBool,
}

async fn http_peer(
    State(peer): State<Arc<Peer>>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Response {
    assert_eq!(message["jsonrpc"], "2.0");
    assert_eq!(headers["accept"], "application/json, text/event-stream");
    assert_eq!(headers["authorization"], "Bearer mock-value");
    let id = &message["id"];
    let session = headers.get("mcp-session-id").and_then(|s| s.to_str().ok());
    let method = message["method"].as_str().unwrap();
    if method != "initialize" {
        assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
        assert!(session.is_some());
    }
    if !matches!(method, "initialize" | "notifications/initialized") {
        assert!(peer.initialized.lock().unwrap().contains(session.unwrap()));
    }
    let rpc = |result| json!({"jsonrpc":"2.0","id":id,"result":result});
    let tool = |name| json!({"name":name,"inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}});
    match method {
        "initialize" => {
            assert!(session.is_none());
            assert_eq!(message["params"]["protocolVersion"], "2025-03-26");
            if peer.hang_initialize.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            let id = format!(
                "test-session-{}",
                peer.sessions.fetch_add(1, Ordering::SeqCst)
            );
            (
                [("mcp-session-id", id)],
                Json(rpc(
                    json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}}}),
                )),
            )
                .into_response()
        }
        "notifications/initialized" => {
            assert!(message.get("id").is_none());
            assert!(
                peer.initialized
                    .lock()
                    .unwrap()
                    .insert(session.unwrap().to_owned())
            );
            StatusCode::ACCEPTED.into_response()
        }
        "tools/list" if message["params"]["cursor"].is_null() => {
            let name = if session == Some("test-session-0") {
                "echo"
            } else {
                "echo-v2"
            };
            Json(rpc(json!({"tools":[tool(name)],"nextCursor":"page-2"}))).into_response()
        }
        "tools/list" => {
            assert_eq!(message["params"]["cursor"], "page-2");
            Json(rpc(json!({"tools":[tool("lookup")]}))).into_response()
        }
        "tools/call" => {
            if peer.hang_call.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if peer.expire.load(Ordering::SeqCst) {
                return StatusCode::NOT_FOUND.into_response();
            }
            let params = &message["params"];
            if params["arguments"]["fail"] == true {
                return Json(
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"bad args"}}),
                )
                .into_response();
            }
            let result = rpc(
                json!({"content":[{"type":"text","text":params["arguments"]["msg"]}],"structuredContent":{"raw_name":params["name"]}}),
            );
            if params["name"] == "lookup" {
                return Json(result).into_response();
            }
            let payload = format!(
                ": heartbeat\r\ndata: {{\"id\":-99}}\r\n\r\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}}\n\ndata: \r\nevent: message\r\ndata: {result}\r\n\r\n"
            );
            // Fragment lines and UTF-8, and never terminate the stream after the result.
            let chunks = payload
                .into_bytes()
                .chunks(7)
                .map(|part| Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(part)))
                .collect::<Vec<_>>();
            let stream = futures::StreamExt::chain(
                futures::stream::iter(chunks),
                futures::stream::pending(),
            );
            (
                [("content-type", "text/event-stream; charset=utf-8")],
                Body::from_stream(stream),
            )
                .into_response()
        }
        _ => panic!("unexpected RPC method {method}"),
    }
}

async fn serve() -> (String, Arc<Peer>, AbortOnDropHandle<()>) {
    let peer = Arc::new(Peer::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/mcp", post(http_peer))
        .with_state(peer.clone());
    (
        url,
        peer,
        AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        })),
    )
}

pub(super) async fn status(registry: &Registry, id: &str, phase: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = registry.mcp_status();
            if let Some(row) = value
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id && r["status"] == phase)
            {
                return row.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{id} did not become {phase}: {}", registry.mcp_status()))
}

#[tokio::test]
async fn mcp_http_json_and_sse_pagination_session_expiry_and_reconnect() {
    let (runtime, catalog) = super::super::test_runtime();
    let registry = &catalog.packages;
    let (url, peer, _server) = serve().await;
    register(
        &runtime,
        registry,
        BTreeMap::from([(
            "demo".into(),
            json!({"url":url,"headers":{"Authorization":"Bearer mock-value"},"call_timeout_ms":1000}),
        )]),
        &BTreeMap::new(),
    );
    let connected = status(registry, "mcp-demo", "connected").await;
    assert_eq!(connected["tool_count"], 2);
    assert_eq!(connected["transport"], "streamable_http");
    let work = Arc::new(crate::tools::Workspace::new(std::env::current_dir().unwrap()).unwrap());
    let old = registry.mcp_tool("mcp_demo__echo").unwrap();
    for name in ["echo", "lookup"] {
        let result = catalog
            .execute(
                work.clone(),
                &format!("mcp_demo__{name}"),
                json!({"msg":"你好"}),
            )
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "你好");
        assert_eq!(result["structuredContent"]["raw_name"], name);
    }
    let ordinary = catalog
        .execute(work.clone(), "mcp_demo__echo", json!({"fail":true}))
        .await
        .unwrap_err();
    assert!(ordinary.to_string().contains("-32602"));
    assert_eq!(registry.mcp_status()[0]["status"], "connected");
    peer.expire.store(true, Ordering::SeqCst);
    assert!(
        catalog
            .execute(work.clone(), "mcp_demo__echo", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("404")
    );
    assert_eq!(registry.mcp_status()[0]["status"], "disconnected");
    assert!(
        registry
            .tools(&BTreeSet::from(["mcp-demo".into()]))
            .is_empty()
    );
    peer.expire.store(false, Ordering::SeqCst);
    reconnect(&runtime, registry, "mcp-demo").await.unwrap();
    assert!(registry.mcp_tool("mcp_demo__echo").is_err());
    let next = catalog
        .execute(
            work.clone(),
            "mcp_demo__echo_v2",
            json!({"msg":"new session"}),
        )
        .await
        .unwrap();
    assert_eq!(next["structuredContent"]["raw_name"], "echo-v2");
    assert!(
        (old.handler)(work, "mcp_demo__echo".into(), json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("stopped")
    );
    set_enabled(&runtime, registry, "mcp-demo", false, false)
        .await
        .unwrap();
    assert_eq!(registry.mcp_status()[0]["enabled"], false);
    assert!(!catalog.contains("mcp_demo__echo_v2"));
    assert!(reconnect(&runtime, registry, "mcp-demo").await.is_err());
    set_enabled(&runtime, registry, "mcp-demo", true, false)
        .await
        .unwrap();
    assert_eq!(registry.mcp_status()[0]["status"], "connected");
    assert_eq!(peer.sessions.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn mcp_startup_isolated_and_deadlines_do_not_hang() {
    let (runtime, catalog) = super::super::test_runtime();
    let registry = &catalog.packages;
    let (slow_url, slow, _slow_server) = serve().await;
    slow.hang_initialize.store(true, Ordering::SeqCst);
    let (url, peer, _server) = serve().await;
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    register(
        &runtime,
        registry,
        BTreeMap::from([
            (
                "slow".into(),
                json!({"url":slow_url,"headers":{"Authorization":"Bearer mock-value"},"initialize_timeout_ms":1500}),
            ),
            (
                "good".into(),
                json!({"url":url,"headers":{"Authorization":"Bearer mock-value"},"call_timeout_ms":100}),
            ),
            (
                "closed".into(),
                json!({"url":format!("http://{address}/mcp"),"initialize_timeout_ms":1000}),
            ),
            ("invalid".into(), json!({"url":1})),
            ("disabled".into(), json!({"url":slow_url,"enabled":false})),
        ]),
        &BTreeMap::new(),
    );
    status(registry, "mcp-good", "connected").await;
    assert_eq!(
        registry
            .mcp_status()
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "mcp-slow")
            .unwrap()["status"],
        "connecting"
    );
    for id in ["mcp-closed", "mcp-invalid", "mcp-slow"] {
        let state = status(registry, id, "disconnected").await;
        assert!(state["last_error"].is_string());
    }
    assert_eq!(
        status(registry, "mcp-disabled", "disconnected").await["enabled"],
        false
    );
    peer.hang_call.store(true, Ordering::SeqCst);
    let work = Arc::new(crate::tools::Workspace::new(std::env::current_dir().unwrap()).unwrap());
    let error = catalog
        .execute(work, "mcp_good__lookup", json!({}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert_eq!(
        status(registry, "mcp-good", "disconnected").await["tool_count"],
        0
    );
    peer.hang_call.store(false, Ordering::SeqCst);
    reconnect(&runtime, registry, "mcp-good").await.unwrap();
    assert_eq!(
        status(registry, "mcp-good", "connected").await["tool_count"],
        2
    );
}
