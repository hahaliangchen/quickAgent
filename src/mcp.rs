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
    EditFileRequest, ListDirRequest, ReadFileLinesRequest, ReadFileRequest, ReplaceRangeRequest,
    SearchTextRequest, Workspace, WorkspaceInfoRequest, WriteFileRequest,
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
    let Some(origin) = headers.get("origin") else {
        return true;
    };
    let Some(host) = headers.get("host").and_then(|value| value.to_str().ok()) else {
        return false;
    };
    origin.to_str().is_ok_and(|origin| {
        origin == format!("http://{host}") || origin == format!("https://{host}")
    })
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
    request: JsonRpcRequest,
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
        "list_dir" | "read_file" | "read_file_lines" | "search_text" => {
            let workspace = Workspace::new(workspace.root())?;
            let name = name.to_owned();
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                Ok(match name.as_str() {
                    "list_dir" => serde_json::to_value(workspace.list_dir(serde_json::from_value::<ListDirRequest>(arguments)?)?)?,
                    "read_file" => serde_json::to_value(workspace.read_file(serde_json::from_value::<ReadFileRequest>(arguments)?)?)?,
                    "read_file_lines" => serde_json::to_value(workspace.read_file_lines(serde_json::from_value::<ReadFileLinesRequest>(arguments)?)?)?,
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
            "name": "list_dir",
            "description": "List a directory by relative workspace path or absolute filesystem path with optional recursion and filtering. By default this shows the real filesystem view, including gitignored files. Use to understand project layout or locate files by path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string", "default": "." },
                    "recursive": { "type": "boolean", "default": false },
                    "max_depth": { "type": "integer", "default": 1 },
                    "respect_gitignore": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, hide files ignored by .gitignore/.ignore. Defaults to false so logs and generated files remain visible."
                    }
                }
            }
        },
        {
            "name": "read_file",
            "description": "Read a UTF-8 file by path with a byte limit. For a known line range, use read_file_lines.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string" },
                    "max_bytes": { "type": "integer", "default": 1048576 }
                }
            }
        },
        {
            "name": "read_file_lines",
            "description": "Read an inclusive 1-indexed line range. Use known locations directly. Choose text search for literals and symbol search for unknown definitions.",
            "inputSchema": {
                "type": "object",
                "required": ["path", "start_line", "end_line"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "path": { "type": "string" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" }
                }
            }
        },
        {
            "name": "search_text",
            "description": "Search workspace text. Literal matching is the default; set regex=true for patterns such as A|B, anchors, or character classes. Patterns match each line; invalid expressions return an error. Search directly for literals, UI strings, config keys, error messages, log lines, imports, and syntax patterns. Symbol tools are useful for definitions and relationships; text searches do not require trying a symbol tool first. By default this searches the real filesystem view, including gitignored files. Use `path` for one file/directory, or `paths` as an array for multiple files/directories; do not put multiple paths in one space-separated string. Each match has path (with / separators), line, column and text; a line longer than 400 characters comes back as an excerpt around the match, with line_chars giving the whole line's length.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional absolute project directory to use for this call. Defaults to the server startup directory."
                    },
                    "query": { "type": "string" },
                    "regex": { "type": "boolean", "default": false, "description": "Enable regular expressions explicitly. Use regex=true for alternatives such as loadPptx|loadVirtualDocument. Literal mode does not interpret | as OR. Regex matching is per line; invalid expressions return an error." },
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
                        "default": false,
                        "description": "When true, skip files ignored by .gitignore/.ignore. Defaults to false so logs and generated files remain searchable."
                    },
                    "max_matches": { "type": "integer", "default": 100 }
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
    for tool in definitions.as_array_mut().into_iter().flatten() {
        let name = tool["name"].as_str().unwrap_or("");
        if crate::source_read::is_source_read(name) {
            tool["description"] = json!(format!(
                "{} Source pages preserve original bytes and line delimiters; complete/next_start_line identify pagination. max_chars defaults to 24000, max 60000. Reads are bounded to 16 MiB; force_read bypasses snapshot caching.",
                tool["description"].as_str().unwrap_or("")
            ));
            let props = tool["inputSchema"]["properties"].as_object_mut().unwrap();
            props.insert(
                "max_chars".into(),
                json!({"type":"integer","minimum":1000,"maximum":60000,"default":24000}),
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
mod native_tool_tests {
    use super::*;

    struct TestMcpSession {
        id: String,
        skills: Arc<tokio::sync::Mutex<SkillActivation>>,
    }
    impl TestMcpSession {
        async fn new(workspace: &Arc<Workspace>) -> Self {
            let (headers, response) = rpc(workspace, None, "initialize", json!({})).await;
            assert!(response["error"].is_null(), "{response}");
            let id = headers["mcp-session-id"].to_str().unwrap().to_owned();
            let skills = sessions().lock().unwrap().get(&id).unwrap().skills.clone();
            Self { id, skills }
        }
        fn headers(&self) -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert("mcp-session-id", self.id.parse().unwrap());
            headers
        }
        async fn call(&self, workspace: &Arc<Workspace>, name: &str, args: Value) -> Value {
            let (_, response) = rpc(workspace, Some(&self.id), "tools/call",
                json!({"name":name,"arguments":args})).await;
            assert!(response["error"].is_null(), "{response}");
            response["result"]["structuredContent"].clone()
        }
    }
    impl Drop for TestMcpSession {
        fn drop(&mut self) {
            sessions().lock().unwrap().remove(&self.id);
        }
    }

    async fn rpc(
        workspace: &Arc<Workspace>,
        token: Option<&str>,
        method: &str,
        params: Value,
    ) -> (HeaderMap, Value) {
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            headers.insert("mcp-session-id", token.parse().unwrap());
        }
        let response = handle_mcp(
            State(workspace.clone()),
            headers,
            Json(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})),
        )
        .await;
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (headers, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn shell_requires_scope_and_stateless_listing_hides_it() {
        let workspace = Arc::new(Workspace::new(std::env::current_dir().unwrap()).unwrap());
        let (_, listed) = rpc(&workspace, None, "tools/list", json!({})).await;
        assert!(listed["result"]["tools"].as_array().unwrap().iter()
            .all(|tool| !crate::shell_process::is_tool(tool["name"].as_str().unwrap())));
        for name in crate::shell_process::TOOLS {
            let params = json!({"name":name,"arguments":{"cmd":"echo must-not-run","session_id":"unowned"}});
            let error = call_tool(&workspace, params.clone()).await.unwrap_err();
            assert!(error.to_string().contains("scoped MCP session"), "{error:#}");
            let (_, response) = rpc(&workspace, None, "tools/call", params).await;
            assert!(response["error"]["message"].as_str().unwrap()
                .contains("initialize an MCP session"), "{response}");
        }
    }

    #[tokio::test]
    async fn scoped_shell_definitions_follow_environment_availability() {
        let workspace = Arc::new(Workspace::new(std::env::current_dir().unwrap()).unwrap());
        let session = TestMcpSession::new(&workspace).await;
        let available = crate::shell_environment::resolve().is_ok();
        let (_, listed) = rpc(&workspace, Some(&session.id), "tools/list", json!({})).await;
        for name in crate::shell_process::TOOLS {
            assert_eq!(listed["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == *name), available);
        }
    }

    #[test]
    fn native_registry_always_contains_shell_schemas() {
        let definitions = tool_definitions();
        let definitions = definitions.as_array().unwrap();
        for name in crate::shell_process::TOOLS {
            let schemas = definitions.iter().filter(|tool| tool["name"] == *name).collect::<Vec<_>>();
            assert_eq!(schemas.len(), 1, "{name} must be registered exactly once");
            assert_eq!(schemas[0]["inputSchema"]["type"], "object");
            assert!(!schemas[0]["inputSchema"]["required"].as_array().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn shell_scope_cancels_on_delete_expiration_and_activation_drop() {
        let workspace = Arc::new(Workspace::new(std::env::current_dir().unwrap()).unwrap());
        let first = TestMcpSession::new(&workspace).await;
        let second = TestMcpSession::new(&workspace).await;
        let first_cancel = first.skills.lock().await.shell.cancel.clone();
        let second_cancel = second.skills.lock().await.shell.cancel.clone();
        assert_ne!(first.skills.lock().await.shell.task, second.skills.lock().await.shell.task);
        // Deleting must cancel even with an in-flight RPC holding activation.
        let mut held = first.skills.lock().await;
        assert_eq!(handle_mcp_delete(first.headers()).await.status(), StatusCode::NO_CONTENT);
        assert!(first_cancel.is_cancelled());
        assert!(!second_cancel.is_cancelled());
        assert!(held.shell.context(&workspace)
            .await.err().unwrap().to_string().contains("closed"));
        drop(held);
        // Windows Instant cannot predate boot. Inject expiration instead of
        // subtracting two hours (which panics on a freshly restarted machine).
        expire_session_with_timeout(second.id.clone(), second_cancel.clone(),std::time::Duration::ZERO).await;
        assert!(second_cancel.is_cancelled());
        assert!(!sessions().lock().unwrap().contains_key(&second.id));
        let activation = SkillActivation::default();
        let cancel = activation.shell.cancel.clone();
        drop(activation);
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn scoped_shell_exec_write_kill_and_assets_stay_out_of_ui_sessions() {
        if let Err(error) = crate::shell_environment::resolve() {
            eprintln!("skipped: shell environment unavailable: {error:#}");
            return;
        }
        let root = std::env::temp_dir().join(format!("mcp-shell-{}", crate::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = Arc::new(Workspace::new(&root).unwrap());
        let first = TestMcpSession::new(&workspace).await;
        let second = TestMcpSession::new(&workspace).await;
        let (_, listed) = rpc(&workspace, Some(&first.id), "tools/list", json!({})).await;
        for name in crate::shell_process::TOOLS {
            assert!(listed["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == *name));
        }
        let read_command = if cfg!(windows) {
            r#"$line = [Console]::ReadLine(); Write-Output "received:$line"; Start-Sleep -Seconds 120"#
        } else {
            "read -r line; printf 'received:%s\\n' \"$line\"; sleep 120 & wait"
        };
        let started = first.call(&workspace, "exec_command", json!({
            "cmd":read_command,
            "yield_time_ms":0
        })).await;
        let id = started["session_id"].as_str().unwrap();
        assert_eq!(started["running"], true);
        for name in ["write_stdin", "kill_session"] {
            let (_, rejected) = rpc(&workspace, Some(&second.id), "tools/call",
                json!({"name":name,"arguments":{"session_id":id,"chars":"must-not-send\n"}})).await;
            assert!(rejected["error"]["message"].as_str().unwrap().contains("SHELL_SESSION_SCOPE_DENIED"), "{rejected}");
        }
        let (_, busy) = rpc(&workspace, Some(&second.id), "tools/call",
            json!({"name":"exec_command","arguments":{"cmd":"echo must-not-run"}})).await;
        assert!(busy["error"]["message"].as_str().unwrap().contains("SHELL_BUSY"), "{busy}");
        let coexist_command = if cfg!(windows) { "Write-Output coexist" } else { "printf coexist" };
        let alongside = first.call(&workspace, "exec_command",
            json!({"cmd":coexist_command,"yield_time_ms":10000})).await;
        assert_eq!(alongside["running"], false, "{alongside}");
        assert_eq!(alongside["stdout"].as_str().unwrap().trim_end(), "coexist");
        assert!(alongside["capture_error"].is_null(), "{alongside}");
        let mut written = first.call(&workspace, "write_stdin",
            json!({"session_id":id,"chars":"hello\n","yield_time_ms":1000})).await;
        let mut stdout = written["stdout"].as_str().unwrap().to_owned();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !stdout.contains("received:hello") {
                written = first.call(&workspace, "write_stdin",
                    json!({"session_id":id,"yield_time_ms":100})).await;
                stdout.push_str(written["stdout"].as_str().unwrap());
            }
        }).await.unwrap();
        let killed = first.call(&workspace, "kill_session", json!({"session_id":id})).await;
        assert_eq!(killed["running"], false, "{killed}");
        assert_eq!(killed["termination_reason"], "requested_stop");
        assert!(killed["capture_error"].is_null(), "{killed}");

        let output_command = if cfg!(windows) {
            "[Console]::Out.Write(('0' * 5000)); exit 7"
        } else {
            "printf '%05000d' 0; exit 7"
        };
        let mut output = first.call(&workspace, "exec_command",
            json!({"cmd":output_command,"max_output_tokens":128})).await;
        let mut saved = output["output_asset"].is_object();
        let id = output["session_id"].as_str().unwrap().to_owned();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while output["running"] == true {
                output = first.call(&workspace, "write_stdin",
                    json!({"session_id":id,"yield_time_ms":100,"max_output_tokens":128})).await;
                saved |= output["output_asset"].is_object();
            }
        }).await.unwrap();
        assert_eq!(output["exit_code"], 7);
        assert!(saved, "large output must have a durable asset");
        let page = first.call(&workspace, "write_stdin",
            json!({"session_id":id,"stream":"stdout","char_offset":0,"max_output_tokens":128})).await;
        assert_eq!(page["total_chars"], 5000);

        let state = first.skills.lock().await.shell.state.clone().unwrap();
        assert!(matches!(state.permission_mode, crate::agent_service::PermissionMode::FullAccess));
        let response = crate::agent_service::list_tasks(State(state),
            axum::extract::Query(crate::agent_service::ListTasksQuery { limit: None })).await.into_response();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let ui: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(ui["tasks"], json!([]), "{ui}");
        let task = first.skills.lock().await.shell.task.clone();
        let conn = crate::agent_service::open_db(&root).unwrap();
        let events: i64 = conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id=?1 AND kind IN ('workspace/changes','shell/output')",
            [&task], |row| row.get(0)).unwrap();
        assert!(events >= 3);
        drop(conn);
        drop(first);
        drop(second);
        // The shared DB writer can retain its SQLite handle until its idle timeout on Windows.
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn kill_rpc_interrupts_stdin_larger_than_pipe_without_deleting_session() {
        if let Err(error) = crate::shell_environment::resolve() {
            eprintln!("skipped: shell environment unavailable: {error:#}");
            return;
        }
        let root = std::env::temp_dir().join(format!("mcp-shell-blocked-stdin-{}", crate::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = Arc::new(Workspace::new(&root).unwrap());
        let session = TestMcpSession::new(&workspace).await;
        let cmd = if cfg!(windows) {
            "Write-Output ready; Start-Sleep -Seconds 120"
        } else {
            "printf 'ready\\n'; sleep 120"
        };
        let mut started = session.call(&workspace, "exec_command", json!({"cmd":cmd,"yield_time_ms":100})).await;
        let id = started["session_id"].as_str().unwrap().to_owned();
        let mut stdout = started["stdout"].as_str().unwrap().to_owned();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !stdout.contains("ready") {
                assert_eq!(started["running"], true, "{started}");
                started = session.call(&workspace, "write_stdin",
                    json!({"session_id":id,"yield_time_ms":100})).await;
                stdout.push_str(started["stdout"].as_str().unwrap());
            }
        }).await.unwrap();
        // Far beyond the pipe buffer. yield=0 means a pending RPC is blocked
        // delivering stdin, rather than simply waiting for command completion.
        let mut write = Box::pin(rpc(&workspace, Some(&session.id), "tools/call", json!({
            "name":"write_stdin", "arguments":{
                "session_id":id,"chars":"x".repeat(4 * 1024 * 1024),"yield_time_ms":0
            }
        })));
        assert!(tokio::time::timeout(std::time::Duration::from_millis(300), write.as_mut()).await.is_err(),
            "stdin must remain blocked until the separate kill RPC");
        let (killed, (_, written)) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(
                session.call(&workspace, "kill_session", json!({"session_id":id})),
                write.as_mut()
            )
        }).await.expect("kill RPC must interrupt blocked stdin without DELETE");
        drop(write);
        assert_eq!(killed["running"], false, "{killed}");
        assert_eq!(killed["termination_reason"], "requested_stop", "{killed}");
        assert!(written["error"]["message"].is_string(), "blocked write should fail after kill: {written}");
        assert!(sessions().lock().unwrap().contains_key(&session.id));
        assert!(!session.skills.lock().await.shell.cancel.is_cancelled());
        let next = session.call(&workspace, "exec_command", json!({
            "cmd":if cfg!(windows) { "Write-Output recovered" } else { "printf recovered" },
            "yield_time_ms":10000
        })).await;
        assert_eq!(next["running"], false, "{next}");
        assert_eq!(next["stdout"].as_str().unwrap().trim_end(), "recovered");
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn deleting_shell_session_stops_live_process_tree_and_retained_scope() {
        if let Err(error) = crate::shell_environment::resolve() {
            eprintln!("skipped: shell environment unavailable: {error:#}");
            return;
        }
        let root = std::env::temp_dir().join(format!("mcp-shell-cancel-{}", crate::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = Arc::new(Workspace::new(&root).unwrap());
        let session = TestMcpSession::new(&workspace).await;
        let tree_command = if cfg!(windows) {
            "$child = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120' -WindowStyle Hidden -PassThru; Write-Output 'child-started'; $child.WaitForExit()"
        } else {
            "sleep 120 & printf 'child-started\\n'; wait"
        };
        let mut started = session.call(&workspace, "exec_command",
            json!({"cmd":tree_command,"yield_time_ms":1000})).await;
        let id = started["session_id"].as_str().unwrap().to_owned();
        let mut stdout = started["stdout"].as_str().unwrap().to_owned();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !stdout.contains("child-started") {
                assert_eq!(started["running"], true, "{started}");
                started = session.call(&workspace, "write_stdin",
                    json!({"session_id":id,"yield_time_ms":100})).await;
                stdout.push_str(started["stdout"].as_str().unwrap());
            }
        }).await.unwrap();
        let task = session.skills.lock().await.shell.task.clone();
        let state = session.skills.lock().await.shell.state.clone().unwrap();
        assert_eq!(handle_mcp_delete(session.headers()).await.status(), StatusCode::NO_CONTENT);
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let result = crate::shell_process::execute(&state, &workspace, &task, 1, None, "write_stdin",
                    &json!({"session_id":started["session_id"],"yield_time_ms":100}),
                    &tokio_util::sync::CancellationToken::new(), false).await.unwrap();
                if result["running"] == false { break result; }
            }
        }).await.unwrap();
        assert!(crate::shell_process::active(&root).await.is_none());
        assert_eq!(result["running"], false, "{result}");
        assert_eq!(result["termination_reason"], "task_cancelled", "{result}");
        assert!(result["capture_error"].is_null(), "{result}");
        let error = session.skills.lock().await.shell.context(&workspace).await.err().unwrap();
        assert!(error.to_string().contains("closed"));
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn mcp_retires_code_index_tools_and_resources() {
        let workspace = Arc::new(Workspace::new(std::env::current_dir().unwrap()).unwrap());
        let (_, listed) = rpc(&workspace, None, "tools/list", json!({})).await;
        let names = listed["result"]["tools"].as_array().unwrap().iter()
            .filter_map(|tool| tool["name"].as_str()).collect::<Vec<_>>();
        assert!(names.contains(&"search_text") && names.contains(&"read_file_lines"));
        assert!(RETIRED_CODE_INDEX_TOOLS.iter().all(|name| !names.contains(name)));
        let (_, result) = rpc(&workspace, None, "tools/call",
            json!({"name":"search_code_map","arguments":{"query":"main"}})).await;
        assert!(result["error"]["message"].as_str().unwrap().contains("TOOL_UNAVAILABLE"), "{result}");
        let (_, result) = rpc(&workspace, None, "resources/read",
            json!({"uri":"mcp://codex-workspace-mcp/ast/status"})).await;
        assert!(result["error"]["message"].is_string(), "{result}");
    }

    #[tokio::test]
    async fn mcp_skill_sessions_isolate_activation_and_invalidate_reloaded_packages() {
        let root =
            std::env::temp_dir().join(format!("mcp-skill-{}", crate::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("code-flow.json"),
            r#"{"version":1,"features":[]}"#,
        )
        .unwrap();
        let workspace = Arc::new(Workspace::new(&root).unwrap());
        let (runtime, catalog) = crate::skill_packages::test_runtime();
        crate::skill_packages::bind_root(&catalog);
        let (headers, init) = rpc(&workspace, None, "initialize", json!({})).await;
        assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], true);
        let first = headers["mcp-session-id"].to_str().unwrap().to_owned();
        let (headers, _) = rpc(&workspace, None, "initialize", json!({})).await;
        let second = headers["mcp-session-id"].to_str().unwrap().to_owned();
        let has_flow = |packet: &Value| {
            packet["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "read_code_flow")
        };
        assert!(!has_flow(
            &rpc(&workspace, Some(&first), "tools/list", json!({}))
                .await
                .1
        ));
        let mut changed = sessions()
            .lock()
            .unwrap()
            .get(&first)
            .unwrap()
            .changed
            .subscribe();
        let packet = rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"read_skill","arguments":{"name":"code-flow"}}),
        )
        .await
        .1;
        assert!(packet["error"].is_null());
        assert!(changed.has_changed().unwrap());
        changed.borrow_and_update();
        assert!(has_flow(
            &rpc(&workspace, Some(&first), "tools/list", json!({}))
                .await
                .1
        ));
        assert!(!has_flow(
            &rpc(&workspace, Some(&second), "tools/list", json!({}))
                .await
                .1
        ));
        assert!(!has_flow(
            &rpc(&workspace, None, "tools/list", json!({})).await.1
        ));
        let rejected = rpc(
            &workspace,
            Some(&second),
            "tools/call",
            json!({"name":"read_code_flow"}),
        )
        .await
        .1;
        assert!(
            rejected["error"]["message"]
                .as_str()
                .unwrap()
                .contains("TOOL_UNAVAILABLE")
        );
        let result = rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"read_code_flow"}),
        )
        .await
        .1;
        assert!(result["result"]["structuredContent"]["features"].is_array());
        crate::skill_packages::reload(&runtime, &catalog.packages, "code-flow").unwrap();
        assert!(!has_flow(
            &rpc(&workspace, Some(&first), "tools/list", json!({}))
                .await
                .1
        ));
        rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"read_skill","arguments":{"name":"code-flow"}}),
        )
        .await;
        rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"unload_skill","arguments":{"name":"code-flow"}}),
        )
        .await;
        assert!(!has_flow(
            &rpc(&workspace, Some(&first), "tools/list", json!({}))
                .await
                .1
        ));
        rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"read_skill","arguments":{"name":"code-flow"}}),
        )
        .await;
        rpc(
            &workspace,
            Some(&first),
            "tools/call",
            json!({"name":"reset_skill_activation"}),
        )
        .await;
        assert!(!has_flow(
            &rpc(&workspace, Some(&first), "tools/list", json!({}))
                .await
                .1
        ));
        let mut headers = HeaderMap::new();
        headers.insert("mcp-session-id", first.parse().unwrap());
        assert_eq!(
            handle_mcp_delete(headers.clone()).await.status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            handle_mcp_get(headers).await.status(),
            StatusCode::NOT_FOUND
        );
        sessions().lock().unwrap().remove(&second);
        runtime.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn legacy_run_command_is_disabled_and_native_tool_is_advertised() {
        let root = std::env::current_dir().unwrap();
        let workspace = Workspace::new(&root).unwrap();
        let result = call_tool(
            &workspace,
            json!({"name":"run_command","arguments":{"command":"echo should-not-run"}}),
        )
        .await
        .unwrap();
        assert_eq!(result["structuredContent"]["outcome"], "disabled");
        assert!(
            result["structuredContent"]["error"]
                .as_str()
                .unwrap()
                .contains("run_program")
        );
        let definitions = tool_definitions();
        let names = definitions
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"run_program"));
        assert!(!names.contains(&"run_command"));
    }
}
