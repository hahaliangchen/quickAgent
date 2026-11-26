//! Local document helpers for Office Studio's WPS-style File menu:
//! - Stream allowlisted local office documents (`.md`, `.docx`, `.xlsx`, `.pptx`, `.pdf`)
//! - Reveal a file's containing folder in the OS file manager
//! - List recently modified office documents in the active workspace and `chat-assets/`

use super::AgentServiceState;
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::UNIX_EPOCH,
};

const ALLOWED_OFFICE_EXTS: &[&str] = &["md", "markdown", "docx", "xlsx", "pptx", "pdf"];
const SKIP_SCAN_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".codex-workspace-mcp",
    "__pycache__",
    ".venv",
    "venv",
];
const MAX_SCAN_DEPTH: usize = 3;
const MAX_RECENT_DOCS: usize = 20;

#[derive(Deserialize)]
pub struct OfficeLocalFileQuery {
    path: String,
}

#[derive(Deserialize)]
pub struct OfficeRevealFolderRequest {
    path: String,
}

#[derive(Deserialize)]
pub struct OfficeRecentDocsQuery {
    #[serde(default)]
    workspace: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OfficeRecentDocItem {
    pub path: String,
    pub filename: String,
    pub dir: String,
    pub ext: String,
    pub modified_ms: u64,
    pub source: &'static str,
}

pub(crate) fn is_allowed_office_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .is_some_and(|ext| ALLOWED_OFFICE_EXTS.contains(&ext.as_str()))
}

fn decode_file_uri_or_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("file://") {
        let path_part = if rest.starts_with('/') {
            rest
        } else {
            rest.strip_prefix("localhost")?
                .strip_prefix('/')
                .map(|_| &rest["localhost".len()..])?
        };
        let mut bytes = Vec::with_capacity(path_part.len());
        let mut iter = path_part.bytes();
        while let Some(b) = iter.next() {
            if b == b'%' {
                let hi = (iter.next()? as char).to_digit(16)?;
                let lo = (iter.next()? as char).to_digit(16)?;
                bytes.push((hi * 16 + lo) as u8);
            } else {
                bytes.push(b);
            }
        }
        let decoded = String::from_utf8(bytes).ok()?;
        #[cfg(windows)]
        let decoded = if decoded.as_bytes().get(2) == Some(&b':') {
            decoded[1..].to_owned()
        } else {
            decoded
        };
        return Some(PathBuf::from(decoded));
    }
    Some(PathBuf::from(trimmed))
}

fn validate_local_office_file(raw: &str) -> Result<PathBuf, (StatusCode, &'static str)> {
    let path = decode_file_uri_or_path(raw).ok_or((StatusCode::BAD_REQUEST, "invalid file path"))?;
    if !path.is_absolute() {
        return Err((StatusCode::BAD_REQUEST, "path must be absolute"));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err((StatusCode::BAD_REQUEST, "parent traversal is not allowed"));
    }
    let canonical =
        std::fs::canonicalize(&path).map_err(|_| (StatusCode::NOT_FOUND, "file not found"))?;
    if !canonical.is_file() {
        return Err((StatusCode::NOT_FOUND, "not a regular file"));
    }
    if !is_allowed_office_extension(&canonical) {
        return Err((StatusCode::FORBIDDEN, "unsupported office document type"));
    }
    Ok(canonical)
}

/// Resolve an existing file or directory path to `(target_to_open, containing_dir)`.
pub(crate) fn resolve_reveal_target(raw: &str) -> Result<(PathBuf, PathBuf), (StatusCode, &'static str)> {
    let path = decode_file_uri_or_path(raw).ok_or((StatusCode::BAD_REQUEST, "invalid path"))?;
    if !path.is_absolute() {
        return Err((StatusCode::BAD_REQUEST, "path must be absolute"));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err((StatusCode::BAD_REQUEST, "parent traversal is not allowed"));
    }
    let canonical =
        std::fs::canonicalize(&path).map_err(|_| (StatusCode::NOT_FOUND, "path not found"))?;
    if canonical.is_dir() {
        Ok((canonical.clone(), canonical))
    } else if canonical.is_file() {
        let dir = canonical
            .parent()
            .map(Path::to_path_buf)
            .ok_or((StatusCode::BAD_REQUEST, "no parent directory"))?;
        Ok((canonical, dir))
    } else {
        Err((StatusCode::NOT_FOUND, "path not found"))
    }
}

fn spawn_os_reveal(target: &Path, dir: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        if target.is_file() {
            cmd.arg("-R").arg(target);
        } else {
            cmd.arg(dir);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let clean_target = crate::tools::file_edit::strip_verbatim_prefix(target);
        let clean_dir = crate::tools::file_edit::strip_verbatim_prefix(dir);
        let mut cmd = Command::new("explorer");
        if target.is_file() {
            cmd.arg(format!("/select,{}", clean_target.display()));
        } else {
            cmd.arg(&clean_dir);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = target;
        Command::new("xdg-open")
            .arg(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }
}

fn scan_office_docs_in_dir(
    root: &Path,
    source: &'static str,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<OfficeRecentDocItem>,
) {
    let Ok(canonical_root) = std::fs::canonicalize(root) else {
        return;
    };
    if !canonical_root.is_dir() {
        return;
    }
    let mut stack: Vec<(PathBuf, usize)> = vec![(canonical_root, 0)];
    let mut visited_entries: usize = 0;
    const MAX_VISITED_ENTRIES: usize = 1500;

    while let Some((dir, depth)) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            visited_entries += 1;
            if visited_entries > MAX_VISITED_ENTRIES {
                return;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if ft.is_dir() {
                if depth < MAX_SCAN_DEPTH
                    && !SKIP_SCAN_DIRS
                        .iter()
                        .any(|skip| name_str.eq_ignore_ascii_case(skip))
                {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !ft.is_file() || !is_allowed_office_extension(&path) {
                continue;
            }
            let Ok(canonical) = std::fs::canonicalize(&path) else {
                continue;
            };
            if !seen.insert(canonical.clone()) {
                continue;
            }
            let modified_ms = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let ext = canonical
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default();
            let filename = canonical
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            let dir_str = canonical
                .parent()
                .map(crate::tools::file_edit::display_path)
                .unwrap_or_default();
            out.push(OfficeRecentDocItem {
                path: crate::tools::file_edit::display_path(&canonical),
                filename,
                dir: dir_str,
                ext,
                modified_ms,
                source,
            });
        }
    }
}

pub(crate) fn collect_recent_office_docs(
    workspace: Option<&Path>,
    chat_assets: Option<&Path>,
) -> Vec<OfficeRecentDocItem> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    if let Some(ws) = workspace {
        scan_office_docs_in_dir(ws, "workspace", &mut seen, &mut items);
    }
    if let Some(chat) = chat_assets {
        scan_office_docs_in_dir(chat, "chat-assets", &mut seen, &mut items);
    }
    items.sort_by(|a, b| {
        b.modified_ms
            .cmp(&a.modified_ms)
            .then_with(|| a.filename.cmp(&b.filename))
    });
    items.truncate(MAX_RECENT_DOCS);
    items
}

/// `GET /agent/office/local-file?path=<abs_path>`
pub async fn office_local_file(
    Query(query): Query<OfficeLocalFileQuery>,
    method: Method,
    headers: HeaderMap,
) -> axum::response::Response {
    use crate::server::file_stream;
    let raw = query.path;
    let opened = tokio::task::spawn_blocking(move || -> Result<(std::fs::File, mime_guess::Mime), (StatusCode, &'static str)> {
        let canonical = validate_local_office_file(&raw)?;
        let file = std::fs::File::open(&canonical)
            .map_err(|_| (StatusCode::NOT_FOUND, "unable to open file"))?;
        let media = mime_guess::from_path(&canonical).first_or_octet_stream();
        Ok((file, media))
    })
    .await;

    match opened {
        Ok(Ok((file, media))) => {
            match file_stream::respond(file, media.as_ref(), false, method, &headers).await {
                Ok(resp) => resp,
                Err(_) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "failed to stream file"})),
                )
                    .into_response(),
            }
        }
        Ok(Err((status, msg))) => (status, Json(json!({"error": msg}))).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "failed to open file"})),
        )
            .into_response(),
    }
}

/// `POST /agent/office/reveal-folder`
pub async fn office_reveal_folder(
    Json(req): Json<OfficeRevealFolderRequest>,
) -> impl IntoResponse {
    let raw = req.path;
    let result = tokio::task::spawn_blocking(move || -> Result<String, (StatusCode, String)> {
        let (target, dir) =
            resolve_reveal_target(&raw).map_err(|(code, msg)| (code, msg.to_owned()))?;
        spawn_os_reveal(&target, &dir).map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("无法打开所在文件夹: {err}"),
            )
        })?;
        Ok(crate::tools::file_edit::display_path(&dir))
    })
    .await;

    match result {
        Ok(Ok(dir)) => (StatusCode::OK, Json(json!({"ok": true, "dir": dir}))).into_response(),
        Ok(Err((status, msg))) => (status, Json(json!({"error": msg}))).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "failed to reveal folder"})),
        )
            .into_response(),
    }
}

/// `GET /agent/office/recent-docs?workspace=<dir>`
pub async fn office_recent_docs(
    State(state): State<AgentServiceState>,
    Query(query): Query<OfficeRecentDocsQuery>,
) -> impl IntoResponse {
    let store_root = state.workspace.root().to_path_buf();
    let workspace_opt = query
        .workspace
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let items = tokio::task::spawn_blocking(move || {
        let chat_dir = crate::context::session_browser::chat_assets_dir(&store_root).ok();
        collect_recent_office_docs(workspace_opt.as_deref(), chat_dir.as_deref())
    })
    .await
    .unwrap_or_default();

    Json(json!({ "items": items })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn office_extension_allowlist_accepts_docs_and_rejects_secrets() {
        assert!(is_allowed_office_extension(Path::new("notes.md")));
        assert!(is_allowed_office_extension(Path::new("SPEC.MARKDOWN")));
        assert!(is_allowed_office_extension(Path::new("report.docx")));
        assert!(is_allowed_office_extension(Path::new("sheet.xlsx")));
        assert!(is_allowed_office_extension(Path::new("deck.pptx")));
        assert!(is_allowed_office_extension(Path::new("manual.PDF")));
        assert!(!is_allowed_office_extension(Path::new("id_rsa")));
        assert!(!is_allowed_office_extension(Path::new(".env")));
        assert!(!is_allowed_office_extension(Path::new("config.yml")));
    }

    #[test]
    fn validate_local_office_file_enforces_extension_and_existence() {
        let dir = tempdir().unwrap();
        let md = dir.path().join("guide.md");
        let secret = dir.path().join("secret.env");
        fs::write(&md, "# Hello").unwrap();
        fs::write(&secret, "API_KEY=123").unwrap();

        let ok = validate_local_office_file(md.to_str().unwrap());
        assert!(ok.is_ok());

        let denied = validate_local_office_file(secret.to_str().unwrap());
        assert_eq!(denied.unwrap_err().0, StatusCode::FORBIDDEN);

        let missing = validate_local_office_file(dir.path().join("missing.pdf").to_str().unwrap());
        assert_eq!(missing.unwrap_err().0, StatusCode::NOT_FOUND);

        let relative = validate_local_office_file("guide.md");
        assert_eq!(relative.unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn resolve_reveal_target_handles_file_dir_and_missing_path() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("sub");
        fs::create_dir_all(&nested).unwrap();
        let doc = nested.join("slides.pptx");
        fs::write(&doc, b"pptx").unwrap();

        let (target, parent) = resolve_reveal_target(doc.to_str().unwrap()).unwrap();
        assert_eq!(target, fs::canonicalize(&doc).unwrap());
        assert_eq!(parent, fs::canonicalize(&nested).unwrap());

        let (dir_target, dir_parent) = resolve_reveal_target(nested.to_str().unwrap()).unwrap();
        assert_eq!(dir_target, fs::canonicalize(&nested).unwrap());
        assert_eq!(dir_parent, fs::canonicalize(&nested).unwrap());

        let missing = resolve_reveal_target(dir.path().join("nope.docx").to_str().unwrap());
        assert_eq!(missing.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn collect_recent_office_docs_skips_ignored_dirs_and_deduplicates() {
        let ws = tempdir().unwrap();
        let chat = tempdir().unwrap();
        let docs = ws.path().join("docs");
        let ignored = ws.path().join("node_modules/pkg");
        fs::create_dir_all(&docs).unwrap();
        fs::create_dir_all(&ignored).unwrap();

        fs::write(docs.join("readme.md"), "# readme").unwrap();
        fs::write(docs.join("main.rs"), "fn main() {}").unwrap();
        fs::write(ignored.join("ignored.md"), "# ignored").unwrap();
        fs::write(chat.path().join("summary.docx"), b"docx").unwrap();

        let items = collect_recent_office_docs(Some(ws.path()), Some(chat.path()));
        let names: Vec<&str> = items.iter().map(|i| i.filename.as_str()).collect();
        assert!(names.contains(&"readme.md"));
        assert!(names.contains(&"summary.docx"));
        assert!(!names.contains(&"ignored.md"));
        assert!(!names.contains(&"main.rs"));
    }
}
