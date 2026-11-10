//! Model-independent search and immutable, paged web evidence.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

const API_BASE: &str = "https://api.deepseek.com/v1/tools";
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_PAGE_CHARS: usize = 12_000;
const MAX_PAGE_CHARS: usize = 24_000;

struct WebClient {
    http: reqwest::Client,
    base: String,
    provider: String,
    key: String,
}

impl WebClient {
    async fn post(&self, method: &str, body: Value) -> Result<Value> {
        let response = self
            .http
            .post(format!("{}/{method}", self.base))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .map_err(|_| {
                anyhow::anyhow!("WEB_NETWORK_ERROR: {method} request failed or timed out")
            })?;
        let status = response.status();
        if !status.is_success() {
            let (code, action) = match status.as_u16() {
                401 => ("WEB_AUTH_ERROR", "check the selected search credential"),
                402 => (
                    "WEB_BALANCE_ERROR",
                    "the search account has insufficient balance",
                ),
                404 => (
                    "WEB_PAGE_UNAVAILABLE",
                    "no usable page content was available",
                ),
                408 | 429 | 500..=599 => ("WEB_TEMPORARY_ERROR", "retry later"),
                _ => (
                    "WEB_UPSTREAM_ERROR",
                    "check the request or page availability",
                ),
            };
            // Never forward an upstream error body: it may echo credentials or request internals.
            bail!(
                "{code}: {method} returned HTTP {}; {action}",
                status.as_u16()
            );
        }
        ensure!(
            response
                .content_length()
                .is_none_or(|n| n <= MAX_RESPONSE_BYTES as u64),
            "WEB_RESPONSE_TOO_LARGE: narrow the request; no partial result delivered"
        );
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|_| anyhow::anyhow!("WEB_NETWORK_ERROR: incomplete response"))?;
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= MAX_RESPONSE_BYTES,
                "WEB_RESPONSE_TOO_LARGE: narrow the request; no partial result delivered"
            );
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("WEB_INVALID_RESPONSE: expected JSON"))
    }

    async fn search(&self, args: SearchArgs) -> Result<Value> {
        let query = args.query.trim();
        ensure!(
            (1..=128).contains(&query.chars().count()),
            "WEB_INVALID_ARGUMENT: query must contain 1–128 characters"
        );
        ensure!(
            (1..=20).contains(&args.count),
            "WEB_INVALID_ARGUMENT: count must be 1–20"
        );
        let data = self
            .post(
                "search",
                json!({"query":query,"count":args.count,"need_content":false}),
            )
            .await?;
        let rows = data["results"]
            .as_array()
            .context("WEB_INVALID_RESPONSE: search results missing")?;
        ensure!(
            rows.len() <= args.count,
            "WEB_INVALID_RESPONSE: result count exceeds the request"
        );
        let mut results = Vec::new();
        for row in rows {
            let title = text(row, "title", 1000)?;
            let url = public_url(&text(row, "url", 8192)?)?;
            let snippet = text(row, "snippet", 4000)?;
            results.push(json!({"title":title,"url":url,"snippet":snippet,
                "date":optional_text(row,"date",100)?,"site_name":optional_text(row,"site_name",500)?}));
        }
        Ok(
            json!({"query":query,"results":results,"provider":self.provider,
            "retrieved_at":chrono::Utc::now().to_rfc3339()}),
        )
    }

    async fn fetch(&self, root: &Path, args: &FetchArgs, url: &str) -> Result<Value> {
        let data = self.post("fetch", json!({"url":url})).await?;
        let page = Page {
            url: url.to_owned(),
            title: text(&data, "title", 1000)?,
            content: text(&data, "content", MAX_RESPONSE_BYTES)?,
            provider: self.provider.clone(),
            retrieved_at: chrono::Utc::now().to_rfc3339(),
        };
        ensure!(
            !page.content.trim().is_empty(),
            "WEB_PAGE_UNAVAILABLE: page returned no usable content"
        );
        let root = root.to_owned();
        let limit = args.max_chars;
        tokio::task::spawn_blocking(move || {
            let bytes = serde_json::to_vec(&page)?;
            let id = digest(&bytes);
            let path = crate::file_edit::workspace_path(
                &root,
                &format!(".codex-workspace-mcp/web/{id}.json"),
            )?;
            std::fs::create_dir_all(path.parent().context("web snapshot directory missing")?)?;
            let lock = crate::file_edit::file_lock(&path);
            let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let previous = crate::file_edit::snapshot(&path)?;
            ensure!(
                previous.as_ref().is_none_or(|data| data == &bytes),
                "WEB_SNAPSHOT_CHANGED: snapshot identity collision"
            );
            crate::file_edit::commit(&path, previous.as_deref(), &bytes)?;
            page_result(&page, &id, 0, limit)
        })
        .await?
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn text(value: &Value, key: &str, limit: usize) -> Result<String> {
    let value = value[key]
        .as_str()
        .with_context(|| format!("WEB_INVALID_RESPONSE: {key} must be text"))?;
    ensure!(
        value.chars().count() <= limit,
        "WEB_INVALID_RESPONSE: {key} exceeds its size limit"
    );
    Ok(value.to_owned())
}

fn optional_text(value: &Value, key: &str, limit: usize) -> Result<Option<String>> {
    if value.get(key).is_none_or(Value::is_null) {
        return Ok(None);
    }
    text(value, key, limit).map(Some)
}

fn public_url(input: &str) -> Result<String> {
    ensure!(input.len() <= 8192, "WEB_INVALID_ARGUMENT: URL is too long");
    let url = reqwest::Url::parse(input)
        .map_err(|_| anyhow::anyhow!("WEB_INVALID_ARGUMENT: invalid URL"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "WEB_INVALID_ARGUMENT: expected an HTTP(S) URL without credentials"
    );
    Ok(url.to_string())
}

fn default_count() -> usize {
    5
}
fn default_page_chars() -> usize {
    DEFAULT_PAGE_CHARS
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_count")]
    count: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchArgs {
    url: String,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default = "default_page_chars")]
    max_chars: usize,
}

#[derive(Serialize, Deserialize)]
struct Page {
    url: String,
    title: String,
    content: String,
    provider: String,
    retrieved_at: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    snapshot: String,
    offset: usize,
}

fn page_result(page: &Page, id: &str, offset: usize, limit: usize) -> Result<Value> {
    let total = page.content.chars().count();
    ensure!(
        offset <= total,
        "WEB_INVALID_CURSOR: offset exceeds the snapshot"
    );
    let content: String = page.content.chars().skip(offset).take(limit).collect();
    let returned = content.chars().count();
    let next = (offset + returned < total).then(|| {
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&Cursor {
                snapshot: id.to_owned(),
                offset: offset + returned,
            })
            .unwrap(),
        )
    });
    Ok(
        json!({"url":page.url,"title":page.title,"content":content,"provider":page.provider,
        "retrieved_at":page.retrieved_at,"snapshot_id":id,"char_offset":offset,
        "returned_chars":returned,"total_chars":total,"complete":next.is_none(),"next_cursor":next}),
    )
}

fn read_page(root: &Path, args: &FetchArgs, url: &str) -> Result<Value> {
    let cursor = args
        .cursor
        .as_deref()
        .context("WEB_INVALID_CURSOR: missing cursor")?;
    ensure!(
        cursor.len() <= 256,
        "WEB_INVALID_CURSOR: cursor is too long"
    );
    let cursor: Cursor = URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .context("WEB_INVALID_CURSOR: invalid cursor")?;
    ensure!(
        cursor.snapshot.len() == 64 && cursor.snapshot.bytes().all(|b| b.is_ascii_hexdigit()),
        "WEB_INVALID_CURSOR: invalid snapshot identity"
    );
    let path = crate::file_edit::workspace_path(
        root,
        &format!(".codex-workspace-mcp/web/{}.json", cursor.snapshot),
    )?;
    ensure!(
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) <= (MAX_RESPONSE_BYTES * 2) as u64,
        "WEB_SNAPSHOT_CHANGED: saved snapshot exceeds its size limit"
    );
    let bytes = std::fs::read(path)
        .context("WEB_SNAPSHOT_UNAVAILABLE: fetch the page again without a cursor")?;
    ensure!(
        digest(&bytes) == cursor.snapshot,
        "WEB_SNAPSHOT_CHANGED: saved page failed integrity verification"
    );
    let page: Page = serde_json::from_slice(&bytes)?;
    ensure!(
        page.url == url,
        "WEB_INVALID_CURSOR: cursor belongs to a different URL"
    );
    page_result(&page, &cursor.snapshot, cursor.offset, args.max_chars)
}

async fn execute(
    workspace: Arc<crate::tools::Workspace>,
    method: &str,
    args: Value,
    http: reqwest::Client,
    config_path: std::path::PathBuf,
) -> Result<Value> {
    // A cursor reads the saved snapshot and never makes another billable request.
    let fetch = if method == "fetch" {
        let args: FetchArgs = serde_json::from_value(args.clone())?;
        ensure!(
            (1..=MAX_PAGE_CHARS).contains(&args.max_chars),
            "WEB_INVALID_ARGUMENT: max_chars must be 1–24000"
        );
        let url = public_url(&args.url)?;
        if args.cursor.is_some() {
            return tokio::task::spawn_blocking(move || read_page(workspace.root(), &args, &url))
                .await?;
        }
        Some((args, url))
    } else {
        ensure!(method == "search", "unknown web method");
        None
    };
    let (provider, key) =
        tokio::task::spawn_blocking(move || crate::ai_proxy::web_search_credentials(&config_path))
            .await??;
    let client = WebClient {
        http,
        base: API_BASE.into(),
        provider,
        key,
    };
    if let Some((args, url)) = fetch {
        client.fetch(workspace.root(), &args, &url).await
    } else {
        client.search(serde_json::from_value(args)?).await
    }
}

pub fn package() -> Result<crate::skill_packages::Package> {
    use crate::plugin_protocol::{NativeContributions, NativeTool, ToolPermission};
    let definitions: Vec<Value> = serde_json::from_str(include_str!("tools.json"))?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut tools = BTreeMap::new();
    for definition in definitions {
        let name = definition["name"]
            .as_str()
            .context("web tool name missing")?
            .to_owned();
        let client = http.clone();
        let handler: crate::builtin_plugins::ToolHandler =
            Arc::new(move |workspace, name, args| {
                let client = client.clone();
                Box::pin(async move {
                    let method = name.strip_prefix("web__").context("invalid web method")?;
                    execute(
                        workspace,
                        method,
                        args,
                        client,
                        crate::ai_proxy::config_path(),
                    )
                    .await
                })
            });
        tools.insert(
            name,
            NativeTool {
                definition,
                handler,
                permission: ToolPermission::ReadOnly,
            },
        );
    }
    let root = std::env::var_os("GROW_RESOURCES_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    crate::skill_packages::load_builtin(
        &root.join("src/builtin_plugins/web"),
        Arc::new(NativeContributions {
            tools,
            frontend_entries: vec![],
        }),
    )
}

#[cfg(test)]
mod tests;
