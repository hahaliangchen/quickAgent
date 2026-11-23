use super::*;
use std::{io, time::Duration};

use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{Method, header},
    routing::any,
};
use futures::{StreamExt, stream};
use tokio::{net::TcpSocket, sync::Notify, task::JoinHandle, time};

struct Server {
    port: u16,
    task: JoinHandle<()>,
}

impl Server {
    async fn spawn(app: Router) -> Self {
        Self::from_listener(TcpListener::bind(("127.0.0.1", 0)).await.unwrap(), app)
    }

    fn from_listener(listener: TcpListener, app: Router) -> Self {
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { port, task }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn browser() -> Client {
    local_api_proxy::client(Duration::from_secs(10)).unwrap()
}

async fn host(upstream_port: u16, read_timeout: Duration) -> Server {
    let (runtime, catalog) = crate::server::skill_packages::test_runtime();
    catalog
        .packages
        .set_test_local_service_port("code-flow", upstream_port);
    let state = AiProxyState {
        config: Default::default(),
        config_path: Default::default(),
        settings_write: Default::default(),
        client: browser(),
        local_api_client: local_api_proxy::client(read_timeout).unwrap(),
        runtime,
    };
    Server::spawn(
        Router::new()
            .route(local_api_proxy::ROUTE, skill_local_api_route())
            .with_state(state),
    )
    .await
}

const PREFIX: &str = "/agent/skill-packages/code-flow/local-api";

#[tokio::test]
async fn local_api_streams_24_mib_through_the_real_route_in_both_directions() {
    const CHUNK: usize = 64 * 1024;
    const N: usize = 24 * 1024 * 1024 / CHUNK;
    const TOTAL: usize = N * CHUNK;
    let upload_observed = Arc::new(Notify::new());
    let release_download = Arc::new(Notify::new());
    let app = Router::new().route(
        "/api/echo",
        any({
            let upload_observed = upload_observed.clone();
            let release_download = release_download.clone();
            move |request: Request| async move {
                assert_eq!(request.method(), Method::POST);
                let headers = request.headers();
                assert_eq!(
                    headers[header::CONTENT_LENGTH].to_str().unwrap(),
                    TOTAL.to_string()
                );
                assert_eq!(headers[header::CONTENT_TYPE], "application/octet-stream");
                assert_eq!(headers["x-chunk-offset"], "0");
                let mut incoming = request.into_body().into_data_stream();
                let first = incoming.next().await.unwrap().unwrap();
                assert!(!first.is_empty());
                upload_observed.notify_one();
                let rest = stream::once(async move {
                    release_download.notified().await;
                    incoming
                })
                .flatten();
                let body = stream::once(async move { Ok(first) }).chain(rest);
                let headers = [
                    (header::CONTENT_LENGTH, TOTAL.to_string()),
                    (header::CONTENT_TYPE, "application/octet-stream".into()),
                    (
                        header::CONTENT_DISPOSITION,
                        "attachment; filename=test.bin".into(),
                    ),
                    (header::CACHE_CONTROL, "no-store".into()),
                ];
                (headers, Body::from_stream(body)).into_response()
            }
        }),
    );
    let upstream = Server::spawn(app).await;
    let host = host(upstream.port, local_api_proxy::READ_TIMEOUT).await;
    let upload = stream::iter(0..N).then(move |i| {
        let observed = upload_observed.clone();
        async move {
            if i == 1 {
                // The producer cannot complete until the upstream reads the first chunk.
                observed.notified().await;
            }
            Ok::<_, io::Error>(Bytes::from(vec![(i % 251) as u8; CHUNK]))
        }
    });
    time::timeout(Duration::from_secs(10), async {
        let response = browser()
            .post(host.url(&format!("{PREFIX}/api/echo")))
            .header(header::CONTENT_LENGTH, TOTAL)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header("x-chunk-offset", "0")
            .body(reqwest::Body::wrap_stream(upload))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.content_length(), Some(TOTAL as u64));
        assert_eq!(
            response.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=test.bin"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let mut download = response.bytes_stream();
        let first = download.next().await.unwrap().unwrap();
        assert!(!first.is_empty());
        // The download cannot complete until the downstream receives the first chunk.
        release_download.notify_one();
        let mut offset = 0;
        let mut check = |bytes: Bytes| {
            for byte in bytes {
                assert_eq!(byte, ((offset / CHUNK) % 251) as u8, "offset {offset}");
                offset += 1;
            }
        };
        check(first);
        while let Some(chunk) = download.next().await {
            check(chunk.unwrap());
        }
        assert_eq!(offset, TOTAL);
    })
    .await
    .expect("upload or download buffered before forwarding the first chunk");
}

fn reserved_port() -> TcpSocket {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    socket
}

#[tokio::test]
async fn local_api_waits_for_listen_without_replaying_a_post() {
    let socket = reserved_port();
    let host = host(
        socket.local_addr().unwrap().port(),
        local_api_proxy::READ_TIMEOUT,
    )
    .await;
    let client = browser();
    let health = client.get(host.url(&format!("{PREFIX}/api/health"))).send();
    let upload = client
        .post(host.url(&format!("{PREFIX}/api/ping")))
        .body("sent once")
        .send();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let start = async {
        time::sleep(Duration::from_millis(300)).await;
        Server::from_listener(
            socket.listen(32).unwrap(),
            Router::new()
                .route("/api/health", get(|| async { "ready" }))
                .route(
                    "/api/ping",
                    post({
                        let calls = calls.clone();
                        move |body: String| async move {
                            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            body
                        }
                    }),
                ),
        )
    };
    let (_upstream, health, upload) = tokio::join!(start, health, upload);
    assert_eq!(
        health
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap(),
        "ready"
    );
    assert_eq!(
        upload
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap(),
        "sent once"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn local_api_unreachable_service_is_a_json_502_and_unknown_package_is_404() {
    let socket = reserved_port();
    let host = host(
        socket.local_addr().unwrap().port(),
        local_api_proxy::READ_TIMEOUT,
    )
    .await;
    let client = browser();
    let response = client
        .get(host.url(&format!("{PREFIX}/api/health")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"],
        "plugin local service unavailable"
    );
    let response = client
        .get(host.url("/agent/skill-packages/missing/local-api/api/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn local_api_preserves_ranges_and_raw_uri_and_filters_headers() {
    let upstream = Server::spawn(Router::new().fallback(|request: Request| async move {
        assert_eq!(
            request.uri().path_and_query().unwrap().as_str(),
            "/api/a%3Fb%2Fc?raw=%26%23"
        );
        let headers = request.headers();
        assert_eq!(headers[header::RANGE], "bytes=2-4");
        assert_eq!(headers[header::IF_RANGE], r#""v1""#);
        assert_eq!(headers["x-upload-id"], "u1");
        for name in [
            "authorization",
            "cookie",
            "x-file-name",
            "connection",
            "upgrade",
        ] {
            assert!(!headers.contains_key(name), "unexpected header {name}");
        }
        assert!(
            headers[header::HOST]
                .to_str()
                .unwrap()
                .starts_with("127.0.0.1:")
        );
        (
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_RANGE, "bytes 2-4/9"),
                (header::ACCEPT_RANGES, "bytes"),
                (header::ETAG, r#""v1""#),
                (header::SET_COOKIE, "not-for-the-host=1"),
                (header::CONNECTION, "cache-control"),
                (header::CACHE_CONTROL, "public"),
            ],
            "cde",
        )
    }))
    .await;
    let host = host(upstream.port, local_api_proxy::READ_TIMEOUT).await;
    let response = browser()
        .get(host.url(&format!("{PREFIX}/api/a%3Fb%2Fc?raw=%26%23")))
        .header(header::RANGE, "bytes=2-4")
        .header(header::IF_RANGE, r#""v1""#)
        .header(header::AUTHORIZATION, "Bearer test")
        .header(header::COOKIE, "test=1")
        .header(header::CONNECTION, "X-File-Name")
        .header("x-file-name", "private-name.bin")
        .header("x-upload-id", "u1")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-4/9");
    assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(response.headers()[header::ETAG], r#""v1""#);
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    assert!(!response.headers().contains_key(header::CACHE_CONTROL));
    assert_eq!(response.text().await.unwrap(), "cde");
}

#[tokio::test]
async fn local_api_handles_empty_methods_and_an_unknown_length_body() {
    let upstream = Server::spawn(Router::new().fallback(|request: Request| async move {
        let (parts, body) = request.into_parts();
        let body = axum::body::to_bytes(body, 128).await.unwrap();
        match (parts.method, parts.uri.path()) {
            (Method::HEAD, _) => {
                assert!(body.is_empty());
                assert!(!parts.headers.contains_key(header::TRANSFER_ENCODING));
                "head probe".into_response()
            }
            (Method::DELETE, "/empty") => {
                assert!(body.is_empty());
                assert!(!parts.headers.contains_key(header::TRANSFER_ENCODING));
                StatusCode::NO_CONTENT.into_response()
            }
            (Method::DELETE, "/body") => {
                assert!(!parts.headers.contains_key(header::CONTENT_LENGTH));
                assert_eq!(body, "chunked delete");
                body.into_response()
            }
            _ => StatusCode::BAD_REQUEST.into_response(),
        }
    }))
    .await;
    let host = host(upstream.port, local_api_proxy::READ_TIMEOUT).await;
    let client = browser();
    let response = client
        .head(host.url(&format!("{PREFIX}/head")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
    assert!(response.bytes().await.unwrap().is_empty());
    let response = client
        .delete(host.url(&format!("{PREFIX}/empty")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = client
        .delete(host.url(&format!("{PREFIX}/body")))
        .body(reqwest::Body::wrap_stream(stream::iter([
            Ok::<_, io::Error>("chunked "),
            Ok("delete"),
        ])))
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "chunked delete");
}

#[tokio::test]
async fn local_api_does_not_follow_redirects() {
    let redirect_target =
        Server::spawn(Router::new().fallback(|| async { StatusCode::IM_A_TEAPOT })).await;
    let location = redirect_target.url("/different-service");
    let upstream = Server::spawn(
        Router::new()
            .fallback(move || async move { (StatusCode::FOUND, [(header::LOCATION, location)]) }),
    )
    .await;
    let host = host(upstream.port, local_api_proxy::READ_TIMEOUT).await;
    let response = browser()
        .get(host.url(&format!("{PREFIX}/redirect")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert!(!response.headers().contains_key(header::LOCATION));
}

#[tokio::test]
async fn local_api_times_out_before_headers_and_reports_a_stalled_download_as_error() {
    let upstream = Server::spawn(
        Router::new()
            .route(
                "/headers",
                get(|| async { std::future::pending::<StatusCode>().await }),
            )
            .route(
                "/body",
                get(|| async {
                    let first =
                        stream::once(async { Ok::<_, io::Error>(Bytes::from_static(b"first")) });
                    Body::from_stream(first.chain(stream::pending()))
                }),
            ),
    )
    .await;
    let host = host(upstream.port, Duration::from_millis(200)).await;
    let client = browser();
    let response = client
        .get(host.url(&format!("{PREFIX}/headers")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"],
        "plugin local API request failed"
    );
    let response = client
        .get(host.url(&format!("{PREFIX}/body")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut chunks = response.bytes_stream();
    assert_eq!(chunks.next().await.unwrap().unwrap(), "first");
    assert!(chunks.next().await.unwrap().is_err());
}
