//! Browser UI for the standalone Rust agent, served from the frontend build.

use std::path::{Path, PathBuf};

use axum::{
    Json,
    extract::State,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use base64::Engine;
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

const MAX_CLIPBOARD_FILES: usize = 20;
const MAX_CLIPBOARD_FILE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_CLIPBOARD_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

/// Read local files currently copied on the OS desktop clipboard (e.g. `.pptx`,
/// `.pdf`, `.docx`, `.xlsx`, images, code files copied from Dolphin / Nautilus /
/// Windows Explorer / macOS Finder) when WebView `clipboardData.files` is empty.
pub async fn clipboard_files() -> Response {
    let paths = read_os_clipboard_file_paths().await;
    if paths.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let mut out = Vec::new();
    let mut total_bytes: u64 = 0;
    for raw_path in paths.into_iter().take(MAX_CLIPBOARD_FILES) {
        let clean = crate::tools::file_edit::strip_verbatim_prefix(&raw_path);
        let Ok(meta) = tokio::fs::metadata(&clean).await else {
            continue;
        };
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_CLIPBOARD_FILE_BYTES {
            continue;
        }
        if total_bytes.saturating_add(meta.len()) > MAX_CLIPBOARD_TOTAL_BYTES {
            break;
        }
        let Ok(bytes) = tokio::fs::read(&clean).await else {
            continue;
        };
        if bytes.is_empty() || bytes.len() as u64 > MAX_CLIPBOARD_FILE_BYTES {
            continue;
        }
        total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        let name = clean
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "clipboard-file".to_owned());
        let mime = guess_mime_for_path(&clean);
        out.push(json!({
            "name": name,
            "mime": mime,
            "base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
        }));
    }

    if out.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "files": out })),
    )
        .into_response()
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

fn guess_mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("ppt") => "application/vnd.ms-powerpoint",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("doc") => "application/msword",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("xls") => "application/vnd.ms-excel",
        Some("pdf") => "application/pdf",
        Some("md" | "markdown") => "text/markdown",
        Some("txt" | "log") => "text/plain",
        Some("csv") => "text/csv",
        Some("json" | "jsonc" | "jsonl") => "application/json",
        Some("xml") => "application/xml",
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("js" | "mjs" | "cjs") => "text/javascript",
        Some("ts" | "tsx") => "text/typescript",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("bmp") => "image/bmp",
        Some("zip") => "application/zip",
        Some("mp4") => "video/mp4",
        Some("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

fn percent_decode_utf8(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse `text/uri-list`, `x-special/gnome-copied-files`, or newline-separated
/// local file paths from the OS clipboard into deduplicated `PathBuf`s.
fn parse_clipboard_file_uris(raw: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.eq_ignore_ascii_case("copy")
            || trimmed.eq_ignore_ascii_case("cut")
        {
            continue;
        }
        let local = if let Some(rest) = trimmed
            .strip_prefix("file://")
            .or_else(|| trimmed.strip_prefix("FILE://"))
        {
            let hostless = if let Some(after_local) = rest
                .strip_prefix("localhost/")
                .or_else(|| rest.strip_prefix("LOCALHOST/"))
            {
                format!("/{after_local}")
            } else if rest.starts_with('/') {
                rest.to_owned()
            } else {
                // Reject remote `file://host/share/...` URIs.
                continue;
            };
            let decoded = percent_decode_utf8(&hostless);
            strip_leading_slash_before_drive(&decoded)
        } else if trimmed.starts_with('/')
            || trimmed.starts_with("\\\\?\\")
            || (trimmed.len() >= 3
                && trimmed.as_bytes()[0].is_ascii_alphabetic()
                && trimmed.as_bytes()[1] == b':'
                && matches!(trimmed.as_bytes()[2], b'/' | b'\\'))
        {
            let cleaned = trimmed.strip_prefix("\\\\?\\").unwrap_or(trimmed);
            strip_leading_slash_before_drive(cleaned)
        } else {
            continue;
        };
        if !local.is_empty() && seen.insert(local.clone()) {
            paths.push(PathBuf::from(local));
        }
    }
    paths
}

fn strip_leading_slash_before_drive(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && matches!(bytes[3], b'/' | b'\\')
    {
        path[1..].to_owned()
    } else {
        path.to_owned()
    }
}

async fn run_clipboard_cmd(program: &str, args: &[&str], max_bytes: usize) -> Option<Vec<u8>> {
    use std::process::Stdio;
    use tokio::{io::AsyncReadExt, process::Command};

    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let mut child = cmd.spawn().ok()?;
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
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 16 * 1024 * 1024;

#[cfg(target_os = "linux")]
const PREFERRED_IMAGE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

#[cfg(target_os = "linux")]
const PREFERRED_FILE_URI_MIMES: [&str; 3] = [
    "text/uri-list",
    "x-special/gnome-copied-files",
    "application/x-kde4-urilist",
];

#[cfg(target_os = "linux")]
fn pick_clipboard_mime(types_stdout: &str, preferred: &[&'static str]) -> Option<&'static str> {
    let available: std::collections::BTreeSet<&str> = types_stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    preferred
        .iter()
        .copied()
        .find(|mime| available.contains(mime))
}

#[cfg(target_os = "linux")]
fn pick_clipboard_image_mime(types_stdout: &str) -> Option<&'static str> {
    pick_clipboard_mime(types_stdout, &PREFERRED_IMAGE_MIMES)
}

#[cfg(target_os = "linux")]
async fn read_os_clipboard_file_paths() -> Vec<PathBuf> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland");
    if wayland {
        if let Some(types_bytes) = run_clipboard_cmd("wl-paste", &["--list-types"], 64 * 1024).await {
            let types_text = String::from_utf8_lossy(&types_bytes);
            if let Some(mime) = pick_clipboard_mime(&types_text, &PREFERRED_FILE_URI_MIMES)
                && let Some(raw) = run_clipboard_cmd("wl-paste", &["--no-newline", "--type", mime], 256 * 1024).await
            {
                return parse_clipboard_file_uris(&String::from_utf8_lossy(&raw));
            }
            // Wayland clipboard is authoritative when `wl-paste` succeeded; don't read stale XWayland data.
            if !types_text.trim().is_empty() {
                return Vec::new();
            }
        }
    }
    if std::env::var_os("DISPLAY").is_some()
        && let Some(types_bytes) =
            run_clipboard_cmd("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"], 64 * 1024)
                .await
    {
        let types_text = String::from_utf8_lossy(&types_bytes);
        if let Some(mime) = pick_clipboard_mime(&types_text, &PREFERRED_FILE_URI_MIMES)
            && let Some(raw) =
                run_clipboard_cmd("xclip", &["-selection", "clipboard", "-t", mime, "-o"], 256 * 1024)
                    .await
        {
            return parse_clipboard_file_uris(&String::from_utf8_lossy(&raw));
        }
    }
    Vec::new()
}

#[cfg(target_os = "windows")]
async fn read_os_clipboard_file_paths() -> Vec<PathBuf> {
    let script = "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $f = Get-Clipboard -Format FileDropList; if ($f) { $f | ForEach-Object { $_.FullName } }";
    let Some(raw) = run_clipboard_cmd(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-STA", "-Command", script],
        256 * 1024,
    )
    .await
    else {
        return Vec::new();
    };
    parse_clipboard_file_uris(&String::from_utf8_lossy(&raw))
}

#[cfg(target_os = "macos")]
async fn read_os_clipboard_file_paths() -> Vec<PathBuf> {
    let jxa = "ObjC.import('AppKit'); var pb = $.NSPasteboard.generalPasteboard; var urls = pb.readObjectsForClassesOptions($([$.NSURL]), $({NSPasteboardURLReadingFileURLsOnlyKey: true})); var out = []; if (urls) { for (var i = 0; i < urls.count; i++) { var p = ObjC.unwrap(urls.objectAtIndex(i).path); if (p) out.push(p); } } out.join('\\n');";
    let Some(raw) = run_clipboard_cmd("osascript", &["-l", "JavaScript", "-e", jxa], 256 * 1024).await else {
        return Vec::new();
    };
    parse_clipboard_file_uris(&String::from_utf8_lossy(&raw))
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
async fn read_os_clipboard_file_paths() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(target_os = "linux")]
async fn read_linux_clipboard_image() -> Option<(&'static str, Vec<u8>)> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland");
    if wayland {
        if let Some(types_bytes) = run_clipboard_cmd("wl-paste", &["--list-types"], 64 * 1024).await {
            let types_text = String::from_utf8_lossy(&types_bytes);
            if let Some(mime) = pick_clipboard_image_mime(&types_text)
                && let Some(bytes) = run_clipboard_cmd(
                    "wl-paste",
                    &["--no-newline", "--type", mime],
                    MAX_CLIPBOARD_IMAGE_BYTES,
                )
                .await
            {
                return Some((mime, bytes));
            }
            // Wayland clipboard is active and holds non-image data (e.g. text/uri-list); don't read stale X11 image.
            if !types_text.trim().is_empty() {
                return None;
            }
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

#[cfg(test)]
mod tests {
    use super::parse_clipboard_file_uris;
    use std::path::PathBuf;

    #[test]
    fn parses_file_uris_gnome_copied_files_and_windows_paths() {
        let uri_list = "# comment\r\nfile:///home/goodtime/%E6%BC%94%E7%A4%BA%20deck.pptx\r\nfile://localhost/tmp/a.pdf\r\nfile://remote-host/share/nope.pptx\r\n";
        assert_eq!(
            parse_clipboard_file_uris(uri_list),
            vec![
                PathBuf::from("/home/goodtime/演示 deck.pptx"),
                PathBuf::from("/tmp/a.pdf"),
            ]
        );

        let gnome = "copy\nfile:///home/goodtime/report.docx\nfile:///home/goodtime/report.docx\n";
        assert_eq!(
            parse_clipboard_file_uris(gnome),
            vec![PathBuf::from("/home/goodtime/report.docx")]
        );

        let win = "file:///C:/Users/dev/Desktop/%E6%B1%87%E6%8A%A5.pptx\n\\\\?\\D:\\Docs\\plan.xlsx\n";
        assert_eq!(
            parse_clipboard_file_uris(win),
            vec![
                PathBuf::from("C:/Users/dev/Desktop/汇报.pptx"),
                PathBuf::from("D:\\Docs\\plan.xlsx"),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn picks_preferred_image_mime_and_ignores_text_only_clipboards() {
        use super::pick_clipboard_image_mime;
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
