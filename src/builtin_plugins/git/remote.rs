//! Read GitHub repositories through its API; no checkout or local writes.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use url::Url;

const API: &str = "https://api.github.com/";
const MAX_RESPONSE: usize = 12 * 1024 * 1024;
const MAX_FILE: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    repository: String,
    view: String,
    #[serde(default, rename = "ref")]
    reference: Option<String>,
    path: Option<String>,
    limit: Option<usize>,
}

fn repository(input: &str) -> Result<(String, String)> {
    let input = input.trim();
    let path = if input.contains("://") {
        let url = Url::parse(input).context("无效的仓库 URL")?;
        ensure!(
            url.scheme() == "https"
                && url.host_str() == Some("github.com")
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "仅支持不含凭据的 https://github.com/owner/repo 地址"
        );
        url.path().trim_matches('/').to_owned()
    } else {
        input.trim_matches('/').to_owned()
    };
    let mut parts = path.split('/');
    let owner = parts.next().unwrap_or("");
    let name = parts
        .next()
        .unwrap_or("")
        .strip_suffix(".git")
        .unwrap_or_else(|| path.split('/').nth(1).unwrap_or(""));
    let valid = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    ensure!(
        parts.next().is_none() && valid(owner) && valid(name),
        "仓库必须是 GitHub 的 owner/repo 地址"
    );
    Ok((owner.into(), name.into()))
}

fn path_parts(path: &str) -> Result<Vec<&str>> {
    if path.is_empty() {
        return Ok(vec![]);
    }
    let parts: Vec<_> = path.split('/').collect();
    ensure!(
        parts.iter().all(|p| !p.is_empty()
            && *p != "."
            && *p != ".."
            && !p.contains('\\')
            && !p.chars().any(char::is_control)),
        "path 必须是仓库内相对路径"
    );
    Ok(parts)
}

fn endpoint(base: &Url, segments: &[&str]) -> Result<Url> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("无效 API 地址"))?
        .pop_if_empty()
        .extend(segments.iter().copied());
    Ok(url)
}

async fn get(client: &reqwest::Client, url: Url, token: Option<&str>) -> Result<Value> {
    let mut request = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.context("无法连接 GitHub API")?;
    let status = response.status();
    if !status.is_success() {
        if status.as_u16() == 404 {
            bail!("GITHUB_NOT_FOUND: 仓库、路径或版本不存在，或当前凭据无权访问");
        }
        if status.as_u16() == 403 || status.as_u16() == 429 {
            bail!("GITHUB_ACCESS_LIMITED: GitHub 拒绝访问或请求次数受限，请检查权限及 API 限额");
        }
        bail!("GITHUB_HTTP_ERROR: GitHub API 返回 HTTP {status}");
    }
    ensure!(
        response
            .content_length()
            .is_none_or(|len| len <= MAX_RESPONSE as u64),
        "GITHUB_RESPONSE_TOO_LARGE: 响应超过大小限制"
    );
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("读取 GitHub 响应失败")?;
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= MAX_RESPONSE,
            "GITHUB_RESPONSE_TOO_LARGE: 响应超过大小限制"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("无法解析 GitHub API 响应")
}

pub async fn read(request: Request) -> Result<Value> {
    let client = reqwest::Client::builder()
        .user_agent("codex-workspace-git-plugin")
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("GH_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
    read_with(&client, &Url::parse(API)?, token.as_deref(), request).await
}

async fn read_with(
    client: &reqwest::Client,
    base: &Url,
    token: Option<&str>,
    request: Request,
) -> Result<Value> {
    let (owner, repo) = repository(&request.repository)?;
    ensure!(
        ["overview", "tree", "directory", "file", "commits"].contains(&request.view.as_str()),
        "未知的 view"
    );
    let root = ["repos", owner.as_str(), repo.as_str()];
    let metadata = if request.reference.is_none() || request.view == "overview" {
        Some(get(client, endpoint(base, &root)?, token).await?)
    } else {
        None
    };
    let reference = request
        .reference
        .as_deref()
        .or_else(|| metadata.as_ref()?.get("default_branch")?.as_str())
        .context("GitHub 未返回默认分支")?;
    ensure!(
        !reference.is_empty() && reference.len() <= 255 && !reference.chars().any(char::is_control),
        "ref 无效"
    );
    let commit = get(
        client,
        endpoint(base, &[&root[..], &["commits", reference]].concat())?,
        token,
    )
    .await?;
    let sha = commit["sha"].as_str().context("GitHub 未返回提交 SHA")?;
    let tree_sha = commit["commit"]["tree"]["sha"]
        .as_str()
        .context("GitHub 未返回目录树 SHA")?;
    let identity = format!("{owner}/{repo}");
    match request.view.as_str() {
        "overview" => {
            let meta = metadata.as_ref().unwrap();
            Ok(
                json!({"repository":identity,"url":format!("https://github.com/{identity}"),"default_branch":meta["default_branch"],"description":meta["description"],"private":meta["private"],"ref":reference,"commit_sha":sha,"tree_sha":tree_sha}),
            )
        }
        "tree" => {
            let mut url = endpoint(base, &[&root[..], &["git", "trees", tree_sha]].concat())?;
            url.query_pairs_mut().append_pair("recursive", "1");
            let tree = get(client, url, token).await?;
            let entries = tree["tree"].as_array().context("GitHub 未返回目录树")?;
            let truncated = tree["truncated"].as_bool().unwrap_or(false) || entries.len() > 20_000;
            Ok(
                json!({"repository":identity,"commit_sha":sha,"tree_sha":tree_sha,"entries":entries.iter().take(20_000).map(|e| json!({"path":e["path"],"type":e["type"],"size":e["size"],"sha":e["sha"]})).collect::<Vec<_>>(),"truncated":truncated}),
            )
        }
        "directory" | "file" => {
            let path = request.path.as_deref().unwrap_or("");
            let parts = path_parts(path)?;
            ensure!(
                request.view != "file" || !parts.is_empty(),
                "file 需要 path"
            );
            let mut segments = root.to_vec();
            segments.push("contents");
            segments.extend(parts);
            let mut url = endpoint(base, &segments)?;
            url.query_pairs_mut().append_pair("ref", sha);
            let content = get(client, url, token).await?;
            if request.view == "directory" {
                let entries = content
                    .as_array()
                    .context("指定路径不是目录，或 GitHub 未返回目录列表")?;
                return Ok(
                    json!({"repository":identity,"commit_sha":sha,"path":path,"entries":entries.iter().map(|v| json!({"name":v["name"],"path":v["path"],"type":v["type"],"size":v["size"],"sha":v["sha"]})).collect::<Vec<_>>(),"possibly_truncated":entries.len() >= 1000}),
                );
            }
            ensure!(content["type"] == "file", "指定路径不是普通文件");
            let size = content["size"].as_u64().context("GitHub 未返回文件大小")?;
            if size > MAX_FILE as u64 {
                return Ok(
                    json!({"repository":identity,"commit_sha":sha,"path":path,"bytes":size,"too_large":true}),
                );
            }
            ensure!(content["encoding"] == "base64", "GitHub 未提供文件内容");
            let encoded: String = content["content"]
                .as_str()
                .context("GitHub 未返回文件内容")?
                .chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect();
            let bytes = STANDARD.decode(encoded).context("无法解码 GitHub 文件")?;
            ensure!(bytes.len() <= MAX_FILE, "文件超过大小限制");
            let text = std::str::from_utf8(&bytes)
                .ok()
                .filter(|s| !s.contains('\0'));
            Ok(
                json!({"repository":identity,"commit_sha":sha,"path":path,"bytes":size,"too_large":false,"binary":text.is_none(),"content":text}),
            )
        }
        "commits" => {
            let mut url = endpoint(base, &[&root[..], &["commits"]].concat())?;
            url.query_pairs_mut().append_pair("sha", sha).append_pair(
                "per_page",
                &request.limit.unwrap_or(30).clamp(1, 100).to_string(),
            );
            let commits = get(client, url, token).await?;
            let commits = commits.as_array().context("GitHub 未返回提交列表")?;
            Ok(
                json!({"repository":identity,"commit_sha":sha,"commits":commits.iter().map(|c| json!({"sha":c["sha"],"message":c["commit"]["message"],"author":c["commit"]["author"],"parents":c["parents"].as_array().map(|p| p.iter().map(|v| v["sha"].clone()).collect::<Vec<_>>())})).collect::<Vec<_>>() }),
            )
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::OriginalUri, response::IntoResponse, routing::get};

    #[test]
    fn validates_repositories_and_paths() {
        assert_eq!(
            repository("https://github.com/hahaliangchen/pptx-editor-engine.git").unwrap(),
            ("hahaliangchen".into(), "pptx-editor-engine".into())
        );
        assert_eq!(
            repository("owner/repo").unwrap(),
            ("owner".into(), "repo".into())
        );
        for bad in [
            "https://evil.example/a/b",
            "https://user:token@github.com/a/b",
            "https://github.com/a/b/tree/main",
            "a/../c",
            "https://github.com/a/b?x=1",
        ] {
            assert!(repository(bad).is_err(), "{bad}");
        }
        assert!(path_parts("src/你好.rs").is_ok());
        for bad in ["../secret", "/a", "a//b", "a\\b"] {
            assert!(path_parts(bad).is_err());
        }
    }

    #[tokio::test]
    async fn remote_views_pin_the_commit_and_read_without_a_checkout() {
        async fn fixture(OriginalUri(uri): OriginalUri) -> impl IntoResponse {
            let path = uri.path();
            let query = uri.query().unwrap_or("");
            let value = match path {
                "/repos/o/r" => {
                    json!({"default_branch":"main","description":"demo","private":false})
                }
                "/repos/o/r/commits/main" | "/repos/o/r/commits/abc123" => {
                    json!({"sha":"abc123","commit":{"tree":{"sha":"tree123"}}})
                }
                "/repos/o/r/git/trees/tree123" if query == "recursive=1" => {
                    json!({"tree":[{"path":"src/a.rs","type":"blob","sha":"blob","size":4}],"truncated":false})
                }
                "/repos/o/r/contents/src/a.rs" if query == "ref=abc123" => {
                    json!({"type":"file","size":4,"encoding":"base64","content":"aGkK\n"})
                }
                "/repos/o/r/contents/src" if query == "ref=abc123" => {
                    json!([{"name":"a.rs","path":"src/a.rs","type":"file","size":4,"sha":"blob"}])
                }
                "/repos/o/r/commits" if query.contains("sha=abc123") => {
                    json!([{"sha":"abc123","commit":{"message":"test","author":{"name":"A"}},"parents":[]}])
                }
                _ => return (axum::http::StatusCode::NOT_FOUND, axum::Json(json!({}))),
            };
            (axum::http::StatusCode::OK, axum::Json(value))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(get(fixture)))
                .await
                .unwrap()
        });
        let base = Url::parse(&format!("http://{address}/")).unwrap();
        let client = reqwest::Client::new();
        let req = |view: &str, path: Option<&str>, reference: Option<&str>| Request {
            repository: "o/r".into(),
            view: view.into(),
            reference: reference.map(str::to_owned),
            path: path.map(str::to_owned),
            limit: None,
        };
        let overview = read_with(&client, &base, None, req("overview", None, None))
            .await
            .unwrap();
        assert_eq!(overview["commit_sha"], "abc123");
        let tree = read_with(&client, &base, None, req("tree", None, Some("abc123")))
            .await
            .unwrap();
        assert_eq!(tree["entries"][0]["path"], "src/a.rs");
        let file = read_with(
            &client,
            &base,
            None,
            req("file", Some("src/a.rs"), Some("abc123")),
        )
        .await
        .unwrap();
        assert_eq!(file["content"], "hi\n");
        let directory = read_with(
            &client,
            &base,
            None,
            req("directory", Some("src"), Some("abc123")),
        )
        .await
        .unwrap();
        assert_eq!(directory["entries"][0]["name"], "a.rs");
        let commits = read_with(&client, &base, None, req("commits", None, Some("abc123")))
            .await
            .unwrap();
        assert_eq!(commits["commits"][0]["message"], "test");
        server.abort();
    }

    #[tokio::test]
    async fn live_public_repository_when_requested() {
        if std::env::var_os("GIT_REMOTE_LIVE_TEST").is_none() {
            return;
        }
        let req = |view: &str, path: Option<&str>, reference: Option<String>| Request {
            repository: "https://github.com/hahaliangchen/pptx-editor-engine.git".into(),
            view: view.into(),
            reference,
            path: path.map(str::to_owned),
            limit: None,
        };
        let overview = read(req("overview", None, None)).await.unwrap();
        let sha = overview["commit_sha"].as_str().unwrap().to_owned();
        let tree = read(req("tree", None, Some(sha.clone()))).await.unwrap();
        assert!(
            tree["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["path"] == "README.md")
        );
        let file = read(req("file", Some("README.md"), Some(sha.clone())))
            .await
            .unwrap();
        assert!(
            file["content"]
                .as_str()
                .unwrap()
                .contains("pptx-editor-engine")
        );
        assert_eq!(file["commit_sha"], sha);
    }
}
