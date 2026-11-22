use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{error, info, warn};

use crate::go_index::{
    IndexGoWorkspaceRequest, ListGoSymbolsRequest, ReadGoSymbolRequest, SearchGoSymbolsRequest,
};
use crate::memory::{
    ListArchitectureMemoryRequest, ListSymbolBusinessContextRequest, ListWorkMemoryRequest,
    RecordArchitectureMemoryRequest, RecordSymbolBusinessContextRequest, RecordWorkMemoryRequest,
    SearchArchitectureMemoryRequest, SearchSymbolBusinessContextRequest, SearchWorkMemoryRequest,
};
use crate::python_index::{
    IndexPythonWorkspaceRequest, ListPythonSymbolsRequest, ReadPythonSymbolRequest,
    SearchPythonSymbolsRequest,
};
use crate::rust_index::{
    IndexRustWorkspaceRequest, ListRustSymbolsRequest, ReadRustSymbolRequest,
    SearchRustSymbolsRequest,
};
use crate::tools::{
    CreateDirRequest, DeletePathRequest, EditFileRequest, GitDiffRequest, GitLogRequest,
    GitStatusRequest, ListDirRequest, ReadFileChunkRequest, ReadFileLinesRequest, ReadFileRequest,
    RenamePathRequest, ReplaceRangeRequest, SearchTextRequest, Workspace, WorkspaceInfoRequest,
    WriteFileRequest,
};
use crate::ts_index::{
    IndexTsWorkspaceRequest, ListTsSymbolsRequest, ReadTsSymbolRequest, SearchTsSymbolsRequest,
};

pub async fn handle_mcp_get(headers: HeaderMap) -> Response {
    if !valid_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(id) = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
    else {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", "POST, DELETE")],
            "Initialize an MCP session before opening its notification stream",
        )
            .into_response();
    };
    let (receiver, cancel) = {
        let mut sessions = sessions().lock().unwrap();
        sessions.retain(|_, session| session.last_used.elapsed() < SESSION_IDLE_TIMEOUT);
        let Some(session) = sessions.get_mut(id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        session.last_used = std::time::Instant::now();
        session.stream.cancel();
        session.stream = tokio_util::sync::CancellationToken::new();
        (session.changed.subscribe(), session.stream.clone())
    };
    let stream = futures::stream::unfold((receiver, cancel), |(mut receiver, cancel)| async move {
        tokio::select! {
            _ = cancel.cancelled() => None,
            changed = receiver.changed() => {
                if changed.is_err() { return None; }
                Some((Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().event("message").data(r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#)), (receiver, cancel)))
            }
        }
    });
    axum::response::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}
fn valid_origin(headers: &HeaderMap) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
    {
        return false;
    }
    let Some(origin) = headers.get("origin") else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    if let Some(host) = headers.get("host").and_then(|value| value.to_str().ok())
        && (origin == format!("http://{host}") || origin == format!("https://{host}"))
    {
        return true;
    }
    crate::process_sandbox::is_loopback_origin(origin)
}
pub fn notify_tools_changed() {
    for session in sessions().lock().unwrap().values() {
        session.changed.send_modify(|seq| *seq += 1);
    }
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default)]
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Default)]
struct SkillActivation {
    active: std::collections::BTreeSet<String>,
    scopes: std::collections::BTreeMap<String, crate::plugin_runtime::ScopeId>,
    shell: McpShellScope,
}

struct McpShellScope {
    task: String,
    cancel: tokio_util::sync::CancellationToken,
    state: Option<crate::agent_service::AgentServiceState>,
}
struct McpShellContext {
    state: crate::agent_service::AgentServiceState,
    task: String,
    cancel: tokio_util::sync::CancellationToken,
}
impl Default for McpShellScope {
    fn default() -> Self {
        Self {
            task: format!("mcp_shell_{}", crate::agent_service::uuid_like()),
            cancel: tokio_util::sync::CancellationToken::new(),
            state: None,
        }
    }
}
impl Drop for McpShellScope {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl McpShellScope {
    async fn context(
        &mut self,
        workspace: &Arc<Workspace>,
    ) -> anyhow::Result<McpShellContext> {
        anyhow::ensure!(!self.cancel.is_cancelled(), "MCP shell session is closed");
        if self.state.is_none() {
            let state = crate::agent_service::AgentServiceState {
                workspace: workspace.clone(),
                model_client: crate::model_transport::client()?,
                model_request_slots: Arc::new(tokio::sync::Semaphore::new(1)),
                provider_url: String::new(),
                api_key: String::new(),
                provider_name: String::new(),
                provider_routes: Default::default(),
                default_model: String::new(),
                model_map: Default::default(),
                reasoning_effort: None,
                fast_mode: false,
                // External MCP keeps the same host access as run_program.
                permission_mode: crate::agent_service::PermissionMode::FullAccess,
                visual: Default::default(),
                visual_tool_settings: Default::default(),
                image_generation_settings: Default::default(),
                delegate_agent_settings: Default::default(),
                config: None,
                config_path: None,
                tool_catalog: None,
                plugin_cancel: self.cancel.clone(),
                cancellations: Arc::default(),
            };
            let root = workspace.root().to_path_buf();
            let task = self.task.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let conn = crate::agent_service::open_db(&root)?;
                // Blank prompt, terminal status and no user/message keep this
                // event/asset owner out of the normal UI conversation list.
                conn.execute(
                    "INSERT OR IGNORE INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES (?1,'','','completed',?2,?2)",
                    rusqlite::params![task, crate::agent_service::now()],
                )?;
                Ok(())
            }).await??;
            self.state = Some(state);
        }
        let state = self.state.as_ref().unwrap();
        anyhow::ensure!(
            state.workspace.root().canonicalize()? == workspace.root().canonicalize()?,
            "MCP shell session belongs to another workspace"
        );
        anyhow::ensure!(!self.cancel.is_cancelled(), "MCP shell session is closed");
        Ok(McpShellContext {
            state: state.clone(),
            task: self.task.clone(),
            cancel: self.cancel.clone(),
        })
    }
}
impl McpShellContext {
    async fn execute(&self, workspace: &Arc<Workspace>, name: &str, args: &Value) -> anyhow::Result<Value> {
        // MCP has no automatic user-turn signal. Keep one execution turn per
        // session so its background shells can coexist with later commands.
        crate::shell_process::execute(
            &self.state, workspace, &self.task, 1, None, name, args, &self.cancel, false,
        ).await
    }
}
struct McpSession {
    last_used: std::time::Instant,
    skills: Arc<tokio::sync::Mutex<SkillActivation>>,
    changed: tokio::sync::watch::Sender<u64>,
    stream: tokio_util::sync::CancellationToken,
    cancel: tokio_util::sync::CancellationToken,
}
impl Drop for McpSession {
    fn drop(&mut self) {
        self.stream.cancel();
        self.cancel.cancel();
    }
}
static MCP_SESSIONS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::BTreeMap<String, McpSession>>,
> = std::sync::OnceLock::new();
fn sessions() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, McpSession>> {
    MCP_SESSIONS.get_or_init(Default::default)
}
const SESSION_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(7200);

// Expiration must also work when a disconnected client never sends another RPC.
async fn expire_session(id: String, cancel: tokio_util::sync::CancellationToken) {
    expire_session_with_timeout(id,cancel,SESSION_IDLE_TIMEOUT).await;
}
async fn expire_session_with_timeout(id: String, cancel: tokio_util::sync::CancellationToken, timeout: std::time::Duration) {
    loop {
        let remaining = {
            let mut sessions = sessions().lock().unwrap();
            let Some(session) = sessions.get(&id) else { return };
            let remaining = timeout.saturating_sub(session.last_used.elapsed());
            if remaining.is_zero() {
                sessions.remove(&id);
                return;
            }
            remaining
        };
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(remaining) => {},
        }
    }
}
fn session(
    headers: &HeaderMap,
) -> anyhow::Result<Option<Arc<tokio::sync::Mutex<SkillActivation>>>> {
    let Some(id) = headers.get("mcp-session-id") else {
        return Ok(None);
    };
    let id = id.to_str()?;
    let mut sessions = sessions().lock().unwrap();
    sessions.retain(|_, session| session.last_used.elapsed() < SESSION_IDLE_TIMEOUT);
    let session = sessions.get_mut(id).ok_or_else(|| {
        anyhow::anyhow!("MCP session expired or unknown; initialize a new session")
    })?;
    session.last_used = std::time::Instant::now();
    Ok(Some(session.skills.clone()))
}
pub async fn handle_mcp_delete(headers: HeaderMap) -> Response {
    if !valid_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(id) = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if sessions().lock().unwrap().remove(id).is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

pub async fn handle_mcp(
    State(workspace): State<Arc<Workspace>>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Response {
    if !valid_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    info!(body = %message, "mcp POST received");

    if message.is_array() {
        warn!("mcp batch request rejected");
        return json_error(None, -32600, "JSON-RPC batches are not supported yet");
    }

    let request = match serde_json::from_value::<JsonRpcRequest>(message) {
        Ok(request) => request,
        Err(error) => {
            error!(%error, "mcp request parse failed");
            return json_error(None, -32700, &format!("invalid JSON-RPC message: {error}"));
        }
    };

    if request.method.is_none() {
        info!(id = ?request.id, "mcp response/empty message acknowledged");
        return StatusCode::ACCEPTED.into_response();
    }

    let id = request.id.clone();
    let is_notification = id.is_none();
    info!(id = ?id, method = ?request.method, notification = is_notification, "mcp json-rpc dispatch");
    let mut new_session = None;
    let activation = if request.method.as_deref() == Some("initialize") {
        if request.jsonrpc.as_deref() != Some("2.0") || is_notification {
            return json_error(id, -32600, "initialize requires JSON-RPC 2.0 and an id");
        }
        let mut sessions = sessions().lock().unwrap();
        sessions.retain(|_, session| session.last_used.elapsed() < SESSION_IDLE_TIMEOUT);
        if sessions.len() >= 1024 {
            return json_error(id, -32000, "MCP session capacity reached");
        }
        let mut random = [0u8; 32];
        if getrandom::fill(&mut random).is_err() {
            return json_error(id, -32000, "could not generate an MCP session ID");
        }
        let token = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let activation = SkillActivation::default();
        let cancel = activation.shell.cancel.clone();
        let skills = Arc::new(tokio::sync::Mutex::new(activation));
        sessions.insert(
            token.clone(),
            McpSession {
                last_used: std::time::Instant::now(),
                skills: skills.clone(),
                changed: tokio::sync::watch::channel(0).0,
                stream: tokio_util::sync::CancellationToken::new(),
                cancel: cancel.clone(),
            },
        );
        tokio::spawn(expire_session(token.clone(), cancel));
        new_session = Some(token);
        Some(skills)
    } else {
        match session(&headers) {
            Ok(session) => session,
            Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
        }
    };
    let changed = request.method.as_deref() == Some("tools/call")
        && matches!(
            request.params["name"].as_str(),
            Some("read_skill" | "unload_skill" | "reset_skill_activation")
        );
    let result = dispatch(&workspace, request, activation.as_ref()).await;
    if result.is_ok()
        && changed
        && let Some(id) = headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
    {
        if let Some(session) = sessions().lock().unwrap().get(id) {
            session.changed.send_modify(|seq| *seq += 1);
        }
    }
    if is_notification {
        return match result {
            Ok(_) => {
                info!("mcp notification accepted");
                StatusCode::ACCEPTED.into_response()
            }
            Err(error) => {
                error!(%error, "mcp notification failed");
                json_error(None, -32000, &error.to_string())
            }
        };
    }

    let response = Json(match result {
        Ok(result) => {
            info!(id = ?id, "mcp json-rpc success");
            JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(result),
                error: None,
            }
        }
        Err(error) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: {
                error!(%error, "mcp json-rpc failed");
                Some(JsonRpcError {
                    code: -32000,
                    message: error.to_string(),
                })
            },
        },
    })
    .into_response();
    if let Some(token) = new_session {
        let mut response = response;
        response
            .headers_mut()
            .insert("mcp-session-id", token.parse().unwrap());
        response
    } else {
        response
    }
}

fn mcp_value(value: Value) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value})
}

const RETIRED_CODE_INDEX_TOOLS: &[&str] = &[
    "search_code_map",
    "index_go_workspace", "go_index_status", "list_go_symbols", "search_go_symbols", "read_go_symbol",
    "index_rust_workspace", "rust_index_status", "list_rust_symbols", "search_rust_symbols", "read_rust_symbol",
    "index_ts_workspace", "ts_index_status", "list_ts_symbols", "search_ts_symbols", "read_ts_symbol",
    "index_python_workspace", "python_index_status", "list_python_symbols", "search_python_symbols", "read_python_symbol",
];

async fn dispatch(
    workspace: &Arc<Workspace>,
    request: JsonRpcRequest,
    activation: Option<&Arc<tokio::sync::Mutex<SkillActivation>>>,
) -> anyhow::Result<Value> {
    anyhow::ensure!(request.jsonrpc.as_deref() == Some("2.0"), "jsonrpc must be \"2.0\"");
    if request.method.as_deref() == Some("tools/call")
        && let Some(name) = request.params["name"].as_str().filter(|name| crate::shell_process::is_tool(name))
    {
        let args = request.params.get("arguments").cloned().unwrap_or(json!({}));
        let context = {
            let activation = activation.ok_or_else(|| {
                anyhow::anyhow!("TOOL_UNAVAILABLE: initialize an MCP session before using shell tools")
            })?;
            let mut activation = activation.lock().await;
            activation.shell.context(workspace).await?
        };
        return Ok(mcp_value(context.execute(workspace, name, &args).await?));
    }
    // Preserve skill activation and package scopes under the existing lock for
    // every other RPC. Shell I/O alone runs outside it so kill can interrupt it.
    let mut activation = if let Some(activation) = activation {
        Some(activation.lock().await)
    } else {
        None
    };
    dispatch_activated(workspace, request, activation.as_deref_mut()).await
}

async fn dispatch_activated(
    workspace: &Arc<Workspace>,
    mut request: JsonRpcRequest,
    mut activation: Option<&mut SkillActivation>,
) -> anyhow::Result<Value> {
    if request.jsonrpc.as_deref() != Some("2.0") {
        anyhow::bail!("jsonrpc must be \"2.0\"");
    }

    let method = request
        .method
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing method"))?;

    match method {
        "initialize" => {
            let protocol_version = request
                .params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-06-18");
            Ok(json!({
                "protocolVersion": protocol_version,
                "serverInfo": {
                    "name": "codex-workspace-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "tools": {"listChanged": true},
                    "resources": {}
                }
            }))
        }
        "tools/list" => {
            let catalog = crate::skill_packages::root_catalog();
            let mut definitions = tool_definitions()
                .as_array()
                .unwrap()
                .iter()
                .filter(|tool| {
                    let name = tool["name"].as_str().unwrap_or("");
                    catalog
                        .as_ref()
                        .is_none_or(|catalog| !catalog.packages.declares_tool(name))
                })
                .cloned()
                .collect::<Vec<_>>();
            if let (Some(catalog), Some(activation)) = (&catalog, activation.as_deref()) {
                let active = catalog
                    .packages
                    .valid_active(&activation.active, &activation.scopes);
                definitions.extend(catalog.packages.tools(&active).into_iter().map(|tool| json!({"name":tool["function"]["name"],"description":tool["function"]["description"],"inputSchema":tool["function"]["parameters"]})));
            }
            // Only this code-facing, scoped catalog needs to resolve a shell.
            // Registration and stateless listings must not probe the host.
            let shell_available = activation.as_deref().is_some_and(|a| !a.shell.cancel.is_cancelled())
                && crate::shell_environment::resolve().is_ok();
            definitions.retain(|tool| {
                !crate::shell_process::is_tool(tool["name"].as_str().unwrap_or("")) || shell_available
            });
            Ok(json!({"tools":definitions}))
        }
        "tools/call" => {
            // An MCP client is never program code: it gets an AI's pages.
            if let Some(arguments) = request.params.get_mut("arguments") {
                crate::source_read::unmark(arguments);
            }
            let name = request.params["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("tool name required"))?;
            let args = request
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(json!({}));
            if name == "unload_skill" || name == "reset_skill_activation" {
                let activation = activation.as_deref_mut().ok_or_else(|| {
                    anyhow::anyhow!("initialize an MCP session before changing skill activation")
                })?;
                if name == "reset_skill_activation" {
                    activation.active.clear();
                    activation.scopes.clear();
                } else {
                    let skill = args["name"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("skill name required"))?;
                    activation.active.remove(skill);
                    activation.scopes.remove(skill);
                }
                return Ok(mcp_value(json!({"tools_hidden":true})));
            }
            if let Some(catalog) = crate::skill_packages::root_catalog() {
                if name == "read_skill"
                    && let Some(result) =
                        crate::skill_packages::root_read(args["name"].as_str().unwrap_or(""))
                {
                    let activation = activation.as_deref_mut().ok_or_else(|| {
                        anyhow::anyhow!("initialize an MCP session before loading package tools")
                    })?;
                    let result = result?;
                    let skill = result["skill"].as_str().unwrap().to_owned();
                    activation.active.insert(skill.clone());
                    activation.scopes.insert(
                        skill,
                        result["package_scope"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("package activation version missing"))?,
                    );
                    return Ok(mcp_value(result));
                }
                if catalog.packages.declares_tool(name) {
                    let activation = activation.as_deref().ok_or_else(|| {
                        anyhow::anyhow!("TOOL_UNAVAILABLE: load the owning skill in an MCP session")
                    })?;
                    let active = catalog
                        .packages
                        .valid_active(&activation.active, &activation.scopes);
                    anyhow::ensure!(
                        catalog
                            .packages
                            .tools(&active)
                            .iter()
                            .any(|tool| tool["function"]["name"] == name),
                        "TOOL_UNAVAILABLE: load the owning skill"
                    );
                    return Ok(mcp_value(
                        catalog.execute(workspace.clone(), name, args).await?,
                    ));
                }
            }
            anyhow::ensure!(
                !RETIRED_CODE_INDEX_TOOLS.contains(&name),
                "TOOL_UNAVAILABLE: {name} has been retired; use search_text and read_file_lines"
            );
            call_tool(workspace, request.params).await
        }
        "notifications/initialized" => Ok(json!({})),
        "ping" => Ok(json!({})),
        "resources/list" => {
            let w_root = workspace.root().display().to_string();
            let mut resources = Vec::new();

            // If work memory has records, expose it as a timeline resource
            if let Ok(st) = workspace.list_work_memory(ListWorkMemoryRequest {
                workspace_root: w_root.clone(),
                limit: 100,
            }) {
                if !st.memories.is_empty() {
                    resources.push(json!({
                        "uri": "mcp://codex-workspace-mcp/work-memory",
                        "name": "Developer Work Memory Timeline",
                        "mimeType": "text/plain",
                        "description": "Read the timeline of recorded memories, changes, test logs, and risks in this project workspace."
                    }));
                }
            }

            if let Ok(st) = workspace.list_architecture_memory(ListArchitectureMemoryRequest {
                workspace_root: w_root.clone(),
                limit: 100,
            }) {
                if !st.memories.is_empty() {
                    resources.push(json!({
                        "uri": "mcp://codex-workspace-mcp/architecture-memory",
                        "name": "Architecture Memory",
                        "mimeType": "text/plain",
                        "description": "Read durable feature/logic area summaries, key symbols, boundaries, common tasks, and risks for this workspace."
                    }));
                }
            }

            Ok(json!({
                "resources": resources
            }))
        }
        "resources/read" => {
            let uri = request
                .params
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("resources/read requires params.uri"))?;

            let w_root = workspace.root().display().to_string();

            let text_content = match uri {
                "mcp://codex-workspace-mcp/work-memory" => {
                    let st = workspace.list_work_memory(ListWorkMemoryRequest {
                        workspace_root: w_root.clone(),
                        limit: 100,
                    })?;
                    let mut md = String::from("# Developer Work Memory Timeline\n\n");
                    if st.memories.is_empty() {
                        md.push_str("No memories have been recorded in this workspace yet. You can use the `record_work_memory` tool to log your work progress, files changed, and risks.\n");
                    } else {
                        for (idx, mem) in st.memories.iter().enumerate() {
                            let local_time = chrono::DateTime::from_timestamp(mem.time_unix as i64, 0)
                                .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
                                .unwrap_or_else(|| format!("Unix Epoch {}", mem.time_unix));

                            md.push_str(&format!("### Memory #{}: {}\n", st.memories.len() - idx, mem.summary));
                            md.push_str(&format!("- **Recorded Time**: {}\n", local_time));
                            if mem.kind == "observer" {
                                md.push_str("- **Recorded By**: Observer\n");
                            }
                            if let Some(task_id) = &mem.source_task_id {
                                md.push_str(&format!("- **Source Task**: {}\n", task_id));
                            }
                            if !mem.applies_to.is_empty() {
                                md.push_str(&format!("- **Applies To**: {}\n", mem.applies_to));
                            }
                            if !mem.source_refs.is_empty() {
                                md.push_str(&format!("- **Source Paths/Symbols**: {}\n", mem.source_refs.join(", ")));
                            }
                            if !mem.files_changed.is_empty() {
                                md.push_str(&format!("- **Files Changed**:\n  - {}\n", mem.files_changed.join("\n  - ")));
                            }
                            if !mem.implementation.is_empty() {
                                md.push_str(&format!("- **Implementation Details**:\n  > {}\n", mem.implementation.replace("\n", "\n  > ")));
                            }
                            if !mem.tests.is_empty() {
                                md.push_str(&format!("- **Tests Run**:\n  > {}\n", mem.tests.replace("\n", "\n  > ")));
                            }
                            if !mem.risks.is_empty() {
                                md.push_str(&format!("- **Potential Risks & Blockers**:\n  > {}\n", mem.risks.replace("\n", "\n  > ")));
                            }
                            md.push_str("\n---\n\n");
                        }
                    }
                    md
                }
                "mcp://codex-workspace-mcp/architecture-memory" => {
                    let st = workspace.list_architecture_memory(ListArchitectureMemoryRequest {
                        workspace_root: w_root.clone(),
                        limit: 100,
                    })?;
                    let mut md = String::from("# Architecture Memory\n\n");
                    if st.memories.is_empty() {
                        md.push_str("No architecture memory has been recorded in this workspace yet. Use `record_architecture_memory` after verifying a feature's key symbols and boundaries.\n");
                    } else {
                        for mem in st.memories {
                            let updated = chrono::DateTime::from_timestamp(mem.updated_at_unix as i64, 0)
                                .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
                                .unwrap_or_else(|| format!("Unix Epoch {}", mem.updated_at_unix));
                            md.push_str(&format!("## {}\n", mem.area));
                            md.push_str(&format!("- **Updated**: {}\n", updated));
                            md.push_str(&format!("- **Summary**: {}\n", mem.summary));
                            if !mem.key_symbols.is_empty() {
                                md.push_str(&format!("- **Key Symbols**:\n  - {}\n", mem.key_symbols.join("\n  - ")));
                            }
                            if !mem.key_files.is_empty() {
                                md.push_str(&format!("- **Key Files**:\n  - {}\n", mem.key_files.join("\n  - ")));
                            }
                            if !mem.common_tasks.is_empty() {
                                md.push_str(&format!("- **Common Tasks**:\n  - {}\n", mem.common_tasks.join("\n  - ")));
                            }
                            if !mem.boundaries.is_empty() {
                                md.push_str(&format!("- **Boundaries**:\n  > {}\n", mem.boundaries.replace("\n", "\n  > ")));
                            }
                            if !mem.risks.is_empty() {
                                md.push_str(&format!("- **Risks**:\n  > {}\n", mem.risks.replace("\n", "\n  > ")));
                            }
                            md.push('\n');
                        }
                    }
                    md
                }
                _ => anyhow::bail!("unknown resource: {}", uri),
            };

            Ok(json!({
                "contents": [
                    {
                        "uri": uri,
                        "mimeType": "text/plain",
                        "text": text_content
                    }
                ]
            }))
        }
        "resources/templates/list" => Ok(json!({
            "resourceTemplates": []
        })),
        "prompts/list" => Ok(json!({
            "prompts": []
        })),
        _ => anyhow::bail!("unknown method: {}", method),
    }
}

fn json_error(id: Option<Value>, code: i64, message: &str) -> Response {
    Json(JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
        }),
    })
    .into_response()
}

pub async fn call_tool(workspace: &Workspace, params: Value) -> anyhow::Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("tools/call requires params.name"))?;

    anyhow::ensure!(
        !crate::shell_process::is_tool(name),
        "TOOL_UNAVAILABLE: shell tools require a scoped MCP session"
    );

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    if name == "run_command" {
        let disabled = json!({"outcome":"disabled","error":"run_command has been removed and was not executed. Use run_program with a supported program and an args array, or use a dedicated project, file, HTTP, or browser tool."});
        return Ok(
            json!({"content":[{"type":"text","text":disabled.to_string()}],"structuredContent":disabled}),
        );
    }

    if crate::program_execution::is_tool(name) {
        let value = crate::program_execution::execute(workspace, &arguments).await?;
        return Ok(
            json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&value)?}],"structuredContent":value}),
        );
    }

    if crate::git_tools::is_tool(name) {
        let boundary = crate::process_sandbox::current_tool_sandbox().map_or(crate::process_sandbox::ExecutionBoundary::Host,|ctx|ctx.boundary);
        let value = crate::git_tools::execute(workspace,name,&arguments,boundary,&tokio_util::sync::CancellationToken::new()).await?;
        return Ok(json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value}));
    }
    if name == crate::batch_edits::NAME {
        let workspace = Workspace::new(workspace.root())?;
        let value = tokio::task::spawn_blocking(move ||crate::batch_edits::apply(&workspace,&arguments)).await??;
        return Ok(json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value}));
    }

    if crate::plan_tools::is_tool(name) {
        let value = crate::plan_tools::execute(name, arguments).await?;
        return Ok(json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value}));
    }

    if crate::verify_tools::is_tool(name) {
        let boundary = crate::process_sandbox::current_tool_sandbox().map_or(crate::process_sandbox::ExecutionBoundary::Host, |ctx| ctx.boundary);
        let mut value = crate::verify_tools::execute(workspace, name, &arguments, &tokio_util::sync::CancellationToken::new(), boundary).await?;
        value.as_object_mut().unwrap().remove("__verify_raw");
        return Ok(json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value}));
    }

    if crate::http_probe::is_tool(name) {
        let value = crate::http_probe::execute(&arguments).await?;
        return Ok(
            json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&value)?}],"structuredContent":value}),
        );
    }
    if crate::browser_control::is_tool(name) {
        let value = crate::browser_control::execute(workspace, name, &arguments).await?;
        return crate::visual_artifacts::mcp_result(
            workspace.root(),
            &crate::visual_artifacts::VisualContext::mcp(workspace.root()),
            value,
        );
    }
    if crate::project_process::is_tool(name) {
        let value = crate::project_process::execute(workspace, name, arguments).await?;
        return Ok(
            json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&value)?}],"structuredContent":value}),
        );
    }

    if crate::source_read::is_source_read(name) {
        let workspace = Workspace::new(workspace.root())?;
        let tool_name = name.to_owned();
        let source_args = arguments.clone();
        let mut value =
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                if source_args["force_read"] == true {
                    crate::source_read::clear();
                }
                Ok(match tool_name.as_str() {
                    "read_file" => {
                        serde_json::to_value(workspace.read_file(serde_json::from_value::<
                            ReadFileRequest,
                        >(
                            source_args
                        )?)?)?
                    }
                    "read_file_lines" => serde_json::to_value(workspace.read_file_lines(
                        serde_json::from_value::<ReadFileLinesRequest>(source_args)?,
                    )?)?,
                    "read_rust_symbol" => serde_json::to_value(workspace.read_rust_symbol(
                        serde_json::from_value::<ReadRustSymbolRequest>(source_args)?,
                    )?)?,
                    "read_go_symbol" => serde_json::to_value(workspace.read_go_symbol(
                        serde_json::from_value::<ReadGoSymbolRequest>(source_args)?,
                    )?)?,
                    "read_ts_symbol" => serde_json::to_value(workspace.read_ts_symbol(
                        serde_json::from_value::<ReadTsSymbolRequest>(source_args)?,
                    )?)?,
                    "read_python_symbol" => serde_json::to_value(workspace.read_python_symbol(
                        serde_json::from_value::<ReadPythonSymbolRequest>(source_args)?,
                    )?)?,
                    _ => unreachable!(),
                })
            })
            .await??;
        crate::source_read::page(&mut value, &arguments)?;
        if value.get("symbol").is_some() {
            value["source_index_check"] = json!("current_source_hash_verified");
            if arguments["include_context"] == true {
                value["related_context_freshness"] =
                    json!("indexed_navigation_hints; related files refresh when read");
            }
        }
        return Ok(
            json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&value)?}],"structuredContent":value}),
        );
    }

    // Retained for internal callers with legacy tool references. Public MCP
    // dispatch and the current Agent tool catalog exclude these methods.
    if RETIRED_CODE_INDEX_TOOLS.contains(&name) {
        let selected_root = arguments
            .get("workspace_root")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(std::path::Path::new)
            .unwrap_or(workspace.root());
        let workspace = Workspace::new(selected_root)?;
        let name = name.to_owned();
        let value = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
            let value =
                match name.as_str() {
                    "search_code_map" => crate::code_map::search(
                        workspace.root(),
                        serde_json::from_value(arguments)?,
                    )?,
                    "index_go_workspace" => serde_json::to_value(workspace.index_go_workspace(
                        serde_json::from_value::<IndexGoWorkspaceRequest>(arguments)?,
                    )?)?,
                    "go_index_status" => serde_json::to_value(workspace.go_index_status(
                        serde_json::from_value::<IndexGoWorkspaceRequest>(arguments)?,
                    )?)?,
                    "list_go_symbols" => serde_json::to_value(workspace.list_go_symbols(
                        serde_json::from_value::<ListGoSymbolsRequest>(arguments)?,
                    )?)?,
                    "search_go_symbols" => serde_json::to_value(workspace.search_go_symbols(
                        serde_json::from_value::<SearchGoSymbolsRequest>(arguments)?,
                    )?)?,
                    "read_go_symbol" => serde_json::to_value(workspace.read_go_symbol(
                        serde_json::from_value::<ReadGoSymbolRequest>(arguments)?,
                    )?)?,
                    "index_rust_workspace" => {
                        serde_json::to_value(workspace.index_rust_workspace(
                            serde_json::from_value::<IndexRustWorkspaceRequest>(arguments)?,
                        )?)?
                    }
                    "rust_index_status" => serde_json::to_value(workspace.rust_index_status(
                        serde_json::from_value::<IndexRustWorkspaceRequest>(arguments)?,
                    )?)?,
                    "list_rust_symbols" => serde_json::to_value(workspace.list_rust_symbols(
                        serde_json::from_value::<ListRustSymbolsRequest>(arguments)?,
                    )?)?,
                    "search_rust_symbols" => serde_json::to_value(workspace.search_rust_symbols(
                        serde_json::from_value::<SearchRustSymbolsRequest>(arguments)?,
                    )?)?,
                    "read_rust_symbol" => serde_json::to_value(workspace.read_rust_symbol(
                        serde_json::from_value::<ReadRustSymbolRequest>(arguments)?,
                    )?)?,
                    "index_ts_workspace" => serde_json::to_value(workspace.index_ts_workspace(
                        serde_json::from_value::<IndexTsWorkspaceRequest>(arguments)?,
                    )?)?,
                    "ts_index_status" => serde_json::to_value(workspace.ts_index_status(
                        serde_json::from_value::<IndexTsWorkspaceRequest>(arguments)?,
                    )?)?,
                    "list_ts_symbols" => serde_json::to_value(workspace.list_ts_symbols(
                        serde_json::from_value::<ListTsSymbolsRequest>(arguments)?,
                    )?)?,
                    "search_ts_symbols" => serde_json::to_value(workspace.search_ts_symbols(
                        serde_json::from_value::<SearchTsSymbolsRequest>(arguments)?,
                    )?)?,
                    "read_ts_symbol" => serde_json::to_value(workspace.read_ts_symbol(
                        serde_json::from_value::<ReadTsSymbolRequest>(arguments)?,
                    )?)?,
                    "index_python_workspace" => {
                        serde_json::to_value(workspace.index_python_workspace(
                            serde_json::from_value::<IndexPythonWorkspaceRequest>(arguments)?,
                        )?)?
                    }
                    "python_index_status" => serde_json::to_value(workspace.python_index_status(
                        serde_json::from_value::<IndexPythonWorkspaceRequest>(arguments)?,
                    )?)?,
                    "list_python_symbols" => serde_json::to_value(workspace.list_python_symbols(
                        serde_json::from_value::<ListPythonSymbolsRequest>(arguments)?,
                    )?)?,
                    "search_python_symbols" => {
                        serde_json::to_value(workspace.search_python_symbols(
                            serde_json::from_value::<SearchPythonSymbolsRequest>(arguments)?,
                        )?)?
                    }
                    "read_python_symbol" => serde_json::to_value(workspace.read_python_symbol(
                        serde_json::from_value::<ReadPythonSymbolRequest>(arguments)?,
                    )?)?,
                    _ => anyhow::bail!("unknown index tool: {name}"),
                };
            Ok(value)
        })
        .await??;
        return Ok(
            json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&value)?}],"structuredContent":value}),
        );
    }

    let value = match name {
        "workspace_info" => serde_json::to_value(
            workspace.workspace_info(serde_json::from_value::<WorkspaceInfoRequest>(arguments)?)?,
        )?,
        "git_status" | "git_diff" | "git_log" => {
            let workspace = Workspace::new(workspace.root())?;
            let name = name.to_owned();
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                Ok(match name.as_str() {
                    "git_status" => serde_json::to_value(workspace.git_status(serde_json::from_value::<GitStatusRequest>(arguments)?)?)?,
                    "git_diff" => serde_json::to_value(workspace.git_diff(serde_json::from_value::<GitDiffRequest>(arguments)?)?)?,
                    "git_log" => serde_json::to_value(workspace.git_log(serde_json::from_value::<GitLogRequest>(arguments)?)?)?,
                    _ => unreachable!(),
                })
            }).await??
        },
        "list_dir" | "read_file" | "read_file_lines" | "read_file_chunk" | "search_text" => {
            let workspace = Workspace::new(workspace.root())?;
            let name = name.to_owned();
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                Ok(match name.as_str() {
                    "list_dir" => serde_json::to_value(workspace.list_dir(serde_json::from_value::<ListDirRequest>(arguments)?)?)?,
                    "read_file" => serde_json::to_value(workspace.read_file(serde_json::from_value::<ReadFileRequest>(arguments)?)?)?,
                    "read_file_lines" => serde_json::to_value(workspace.read_file_lines(serde_json::from_value::<ReadFileLinesRequest>(arguments)?)?)?,
                    "read_file_chunk" => serde_json::to_value(workspace.read_file_chunk(serde_json::from_value::<ReadFileChunkRequest>(arguments)?)?)?,
                    "search_text" => serde_json::to_value(workspace.search_text(serde_json::from_value::<SearchTextRequest>(arguments)?)?)?,
                    _ => unreachable!(),
                })
            }).await??
        },
        "write_file" => {
            let workspace = Workspace::new(workspace.root())?;
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                Ok(serde_json::to_value(workspace.write_file(
                    serde_json::from_value::<WriteFileRequest>(arguments)?,
                )?)?)
            })
            .await??
        }
        "replace_range" => {
            let workspace = Workspace::new(workspace.root())?;
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                Ok(serde_json::to_value(workspace.replace_range(
                    serde_json::from_value::<ReplaceRangeRequest>(arguments)?,
                )?)?)
            })
            .await??
        }
        "edit_file" => {
            let workspace = Workspace::new(workspace.root())?;
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                Ok(serde_json::to_value(workspace.edit_file(
                    serde_json::from_value::<EditFileRequest>(arguments)?,
                )?)?)
            })
            .await??
        }
        "delete_path" => {
            let workspace = Workspace::new(workspace.root())?;
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                Ok(serde_json::to_value(workspace.delete_path(
                    serde_json::from_value::<DeletePathRequest>(arguments)?,
                )?)?)
            })
            .await??
        }
        "rename_path" => {
            let workspace = Workspace::new(workspace.root())?;
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                Ok(serde_json::to_value(workspace.rename_path(
                    serde_json::from_value::<RenamePathRequest>(arguments)?,
                )?)?)
            })
            .await??
        }
        "create_dir" => {
            let workspace = Workspace::new(workspace.root())?;
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                Ok(serde_json::to_value(workspace.create_dir(
                    serde_json::from_value::<CreateDirRequest>(arguments)?,
                )?)?)
            })
            .await??
        }
        "record_work_memory" => {
            serde_json::to_value(workspace.record_work_memory(serde_json::from_value::<
                RecordWorkMemoryRequest,
            >(arguments)?)?)?
        }
        "list_work_memory" => serde_json::to_value(
            workspace
                .list_work_memory(serde_json::from_value::<ListWorkMemoryRequest>(arguments)?)?,
        )?,
        "search_work_memory" => {
            serde_json::to_value(workspace.search_work_memory(serde_json::from_value::<
                SearchWorkMemoryRequest,
            >(arguments)?)?)?
        }
        "record_architecture_memory" => {
            serde_json::to_value(workspace.record_architecture_memory(
                serde_json::from_value::<RecordArchitectureMemoryRequest>(arguments)?,
            )?)?
        }
        "list_architecture_memory" => {
            serde_json::to_value(workspace.list_architecture_memory(serde_json::from_value::<
                ListArchitectureMemoryRequest,
            >(arguments)?)?)?
        }
        "search_architecture_memory" => {
            serde_json::to_value(workspace.search_architecture_memory(
                serde_json::from_value::<SearchArchitectureMemoryRequest>(arguments)?,
            )?)?
        }
        "record_symbol_business_context" => {
            serde_json::to_value(workspace.record_symbol_business_context(
                serde_json::from_value::<RecordSymbolBusinessContextRequest>(arguments)?,
            )?)?
        }
        "list_symbol_business_context" => {
            serde_json::to_value(workspace.list_symbol_business_context(
                serde_json::from_value::<ListSymbolBusinessContextRequest>(arguments)?,
            )?)?
        }
        "search_symbol_business_context" => {
            serde_json::to_value(workspace.search_symbol_business_context(
                serde_json::from_value::<SearchSymbolBusinessContextRequest>(arguments)?,
            )?)?
        }
        "list_skills" => crate::skill_packages::root_skills().unwrap_or_else(|| json!([])),
        "read_skill" => {
            let name = arguments.get("name").and_then(Value::as_str).unwrap_or("");
            crate::skill_packages::root_read(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "Skill '{name}' not found. Call list_skills to see available skills."
                )
            })??
        }
        _ => anyhow::bail!("unknown tool: {name}"),
    };

    Ok(json!({
        "content": [
            {
                "type": "text",
                "text": serde_json::to_string_pretty(&value)?
            }
        ],
        "structuredContent": value
    }))
}

/// Complete native schemas for registration; callers filter their visible catalog.
pub fn tool_definitions() -> Value {
    let mut definitions = json!([
        {
            "name": "workspace_info",
            "description": "Return workspace root, platform, file access scope, and ignore summary. Requires workspace_root.",
            "inputSchema": {
                "type": "object",
                "required": ["workspace_root"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Absolute project directory to use for this call."
                    }
                }
            }
        },
        {
            "name": "git_status",
            "description": "Read the repository status as bounded structured data. This never writes the index or worktree.",
            "inputSchema": {"type":"object","properties":{
                "workspace_root":{"type":"string","description":"Optional absolute project directory."},
                "paths":{"type":"array","items":{"type":"string"},"description":"Optional repository-relative path filters."},
                "include_untracked":{"type":"boolean","default":true},
                "max_entries":{"type":"integer","minimum":1,"maximum":1000,"default":200}
            }},
            "outputSchema":{"type":"object","required":["head","branch","clean","staged","modified","untracked","total_entries","truncated"],"properties":{
                "head":{"type":["string","null"]},"branch":{"type":["string","null"]},"clean":{"type":"boolean"},
                "staged":{"type":"array","items":{"type":"string"}},"modified":{"type":"array","items":{"type":"string"}},
                "untracked":{"type":"array","items":{"type":"string"}},"total_entries":{"type":"integer"},"truncated":{"type":"boolean"}
            }}
        },
        {
            "name": "git_diff",
            "description": "Read a bounded repository diff. Prefer stat_only first, then request a limited preview when details are needed.",
            "inputSchema": {"type":"object","properties":{
                "workspace_root":{"type":"string"},"rev_range":{"type":"string","description":"A git revision or revision range."},
                "staged":{"type":"boolean","default":false},"paths":{"type":"array","items":{"type":"string"}},
                "stat_only":{"type":"boolean","default":false},"unified":{"type":"integer","minimum":0,"maximum":100,"default":3},
                "max_lines":{"type":"integer","minimum":1,"maximum":20000,"default":2000},"max_chars":{"type":"integer","minimum":256,"maximum":256000,"default":48000}
            }},
            "outputSchema":{"type":"object","required":["diff","files","truncated","stat_only"],"properties":{
                "diff":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},"truncated":{"type":"boolean"},"stat_only":{"type":"boolean"}
            }}
        },
        {
            "name": "git_log",
            "description": "Read a bounded structured commit history without invoking a shell pipeline.",
            "inputSchema": {"type":"object","properties":{
                "workspace_root":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":20},
                "paths":{"type":"array","items":{"type":"string"}},"rev":{"type":"string"}
            }},
            "outputSchema":{"type":"object","required":["commits","truncated"],"properties":{
                "commits":{"type":"array","items":{"type":"object","required":["commit","short_commit","author","date","subject"],"properties":{
                    "commit":{"type":"string"},"short_commit":{"type":"string"},"author":{"type":"string"},"date":{"type":"string"},"subject":{"type":"string"}
                }}},"truncated":{"type":"boolean"}
            }}
        },
        {
            "name": "list_dir",
            "description": "List or find files and directories (also accepts a single file path to inspect its size_bytes/modified_at metadata without reading contents) by path, glob pattern, entry kind, modification time, size/mtime sort order, and limit. By default this shows the real filesystem view, including gitignored files. Default noise/dependency directories (such as node_modules, target, dist, .git) are shown as directory entries and listed in unexpanded_dirs without traversing inside them unless you explicitly target their path or name them in glob; do not inspect inside them unless the user asked or the task genuinely requires it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string", "default": "." },
                    "recursive": { "type": "boolean", "default": false, "description": "When true, walk subdirectories recursively. Passing glob also searches subdirectories recursively unless max_depth is set." },
                    "max_depth": { "type": "integer", "minimum": 0, "description": "Maximum subdirectory depth beneath path (default 1 when recursive is false and glob is empty; unbounded when recursive is true or glob is set)." },
                    "respect_gitignore": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, hide files ignored by .gitignore/.ignore. Defaults to false so logs and generated files remain visible."
                    },
                    "glob": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }], "description": "Filter entries by glob pattern (\"*.rs\" matches at any depth; \"*.rs|*.ts\" splits alternatives on |; \"src/**/*.ts\" or \"target/**/*.log\" from workspace root; leading ! excludes). Explicitly naming an ignored/noise directory such as \"target/**\" traverses into it." },
                    "exclude": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }], "description": "Exclude files or directories matching these patterns (supports | pipe separation, e.g. \"kylin-arm64|*.tmp\" or [\"target/logs/**\", \"*.snap\"]); takes priority over glob." },
                    "kind": { "type": "string", "enum": ["file", "directory", "symlink", "other"], "description": "Return only entries of this kind." },
                    "modified_after": { "type": "string", "description": "Keep entries modified at or after this time (RFC3339, YYYY-MM-DD in UTC, epoch seconds/ms, or relative duration like \"24h\" or \"7d\")." },
                    "modified_before": { "type": "string", "description": "Keep entries modified at or before this time (same formats as modified_after)." },
                    "sort_by": { "type": "string", "enum": ["path", "modified_desc", "modified_asc", "size_desc", "size_asc"], "default": "path", "description": "Sort entries before applying max_entries." },
                    "max_entries": { "type": "integer", "minimum": 1, "description": "Maximum entries to return after filtering and sorting; limit and head_limit are accepted as aliases." }
                }
            }
        },
        {
            "name": "read_file",
            "description": "Read a UTF-8 file by path. max_bytes is a file-size admission limit (default 1 MiB, hard maximum 16 MiB): if the file is larger than that limit, the call is rejected; it never truncates a prefix. To limit returned text, use max_chars and normally omit max_bytes instead of deriving a smaller byte limit. max_chars does not bypass the admission limit; for files beyond it, use read_file_lines or read_file_chunk.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string" },
                    "max_bytes": { "type": "integer", "default": 1048576, "description": "Maximum file size admitted for this call. A file larger than this value is rejected; it is not truncated. Omit to use the 1 MiB default." }
                }
            }
        },
        {
            "name": "read_file_lines",
            "description": "Read an inclusive line range (positive 1-indexed from start, or negative from EOF such as start_line=-80, end_line=-1 for tail; streams files larger than 16 MiB). Choose text search for literals and symbol search for unknown definitions.",
            "inputSchema": {
                "type": "object",
                "required": ["path", "start_line", "end_line"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string" },
                    "start_line": { "type": "integer", "description": "1-based line from start (>0), or lines from end (<0, e.g. -80)." },
                    "end_line": { "type": "integer", "default": -1, "description": "Inclusive end line (>0, or <0 from end such as -1 for last line)." }
                }
            }
        },
        {
            "name": "read_file_chunk",
            "description": "Stream a UTF-8 file in bounded byte/line chunks from byte_offset without loading the whole file into memory. Pass next_byte_offset (and next_line) from each response to continue until eof=true, summarizing or discarding each chunk as you go.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string" },
                    "byte_offset": { "type": "integer", "minimum": 0, "default": 0, "description": "Byte offset to resume streaming from (use previous next_byte_offset)." },
                    "start_line": { "type": "integer", "minimum": 1, "description": "Optional 1-based line number at byte_offset (pass previous next_line; defaults to 1 at byte_offset=0)." },
                    "max_bytes": { "type": "integer", "minimum": 4, "maximum": 524288, "default": 65536, "description": "Maximum UTF-8 bytes per chunk (always cuts on a line boundary unless a single line exceeds max_bytes)." },
                    "max_lines": { "type": "integer", "minimum": 1, "description": "Optional maximum lines to return in this chunk." }
                }
            }
        },
        {
            "name": "search_text",
            "description": "Search workspace text line by line, like ripgrep. A query written as a regular expression (A|B, name\\(, ^fn, foo.*bar) is matched as one; other text is matched literally; set regex explicitly to force either. output_mode picks what comes back: content (default) gives each matching line with path (with / separators), line, column and text, plus before/after lines with context_lines; files lists only the matching files; count gives matching lines per file and their total. Start with files or count for broad searches, then read the lines you need. glob limits the files searched, as rg -g does: \"*.rs\" at any depth, \"src/**/*.ts\" from the workspace root, \"!*.lock\" to exclude. max_matches (alias limit) caps matching lines in content mode and files otherwise; truncated says more exist. Search directly for literals, UI strings, config keys, error messages, log lines, imports, and syntax patterns; symbol tools are useful for definitions and relationships, and text searches do not require trying a symbol tool first. By default files that .gitignore or .ignore excludes and default noise/dependency directories (such as node_modules, target, dist, .git) are skipped; avoid searching inside noise/dependency directories unless the user asked or the task genuinely requires it (naming their path or matching them in glob searches them, and respect_gitignore:false searches gitignored files). Use `path` for one file/directory, or `paths` as an array for multiple files/directories; do not put multiple paths in one space-separated string. A line longer than 400 characters comes back as an excerpt around the match, with line_chars giving the whole line's length.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "query": { "type": "string" },
                    "regex": { "type": "boolean", "description": "Omit to let the query decide: written as a regular expression (alternation |, a backslash escape, ^ or $ anchors, .* or .+), it is matched as one, otherwise literally. true forces a regular expression, false literal text. Matching is per line; an invalid expression returns an error." },
                    "path": {
                        "type": "string",
                        "default": ".",
                        "description": "Single file or directory to search. For multiple targets, use `paths` instead."
                    },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of files/directories to search. Prefer this over a space-separated `path` string when searching multiple targets."
                    },
                    "case_sensitive": { "type": "boolean", "default": false },
                    "respect_gitignore": {
                        "type": "boolean",
                        "default": true,
                        "description": "Skip files ignored by .gitignore/.ignore. Pass false to search logs, build output and other ignored files too."
                    },
                    "max_matches": { "type": "integer", "default": 100, "minimum": 1, "description": "Matching lines in content mode, files in files and count modes. limit is accepted as an alias." },
                    "max_matches_per_file": { "type": "integer", "minimum": 1, "description": "In content mode, maximum matches to collect from any single file before moving on to the next file." },
                    "output_mode": { "type": "string", "enum": ["content", "files", "count"], "default": "content", "description": "content: matching lines. files: only the paths of matching files. count: matching lines per file, with their total." },
                    "glob": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }], "description": "Search only files matching these globs, as rg -g: \"*.rs\" matches at any depth, \"*.rs|*.ts\" splits alternatives on |, \"src/**/*.ts\" from the workspace root, a leading ! excludes." },
                    "exclude": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }], "description": "Exclude files or directories matching these patterns (supports | pipe separation, e.g. \"kylin-arm64|*.tmp\" or [\"tests/**\", \"*.lock\"]); takes priority over glob." },
                    "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 0, "description": "In content mode, lines before and after each match, returned as before and after." }
                }
            }
        },
        {
            "name": "write_file",
            "description": "Create a UTF-8 file or replace its complete contents inside the selected workspace. Existing files REQUIRE expected_code_hash from the latest source read. Prefer edit_file for localized changes. Returns changed, created, code_hash and index_refresh; a failed index refresh does not undo a saved file.",
            "inputSchema": {
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string" },
                    "content": { "type": "string" },
                    "create_parent_dirs": { "type": "boolean", "default": true },
                    "expected_code_hash": {"type":"string","description":"Required when path already exists. Use the code_hash returned by a current source read."}
                }
            }
        },
        {
            "name": "delete_path",
            "description": "Delete a file inside the selected workspace, or a directory tree when recursive is true. Refuses the workspace root and anything under .git. Use it instead of a shell rm so the change log, undo and index stay coherent.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string", "description": "Workspace-relative path, or an absolute path inside the workspace." },
                    "recursive": { "type": "boolean", "default": false, "description": "Required to remove a directory and everything inside it." }
                }
            }
        },
        {
            "name": "rename_path",
            "description": "Move or rename one path inside the selected workspace. An existing destination must be allowed with overwrite: true. Refuses the workspace root and anything under .git. Use it instead of a shell mv.",
            "inputSchema": {
                "type": "object",
                "required": ["path", "to"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string", "description": "Current path." },
                    "to": { "type": "string", "description": "New path." },
                    "overwrite": { "type": "boolean", "default": false, "description": "Replace an existing destination file." },
                    "create_parent_dirs": { "type": "boolean", "default": true }
                }
            }
        },
        {
            "name": "create_dir",
            "description": "Create a directory inside the selected workspace; recursive (the default) creates the whole chain. Reports created=false when it already existed.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string" },
                    "recursive": { "type": "boolean", "default": true }
                }
            }
        },
        {
            "name": "replace_range",
            "description": "Replace an inclusive 1-indexed line range inside the selected workspace, preserving untouched bytes and the selected region newline style. Requires expected_old_text or expected_code_hash. Prefer edit_file when a unique text anchor is available. Conflicts apply no changes; retrieve only the affected source.",
            "inputSchema": {
                "type": "object",
                "required": ["path", "start_line", "end_line", "replacement"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string" },
                    "start_line": { "type": "integer", "minimum":1 },
                    "end_line": { "type": "integer", "minimum":1 },
                    "replacement": { "type": "string" },
                    "expected_old_text": { "type": "string", "description":"Exact selected source excluding its final line delimiter; required unless expected_code_hash is provided." },
                    "expected_code_hash": { "type":"string","description":"Current file version from a source read." }
                }
            }
        },
        {
            "name": "edit_file",
            "description": "Apply 1-100 exact text edits to ONE existing UTF-8 file inside the selected workspace. Each old_text must match uniquely in the original file; use enough surrounding code. All anchors refer to the same original snapshot and may not overlap. All edits are validated before one atomic commit. For insertion keep the anchor in new_text; for deletion set new_text empty. LF anchors may match CRLF source; untouched bytes stay unchanged. Prefer this over line-number edits. Returns changed, code_hash and index_refresh; mismatches apply no changes.",
            "inputSchema": {
                "type":"object", "required":["path","edits"],
                "properties": {
                    "workspace_root":{"type":"string","description":"Optional absolute selected project directory."},
                    "path":{"type":"string"},
                    "expected_code_hash":{"type":"string","description":"Optional current file version. Recommended when the edits depend on code outside their anchors."},
                    "edits":{"type":"array","minItems":1,"maxItems":100,"items":{"type":"object","required":["old_text","new_text"],"properties":{"old_text":{"type":"string","minLength":1},"new_text":{"type":"string"}}}}
                }
            }
        },
        {
            "name": "record_work_memory",
            "description": "Record durable reusable work findings when useful. Do not create a routine summary for every task or duplicate an Observer retrospective memory.",
            "inputSchema": {
                "type": "object",
                "required": ["workspace_root", "summary"],
                "properties": {
                    "workspace_root": { "type": "string", "description": "Absolute workspace root." },
                    "summary": { "type": "string" },
                    "files_changed": { "type": "array", "items": { "type": "string" }, "default": [] },
                    "implementation": { "type": "string" },
                    "tests": { "type": "string" },
                    "risks": { "type": "string" }
                }
            }
        },
        {
            "name": "list_work_memory",
            "description": "List recent work summaries when a focused search term is unavailable. Reuse relevant entries and check their applicability.",
            "inputSchema": {
                "type": "object",
                "required": ["workspace_root"],
                "properties": {
                    "workspace_root": { "type": "string", "description": "Absolute workspace root." },
                    "limit": { "type": "integer", "default": 10 }
                }
            }
        },
        {
            "name": "search_work_memory",
            "description": "Search prior project work and Observer memories with one focused query when they can shorten the task. Use source paths and applicability as pointers, and verify facts against current code.",
            "inputSchema": {
                "type": "object",
                "required": ["workspace_root", "query"],
                "properties": {
                    "workspace_root": { "type": "string", "description": "Absolute workspace root." },
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "default": 10 }
                }
            }
        },
        {
            "name": "record_architecture_memory",
            "description": "Create or update a durable architecture memory document for one feature/logic area in this workspace. Use after verifying code paths, and update it after large changes that alter responsibilities, key symbols, boundaries, or common tasks.",
            "inputSchema": {
                "type": "object",
                "required": ["workspace_root", "area", "summary"],
                "properties": {
                    "workspace_root": { "type": "string", "description": "Absolute workspace root." },
                    "area": { "type": "string", "description": "Stable feature/logic area name, e.g. 'Responses format translation'." },
                    "summary": { "type": "string", "description": "Concise description of what this area owns and how the flow works." },
                    "key_symbols": { "type": "array", "items": { "type": "string" }, "default": [], "description": "Important symbol names or qualified functions/classes to inspect first." },
                    "key_files": { "type": "array", "items": { "type": "string" }, "default": [], "description": "Primary files for this area." },
                    "boundaries": { "type": "string", "description": "What this area should not own; nearby systems to avoid unless evidence requires touching them." },
                    "common_tasks": { "type": "array", "items": { "type": "string" }, "default": [], "description": "User-facing task phrases that map to this area." },
                    "risks": { "type": "string", "description": "Coupling, compatibility, or regression risks." }
                }
            }
        },
        {
            "name": "list_architecture_memory",
            "description": "List durable architecture memory documents for this workspace. Use when you need an overview of known feature/logic areas before deciding where to inspect code.",
            "inputSchema": {
                "type": "object",
                "required": ["workspace_root"],
                "properties": {
                    "workspace_root": { "type": "string", "description": "Absolute workspace root." },
                    "limit": { "type": "integer", "default": 10 }
                }
            }
        },
        {
            "name": "search_architecture_memory",
            "description": "Search durable architecture memory by user task, feature name, symbol, file, boundary, or risk. Use before code changes to map business wording to key symbols and a minimal inspection scope.",
            "inputSchema": {
                "type": "object",
                "required": ["workspace_root", "query"],
                "properties": {
                    "workspace_root": { "type": "string", "description": "Absolute workspace root." },
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "default": 10 }
                }
            }
        },
        {
            "name": "record_symbol_business_context",
            "description": "Save a reusable responsibility after reading a definition or module. Ordinary symbol searches will match this description and its task-wording keywords. Supply a symbol_id or file_path + qualified_name; scope=file describes a whole source module. Pass expected_code_hash from the read result to reject changed source. Optional: do not add reads or annotation calls merely to complete an inventory.",
            "inputSchema": {
                "type": "object",
                "required": ["workspace_root", "business_role"],
                "properties": {
                    "qualified_name": { "type":"string", "description":"Qualified definition name with file_path, e.g. PptxParser.parsePicture." },
                    "keywords": { "type":"array", "items":{"type":"string"}, "maxItems":24, "description":"Task wording and feature aliases, including Chinese words when useful." },
                    "scope": { "type":"string", "enum":["symbol","file"], "default":"symbol" },
                    "source": { "type":"string", "enum":["worker","observer","architecture"], "default":"worker" },
                    "expected_code_hash": { "type":"string", "description":"Source version from description.code_hash or read_file.code_hash; changed source rejects the write." },
                    "workspace_root": { "type": "string", "description": "Absolute workspace root." },
                    "symbol_id": { "type": "string", "description": "Current symbol ID from a read/search result. Can be omitted when file_path and qualified_name are supplied." },
                    "symbol_name": { "type": "string" },
                    "language": { "type": "string" },
                    "file_path": { "type": "string" },
                    "belongs_to_area": { "type": "string" },
                    "business_role": { "type": "string" },
                    "common_tasks": { "type": "array", "items": { "type": "string" }, "default": [] },
                    "read_when": { "type": "string" },
                    "avoid_when": { "type": "string" },
                    "risks": { "type": "string" },
                    "confidence": { "type": "number", "default": 0.0 }
                }
            }
        },
        {
            "name": "list_symbol_business_context",
            "description": "List semantic business contexts for indexed symbols. Optionally filter by architecture area.",
            "inputSchema": {
                "type": "object",
                "required": ["workspace_root"],
                "properties": {
                    "workspace_root": { "type": "string", "description": "Absolute workspace root." },
                    "belongs_to_area": { "type": "string" },
                    "limit": { "type": "integer", "default": 10 }
                }
            }
        },
        {
            "name": "search_symbol_business_context",
            "description": "Inspect saved responsibility records, including keywords and stale flags. Use source reads to verify current code.",
            "inputSchema": {
                "type": "object",
                "required": ["workspace_root", "query"],
                "properties": {
                    "workspace_root": { "type": "string", "description": "Absolute workspace root." },
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "default": 10 }
                }
            }
        },
        {
            "name": "list_skills",
            "description": "List all available skill packages with their names and one-line descriptions. Call this first before specialized tasks (presentations, documents, spreadsheets, images) to discover if a matching skill exists.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "read_skill",
            "description": "Read the full SKILL.md content for a specific skill by name. Use after list_skills to get the complete instructions for a skill before executing it.",
            "inputSchema": {
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The skill name as returned by list_skills (e.g. 'presentations', 'imagegen', 'documents')."
                    }
                }
            }
        },
        {
            "name": "unload_skill", "description": "Hide a loaded skill's tools in this MCP session; original instructions remain in client history.",
            "inputSchema": {"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}
        },
        {
            "name": "reset_skill_activation", "description": "Begin a new task or user turn by clearing loaded package tools in this MCP session. MCP has no automatic user-turn signal; call explicitly at the boundary.",
            "inputSchema": {"type":"object","properties":{}}
        },
    ]);
    definitions
        .as_array_mut()
        .unwrap()
        .extend(crate::project_process::definitions());
    definitions
        .as_array_mut()
        .unwrap()
        .extend(crate::http_probe::definitions());
    definitions
        .as_array_mut()
        .unwrap()
        .extend(crate::program_execution::definitions());
    definitions
        .as_array_mut()
        .unwrap()
        .extend(crate::shell_process::definitions());
    definitions
        .as_array_mut()
        .unwrap()
        .extend(crate::browser_control::definitions());
    definitions.as_array_mut().unwrap().extend(crate::verify_tools::definitions());
    definitions.as_array_mut().unwrap().extend(crate::plan_tools::definitions());
    definitions.as_array_mut().unwrap().extend(crate::git_tools::definitions());
    definitions.as_array_mut().unwrap().extend(crate::batch_edits::definitions());
    for tool in definitions.as_array_mut().into_iter().flatten() {
        let name = tool["name"].as_str().unwrap_or("").to_owned();
        if crate::source_read::is_source_read(&name) {
            tool["description"] = json!(format!(
                "{} Source pages preserve original bytes and line delimiters; complete/next_start_line identify pagination. {} Reads are bounded to 16 MiB; force_read bypasses snapshot caching.",
                tool["description"].as_str().unwrap_or(""),
                crate::source_read::page_note(false)
            ));
            let props = tool["inputSchema"]["properties"].as_object_mut().unwrap();
            props.insert(
                "max_chars".into(),
                json!({"type":"integer","minimum":1000,"maximum":crate::source_read::PAGE_CHARS,"default":24000,
                    "description": if name == "read_file" {
                        "Maximum characters to return. This limits the returned text, not file admission; leave max_bytes omitted rather than setting it to max_chars multiplied by an estimate."
                    } else {
                        "Maximum characters to return in this page."
                    }}),
            );
            props.entry("start_line").or_insert(json!({"type":"integer","minimum":1,"description":"Optional page start within this file/definition. Use next_start_line to continue."}));
            props
                .entry("end_line")
                .or_insert(json!({"type":"integer","minimum":1}));
            props.insert("start_column".into(),json!({"type":"integer","minimum":1,"description":"Continue a partial long line using next_column. Unicode columns include line delimiters; default 1."}));
            props.insert("force_read".into(),json!({"type":"boolean","default":false,"description":"Bypass source snapshot caching when a fresh disk read is required."}));
        }
    }
    definitions
}

#[cfg(test)]
mod tests {
    include!("parts/mcp_tests.inc.rs");
}
