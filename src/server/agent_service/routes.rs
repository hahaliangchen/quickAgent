//! HTTP/SSE handlers and task lifecycle entry points.

use super::{
    ActiveTask, AgentProviderRoute, AgentServiceState, ContinueTaskRequest,
    CreateTaskRequest, DraftSessionInput, FileQuery, ListTasksQuery, PermissionMode,
    StreamEventsQuery, UpdateTaskRequest, db::cleanup_finished_task_data, emit, finish, now,
    open_db, save_permission_mode, uuid_like,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::stream;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{collections::HashMap, time::Duration};
use tokio_util::sync::CancellationToken;

async fn clear_active_task(state: &AgentServiceState, task_id: &str, run_id: &str) {
    let mut active = state.cancellations.lock().await;
    if active
        .get(task_id)
        .is_some_and(|entry| entry.run_id == run_id)
    {
        active.remove(task_id);
    }
}

fn task_provider_name(
    routes: &HashMap<String, AgentProviderRoute>,
    default_name: &str,
    saved_name: Option<&str>,
    model: &str,
) -> Option<String> {
    if routes.is_empty() {
        return Some(default_name.to_owned());
    }
    if let Some(name) = saved_name.filter(|name| !name.is_empty()) {
        return routes
            .get(name)
            .filter(|route| route.models.contains_key(model))
            .map(|_| name.to_owned());
    }
    if routes
        .get(default_name)
        .is_some_and(|route| route.models.contains_key(model))
    {
        return Some(default_name.to_owned());
    }
    let mut matches = routes
        .iter()
        .filter(|(_, route)| route.models.contains_key(model));
    let name = matches.next()?.0.clone();
    matches.next().is_none().then_some(name)
}

pub async fn create_draft_task(
    State(state): State<AgentServiceState>,
    input: Option<Json<DraftSessionInput>>,
) -> impl IntoResponse {
    let state = state.with_latest_config().await;
    if state.plugin_cancel.is_cancelled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"agent task plugin is stopped"})),
        )
            .into_response();
    }
    let task_id = format!("task_{}", uuid_like());
    let root = state.workspace.root().to_path_buf();
    let input = input.map(|Json(input)| input).unwrap_or_default();
    let kind = match input.kind.as_deref() {
        Some("chat") => "chat",
        Some("observer") => "observer",
        Some("blueprint") => "blueprint",
        _ => "code",
    };
    let watched_task_id = input
        .watched_task_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let archived = i32::from(matches!(kind, "observer" | "blueprint"));
    let selected_workspace = match input.workspace {
        Some(path) => match std::fs::canonicalize(path) {
            Ok(path) if path.is_dir() => crate::tools::file_edit::strip_verbatim_prefix(&path),
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"项目目录不存在"})),
                )
                    .into_response();
            }
        },
        None if kind == "chat" => match crate::context::session_browser::chat_assets_dir(&root) {
            Ok(dir) => dir,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":"could not prepare chat assets folder"})),
                )
                    .into_response();
            }
        },
        None => root.clone(),
    };
    let model = state.default_model.clone();
    let provider_name = state.provider_name.clone();
    let reasoning_effort = state.reasoning_effort.clone();
    let fast_mode = state.fast_mode;
    let id = task_id.clone();
    let inserted = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut conn = open_db(&root)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at,provider_name,reasoning_effort,fast_mode) VALUES (?1,'',?2,'draft',?3,?3,?4,?5,?6)",
            params![id, model, now(), provider_name, reasoning_effort, fast_mode],
        )?;
        tx.execute(
            "INSERT INTO agent_session_meta(task_id,archived,workspace,kind,watched_task_id) VALUES (?1,?2,?3,?4,?5)",
            params![id, archived, selected_workspace.to_string_lossy(), kind, watched_task_id],
        )?;
        tx.commit()?;
        Ok(())
    }).await;
    match inserted {
        Ok(Ok(())) => (StatusCode::CREATED, Json(json!({"task_id":task_id}))).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"could not create session"})),
        )
            .into_response(),
    }
}

enum ResumeLoad {
    InvalidAttachment(String),
    AlreadyAccepted,
    Ready {
        model: String,
        provider_name: String,
        reasoning_effort: Option<String>,
        fast_mode: bool,
        permission_mode: PermissionMode,
        turn: usize,
    },
    NotFound,
    ChildTask,
    Busy,
    ModelUnavailable,
}

pub async fn create_task(
    State(state): State<AgentServiceState>,
    Json(request): Json<CreateTaskRequest>,
) -> impl IntoResponse {
    let mut state = state.with_latest_config().await;
    state.permission_mode = request.permission_mode;
    if state.plugin_cancel.is_cancelled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"agent task plugin is stopped"})),
        )
            .into_response();
    }
    let prompt = request.prompt.trim().to_owned();
    if prompt.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"prompt must not be empty"})),
        )
            .into_response();
    }
    if state.provider_url.is_empty() || state.default_model.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"agent model provider is not configured"})),
        )
            .into_response();
    }
    let model = match request.model {
        Some(requested) => match state.model_map.get(&requested).cloned().or_else(|| {
            state
                .model_map
                .values()
                .any(|value| value == &requested)
                .then_some(requested)
        }) {
            Some(model) => model,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"model is not configured"})),
                )
                    .into_response();
            }
        },
        None => state.default_model.clone(),
    };
    let task_id = format!("task_{}", uuid_like());
    let root = state.workspace.root().to_path_buf();
    let provider_name = state.provider_name.clone();
    let reasoning_effort = state.reasoning_effort.clone();
    let fast_mode = state.fast_mode;
    let insert = tokio::task::spawn_blocking({
        let task_id = task_id.clone(); let prompt = prompt.clone(); let model = model.clone();
        let root = root.clone(); move || -> anyhow::Result<()> {
            let conn = open_db(&root)?;
            conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at,provider_name,reasoning_effort,fast_mode) VALUES (?1,?2,?3,'running',?4,?4,?5,?6,?7)", params![task_id,prompt,model,now(),provider_name,reasoning_effort,fast_mode])?;
            Ok(())
        }
    }).await;
    if !matches!(insert, Ok(Ok(()))) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"could not create task"})),
        )
            .into_response();
    }
    let cancel = state.plugin_cancel.child_token();
    let run_id = uuid_like();
    state.cancellations.lock().await.insert(
        task_id.clone(),
        ActiveTask {
            run_id: run_id.clone(),
            token: cancel.clone(),
        },
    );
    let runner_state = state.clone();
    let runner_id = task_id.clone();
    spawn_supervised_task(
        runner_state,
        runner_id,
        run_id,
        model,
        prompt,
        cancel,
        1,
        false,
        None,
    );
    (StatusCode::ACCEPTED, Json(json!({"task_id":task_id,"status":"running","events_url":format!("/agent/tasks/{task_id}/events")}))).into_response()
}

pub async fn workspace_files(
    State(state): State<AgentServiceState>,
    Query(query): Query<FileQuery>,
) -> impl IntoResponse {
    let root = state.workspace.root().to_path_buf();
    let q = query.q.unwrap_or_default();
    let files =
        tokio::task::spawn_blocking(move || crate::server::composer_catalog::search_files(&root, &q))
            .await
            .unwrap_or_default();
    Json(json!({"files": files})).into_response()
}

pub async fn slash_commands(State(state): State<AgentServiceState>) -> impl IntoResponse {
    let root = state.workspace.root().to_path_buf();
    let mut commands = serde_json::to_value(
        tokio::task::spawn_blocking(move || crate::server::composer_catalog::slash_commands(&root))
            .await
            .unwrap_or_default(),
    )
    .unwrap_or(json!([]));
    if let Some(catalog) = state.tool_catalog {
        for skill in catalog.packages.skills().as_array().into_iter().flatten() {
            if !commands
                .as_array()
                .unwrap()
                .iter()
                .any(|command| command["name"] == skill["name"])
            {
                commands.as_array_mut().unwrap().push(skill.clone());
            }
        }
    }
    Json(json!({"commands": commands})).into_response()
}

pub async fn continue_task(
    State(state): State<AgentServiceState>,
    Path(task_id): Path<String>,
    Json(mut request): Json<ContinueTaskRequest>,
) -> impl IntoResponse {
    let mut state = state.with_latest_config().await;
    if let Err(error) = crate::model::model_catalog::prepare_turn(&mut state, &task_id, &mut request).await
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":format!("{error:#}")})),
        )
            .into_response();
    }
    if state.plugin_cancel.is_cancelled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"agent task plugin is stopped"})),
        )
            .into_response();
    }
    let prompt = if request.prompt.trim().is_empty() {
        request
            .content
            .iter()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        request.prompt.trim().to_owned()
    };
    if prompt.is_empty() && request.content.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"prompt must not be empty"})),
        )
            .into_response();
    }
    if state.provider_url.is_empty() || state.default_model.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"agent model provider is not configured"})),
        )
            .into_response();
    }
    let root = state.workspace.root().to_path_buf();
    let read_id = task_id.clone();
    let default_model = state.default_model.clone();
    let default_provider = state.provider_name.clone();
    let default_effort = state.reasoning_effort.clone();
    let default_fast_mode = state.fast_mode;
    let routes = state.provider_routes.clone();
    let title = if prompt.is_empty() {
        "附件消息".to_owned()
    } else {
        prompt.clone()
    };
    let attachment_content = request.content.clone();
    let attachment_prompt = prompt.clone();
    let attachment_request_id = request.request_id.clone();
    let attachment_policy = match crate::media::attachments::policy(&state, &task_id) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":e.to_string()})),
            )
                .into_response();
        }
    };
    let req_model = request.model.clone();
    let req_provider = request.provider.clone();
    let req_effort = request.reasoning_effort.clone();
    let req_fast = request.fast_mode;
    let req_permission = request.permission_mode;
    let program_resource = request.program_resource.clone();
    if program_resource.is_some()
        && crate::server::plugin_sessions::owner(state.workspace.root(), &task_id)
            .ok()
            .flatten()
            .is_none()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"program resources require a plugin session"})),
        )
            .into_response();
    }
    let loaded = tokio::task::spawn_blocking(move || -> anyhow::Result<ResumeLoad> {
        let mut conn = open_db(&root)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(request_id)=&attachment_request_id {
            match crate::media::attachments::already(&tx,&read_id,request_id,&attachment_prompt,&attachment_content) {
                Ok(true)=>return Ok(ResumeLoad::AlreadyAccepted), Ok(false)=>{},
                Err(e)=>return Ok(ResumeLoad::InvalidAttachment(e.to_string())),
            }
        }
        let task: Option<(String, String, Option<String>, Option<String>, Option<String>, i64, Option<String>)> = tx.query_row(
            "SELECT model,status,parent_task_id,provider_name,reasoning_effort,fast_mode,permission_mode FROM agent_tasks WHERE id=?1", [&read_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        ).optional()?;
        let Some((model, status, parent_id, saved_provider, saved_effort, saved_fast_mode, saved_permission)) = task else { return Ok(ResumeLoad::NotFound); };
        if parent_id.is_some() { return Ok(ResumeLoad::ChildTask); }
        if !matches!(status.as_str(), "draft" | "completed" | "failed" | "cancelled" | "max_steps" | "interrupted") {
            return Ok(ResumeLoad::Busy);
        }
        let events = crate::context::session_history::load_events(&tx, &read_id, &Default::default())?
            .into_iter().map(|event| {
                let mut data = event.data()?;
                data["_replay_source"] = crate::context::session_history::event_source(&read_id,event.seq,event.time,&event.kind,&data);
                Ok((event.kind,data))
            }).collect::<anyhow::Result<Vec<_>>>()?;
        // The single-agent runtime restores its durable window itself, including
        // importing legacy records without requiring legacy native tool pairs.
        let last_event_turn = events.iter().filter_map(|(_,data)|data["turn"].as_u64()).max().unwrap_or(0) as usize;
        let turn = crate::media::attachments::next_turn(&tx, &read_id, last_event_turn)?;
        // This chat's own level, chosen in the composer or sent with this message.
        // A chat that never stored one asks first.
        let permission_mode = req_permission
            .or_else(|| saved_permission.as_deref().and_then(PermissionMode::parse))
            .unwrap_or_default();
        let (model, provider_name, reasoning_effort, fast_mode) = if status == "draft" {
            let m = req_model.unwrap_or(default_model);
            let p = req_provider.or(saved_provider).unwrap_or(default_provider);
            let e = req_effort.or(saved_effort).or(default_effort);
            let f = req_fast.unwrap_or_else(|| saved_fast_mode != 0 || default_fast_mode);
            (m, p, e, f)
        } else {
            let target_model = req_model.unwrap_or(model);
            let target_provider = req_provider.or(saved_provider);
            let Some(provider_name) = task_provider_name(&routes, &default_provider, target_provider.as_deref(), &target_model)
                else { return Ok(ResumeLoad::ModelUnavailable); };
            let reasoning_effort = req_effort.or(saved_effort);
            let fast_mode = req_fast.unwrap_or(saved_fast_mode != 0);
            (target_model, provider_name, reasoning_effort, fast_mode)
        };
        let reasoning_effort=reasoning_effort.filter(|e|!e.is_empty());
        if let Err(e)=crate::media::attachments::admit(&tx,&root,&read_id,turn,&attachment_prompt,&attachment_content,attachment_request_id.as_deref(),attachment_policy.as_deref()) {
            return Ok(ResumeLoad::InvalidAttachment(e.to_string()));
        }
        let changed = tx.execute(
            "UPDATE agent_tasks SET model=?2,status='running',updated_at=?3,prompt=CASE WHEN prompt='' THEN ?5 ELSE prompt END,provider_name=?6,reasoning_effort=?7,fast_mode=?8,permission_mode=?9 WHERE id=?1 AND status=?4",
            params![read_id, model, now(), status, title, provider_name, reasoning_effort, fast_mode, permission_mode.as_str()],
        )?;
        if changed == 0 { return Ok(ResumeLoad::Busy); }
        tx.commit()?;
        Ok(ResumeLoad::Ready { model, provider_name, reasoning_effort, fast_mode, permission_mode, turn })
    }).await;
    let (model, provider_name, reasoning_effort, fast_mode, permission_mode, turn) = match loaded {
        Ok(Ok(ResumeLoad::AlreadyAccepted)) => {
            return (
                StatusCode::ACCEPTED,
                Json(json!({"task_id":task_id,"already_accepted":true})),
            )
                .into_response();
        }
        Ok(Ok(ResumeLoad::InvalidAttachment(error))) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error":error}))).into_response();
        }
        Ok(Ok(ResumeLoad::Ready {
            model,
            provider_name,
            reasoning_effort,
            fast_mode,
            permission_mode,
            turn,
        })) => (
            model,
            provider_name,
            reasoning_effort,
            fast_mode,
            permission_mode,
            turn,
        ),
        Ok(Ok(ResumeLoad::NotFound)) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"task not found"})),
            )
                .into_response();
        }
        Ok(Ok(ResumeLoad::ChildTask)) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"subagent tasks cannot receive follow-up messages"})),
            )
                .into_response();
        }
        Ok(Ok(ResumeLoad::Busy)) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"task is still running"})),
            )
                .into_response();
        }
        Ok(Ok(ResumeLoad::ModelUnavailable)) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"task model is no longer configured"})),
            )
                .into_response();
        }
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"could not load task history"})),
            )
                .into_response();
        }
    };
    if !state.use_provider(&provider_name) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"task provider is no longer configured"})),
        )
            .into_response();
    }
    state.reasoning_effort = reasoning_effort;
    state.fast_mode = fast_mode;
    state.permission_mode = permission_mode;
    let cancel = state.plugin_cancel.child_token();
    let run_id = uuid_like();
    state.cancellations.lock().await.insert(
        task_id.clone(),
        ActiveTask {
            run_id: run_id.clone(),
            token: cancel.clone(),
        },
    );
    let runner_state = state.clone();
    let runner_id = task_id.clone();
    spawn_supervised_task(
        runner_state,
        runner_id,
        run_id,
        model,
        prompt,
        cancel,
        turn,
        false,
        program_resource,
    );
    (
        StatusCode::ACCEPTED,
        Json(json!({"task_id":task_id,"status":"running","turn":turn})),
    )
        .into_response()
}

#[derive(serde::Serialize)]
struct HttpEvent {
    seq: i64,
    time: i64,
    #[serde(rename = "type")]
    kind: String,
    data: Box<serde_json::value::RawValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    surface_op: Option<String>,
}

use crate::context::session_history::{EventQuery, StoredEvent, load_events};

impl TryFrom<StoredEvent> for HttpEvent {
    type Error = serde_json::Error;
    fn try_from(event: StoredEvent) -> Result<Self, Self::Error> {
        Ok(Self {
            seq: event.seq,
            time: event.time,
            kind: event.kind,
            data: serde_json::value::RawValue::from_string(event.raw)?,
            surface_op: event.surface_op,
        })
    }
}

const RESULT_THRESHOLD: usize = 16 * 1024;
const PREVIEW_BYTES: usize = 4 * 1024;

fn truncate_text(text: &mut String, remaining: &mut usize) {
    text.truncate(text.floor_char_boundary(*remaining));
    *remaining -= text.len();
}

fn tool_preview(event: &mut StoredEvent) -> anyhow::Result<()> {
    let mut data = event.data()?;
    let Some(object) = data.as_object_mut() else { return Ok(()); };
    let mut remaining = PREVIEW_BYTES;
    if let Some(content) = object.get_mut("message").and_then(|message| message.get_mut("content")) {
        match content {
            Value::String(text) => truncate_text(text, &mut remaining),
            Value::Array(parts) => {
                parts.truncate(16);
                for part in parts {
                    if let Some(text) = part.get_mut("text").and_then(|text| match text {
                        Value::String(text) => Some(text),
                        _ => None,
                    }) {
                        truncate_text(text, &mut remaining);
                    } else if serde_json::to_vec(part)?.len() > 1024 {
                        *part = json!({"type": part["type"]});
                    }
                }
            }
            _ => *content = Value::Null,
        }
    }
    if let Some(result) = object.get_mut("meta").and_then(|meta| meta.get_mut("result")) {
        if serde_json::to_vec(result)?.len() > PREVIEW_BYTES {
            let mut summary = serde_json::Map::new();
            for key in ["name", "status", "is_error", "child_task_id", "node_id", "asset_id",
                "error", "message", "diff", "summary", "added", "deleted", "exit_code", "session_id"] {
                if let Some(value) = result.get(key) {
                    let mut value = value.clone();
                    if let Value::String(text) = &mut value {
                        truncate_text(text, &mut 512_usize);
                    }
                    if serde_json::to_vec(&value)?.len() <= 512 {
                        summary.insert(key.to_owned(), value);
                    }
                }
            }
            if let Some(diff) = result.get("diff").and_then(Value::as_object) {
                let stats: serde_json::Map<_, _> = ["added", "deleted", "insertions", "deletions"]
                    .into_iter().filter_map(|key| diff.get(key).filter(|value| value.is_number())
                        .map(|value| (key.to_owned(), value.clone()))).collect();
                if !stats.is_empty() && !summary.contains_key("diff") {
                    summary.insert("diff".into(), Value::Object(stats));
                }
            }
            *result = Value::Object(summary);
        }
    }
    object.insert("truncated".into(), json!(true));
    object.insert("full_bytes".into(), json!(event.raw.len()));
    object.insert("detail_seq".into(), json!(event.seq));
    event.raw = serde_json::to_string(&data)?;
    Ok(())
}

fn history_events(conn: &rusqlite::Connection, task_id: &str) -> anyhow::Result<Vec<HttpEvent>> {
    use std::collections::HashSet;
    let mut ended = HashSet::new();
    let mut delivered = HashSet::new();
    let mut snapshots = HashSet::new();
    let mut output = Vec::new();
    // Work backwards: only a later event can supersede a delta or snapshot.
    for mut event in load_events(conn, task_id, &EventQuery::default())?.into_iter().rev() {
        if matches!(event.kind.as_str(), "turn/end" | "assistant/message" | "assistant/delta" | "workspace/changes") {
            #[derive(serde::Deserialize)]
            struct Coordinates<'a> {
                turn: Option<u64>,
                step: Option<u64>,
                task_node_id: Option<&'a str>,
            }
            let data: Coordinates<'_> = serde_json::from_str(&event.raw)?;
            if let Some(turn) = data.turn {
                let slot = (turn, data.step.unwrap_or(0), data.task_node_id.unwrap_or("").to_owned());
                match event.kind.as_str() {
                    "turn/end" => { ended.insert(turn); }
                    "assistant/message" => { delivered.insert(slot); }
                    "assistant/delta" if ended.contains(&turn) || delivered.contains(&slot) => continue,
                    "workspace/changes" if !snapshots.insert(turn) => continue,
                    _ => {}
                }
            }
        }
        if event.kind == "tool/result" && event.raw.len() > RESULT_THRESHOLD {
            tool_preview(&mut event)?;
        }
        output.push(event.try_into()?);
    }
    output.reverse();
    Ok(output)
}

pub async fn get_events(
    State(state): State<AgentServiceState>,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    #[derive(serde::Serialize)]
    struct Events { events: Vec<HttpEvent> }
    let root = state.workspace.root().to_path_buf();
    let result = tokio::task::spawn_blocking(move || history_events(&open_db(&root)?, &task_id)).await;
    match result {
        Ok(Ok(events)) => Json(Events { events }).into_response(),
        _ => event_read_error(),
    }
}

pub async fn get_event(
    State(state): State<AgentServiceState>,
    Path((task_id, seq)): Path<(String, i64)>,
) -> impl IntoResponse {
    let root = state.workspace.root().to_path_buf();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<HttpEvent>> {
        load_events(&open_db(&root)?, &task_id, &EventQuery { exact: Some(seq), ..Default::default() })?
            .pop().map(HttpEvent::try_from).transpose().map_err(Into::into)
    }).await;
    match result {
        Ok(Ok(Some(event))) => Json(event).into_response(),
        Ok(Ok(None)) => (StatusCode::NOT_FOUND, Json(json!({"error":"event not found"}))).into_response(),
        _ => event_read_error(),
    }
}

fn event_read_error() -> axum::response::Response {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"could not read task events"}))).into_response()
}

pub async fn stream_events(
    State(state): State<AgentServiceState>,
    Path(task_id): Path<String>,
    Query(query): Query<StreamEventsQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|seq| *seq >= 0)
        .unwrap_or(-1)
        .max(query.since.unwrap_or(-1));
    let events = stream::unfold(
        (state, task_id, last_event_id, std::collections::VecDeque::<HttpEvent>::new()),
        |(state, task_id, mut last_seq, mut pending)| async move {
            loop {
                if let Some(event) = pending.pop_front() {
                    last_seq = event.seq;
                    let id = event.seq.to_string();
                    let payload = serde_json::to_string(&event).ok()?;
                    let sse = Event::default().event("session/event").id(id).data(payload);
                    return Some((Ok::<Event, std::convert::Infallible>(sse), (state, task_id, last_seq, pending)));
                }
                let root = state.workspace.root().to_path_buf();
                let read_id = task_id.clone();
                let read = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                    let mut conn = open_db(&root)?;
                    // Status and events must come from the same snapshot; otherwise a
                    // concurrent finish could close the stream before its final event.
                    let tx = conn.transaction()?;
                    let events = load_events(&tx, &read_id, &EventQuery {
                        after: Some(last_seq), oldest: Some(32), ..Default::default()
                    })?.into_iter().map(HttpEvent::try_from).collect::<Result<_, _>>()?;
                    let status: Option<String> = tx.query_row(
                        "SELECT status FROM agent_tasks WHERE id=?1", [read_id], |row| row.get(0),
                    ).optional()?;
                    Ok((events, status))
                }).await;
                let (events, status) = read.ok()?.ok()?;
                pending = events;
                if pending.is_empty() {
                    if status.as_deref() != Some("running") { return None; }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        },
    );
    Sse::new(events).keep_alive(KeepAlive::default())
}

#[derive(serde::Deserialize)]
pub struct WorkspaceFileQuery {
    path: String,
}

pub async fn workspace_file(
    State(state): State<AgentServiceState>,
    Path(task_id): Path<String>,
    Query(query): Query<WorkspaceFileQuery>,
    method: axum::http::Method,
    headers: HeaderMap,
) -> axum::response::Response {
    use crate::server::file_stream;
    let store = state.workspace.root().to_path_buf();
    let found = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let db = open_db(&store)?;
        let (workspace, kind): (Option<String>, Option<String>) = db.query_row(
            "SELECT m.workspace, m.kind FROM agent_tasks t LEFT JOIN agent_session_meta m ON m.task_id=t.id WHERE t.id=?1",
            [&task_id], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        anyhow::ensure!(crate::server::plugin_sessions::owner(&store, &task_id)?.is_none(), "plugin session");
        let is_chat = kind.as_deref() == Some("chat");
        let primary_root = if is_chat {
            crate::context::session_browser::task_workspace(&store, &task_id)?
        } else {
            workspace.map(std::path::PathBuf::from).unwrap_or_else(|| store.clone())
        };
        let open_under = |root: &std::path::Path| -> anyhow::Result<_> {
            let path = file_stream::workspace_relative(root, &query.path)
                .ok_or_else(|| anyhow::anyhow!("invalid workspace path"))?;
            let media = mime_guess::from_path(&path).first_or_octet_stream();
            Ok((file_stream::open_regular(root, &path)?, media))
        };
        match open_under(&primary_root) {
            Ok(found) => Ok(found),
            Err(err) if is_chat && primary_root != store => open_under(&store).or(Err(err)),
            Err(err) => Err(err),
        }
    }).await;
    if let Ok(Ok((file, media))) = found {
        let inline = media.type_() == "image";
        if let Ok(response) = file_stream::respond(file, media.as_ref(), inline, method, &headers).await {
            return response;
        }
    }
    (StatusCode::NOT_FOUND, Json(json!({"error":"workspace file not found"}))).into_response()
}

pub async fn get_task(
    State(state): State<AgentServiceState>,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    let root = state.workspace.root().to_path_buf();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Value>> {
        let conn = open_db(&root)?;
        Ok(conn.query_row("SELECT id,prompt,model,status,created_at,updated_at,parent_task_id,provider_name,reasoning_effort,fast_mode FROM agent_tasks WHERE id=?1", [&task_id], |r| Ok(json!({"task_id":r.get::<_,String>(0)?,"prompt":r.get::<_,String>(1)?,"model":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?,"created_at":r.get::<_,i64>(4)?,"updated_at":r.get::<_,i64>(5)?,"parent_task_id":r.get::<_,Option<String>>(6)?,"provider_name":r.get::<_,Option<String>>(7)?,"reasoning_effort":r.get::<_,Option<String>>(8)?,"fast_mode":r.get::<_,i64>(9)? != 0}))).optional()?)
    }).await;
    match result {
        Ok(Ok(Some(task))) => match open_db(state.workspace.root())
            .and_then(|conn| crate::context::session_browser::decorate(&conn, state.workspace.root(), task))
        {
            Ok(task) => Json(task).into_response(),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"could not read sidebar state"})),
            )
                .into_response(),
        },
        Ok(Ok(None)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"task not found"})),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"could not read task"})),
        )
            .into_response(),
    }
}

pub async fn update_task(
    State(state): State<AgentServiceState>,
    Path(task_id): Path<String>,
    Json(request): Json<UpdateTaskRequest>,
) -> impl IntoResponse {
    let root = state.workspace.root().to_path_buf();
    let updated = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Value>> {
        let conn = open_db(&root)?;
        let exists: Option<(String, String)> = conn.query_row(
            "SELECT status, model FROM agent_tasks WHERE id=?1",
            [&task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        let Some((status, _)) = exists else { return Ok(None); };
        // The level belongs to the chat, not to a turn: it is saved even while one
        // runs, so switching it mid-turn is remembered.
        if let Some(mode) = request.permission_mode {
            save_permission_mode(&conn, &task_id, mode)?;
        }
        if status == "running" || status == "cancelling" {
            let changes_generation = request.model.is_some()
                || request.provider.is_some()
                || request.reasoning_effort.is_some()
                || request.fast_mode.is_some();
            if changes_generation {
                anyhow::bail!("task is currently running");
            }
        }
        if let Some(ref model) = request.model {
            conn.execute("UPDATE agent_tasks SET model=?2, updated_at=?3 WHERE id=?1", params![&task_id, model, now()])?;
        }
        if let Some(ref provider) = request.provider {
            conn.execute("UPDATE agent_tasks SET provider_name=?2, updated_at=?3 WHERE id=?1", params![&task_id, provider, now()])?;
        }
        if let Some(ref effort) = request.reasoning_effort {
            conn.execute("UPDATE agent_tasks SET reasoning_effort=?2, updated_at=?3 WHERE id=?1", params![&task_id, effort, now()])?;
        }
        if let Some(fast) = request.fast_mode {
            conn.execute("UPDATE agent_tasks SET fast_mode=?2, updated_at=?3 WHERE id=?1", params![&task_id, if fast { 1 } else { 0 }, now()])?;
        }
        Ok(conn.query_row(
            "SELECT id,prompt,model,status,created_at,updated_at,parent_task_id,provider_name,reasoning_effort,fast_mode FROM agent_tasks WHERE id=?1",
            [&task_id],
            |r| Ok(json!({
                "task_id": r.get::<_,String>(0)?,
                "prompt": r.get::<_,String>(1)?,
                "model": r.get::<_,String>(2)?,
                "status": r.get::<_,String>(3)?,
                "created_at": r.get::<_,i64>(4)?,
                "updated_at": r.get::<_,i64>(5)?,
                "parent_task_id": r.get::<_,Option<String>>(6)?,
                "provider_name": r.get::<_,Option<String>>(7)?,
                "reasoning_effort": r.get::<_,Option<String>>(8)?,
                "fast_mode": r.get::<_,i64>(9)? != 0
            })),
        ).optional()?)
    }).await;
    match updated {
        Ok(Ok(Some(task))) => match open_db(state.workspace.root())
            .and_then(|conn| crate::context::session_browser::decorate(&conn, state.workspace.root(), task))
        {
            Ok(task) => Json(task).into_response(),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"could not read sidebar state"})),
            )
                .into_response(),
        },
        Ok(Ok(None)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"task not found"})),
        )
            .into_response(),
        Ok(Err(err)) => (
            StatusCode::CONFLICT,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"could not update task"})),
        )
            .into_response(),
    }
}

pub async fn list_tasks(
    State(state): State<AgentServiceState>,
    Query(query): Query<ListTasksQuery>,
) -> impl IntoResponse {
    let root = state.workspace.root().to_path_buf();
    let limit = query.limit.unwrap_or(50).clamp(1, 10000) as i64;
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Value>> {
        let conn = open_db(&root)?;
        let mut stmt = conn.prepare("SELECT t.id,t.prompt,t.model,t.status,t.created_at,t.updated_at,t.parent_task_id,t.provider_name,t.reasoning_effort,t.fast_mode,p.plugin_id FROM agent_tasks t LEFT JOIN plugin_sessions p ON p.task_id=t.id WHERE trim(t.prompt)<>'' OR t.status IN ('running','cancelling') OR EXISTS(SELECT 1 FROM agent_task_events e WHERE e.task_id=t.id AND e.kind='user/message') ORDER BY t.updated_at DESC,t.id LIMIT ?1")?;
        let rows = stmt.query_map([limit], |r| Ok(json!({"task_id":r.get::<_,String>(0)?,"prompt":r.get::<_,String>(1)?,"model":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?,"created_at":r.get::<_,i64>(4)?,"updated_at":r.get::<_,i64>(5)?,"parent_task_id":r.get::<_,Option<String>>(6)?,"provider_name":r.get::<_,Option<String>>(7)?,"reasoning_effort":r.get::<_,Option<String>>(8)?,"fast_mode":r.get::<_,i64>(9)? != 0,"plugin_id":r.get::<_,Option<String>>(10)?})))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().map(|task|crate::context::session_browser::decorate(&conn,&root,task)).collect()
    }).await;
    match result {
        Ok(Ok(tasks)) => Json(json!({"tasks":tasks})).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"could not list tasks"})),
        )
            .into_response(),
    }
}

pub async fn cancel_task(
    State(state): State<AgentServiceState>,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    if let Some(active) = state.cancellations.lock().await.get(&task_id).cloned() {
        active.token.cancel();
        let root = state.workspace.root().to_path_buf();
        let id = task_id.clone();
        let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = open_db(&root)?;
            conn.execute(
                "UPDATE agent_tasks SET status='cancelling', updated_at=?1 WHERE id=?2 AND status='running'",
                params![now(), id],
            )?;
            Ok(())
        })
        .await;
        return Json(json!({"task_id":task_id,"status":"cancelling"})).into_response();
    }
    // Recover orphaned tasks stuck in `running`/`cancelling` in SQLite when no active runner exists.
    let root = state.workspace.root().to_path_buf();
    let id = task_id.clone();
    let orphan_turn = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<usize>> {
        let conn = open_db(&root)?;
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM agent_tasks WHERE id=?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        if !matches!(status.as_deref(), Some("running" | "cancelling")) {
            return Ok(None);
        }
        let turn: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(CAST(json_extract(data,'$.turn') AS INTEGER)), 1) FROM agent_task_events WHERE task_id=?1 AND kind='turn/start'",
                [&id],
                |r| r.get(0),
            )
            .unwrap_or(1);
        Ok(Some(turn.max(1) as usize))
    })
    .await
    .ok()
    .and_then(|res| res.ok())
    .flatten();
    if let Some(turn) = orphan_turn {
        let _ = emit(
            state.workspace.root(),
            &task_id,
            "turn/end",
            json!({"turn": turn, "reason": {"kind": "aborted"}}),
        )
        .await;
        let _ = finish(&state, &task_id, "cancelled").await;
        return Json(json!({"task_id":task_id,"status":"cancelled"})).into_response();
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error":"task is not running"})),
    )
        .into_response()
}

fn spawn_supervised_task(
    state: AgentServiceState,
    task_id: String,
    run_id: String,
    model: String,
    prompt: String,
    cancel: CancellationToken,
    turn: usize,
    ban_run_command: bool,
    program_resource: Option<String>,
) {
    tokio::spawn(async move {
        let cancel_check = cancel.clone();
        let worker = tokio::spawn(crate::runtime::single_agent::run_task_with_resource(
            state.clone(),
            task_id.clone(),
            model,
            prompt,
            // A turn runs until the AI answers or the user stops it.
            crate::runtime::single_agent::NO_ROUND_LIMIT,
            cancel,
            turn,
            ban_run_command,
            program_resource,
        ));
        let failure = match worker.await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(error) => Some(format!("task worker stopped unexpectedly: {error}")),
        };
        if let Some(message) = failure {
            let root = state.workspace.root();
            if cancel_check.is_cancelled() {
                tracing::info!(task_id = %task_id, "agent task cancelled by user");
                if let Err(error) = emit(
                    root,
                    &task_id,
                    "turn/end",
                    json!({"turn":turn,"reason":{"kind":"aborted"}}),
                )
                .await
                {
                    tracing::error!(task_id = %task_id, error = %error, "could not record task cancellation");
                }
                if let Err(error) = finish(&state, &task_id, "cancelled").await {
                    tracing::error!(task_id = %task_id, error = %error, "could not finish cancelled task");
                }
            } else {
                tracing::error!(task_id = %task_id, error = %message, "agent task failed");
                if let Err(error) = emit(root, &task_id, "turn/end",
                    json!({"turn":turn,"reason":{"kind":"error","error":{"message":message,"code":"WORKER_FAILED"}}})).await {
                    tracing::error!(task_id = %task_id, error = %error, "could not record task failure");
                }
                if let Err(error) = finish(&state, &task_id, "failed").await {
                    tracing::error!(task_id = %task_id, error = %error, "could not finish failed task");
                }
            }
        }
        if let Err(error) = cleanup_finished_task_data(state.workspace.root(), &task_id).await {
            tracing::warn!(task_id = %task_id, error = %error, "could not clean terminal task execution data");
        }
        clear_active_task(&state, &task_id, &run_id).await;
    });
}
