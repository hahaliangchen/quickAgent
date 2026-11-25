//! Local-only HTTP reachability probes, independent of Agent-managed processes.

use std::{
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use reqwest::{Url, redirect::Policy};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::lookup_host;

pub const TOOLS: &[&str] = &["http_probe", "http_request"];
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 30_000;
pub const REUSE_WINDOW_MS: u64 = 30_000;
const MAX_BODY_SUMMARY_BYTES: usize = 2_048;

#[derive(Debug, Deserialize)]
struct ProbeRequest {
    url: String,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    reason: Option<String>,
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

#[derive(Debug)]
struct ProbeFailure {
    kind: &'static str,
    message: String,
}

struct ProbeResponse {
    status: u16,
    redirect_target: Option<String>,
    body_summary: Option<String>,
    body_summary_truncated: bool,
}

pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}

pub fn check_key(args: &Value) -> String {
    let raw = args["url"].as_str().unwrap_or("").trim();
    let normalized = raw.split_once('#').map(|(url, _)| url).unwrap_or(raw);
    format!("http-probe:{normalized}")
}

pub async fn execute(args: &Value) -> Result<Value> {
    let request: ProbeRequest = serde_json::from_value(args.clone())?;
    anyhow::ensure!(request.url.len() <= 4_096, "url must be at most 4096 bytes");
    anyhow::ensure!(
        request
            .reason
            .as_ref()
            .is_none_or(|reason| reason.chars().count() <= 300),
        "reason must be at most 300 characters"
    );
    let started = Instant::now();
    let sampled_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let timeout_ms = request.timeout_ms.clamp(100, MAX_TIMEOUT_MS);
    let result = probe_url(&request.url, Duration::from_millis(timeout_ms), started).await;
    let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

    let mut value = match result {
        Ok(response) => json!({
            "sampled_at": sampled_at,
            "url": request.url,
            "elapsed_ms": elapsed_ms,
            "reachable": true,
            "http_status": response.status,
            "error_kind": Value::Null,
            "error_message": Value::Null,
            "redirect_target": response.redirect_target,
            "body_summary": response.body_summary,
            "body_summary_truncated": response.body_summary_truncated,
        }),
        Err(error) => json!({
            "sampled_at": sampled_at,
            "url": request.url,
            "elapsed_ms": elapsed_ms,
            "reachable": false,
            "http_status": Value::Null,
            "error_kind": error.kind,
            "error_message": error.message,
            "redirect_target": Value::Null,
            "body_summary": Value::Null,
            "body_summary_truncated": false,
        }),
    };
    value["check_key"] = json!(check_key(args));
    value["check_passed"] = json!(
        value["reachable"] == true
            && value["http_status"]
                .as_u64()
                .is_some_and(|status| (200..300).contains(&status))
    );
    value["timeout_ms"] = json!(timeout_ms);
    value["reuse_window_ms"] = json!(REUSE_WINDOW_MS);
    if let Some(reason) = request.reason.filter(|reason| !reason.trim().is_empty()) {
        value["reason"] = json!(reason.trim());
    }
    Ok(value)
}

async fn probe_url(
    url_text: &str,
    timeout: Duration,
    started: Instant,
) -> std::result::Result<ProbeResponse, ProbeFailure> {
    let mut url = Url::parse(url_text).map_err(|error| ProbeFailure {
        kind: "invalid_url",
        message: format!("The URL could not be parsed: {error}"),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ProbeFailure {
            kind: "unsupported_scheme",
            message: "Only local HTTP and HTTPS URLs can be probed.".to_owned(),
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ProbeFailure {
            kind: "credentials_not_allowed",
            message: "Credentials in the URL are not allowed.".to_owned(),
        });
    }
    url.set_fragment(None);
    let host = url
        .host_str()
        .ok_or_else(|| ProbeFailure {
            kind: "invalid_url",
            message: "The URL does not contain a host.".to_owned(),
        })?
        .to_owned();
    let port = url.port_or_known_default().ok_or_else(|| ProbeFailure {
        kind: "invalid_url",
        message: "The URL does not specify a usable HTTP port.".to_owned(),
    })?;
    if crate::tools::process_sandbox::is_host_control_port(port) {
        return Err(ProbeFailure {
            kind: "control_port_blocked",
            message: "Probing the host control service port is not allowed.".to_owned(),
        });
    }

    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(timeout);
    // Url serializes IPv6 hosts with brackets; strip them before parsing an IP literal.
    let host_ip = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host_ip.parse::<IpAddr>() {
        if !ip.is_loopback() {
            return Err(ProbeFailure {
                kind: "non_local_address",
                message: "Only loopback IP addresses are allowed.".to_owned(),
            });
        }
    } else {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(timeout_failure(timeout));
        }
        let resolved = tokio::time::timeout(remaining, lookup_host((host.as_str(), port)))
            .await
            .map_err(|_| timeout_failure(timeout))?
            .map_err(|error| ProbeFailure {
                kind: "dns",
                message: format!("The local hostname '{host}' could not be resolved: {error}"),
            })?
            .collect::<Vec<SocketAddr>>();
        if resolved.is_empty() {
            return Err(ProbeFailure {
                kind: "dns",
                message: format!("The local hostname '{host}' resolved to no addresses."),
            });
        }
        if resolved.iter().any(|address| !address.ip().is_loopback()) {
            return Err(ProbeFailure {
                kind: "non_local_address",
                message: format!(
                    "The hostname '{host}' resolved to a non-loopback address; the request was blocked."
                ),
            });
        }
        builder = builder.resolve_to_addrs(&host, &resolved);
    }

    let client = builder.build().map_err(|error| ProbeFailure {
        kind: "request",
        message: format!("Could not prepare the local HTTP request: {error}"),
    })?;
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(timeout_failure(timeout));
    }
    let mut response = tokio::time::timeout(remaining, client.get(url.clone()).send())
        .await
        .map_err(|_| timeout_failure(timeout))?
        .map_err(|error| request_failure(&error))?;
    let status = response.status().as_u16();
    let redirect_target = if response.status().is_redirection() {
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(|location| {
                url.join(location)
                    .map(|target| target.to_string())
                    .unwrap_or_else(|_| location.to_owned())
            })
    } else {
        None
    };
    let content_length = response.content_length();
    let textual = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_textual_content_type);
    let mut body = Vec::new();
    let mut body_finished = false;
    if textual {
        while body.len() < MAX_BODY_SUMMARY_BYTES {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining.min(Duration::from_millis(250)), response.chunk())
                .await
            {
                Ok(Ok(Some(chunk))) => {
                    let take = (MAX_BODY_SUMMARY_BYTES - body.len()).min(chunk.len());
                    body.extend_from_slice(&chunk[..take]);
                    if take < chunk.len() {
                        break;
                    }
                }
                Ok(Ok(None)) => {
                    body_finished = true;
                    break;
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
    }
    let body_summary = (!body.is_empty()).then(|| String::from_utf8_lossy(&body).into_owned());
    let body_summary_truncated = body_summary.is_some()
        && (!body_finished || content_length.is_some_and(|length| length > body.len() as u64));
    Ok(ProbeResponse {
        status,
        redirect_target,
        body_summary,
        body_summary_truncated,
    })
}

fn is_textual_content_type(value: &str) -> bool {
    let value = value
        .split(';')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase();
    value.starts_with("text/")
        || value == "application/json"
        || value.ends_with("+json")
        || value.contains("xml")
        || value.contains("javascript")
}

fn timeout_failure(timeout: Duration) -> ProbeFailure {
    ProbeFailure {
        kind: "timeout",
        message: format!(
            "The local HTTP probe timed out after {} ms; no HTTP response was received.",
            timeout.as_millis()
        ),
    }
}

fn request_failure(error: &reqwest::Error) -> ProbeFailure {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    let mut io_kind = None;
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            io_kind = Some(io.kind());
        }
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = std::error::Error::source(cause);
    }
    let lower = message.to_ascii_lowercase();
    let kind = if error.is_timeout() {
        "timeout"
    } else if io_kind == Some(std::io::ErrorKind::ConnectionRefused)
        || lower.contains("connection refused")
        || lower.contains("actively refused")
    {
        "connection_refused"
    } else if lower.contains("dns")
        || lower.contains("resolve")
        || lower.contains("name or service not known")
    {
        "dns"
    } else if lower.contains("tls")
        || lower.contains("certificate")
        || lower.contains("rustls")
        || lower.contains("handshake")
    {
        "tls"
    } else if error.is_connect() {
        "connection"
    } else {
        "request"
    };
    let diagnostic = match kind {
        "timeout" => "The local HTTP probe timed out before receiving an HTTP response.".to_owned(),
        "connection_refused" => {
            "The local endpoint refused the connection; no HTTP response was received.".to_owned()
        }
        "dns" => format!("Local hostname resolution failed: {message}"),
        "tls" => format!("The local HTTPS handshake failed: {message}"),
        _ => format!("The local HTTP request failed before receiving an HTTP response: {message}"),
    };
    ProbeFailure {
        kind,
        message: diagnostic,
    }
}

pub async fn execute_request(args: &Value) -> Result<Value> {
    use reqwest::{Method, header::{HeaderMap, HeaderName, HeaderValue}};
    let url_text = args["url"].as_str().context("url is required")?;
    anyhow::ensure!(url_text.len() <= 4096, "url must be at most 4096 bytes");
    let method_text = args["method"].as_str().unwrap_or("GET").to_ascii_uppercase();
    anyhow::ensure!(matches!(method_text.as_str(), "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE"), "unsupported HTTP method");
    let method = Method::from_bytes(method_text.as_bytes())?;
    let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(15_000).clamp(100, 60_000);
    let max_bytes = args["max_bytes"].as_u64().unwrap_or(65_536).clamp(1, 524_288) as usize;
    let follow_redirects = args["follow_redirects"].as_bool().unwrap_or(false);
    let started = Instant::now();
    let mut url = Url::parse(url_text)?;
    anyhow::ensure!(matches!(url.scheme(), "http" | "https"), "only http and https URLs are allowed");
    anyhow::ensure!(url.username().is_empty() && url.password().is_none(), "URL credentials are not allowed");
    let mut headers = HeaderMap::new();
    if let Some(values) = args["headers"].as_object() {
        anyhow::ensure!(values.len() <= 100, "too many request headers");
        for (key, value) in values {
            let name = HeaderName::from_bytes(key.as_bytes())?;
            let value = value.as_str().context("header values must be strings")?;
            headers.insert(name, HeaderValue::from_str(value)?);
        }
    }
    let body = match &args["body"] {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        value => Some(serde_json::to_string(value)?),
    };
    if body.as_ref().is_some_and(|value| value.len() > 1_048_576) { anyhow::bail!("request body exceeds 1 MiB"); }
    let client = reqwest::Client::builder().no_proxy().redirect(Policy::none()).timeout(Duration::from_millis(timeout_ms)).build()?;
    let original_host = url.host_str().map(str::to_owned);
    let mut redirect_count = 0;
    let mut response = loop {
        crate::tools::process_sandbox::check_not_host_control_url_async(url.as_str()).await?;
        let mut request = client.request(method.clone(), url.clone()).headers(headers.clone());
        if let Some(ref payload) = body {
            if args["body"].is_object() || args["body"].is_array() {
                request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
            }
            request = request.body(payload.clone());
        }
        let response = request.send().await?;
        if !follow_redirects || !response.status().is_redirection() || redirect_count >= 10 { break response; }
        let Some(location) = response.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()) else { break response; };
        let next = url.join(location)?;
        anyhow::ensure!(next.host_str() == original_host.as_deref(), "cross-host redirects are blocked");
        redirect_count += 1;
        anyhow::ensure!(next.scheme() == "http" || next.scheme() == "https", "redirect to unsupported scheme blocked");
        anyhow::ensure!(next.username().is_empty() && next.password().is_none(), "redirect URL credentials are not allowed");
        url = next;
    };
    let status = response.status().as_u16();
    let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let safe_headers = response.headers().iter().filter_map(|(name, value)| {
        let key = name.as_str();
        if matches!(key, "set-cookie" | "authorization" | "proxy-authorization" | "www-authenticate") { return None; }
        value.to_str().ok().map(|value| (key.to_owned(), json!(value)))
    }).collect::<serde_json::Map<String, Value>>();
    let mut bytes = Vec::with_capacity(max_bytes.min(16_384));
    let mut truncated = false;
    while let Some(chunk) = response.chunk().await? {
        let take = max_bytes.saturating_sub(bytes.len()).min(chunk.len());
        bytes.extend_from_slice(&chunk[..take]);
        if take < chunk.len() { truncated = true; break; }
        if bytes.len() == max_bytes { truncated = response.chunk().await?.is_some(); break; }
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json_value = if !truncated && content_type.as_deref().is_some_and(|v| v.contains("json")) { serde_json::from_str::<Value>(&text).ok() } else { None };
    let mut result = json!({"url":url.to_string(),"method":method_text,"status":status,"ok":(200..300).contains(&status),"elapsed_ms":started.elapsed().as_millis().min(u64::MAX as u128) as u64,"headers":safe_headers,"body":text,"truncated":truncated,"bytes":bytes.len()});
    if let Some(content_type) = content_type { result["content_type"] = json!(content_type); }
    if let Some(json_value) = json_value { result["json"] = json_value; }
    Ok(result)
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"http_probe","description":"Probe local HTTP(S) endpoint reachability without following redirects.","inputSchema":{"type":"object","required":["url"],"properties":{"url":{"type":"string"},"timeout_ms":{"type":"integer"},"reason":{"type":"string"}}}}),
        json!({
        "name":"http_request",
        "description":"Send a bounded HTTP request. Control-plane URLs are blocked; redirects are disabled unless explicitly enabled and each hop is checked. External hosts and write methods require approval.",
        "inputSchema":{"type":"object","required":["url"],"properties":{
            "url":{"type":"string","maxLength":4096},"method":{"type":"string","enum":["GET","HEAD","POST","PUT","PATCH","DELETE"],"default":"GET"},
            "headers":{"type":"object","additionalProperties":{"type":"string"}},"body":{"oneOf":[{"type":"string"},{"type":"object"},{"type":"array"}]},
            "timeout_ms":{"type":"integer","minimum":100,"maximum":60000,"default":15000},"max_bytes":{"type":"integer","minimum":1,"maximum":524288,"default":65536},"follow_redirects":{"type":"boolean","default":false}
        }}
    })]
}
