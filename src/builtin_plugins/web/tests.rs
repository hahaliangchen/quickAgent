use super::*;
use axum::{
    Json, Router,
    extract::{Path as RoutePath, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("web-tests-{}", crate::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Upstream {
    calls: AtomicUsize,
    status: Mutex<StatusCode>,
    requests: Mutex<Vec<(String, HeaderMap, Value)>>,
}
struct Server {
    client: WebClient,
    state: Arc<Upstream>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server() -> Server {
    async fn handler(
        State(s): State<Arc<Upstream>>,
        RoutePath(method): RoutePath<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        s.calls.fetch_add(1, Ordering::SeqCst);
        s.requests
            .lock()
            .unwrap()
            .push((method.clone(), headers, body));
        let data = if method == "search" {
            json!({"results":[{"title":"原始文档","url":"https://example.com/docs","snippet":"官方说明","date":"2026-11-10","content":"not requested: do not deliver this field"}]})
        } else {
            json!({"title":"Unicode page","content":"开头🙂\n中段é\n结尾"})
        };
        let status = *s.status.lock().unwrap();
        (
            status,
            Json(if status.is_success() {
                data
            } else {
                json!({"error":{"message":"test-secret must never reach the model"}})
            }),
        )
    }
    let state = Arc::new(Upstream {
        calls: AtomicUsize::new(0),
        status: Mutex::new(StatusCode::OK),
        requests: Mutex::new(Vec::new()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/{method}", post(handler))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server {
        client: WebClient {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            base,
            provider: "independent-search-key".into(),
            key: "test-secret".into(),
        },
        state,
        task,
    }
}

#[tokio::test]
async fn search_sends_a_host_credential_and_returns_only_citable_search_fields() {
    let s = server().await;
    let result = s
        .client
        .search(SearchArgs {
            query: "资料 site:example.com".into(),
            count: 5,
        })
        .await
        .unwrap();
    let requests = s.state.requests.lock().unwrap();
    assert_eq!(requests[0].0, "search");
    assert_eq!(requests[0].1["authorization"], "Bearer test-secret");
    assert_eq!(
        requests[0].2,
        json!({"query":"资料 site:example.com","count":5,"need_content":false})
    );
    assert_eq!(result["results"][0]["url"], "https://example.com/docs");
    assert_eq!(result["results"][0]["date"], "2026-11-10");
    assert!(result["results"][0].get("content").is_none());
    assert!(!result.to_string().contains("test-secret"));
    assert!(result["retrieved_at"].as_str().is_some());
}

#[tokio::test]
async fn fetch_pages_preserve_unicode_identity_and_never_repeat_the_network_request() {
    let f = Fixture::new();
    let s = server().await;
    let mut args = FetchArgs {
        url: "https://example.com/page".into(),
        cursor: None,
        max_chars: 3,
    };
    let mut page = store(&f.0, s.client.fetch(&args.url).await.unwrap(), args.max_chars).await.unwrap();
    assert_eq!(page["content"], "开头🙂");
    assert_eq!(page["complete"], false);
    let id = page["snapshot_id"].clone();
    let at = page["retrieved_at"].clone();
    let mut all = page["content"].as_str().unwrap().to_owned();
    // A saved page remains readable after the service becomes unavailable.
    *s.state.status.lock().unwrap() = StatusCode::SERVICE_UNAVAILABLE;
    while let Some(cursor) = page["next_cursor"].as_str() {
        args.cursor = Some(cursor.to_owned());
        page = read_page(&f.0, &args, &args.url).unwrap();
        assert_eq!(page["snapshot_id"], id);
        assert_eq!(page["retrieved_at"], at);
        all.push_str(page["content"].as_str().unwrap());
    }
    assert_eq!(all, "开头🙂\n中段é\n结尾");
    assert_eq!(page["complete"], true);
    assert_eq!(s.state.calls.load(Ordering::SeqCst), 1);
    args.cursor = Some(
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&Cursor {
                snapshot: id.as_str().unwrap().into(),
                offset: 0,
            })
            .unwrap(),
        ),
    );
    assert!(
        read_page(&f.0, &args, "https://other.example/")
            .unwrap_err()
            .to_string()
            .contains("different URL")
    );
    std::fs::write(
        f.0.join(format!(
            ".codex-workspace-mcp/web/{}.json",
            id.as_str().unwrap()
        )),
        b"changed",
    )
    .unwrap();
    assert!(
        read_page(&f.0, &args, &args.url)
            .unwrap_err()
            .to_string()
            .contains("WEB_SNAPSHOT_CHANGED")
    );
}

#[tokio::test]
async fn invalid_queries_and_urls_do_not_start_requests_and_errors_never_echo_keys() {
    let s = server().await;
    for (query, count) in [
        (" ".into(), 5),
        ("x".repeat(129), 5),
        ("valid".into(), 0),
        ("valid".into(), 21),
    ] {
        assert!(s.client.search(SearchArgs { query, count }).await.is_err());
    }
    assert_eq!(s.state.calls.load(Ordering::SeqCst), 0);
    for url in [
        "file:///etc/passwd",
        "https://user:password@example.com/",
        "not a url",
    ] {
        assert!(public_url(url).is_err());
    }
    for (status, code) in [
        (401, "WEB_AUTH_ERROR"),
        (402, "WEB_BALANCE_ERROR"),
        (404, "WEB_PAGE_UNAVAILABLE"),
        (429, "WEB_TEMPORARY_ERROR"),
        (503, "WEB_TEMPORARY_ERROR"),
    ] {
        *s.state.status.lock().unwrap() = StatusCode::from_u16(status).unwrap();
        let error = s
            .client
            .search(SearchArgs {
                query: "valid".into(),
                count: 5,
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(code), "{error}");
        assert!(!error.contains("test-secret"));
    }
}

#[tokio::test]
async fn oversized_upstream_body_fails_without_a_successful_prefix() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/search",
                post(|| async { "x".repeat(MAX_RESPONSE_BYTES + 1) }),
            ),
        )
        .await
        .unwrap();
    });
    let client = WebClient {
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        base,
        provider: "p".into(),
        key: "test-secret".into(),
    };
    let error = client
        .search(SearchArgs {
            query: "valid".into(),
            count: 5,
        })
        .await
        .unwrap_err()
        .to_string();
    server.abort();
    assert!(error.contains("WEB_RESPONSE_TOO_LARGE"));
}

#[tokio::test]
async fn native_library_is_discoverable_compiles_and_reads_pages_in_read_only_mode() {
    use crate::plugin_runtime::{Plugin, PluginContext, PluginRuntime, ROOT};
    struct Services(Arc<crate::builtin_plugins::ToolCatalog>);
    impl Plugin for Services {
        fn id(&self) -> &str {
            "web-test-services"
        }
        fn start(&self, ctx: &PluginContext<'_>) -> Result<()> {
            ctx.provide("tools", self.0.clone())?;
            ctx.provide("skill-packages", self.0.packages.clone())
        }
    }
    let f = Fixture::new();
    let s = server().await;
    let args = FetchArgs {
        url: "https://example.com/docs".into(),
        cursor: None,
        max_chars: 3,
    };
    let first = store(&f.0, s.client.fetch(&args.url).await.unwrap(), args.max_chars).await.unwrap();
    let runtime = PluginRuntime::new();
    let catalog = Arc::new(crate::builtin_plugins::ToolCatalog::default());
    let parent = runtime.mount(ROOT, Services(catalog.clone())).unwrap();
    runtime
        .mount(
            parent,
            crate::skill_packages::PackagePlugin(package().unwrap()),
        )
        .unwrap();
    let definitions = catalog.packages.tools(&catalog.packages.enabled());
    let libraries =
        crate::agent_program::libraries::catalog(&definitions, &catalog.packages.catalog());
    assert_eq!(libraries["web"]["search"], "web__search");
    assert_eq!(libraries["web"]["fetch"], "web__fetch");
    crate::agent_program::compile_with_libraries(
        "const web = require('web'); console.log(await web.search({query:'docs'}));",
        libraries,
    )
    .unwrap();
    assert!(catalog.packages.allows(
        "web__fetch",
        crate::agent_service::PermissionMode::ReadOnly,
        false
    ));
    let result = catalog
        .execute(
            Arc::new(crate::tools::Workspace::new(&f.0).unwrap()),
            "web__fetch",
            json!({"url":args.url,"cursor":first["next_cursor"],"max_chars":3}),
        )
        .await
        .unwrap();
    assert_eq!(result["char_offset"], 3);
    assert_eq!(result["snapshot_id"], first["snapshot_id"]);
    assert_eq!(s.state.calls.load(Ordering::SeqCst), 1);
    for invalid in ["../../secret", "!not-base64"] {
        let result = execute(
            Arc::new(crate::tools::Workspace::new(&f.0).unwrap()),
            "fetch",
            json!({"url":"https://example.com/","cursor":invalid}),
            s.client.http.clone(),
            f.0.join("missing-config"),
        )
        .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("WEB_INVALID_CURSOR")
        );
    }
    runtime.shutdown().unwrap();
}
