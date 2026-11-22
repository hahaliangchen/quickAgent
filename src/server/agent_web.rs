//! Browser UI for the standalone Rust agent, served from the frontend build.

use std::path::PathBuf;

use axum::{
    Json,
    extract::State,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use serde_json::json;

use crate::server::agent_service::AgentServiceState;

/// Vite output of `frontend/`; `AGENT_WEB_DIR` points elsewhere for packaged installs.
pub fn web_dir() -> PathBuf {
    std::env::var_os("AGENT_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/frontend/dist")))
}

pub async fn page() -> Response {
    match tokio::fs::read_to_string(web_dir().join("index.html")).await {
        Ok(html) => ([(header::CACHE_CONTROL, "no-cache")], Html(html)).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Agent frontend is not built. Run npm install and npm run build in frontend/, or set AGENT_WEB_DIR to the built frontend directory.",
        ).into_response(),
    }
}

pub async fn settings_page() -> Response {
    page().await
}

pub async fn info(State(state): State<AgentServiceState>) -> impl IntoResponse {
    let state = state.with_latest_config().await;
    let ready = !state.plugin_cancel.is_cancelled()
        && !state.provider_url.is_empty()
        && !state.default_model.is_empty();
    Json(json!({
        "ready": ready,
        "workspace": state.workspace.root().display().to_string(),
        "model": state.default_model,
    }))
}

pub async fn star_png() -> impl IntoResponse {
    let path = web_dir().join("star.png");
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return ([(header::CONTENT_TYPE, "image/png")], bytes).into_response();
    }
    let alt = web_dir().join("logo.png");
    if let Ok(bytes) = tokio::fs::read(&alt).await {
        return ([(header::CONTENT_TYPE, "image/png")], bytes).into_response();
    }
    (
        [(header::CONTENT_TYPE, "image/png")],
        include_bytes!("../../frontend/src/assets/star.png").to_vec(),
    )
        .into_response()
}

pub async fn favicon_png() -> impl IntoResponse {
    let path = web_dir().join("favicon.png");
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return ([(header::CONTENT_TYPE, "image/png")], bytes).into_response();
    }
    star_png().await.into_response()
}

/// Read an image from the local desktop clipboard when Linux WebKitGTK does not
/// populate `ClipboardEvent.clipboardData` for bitmap-only pastes.
pub async fn clipboard_image() -> Response {
    #[cfg(target_os = "linux")]
    {
        match read_linux_clipboard_image().await {
            Some((mime, bytes)) => (
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                bytes,
            )
                .into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        StatusCode::NO_CONTENT.into_response()
    }
}

#[cfg(target_os = "linux")]
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 16 * 1024 * 1024;

#[cfg(target_os = "linux")]
const PREFERRED_IMAGE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

#[cfg(target_os = "linux")]
fn pick_clipboard_image_mime(types_stdout: &str) -> Option<&'static str> {
    let available: std::collections::BTreeSet<&str> = types_stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    PREFERRED_IMAGE_MIMES
        .into_iter()
        .find(|mime| available.contains(mime))
}

#[cfg(target_os = "linux")]
async fn run_clipboard_cmd(program: &str, args: &[&str], max_bytes: usize) -> Option<Vec<u8>> {
    use std::process::Stdio;
    use tokio::{io::AsyncReadExt, process::Command};

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut data = Vec::new();
        stdout.take(max_bytes as u64 + 1).read_to_end(&mut data).await.ok()?;
        if data.is_empty() || data.len() > max_bytes {
            return None;
        }
        let status = child.wait().await.ok()?;
        status.success().then_some(data)
    })
    .await
    .ok()?
}

#[cfg(target_os = "linux")]
async fn read_linux_clipboard_image() -> Option<(&'static str, Vec<u8>)> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland");
    if wayland {
        if let Some(types_bytes) = run_clipboard_cmd("wl-paste", &["--list-types"], 64 * 1024).await
            && let Some(mime) = pick_clipboard_image_mime(&String::from_utf8_lossy(&types_bytes))
            && let Some(bytes) = run_clipboard_cmd(
                "wl-paste",
                &["--no-newline", "--type", mime],
                MAX_CLIPBOARD_IMAGE_BYTES,
            )
            .await
        {
            return Some((mime, bytes));
        }
    }
    if std::env::var_os("DISPLAY").is_some()
        && let Some(types_bytes) =
            run_clipboard_cmd("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"], 64 * 1024)
                .await
        && let Some(mime) = pick_clipboard_image_mime(&String::from_utf8_lossy(&types_bytes))
        && let Some(bytes) = run_clipboard_cmd(
            "xclip",
            &["-selection", "clipboard", "-t", mime, "-o"],
            MAX_CLIPBOARD_IMAGE_BYTES,
        )
        .await
    {
        return Some((mime, bytes));
    }
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::pick_clipboard_image_mime;

    #[test]
    fn picks_preferred_image_mime_and_ignores_text_only_clipboards() {
        assert_eq!(
            pick_clipboard_image_mime("text/plain\nimage/jpeg\nimage/png\n"),
            Some("image/png")
        );
        assert_eq!(
            pick_clipboard_image_mime("text/plain\nimage/webp\n"),
            Some("image/webp")
        );
        assert_eq!(
            pick_clipboard_image_mime("text/plain;charset=utf-8\ntext/html\n"),
            None
        );
    }
}
