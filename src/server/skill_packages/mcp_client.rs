//! MCP core client. Configured servers are trusted local integrations, not sandboxes.
use anyhow::{Context, Result, anyhow, bail, ensure};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    sync::{Mutex as AsyncMutex, mpsc},
};
use tokio_util::task::AbortOnDropHandle;

const PROTOCOL: &str = "2025-03-26";
const LEGACY_PROTOCOL: &str = "2024-11-05";
const SESSION: &str = "mcp-session-id";
const VERSION: &str = "mcp-protocol-version";
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const STDERR_BYTES: usize = 4096;

#[derive(Clone, Deserialize)]
pub(super) struct Config {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    pub(super) enabled: Option<bool>,
    initialize_timeout_ms: Option<u64>,
    call_timeout_ms: Option<u64>,
}

impl Config {
    pub(super) fn transport(&self) -> &'static str {
        match (&self.command, &self.url) {
            (Some(_), None) => "stdio",
            (None, Some(_)) => "streamable_http",
            _ => "invalid",
        }
    }
}

pub(super) fn config_path() -> Option<PathBuf> {
    std::env::var_os("QUICKAGENT_MCP_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".config/grow/mcp-servers.json")))
}

// Keep entries independent so a malformed server does not shadow valid siblings.
pub(super) async fn read_config(path: &Path) -> Result<BTreeMap<String, Value>> {
    #[derive(Deserialize)]
    struct File {
        #[serde(rename = "mcpServers", default)]
        servers: BTreeMap<String, Value>,
    }
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(serde_json::from_slice::<File>(&bytes)
            .map_err(|e| {
                anyhow!(
                    "invalid MCP config at line {}, column {}",
                    e.line(),
                    e.column()
                )
            })?
            .servers),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.into()),
    }
}

fn expand(input: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<String> {
    static VAR: OnceLock<regex::Regex> = OnceLock::new();
    let var = VAR.get_or_init(|| regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap());
    let mut expanded = String::new();
    let mut end = 0;
    for capture in var.captures_iter(input) {
        let matched = capture.get(0).unwrap();
        expanded.push_str(&input[end..matched.start()]);
        expanded.push_str(
            &lookup(&capture[1])
                .with_context(|| format!("missing MCP variable {}", &capture[1]))?,
        );
        end = matched.end();
    }
    expanded.push_str(&input[end..]);
    Ok(expanded)
}

fn env_expand(input: &str) -> Result<String> {
    expand(input, |name| std::env::var(name).ok())
}

#[derive(Default)]
pub(super) struct Diagnostics {
    error: Mutex<Option<String>>,
    stderr: Mutex<Vec<u8>>,
}

impl Diagnostics {
    pub(super) fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    pub(super) fn stderr_bytes(&self) -> usize {
        self.stderr.lock().unwrap().len()
    }

    #[cfg(test)]
    pub(super) fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned()
    }

    pub(super) fn disconnect(&self, error: &anyhow::Error) {
        let mut current = self.error.lock().unwrap();
        if current.is_none() {
            let text = format!("{error:#}");
            *current = Some(text[..text.floor_char_boundary(1024)].to_owned());
        }
    }
}

pub(super) struct Connection {
    transport: AsyncMutex<Option<Transport>>,
    diagnostics: Arc<Diagnostics>,
    call_timeout: Duration,
    stopped: tokio_util::sync::CancellationToken,
}

impl Connection {
    pub(super) async fn connect(
        config: &Config,
        diagnostics: Arc<Diagnostics>,
    ) -> Result<(Self, Vec<Value>)> {
        let init_timeout = Duration::from_millis(
            config
                .initialize_timeout_ms
                .unwrap_or(15_000)
                .clamp(1, 300_000),
        );
        let call_timeout =
            Duration::from_millis(config.call_timeout_ms.unwrap_or(60_000).clamp(1, 300_000));
        let (transport, tools) = tokio::time::timeout(init_timeout, async {
            let mut transport = Transport::open(config, diagnostics.clone())?;
            let init = transport
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": PROTOCOL,
                        "capabilities": {},
                        "clientInfo": {"name":"QuickAgent","version":env!("CARGO_PKG_VERSION")}
                    }),
                )
                .await??;
            let version = init["protocolVersion"]
                .as_str()
                .context("missing MCP protocol version")?;
            ensure!(
                matches!(version, PROTOCOL | LEGACY_PROTOCOL),
                "unsupported MCP protocol version"
            );
            if let Wire::Http(http) = &mut transport.wire {
                http.version = Some(HeaderValue::from_str(version)?);
            }
            transport.notify("notifications/initialized").await?;
            let mut tools = Vec::new();
            let mut cursors = BTreeSet::new();
            let mut cursor = None;
            loop {
                let params = cursor.map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
                let mut page = transport.request("tools/list", params).await??;
                let items = page
                    .get_mut("tools")
                    .and_then(Value::as_array_mut)
                    .context("invalid MCP tools/list response")?;
                ensure!(tools.len() + items.len() <= 4096, "too many MCP tools");
                tools.append(items);
                cursor = match page.get("nextCursor") {
                    None | Some(Value::Null) => break,
                    Some(value) => Some(
                        value
                            .as_str()
                            .context("invalid MCP pagination cursor")?
                            .to_owned(),
                    ),
                };
                ensure!(
                    cursors.len() < 100 && cursors.insert(cursor.clone().unwrap()),
                    "invalid or excessive MCP pagination"
                );
            }
            Ok::<_, anyhow::Error>((transport, tools))
        })
        .await
        .context("MCP initialize/discovery timed out")??;
        Ok((
            Self {
                transport: AsyncMutex::new(Some(transport)),
                diagnostics,
                call_timeout,
                stopped: Default::default(),
            },
            tools,
        ))
    }

    pub(super) fn connected(&self) -> bool {
        !self.stopped.is_cancelled() && self.diagnostics.error().is_none()
    }

    pub(super) fn stop(&self) {
        self.stopped.cancel();
        if let Ok(mut slot) = self.transport.try_lock() {
            slot.take();
        }
    }

    pub(super) async fn call(&self, name: &str, args: Value) -> Result<Value> {
        tokio::select! {
            biased;
            _ = self.stopped.cancelled() => bail!("MCP server stopped"),
            result = self.call_connected(name, args) => result,
        }
    }

    async fn call_connected(&self, name: &str, args: Value) -> Result<Value> {
        ensure!(args.is_object(), "MCP tool arguments must be an object");
        // One deadline covers both the queue and the request. Never retry tools/call:
        // they may have performed side effects before their response was lost.
        let deadline = tokio::time::Instant::now() + self.call_timeout;
        let mut slot = tokio::time::timeout_at(deadline, self.transport.lock())
            .await
            .context("MCP call queue timed out")?;
        ensure!(
            self.connected(),
            "MCP server disconnected; reconnect from plugin settings"
        );
        let transport = slot.take().context("MCP server disconnected")?;
        // Dropping a partially written stdio request must not leave the connection reusable.
        struct InFlight<'a> {
            wire: Option<Transport>,
            diagnostics: &'a Diagnostics,
        }
        impl Drop for InFlight<'_> {
            fn drop(&mut self) {
                if self.wire.is_some() {
                    self.diagnostics
                        .disconnect(&anyhow!("MCP request cancelled; reconnect required"));
                }
            }
        }
        let mut flight = InFlight {
            wire: Some(transport),
            diagnostics: &self.diagnostics,
        };
        let response = tokio::time::timeout_at(
            deadline,
            flight
                .wire
                .as_mut()
                .unwrap()
                .request("tools/call", json!({"name":name,"arguments":args})),
        )
        .await
        .context("MCP tools/call timed out")
        .and_then(|result| result);
        match response {
            Ok(result) => {
                *slot = flight.wire.take();
                // Keep the MCP result envelope, including content, structuredContent and isError.
                let value = result?;
                ensure!(value.is_object(), "invalid MCP tools/call result");
                Ok(value)
            }
            Err(error) => {
                self.diagnostics.disconnect(&error);
                Err(error)
            }
        }
    }
}

struct StdioWire {
    _child: AbortOnDropHandle<()>,
    stdin: ChildStdin,
    messages: mpsc::Receiver<Value>,
    _stdout: AbortOnDropHandle<()>,
    _stderr: AbortOnDropHandle<()>,
}

impl StdioWire {
    fn open(config: &Config, diagnostics: Arc<Diagnostics>) -> Result<Self> {
        let mut command = Command::new(env_expand(config.command.as_deref().unwrap())?);
        for arg in &config.args {
            command.arg(env_expand(arg)?);
        }
        command.env_clear();
        for key in [
            "PATH",
            "HOME",
            "LANG",
            "TMPDIR",
            "SystemRoot",
            "USERPROFILE",
            "TEMP",
            "TMP",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        for (key, value) in &config.env {
            command.env(key, env_expand(value)?);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("could not spawn MCP server")?;
        let stdin = child.stdin.take().context("missing MCP stdin")?;
        let mut stdout = BufReader::new(child.stdout.take().context("missing MCP stdout")?);
        let mut stderr = child.stderr.take().context("missing MCP stderr")?;
        let (tx, messages) = mpsc::channel(4);
        let stdout_diag = diagnostics.clone();
        let exit_diag = diagnostics.clone();
        let wait_child = AbortOnDropHandle::new(tokio::spawn(async move {
            let error = match child.wait().await {
                Ok(status) => anyhow!("MCP subprocess exited: {status}"),
                Err(error) => anyhow!("MCP subprocess wait failed: {error}"),
            };
            exit_diag.disconnect(&error);
        }));
        let read_stdout = AbortOnDropHandle::new(tokio::spawn(async move {
            let result: Result<()> = async {
                loop {
                    let mut line = Vec::new();
                    let count = (&mut stdout)
                        .take((MAX_MESSAGE + 1) as u64)
                        .read_until(b'\n', &mut line)
                        .await?;
                    ensure!(count != 0, "MCP stdio closed");
                    ensure!(count <= MAX_MESSAGE, "MCP stdio message too large");
                    let message =
                        serde_json::from_slice(&line).context("invalid MCP stdio JSON")?;
                    if tx.send(message).await.is_err() {
                        return Ok(());
                    }
                }
            }
            .await;
            if let Err(error) = result {
                stdout_diag.disconnect(&error);
            }
        }));
        let read_stderr = AbortOnDropHandle::new(tokio::spawn(async move {
            let mut buffer = [0; STDERR_BYTES];
            while let Ok(count @ 1..) = stderr.read(&mut buffer).await {
                let mut tail = diagnostics.stderr.lock().unwrap();
                let drop = (tail.len() + count).saturating_sub(STDERR_BYTES);
                tail.drain(..drop);
                tail.extend_from_slice(&buffer[..count]);
            }
        }));
        Ok(Self {
            _child: wait_child,
            stdin,
            messages,
            _stdout: read_stdout,
            _stderr: read_stderr,
        })
    }

    async fn send(&mut self, value: &Value) -> Result<()> {
        let mut data = serde_json::to_vec(value)?;
        data.push(b'\n');
        self.stdin
            .write_all(&data)
            .await
            .context("MCP stdio write failed")?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn receive(&mut self, id: u64) -> Result<Value> {
        while let Some(message) = self.messages.recv().await {
            if message.get("method").is_some() {
                if let Some(id) = message.get("id") {
                    let response = if message["method"] == "ping" {
                        json!({"jsonrpc":"2.0","id":id,"result":{}})
                    } else {
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not supported"}})
                    };
                    self.send(&response).await?;
                }
            } else if message["id"] == id {
                return Ok(message);
            }
        }
        bail!("MCP stdio closed before response")
    }
}

struct HttpWire {
    client: reqwest::Client,
    url: reqwest::Url,
    headers: HeaderMap,
    session: Option<HeaderValue>,
    version: Option<HeaderValue>,
}

impl HttpWire {
    fn open(config: &Config) -> Result<Self> {
        let url = reqwest::Url::parse(&env_expand(config.url.as_deref().unwrap())?)
            .context("invalid MCP URL")?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            "invalid MCP HTTP endpoint"
        );
        let mut headers = HeaderMap::new();
        for (name, value) in &config.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .context("invalid MCP request header name")?;
            ensure!(
                !matches!(
                    name.as_str(),
                    SESSION
                        | VERSION
                        | "accept"
                        | "content-type"
                        | "host"
                        | "content-length"
                        | "transfer-encoding"
                ),
                "reserved MCP request header: {name}"
            );
            let mut value = HeaderValue::from_str(&env_expand(value)?)
                .context("invalid MCP request header value")?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            url,
            headers,
            session: None,
            version: None,
        })
    }

    async fn post(&mut self, request: &Value, id: Option<u64>) -> Result<Option<Value>> {
        let mut builder = self
            .client
            .post(self.url.clone())
            .headers(self.headers.clone())
            .header(ACCEPT, "application/json, text/event-stream")
            .json(request);
        if let Some(session) = &self.session {
            builder = builder.header(SESSION, session);
        }
        if let Some(version) = &self.version {
            builder = builder.header(VERSION, version);
        }
        let mut response = builder
            .send()
            .await
            .map_err(|e| e.without_url())
            .context("MCP HTTP request failed")?;
        ensure!(
            response.status().is_success(),
            "MCP HTTP returned status {}",
            response.status()
        );
        if let Some(session) = response.headers().get(SESSION) {
            ensure!(
                !session.is_empty() && session.as_bytes().iter().all(|b| (0x21..=0x7e).contains(b)),
                "invalid MCP session ID"
            );
            let mut session = session.clone();
            session.set_sensitive(true);
            self.session = Some(session);
        }
        let Some(id) = id else { return Ok(None) };
        let mime = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let mut size = 0;
        let mut json = Vec::new();
        let mut sse = Sse::default();
        ensure!(
            matches!(mime.as_str(), "application/json" | "text/event-stream"),
            "unsupported MCP response content type"
        );
        while let Some(chunk) = response.chunk().await.map_err(|e| e.without_url())? {
            size += chunk.len();
            ensure!(size <= MAX_MESSAGE, "MCP HTTP response too large");
            if mime == "application/json" {
                json.extend_from_slice(&chunk);
            } else if let Some(message) = sse.push(&chunk, id)? {
                return Ok(Some(message));
            }
        }
        if mime == "application/json" {
            let message: Value = serde_json::from_slice(&json).context("invalid MCP HTTP JSON")?;
            ensure!(message["id"] == id, "unexpected MCP response ID");
            return Ok(Some(message));
        }
        bail!("MCP SSE ended before matching response")
    }
}

#[derive(Default)]
struct Sse {
    line: Vec<u8>,
    data: Vec<u8>,
    after_cr: bool,
    started: bool,
}

impl Sse {
    fn push(&mut self, chunk: &[u8], id: u64) -> Result<Option<Value>> {
        for &byte in chunk {
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            if !matches!(byte, b'\r' | b'\n') {
                self.line.push(byte);
                continue;
            }
            self.after_cr = byte == b'\r';
            let line = if self.started {
                &self.line
            } else {
                self.started = true;
                self.line
                    .strip_prefix(b"\xef\xbb\xbf")
                    .unwrap_or(&self.line)
            };
            if line.is_empty() && !self.data.is_empty() {
                if self.data.iter().all(u8::is_ascii_whitespace) {
                    self.data.clear();
                    self.line.clear();
                    continue;
                }
                let message: Value =
                    serde_json::from_slice(&self.data).context("invalid MCP SSE data")?;
                self.data.clear();
                if message.get("method").is_none() && message["id"] == id {
                    return Ok(Some(message));
                }
            } else if let Some(data) = line.strip_prefix(b"data:") {
                self.data
                    .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
                self.data.push(b'\n');
            }
            self.line.clear();
        }
        Ok(None)
    }
}

enum Wire {
    Stdio(StdioWire),
    Http(HttpWire),
}

struct Transport {
    wire: Wire,
    id: u64,
}

impl Transport {
    fn open(config: &Config, diagnostics: Arc<Diagnostics>) -> Result<Self> {
        let wire = match config.transport() {
            "stdio" => Wire::Stdio(StdioWire::open(config, diagnostics)?),
            "streamable_http" => Wire::Http(HttpWire::open(config)?),
            _ => bail!("MCP server requires exactly one of command or url"),
        };
        Ok(Self { wire, id: 0 })
    }

    async fn notify(&mut self, method: &str) -> Result<()> {
        let value = json!({"jsonrpc":"2.0","method":method});
        match &mut self.wire {
            Wire::Stdio(stdio) => stdio.send(&value).await,
            Wire::Http(http) => http.post(&value, None).await.map(|_| ()),
        }
    }

    // Outer errors indicate the connection is no longer usable; inner errors are
    // JSON-RPC errors from a healthy peer, not transport failures.
    async fn request(&mut self, method: &str, params: Value) -> Result<Result<Value>> {
        self.id += 1;
        let id = self.id;
        let value = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let mut response = match &mut self.wire {
            Wire::Stdio(stdio) => {
                stdio.send(&value).await?;
                stdio.receive(id).await?
            }
            Wire::Http(http) => http
                .post(&value, Some(id))
                .await?
                .context("missing MCP HTTP response")?,
        };
        ensure!(
            response["jsonrpc"] == "2.0" && response.get("method").is_none(),
            "invalid MCP JSON-RPC envelope"
        );
        match (response.get("result"), response.get("error")) {
            (Some(_), None) => Ok(Ok(response["result"].take())),
            (None, Some(error)) if error["code"].is_i64() && error["message"].is_string() => {
                // Discovery failures go to admin status and logs: do not copy arbitrary peer text there.
                let code = error["code"].as_i64().unwrap();
                let message = error["message"].as_str().unwrap();
                if method == "tools/call" {
                    Ok(Err(anyhow!(
                        "MCP RPC error {code}: {}",
                        &message[..message.floor_char_boundary(1024)]
                    )))
                } else {
                    Ok(Err(anyhow!("MCP {method} rejected with RPC error {code}")))
                }
            }
            _ => bail!("invalid MCP JSON-RPC response"),
        }
    }
}
