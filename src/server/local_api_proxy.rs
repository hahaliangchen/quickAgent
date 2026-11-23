use std::{io, time::Duration};

use axum::{
    Json,
    body::{Body, HttpBody},
    extract::Request,
    http::{HeaderMap, HeaderName, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use tokio::{net::TcpStream, time};

pub(super) const ROUTE: &str = "/agent/skill-packages/{id}/local-api/{*path}";
pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(300);

pub(super) fn client(read_timeout: Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(read_timeout)
        .build()
}

const REQUEST_HEADERS: [HeaderName; 9] = [
    header::CONTENT_TYPE,
    header::CONTENT_ENCODING,
    header::CONTENT_LENGTH,
    header::ACCEPT,
    header::RANGE,
    header::IF_RANGE,
    HeaderName::from_static("x-chunk-offset"),
    HeaderName::from_static("x-upload-id"),
    HeaderName::from_static("x-file-name"),
];
const RESPONSE_HEADERS: [HeaderName; 8] = [
    header::CONTENT_TYPE,
    header::CONTENT_ENCODING,
    header::CACHE_CONTROL,
    header::CONTENT_LENGTH,
    header::CONTENT_DISPOSITION,
    header::ACCEPT_RANGES,
    header::CONTENT_RANGE,
    header::ETAG,
];

fn forward_headers(source: &HeaderMap, allowed: &[HeaderName]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for name in allowed {
        // Connection may nominate even a normally end-to-end header.
        let connection_specific = source
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|token| name.as_str().eq_ignore_ascii_case(token.trim()));
        if connection_specific
            || (*name == header::CONTENT_LENGTH && source.contains_key(header::TRANSFER_ENCODING))
        {
            continue;
        }
        for value in source.get_all(name) {
            headers.append(name.clone(), value.clone());
        }
    }
    headers
}

async fn wait_for_local_service(port: u16) -> io::Result<()> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let error = match time::timeout(
            Duration::from_millis(200),
            TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)),
        )
        .await
        {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(error)) => error,
            Err(error) => io::Error::new(io::ErrorKind::TimedOut, error),
        };
        if attempts == 10 {
            return Err(error);
        }
        time::sleep(Duration::from_millis(150)).await;
    }
}

fn request_target(port: u16, uri: &axum::http::Uri) -> Option<reqwest::Url> {
    // Strip the host route on the raw URI: Axum's Path extractor decodes the
    // wildcard, which can turn an encoded question mark into a query delimiter.
    let (_, rest) = uri
        .path_and_query()?
        .as_str()
        .strip_prefix("/agent/skill-packages/")?
        .split_once('/')?;
    let rest = rest.strip_prefix("local-api/")?;
    reqwest::Url::parse(&format!("http://127.0.0.1:{port}/{rest}")).ok()
}

fn bad_gateway(message: &str) -> Response {
    (StatusCode::BAD_GATEWAY, Json(json!({"error": message}))).into_response()
}

pub(super) async fn forward(client: &reqwest::Client, port: u16, request: Request) -> Response {
    let Some(target) = request_target(port, request.uri()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid plugin API path"})),
        )
            .into_response();
    };
    if let Err(error) = wait_for_local_service(port).await {
        tracing::debug!(port, %error, "plugin local service did not become ready");
        return bad_gateway("plugin local service unavailable");
    }

    let (parts, body) = request.into_parts();
    let headers = forward_headers(&parts.headers, &REQUEST_HEADERS);
    let mut request = client.request(parts.method, target).headers(headers);
    if !body.is_end_stream() {
        request = request.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }
    let upstream = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::debug!(port, error = %error.without_url(), "plugin local API request failed");
            return bad_gateway("plugin local API request failed");
        }
    };
    let status = upstream.status();
    let headers = forward_headers(upstream.headers(), &RESPONSE_HEADERS);
    // After headers are sent, a body error must abort the stream, not turn
    // into a successful, truncated download or a late JSON response.
    (status, headers, Body::from_stream(upstream.bytes_stream())).into_response()
}
