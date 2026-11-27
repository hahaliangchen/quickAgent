//! Page conversations use the host Agent with a durable, non-escalatable owner.
use crate::server::{
    agent_service::{self, AgentServiceState},
    plugin_protocol::PromptProfile,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::stream;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{collections::VecDeque, convert::Infallible, time::Duration};

/// Allowed event kinds exposed to plugin sessions (both JSON `events` and SSE `stream`).
const PLUGIN_EVENT_KINDS: &[&str] = &[
    "user/message",
    "assistant/message",
    "assistant/delta",
    "task/node_started",
    "task/node_returned",
    "tool/call",
    "tool/result",
    "turn/start",
    "turn/end",
    "subagent/start",
    "subagent/event",
    "subagent/end",
    "subagent/review",
];

fn sanitize_plugin_event(
    e: crate::context::session_history::StoredEvent,
) -> Result<agent_service::AgentEvent> {
    let mut data = e.data()?;
    // Plugin pages need frame identity and the returned value, not
    // the potentially huge source HTML or captured inputs.
    if matches!(e.kind.as_str(), "task/node_started" | "task/node_returned") {
        data = json!({
            "node_id": data["node_id"],
            "task_node_id": data["task_node_id"],
            "parent_id": data["parent_id"],
            "metadata": data["metadata"],
            "title": data["title"],
            "goal": data["goal"],
            "result": data["result"],
            "turn": data["turn"]
        });
    }
    Ok(agent_service::AgentEvent {
        seq: e.seq,
        time: e.time,
        data,
        kind: e.kind,
        surface_op: e.surface_op,
    })
}

fn latest_event_seq(root: &std::path::Path, task: &str) -> Result<i64> {
    let conn = agent_service::open_db(root)?;
    let seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), -1) FROM agent_task_events WHERE task_id=?1",
        [task],
        |r| r.get(0),
    )?;
    Ok(seq)
}

fn scope_is_active(state: &AgentServiceState, plugin_id: &str, package_scope: u64) -> bool {
    !state.plugin_cancel.is_cancelled()
        && state.tool_catalog.as_ref().is_some_and(|catalog| {
            catalog
                .packages
                .scopes(&std::collections::BTreeSet::from([plugin_id.to_owned()]))
                .get(plugin_id)
                .copied()
                == Some(package_scope)
        })
}

fn stream_plugin_events(
    state: AgentServiceState,
    plugin_id: String,
    package_scope: u64,
    task: String,
    after_seq: i64,
) -> Response {
    let events = stream::unfold(
        (
            state,
            plugin_id,
            package_scope,
            task,
            after_seq,
            VecDeque::<agent_service::AgentEvent>::new(),
            false,
        ),
        |(state, plugin_id, package_scope, task, mut last_seq, mut pending, done_sent)| async move {
            if done_sent {
                return None;
            }
            loop {
                if let Some(event) = pending.pop_front() {
                    last_seq = event.seq;
                    let id = event.seq.to_string();
                    let payload = serde_json::to_string(&event).ok()?;
                    let sse = Event::default().event("session/event").id(id).data(payload);
                    return Some((
                        Ok::<Event, Infallible>(sse),
                        (state, plugin_id, package_scope, task, last_seq, pending, false),
                    ));
                }
                if !scope_is_active(&state, &plugin_id, package_scope) {
                    let sse = Event::default().event("session/done").data(
                        json!({"task_id": task, "status": "cancelled"}).to_string(),
                    );
                    return Some((
                        Ok::<Event, Infallible>(sse),
                        (state, plugin_id, package_scope, task, last_seq, pending, true),
                    ));
                }
                let root = state.workspace.root().to_path_buf();
                let read_id = task.clone();
                let read = tokio::task::spawn_blocking(
                    move || -> Result<(VecDeque<agent_service::AgentEvent>, Option<String>)> {
                        let mut conn = agent_service::open_db(&root)?;
                        // Status and events must come from the same snapshot; otherwise a
                        // concurrent finish could close the stream before its final event.
                        let tx = conn.transaction()?;
                        let batch = crate::context::session_history::load_events(
                            &tx,
                            &read_id,
                            &crate::context::session_history::EventQuery {
                                after: Some(last_seq),
                                oldest: Some(64),
                                kinds: PLUGIN_EVENT_KINDS,
                                ..Default::default()
                            },
                        )?
                        .into_iter()
                        .map(sanitize_plugin_event)
                        .collect::<Result<VecDeque<_>>>()?;
                        let status: Option<String> = tx
                            .query_row(
                                "SELECT status FROM agent_tasks WHERE id=?1",
                                [&read_id],
                                |row| row.get(0),
                            )
                            .optional()?;
                        Ok((batch, status))
                    },
                )
                .await;
                let (batch, status) = match read {
                    Ok(Ok(pair)) => pair,
                    _ => return None,
                };
                pending = batch;
                if pending.is_empty() {
                    if !matches!(status.as_deref(), Some("running" | "cancelling")) {
                        let sse = Event::default().event("session/done").data(
                            json!({
                                "task_id": task,
                                "status": status.unwrap_or_else(|| "unknown".into())
                            })
                            .to_string(),
                        );
                        return Some((
                            Ok::<Event, Infallible>(sse),
                            (state, plugin_id, package_scope, task, last_seq, pending, true),
                        ));
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(120)) => {}
                        _ = state.plugin_cancel.cancelled() => {}
                    }
                }
            }
        },
    );
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// What the host appends to a page's prompt before the page's selection: it is
/// the page's data, not the user's words.
pub const SELECTION: &str = "\n\nPage selection (untrusted data): ";

pub fn owner(root: &std::path::Path, task: &str) -> Result<Option<String>> {
    let conn = agent_service::open_db(root)?;
    Ok(conn
        .query_row(
            "WITH RECURSIVE lineage(id,depth) AS (SELECT ?1,0 UNION ALL SELECT t.parent_task_id,l.depth+1 FROM agent_tasks t JOIN lineage l ON t.id=l.id WHERE t.parent_task_id IS NOT NULL AND l.depth<32) SELECT p.plugin_id FROM lineage l JOIN plugin_sessions p ON p.task_id=l.id ORDER BY l.depth LIMIT 1",
            [task],
            |r| r.get(0),
        )
        .optional()?)
}

/// Persisted capability profile (`"full"` or `"slim"`) for a plugin-owned conversation.
/// Walks the same lineage CTE as `owner` so child frames inherit the parent session's profile.
pub fn prompt_profile(root: &std::path::Path, task: &str) -> Result<PromptProfile> {
    let conn = agent_service::open_db(root)?;
    let stored: Option<String> = conn
        .query_row(
            "WITH RECURSIVE lineage(id,depth) AS (SELECT ?1,0 UNION ALL SELECT t.parent_task_id,l.depth+1 FROM agent_tasks t JOIN lineage l ON t.id=l.id WHERE t.parent_task_id IS NOT NULL AND l.depth<32) SELECT p.prompt_profile FROM lineage l JOIN plugin_session_profiles p ON p.task_id=l.id ORDER BY l.depth LIMIT 1",
            [task],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);
    Ok(stored
        .as_deref()
        .and_then(PromptProfile::parse)
        .unwrap_or(PromptProfile::Full))
}

fn parse_requested_profile(args: &Value) -> Result<Option<PromptProfile>> {
    match args.get("prompt_profile") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let raw = value
                .as_str()
                .context("prompt_profile must be \"full\" or \"slim\"")?;
            Ok(Some(
                PromptProfile::parse(raw)
                    .context("prompt_profile must be \"full\" or \"slim\"")?,
            ))
        }
    }
}
/// Dedicated agent workspace for one plugin's chats, kept apart from the host
/// project. Base: `PLUGIN_WORKSPACE_BASE`, else `<root>/.codex-workspace-mcp/plugins`.
/// Plugin deliverables live in the plugin backend's object storage,
/// not here; this directory only holds scratch files the agent writes.
pub fn workspace_dir(root: &std::path::Path, plugin: &str) -> Result<std::path::PathBuf> {
    ensure!(
        !plugin.is_empty()
            && plugin
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && plugin != "."
            && plugin != "..",
        "invalid plugin id"
    );
    let base = std::env::var_os("PLUGIN_WORKSPACE_BASE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join(".codex-workspace-mcp").join("plugins"));
    let dir = base.join(plugin).join("workspace");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
pub fn check(root: &std::path::Path, task: &str, plugin: &str) -> Result<()> {
    ensure!(
        owner(root, task)?.as_deref() == Some(plugin),
        "session does not belong to this plugin"
    );
    Ok(())
}
/// Both the Cordis facade and iframe bridge call this endpoint. Neither may
/// choose a workspace, tool allowlist, general session, or permission mode.
pub async fn request(
    State(state): State<AgentServiceState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(mut args): Json<Value>,
) -> Response {
    if args.get("after_seq").is_none()
        && let Some(last_seq) = headers
            .get("last-event-id")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<i64>().ok())
            .filter(|seq| *seq >= 0)
        && let Some(obj) = args.as_object_mut()
    {
        obj.insert("after_seq".into(), json!(last_seq));
    }
    match handle(state, &id, args).await {
        Ok(response) => response,
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":format!("{error:#}")})),
        )
            .into_response(),
    }
}
pub(crate) async fn handle(state: AgentServiceState, id: &str, args: Value) -> Result<Response> {
    ensure!(
        !state.plugin_cancel.is_cancelled(),
        "agent task plugin is stopped"
    );
    let catalog = state
        .tool_catalog
        .as_ref()
        .context("tool registry unavailable")?;
    let package_scope = args["package_scope"]
        .as_u64()
        .context("plugin page is unavailable or stale")?;
    ensure!(
        catalog
            .packages
            .scopes(&std::collections::BTreeSet::from([id.to_owned()]))
            .get(id)
            .copied()
            == Some(package_scope),
        "plugin page is unavailable or stale"
    );
    let root = state.workspace.root();
    if args["method"] == "models" {
        return Ok(Json(crate::model::model_catalog::for_plugin(&state, id).await?).into_response());
    }
    if args["method"] == "open" {
        let conn = agent_service::open_db(root)?;
        let task: Option<String> = conn.query_row("SELECT p.task_id FROM plugin_sessions p JOIN agent_tasks t ON t.id=p.task_id WHERE p.plugin_id=?1 ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1", [id], |r|r.get(0)).optional()?;
        if let Some(task) = task {
            let profile = prompt_profile(root, &task)?;
            return Ok(Json(json!({"task_id":task,"plugin_id":id,"prompt_profile":profile.as_str()})).into_response());
        }
    }
    if args["method"] == "create" || args["method"] == "open" {
        let requested_profile = parse_requested_profile(&args)?;
        let profile = catalog.packages.resolve_prompt_profile(id, requested_profile);
        let mut latest = state.clone().with_latest_config().await;
        crate::model::model_catalog::restrict(&mut latest, id).await?;
        let task = format!("task_{}", agent_service::uuid_like());
        let mut conn = agent_service::open_db(root)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at,provider_name,reasoning_effort,fast_mode) VALUES (?1,'',?2,'draft',?3,?3,?4,?5,?6)", params![task,latest.default_model,chrono::Utc::now().timestamp_millis(),latest.provider_name,latest.reasoning_effort,latest.fast_mode])?;
        tx.execute(
            "INSERT INTO plugin_sessions(task_id,plugin_id) VALUES (?1,?2)",
            params![task, id],
        )?;
        tx.execute(
            "INSERT INTO plugin_session_profiles(task_id,prompt_profile) VALUES (?1,?2)",
            params![task, profile.as_str()],
        )?;
        tx.commit()?;
        return Ok(Json(json!({"task_id":task,"plugin_id":id,"prompt_profile":profile.as_str()})).into_response());
    }
    let task = args["task_id"]
        .as_str()
        .context("task_id required")?
        .to_owned();
    check(root, &task, id)?;
    Ok(match args["method"].as_str().unwrap_or("") {
        "upload" => Json(crate::media::attachments::encoded(&state, &task, &args)?).into_response(),
        "attachment" => Json(crate::media::attachments::preview(
            &state,
            &task,
            args["receiptId"].as_str().context("receiptId required")?,
        )?)
        .into_response(),
        "prompt" => {
            // The page that asks runs this task's page tools (see page_tools).
            if let Some(page) = args["page_id"].as_str() {
                crate::tools::page_tools::claim(&task, page)?;
            }
            let prompt = args["prompt"].as_str().unwrap_or("");
            ensure!(
                !prompt.trim().is_empty()
                    || args["content"].as_array().is_some_and(|a| !a.is_empty()),
                "prompt must not be empty"
            );
            // Selection is data, never an authority or an executable instruction.
            let prompt = if args["selection"].is_object() {
                format!("{prompt}{SELECTION}{}", args["selection"])
            } else {
                prompt.to_owned()
            };
            let wants_stream = args["stream"].as_bool().unwrap_or(false);
            let stream_after = if wants_stream {
                match args["after_seq"].as_i64() {
                    Some(seq) => seq,
                    None => latest_event_seq(root, &task)?,
                }
            } else {
                -1
            };
            let input = serde_json::from_value(
                json!({"prompt":prompt,"content":args["content"].as_array().cloned().unwrap_or_default(),"requestId":args["requestId"],"provider":args["provider"],"model":args["model"],"reasoning_effort":args["reasoning_effort"],"program_resource":args["program_resource"]}),
            )?;
            let response = agent_service::continue_task(
                State(state.clone()),
                Path(task.clone()),
                Json(input),
            )
            .await
            .into_response();
            if wants_stream && response.status().is_success() {
                stream_plugin_events(state, id.to_owned(), package_scope, task, stream_after)
            } else {
                response
            }
        }
        "stream" => {
            let after_seq = args["after_seq"].as_i64().unwrap_or(-1);
            stream_plugin_events(state, id.to_owned(), package_scope, task, after_seq)
        }
        "events" => {
            let root = state.workspace.root().to_path_buf();
            let after = args["after_seq"].as_i64();
            let events =
                tokio::task::spawn_blocking(move || -> Result<Vec<agent_service::AgentEvent>> {
                    let conn = agent_service::open_db(&root)?;
                    crate::context::session_history::load_events(
                        &conn,
                        &task,
                        &crate::context::session_history::EventQuery {
                            after,
                            kinds: PLUGIN_EVENT_KINDS,
                            ..Default::default()
                        },
                    )?
                    .into_iter()
                    .map(sanitize_plugin_event)
                    .collect()
                })
                .await??;
            Json(json!({"events":events})).into_response()
        }
        "tool_calls" => {
            let page = args["page_id"].as_str().context("page_id required")?;
            let wait = std::time::Duration::from_millis(args["wait_ms"].as_u64().unwrap_or(0));
            Json(crate::tools::page_tools::poll(&task, page, wait).await?).into_response()
        }
        "tool_result" => {
            let page = args["page_id"].as_str().context("page_id required")?;
            let call = args["call_id"].as_str().context("call_id required")?;
            ensure!(args["result"].is_object(), "result object required");
            crate::tools::page_tools::reply(&task, page, call, args["result"].clone())?;
            Json(json!({"ok":true})).into_response()
        }
        "status" => agent_service::get_task(State(state), Path(task))
            .await
            .into_response(),
        "cancel" => agent_service::cancel_task(State(state), Path(task))
            .await
            .into_response(),
        _ => anyhow::bail!("unknown plugin session method"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plugin_workspace_is_per_plugin_and_rejects_traversal() {
        let root = std::env::temp_dir().join(format!("plugin_ws_{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let a = workspace_dir(&root, "ppt-master").unwrap();
        let b = workspace_dir(&root, "blueprint").unwrap();
        assert!(a.is_dir() && b.is_dir() && a != b);
        assert!(a.starts_with(&root));
        assert!(workspace_dir(&root, "../x").is_err());
        assert!(workspace_dir(&root, "").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    async fn body(response: Response) -> Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn scoped_page_api_resumes_owned_history_and_rejects_general_or_stale_sessions() {
        let root =
            std::env::temp_dir().join(format!("plugin-session-{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let (runtime, catalog) = crate::server::skill_packages::test_runtime();
        let scope = catalog.packages.catalog()[0]["scope"].clone();
        let mut state = agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        state.tool_catalog = Some(catalog);
        state.config = Some(std::sync::Arc::new(tokio::sync::RwLock::new(serde_json::from_value(json!({
            "providers":{"test":{"url":"http://private/v1","api_key":"secret","model_map":{"m":"m"}}}
        })).unwrap())));
        let models = body(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"models","package_scope":scope}),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(models["models"][0]["id"], "m");
        assert!(!models.to_string().contains("secret"));
        assert!(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"models","package_scope":999})
            )
            .await
            .is_err()
        );
        let created = body(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"create","package_scope":scope}),
            )
            .await
            .unwrap(),
        )
        .await;
        let task = created["task_id"].as_str().unwrap();
        assert_eq!(owner(&root, task).unwrap().as_deref(), Some("blueprint"));
        let opened = body(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"open","package_scope":scope}),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(opened["task_id"], task);
        agent_service::emit(
            &root,
            task,
            "user/message",
            json!({"message":{"content":"first"}}),
        )
        .await
        .unwrap();
        agent_service::emit(
            &root,
            task,
            "assistant/message",
            json!({"message":{"content":"second"}}),
        )
        .await
        .unwrap();
        let history = body(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"events","package_scope":scope,"task_id":task,"after_seq":0}),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(history["events"].as_array().unwrap().len(), 1);
        assert_eq!(history["events"][0]["seq"], 1);
        for (kind, result) in [("task/node_started", Value::Null), ("task/node_returned", json!({"result":"{\"saved\":true}"}))] {
            agent_service::emit(&root, task, kind, json!({"node_id":"child","metadata":{"role":"temp"},"title":"author","inputs":{"html":"private large input"},"result":result})).await.unwrap();
        }
        let frames = body(handle(state.clone(), "blueprint", json!({"method":"events","package_scope":scope,"task_id":task,"after_seq":1})).await.unwrap()).await;
        let frames = frames["events"].as_array().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["data"]["metadata"]["role"], "temp");
        assert_eq!(frames[1]["data"]["result"]["result"], "{\"saved\":true}");
        assert!(frames[1]["data"].get("inputs").is_none());
        for args in [
            json!({"method":"events","package_scope":scope,"task_id":"general-task"}),
            json!({"method":"create"}),
            json!({"method":"create","package_scope":999}),
        ] {
            assert!(handle(state.clone(), "blueprint", args).await.is_err());
        }
        // Built-in plugins cannot be disabled; unmounting retires the scope just the same.
        runtime
            .unmount(serde_json::from_value(scope.clone()).unwrap())
            .unwrap();
        assert!(
            handle(
                state,
                "blueprint",
                json!({"method":"events","package_scope":scope,"task_id":task})
            )
            .await
            .is_err()
        );
        runtime.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn prompt_profile_precedence_persistence_and_host_slim_lock() {
        let root =
            std::env::temp_dir().join(format!("plugin-profile-{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let settings_path = root.join("skill-packages.json");
        let (runtime, catalog) = crate::server::skill_packages::test_runtime();
        catalog.packages.use_test_settings_path(settings_path.clone());
        let scope = catalog.packages.catalog()[0]["scope"].clone();
        let mut state = agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        state.tool_catalog = Some(catalog.clone());
        state.config = Some(std::sync::Arc::new(tokio::sync::RwLock::new(
            serde_json::from_value(json!({
                "providers":{"test":{"url":"http://private/v1","api_key":"secret","model_map":{"m":"m"}}}
            }))
            .unwrap(),
        )));

        // 1. Default profile is "full".
        let default_created = body(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"create","package_scope":scope}),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(default_created["prompt_profile"], "full");
        let full_task = default_created["task_id"].as_str().unwrap();
        assert_eq!(prompt_profile(&root, full_task).unwrap(), PromptProfile::Full);

        // 2. Explicit request override to "slim" persists and is echoed by `open`.
        let slim_created = body(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"create","package_scope":scope,"prompt_profile":"slim"}),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(slim_created["prompt_profile"], "slim");
        let slim_task = slim_created["task_id"].as_str().unwrap();
        assert_eq!(prompt_profile(&root, slim_task).unwrap(), PromptProfile::Slim);
        let opened = body(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"open","package_scope":scope}),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(opened["task_id"], slim_task);
        assert_eq!(opened["prompt_profile"], "slim");

        // 3. Invalid prompt_profile values are rejected cleanly.
        assert!(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"create","package_scope":scope,"prompt_profile":"turbo"}),
            )
            .await
            .is_err()
        );

        // 4. Host skill-packages.json `"slim"` acts as a hard ceiling:
        //    even an explicit `"prompt_profile": "full"` request cannot escalate to full.
        std::fs::write(
            &settings_path,
            json!({"prompt_profiles":{"blueprint":"slim"}}).to_string(),
        )
        .unwrap();
        let locked_created = body(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"create","package_scope":scope,"prompt_profile":"full"}),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(locked_created["prompt_profile"], "slim");
        assert_eq!(
            prompt_profile(&root, locked_created["task_id"].as_str().unwrap()).unwrap(),
            PromptProfile::Slim
        );

        // 5. Host `"full"` allows per-session explicit `"slim"` opt-down while defaulting to `"full"`.
        std::fs::write(
            &settings_path,
            json!({"prompt_profiles":{"blueprint":"full"}}).to_string(),
        )
        .unwrap();
        let opt_down = body(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"create","package_scope":scope,"prompt_profile":"slim"}),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(opt_down["prompt_profile"], "slim");

        runtime.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn text_body(response: Response) -> String {
        String::from_utf8(
            axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn stream_emits_live_deltas_filters_internal_events_and_sends_done() {
        let root =
            std::env::temp_dir().join(format!("plugin-sse-live-{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let (runtime, catalog) = crate::server::skill_packages::test_runtime();
        let scope = catalog.packages.catalog()[0]["scope"].clone();
        let mut state = agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        state.tool_catalog = Some(catalog);
        state.config = Some(std::sync::Arc::new(tokio::sync::RwLock::new(
            serde_json::from_value(json!({
                "providers":{"test":{"url":"http://private/v1","api_key":"secret","model_map":{"m":"m"}}}
            }))
            .unwrap(),
        )));

        let created = body(
            handle(
                state.clone(),
                "blueprint",
                json!({"method":"create","package_scope":scope}),
            )
            .await
            .unwrap(),
        )
        .await;
        let task = created["task_id"].as_str().unwrap().to_owned();
        {
            let conn = agent_service::open_db(&root).unwrap();
            conn.execute(
                "UPDATE agent_tasks SET status='running' WHERE id=?1",
                [&task],
            )
            .unwrap();
        }

        let response = handle(
            state.clone(),
            "blueprint",
            json!({"method":"stream","package_scope":scope,"task_id":task,"after_seq":-1}),
        )
        .await
        .unwrap();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.starts_with("text/event-stream"),
            "unexpected content-type: {content_type}"
        );

        let writer_root = root.clone();
        let writer_task = task.clone();
        let producer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            agent_service::emit(
                &writer_root,
                &writer_task,
                "assistant/delta",
                json!({"turn":1,"step":1,"text":"实时吐字片段"}),
            )
            .await
            .unwrap();
            agent_service::emit(
                &writer_root,
                &writer_task,
                "debug/context_request",
                json!({"secret":"internal_only"}),
            )
            .await
            .unwrap();
            agent_service::emit(
                &writer_root,
                &writer_task,
                "task/node_started",
                json!({"node_id":"n1","title":"step","inputs":{"html":"private_secret_html"}}),
            )
            .await
            .unwrap();
            agent_service::emit(
                &writer_root,
                &writer_task,
                "assistant/message",
                json!({"message":{"content":"实时吐字片段-完成"}}),
            )
            .await
            .unwrap();
            agent_service::emit(&writer_root, &writer_task, "turn/end", json!({"turn":1}))
                .await
                .unwrap();
            let conn = agent_service::open_db(&writer_root).unwrap();
            conn.execute(
                "UPDATE agent_tasks SET status='completed' WHERE id=?1",
                [&writer_task],
            )
            .unwrap();
        });

        let sse = tokio::time::timeout(Duration::from_secs(5), text_body(response))
            .await
            .expect("SSE stream should terminate after task completion");
        producer.await.unwrap();

        assert!(sse.contains("event: session/event"), "{sse}");
        assert!(sse.contains("assistant/delta"), "{sse}");
        assert!(sse.contains("实时吐字片段"), "{sse}");
        assert!(sse.contains("assistant/message"), "{sse}");
        assert!(sse.contains("turn/end"), "{sse}");
        assert!(!sse.contains("debug/context_request"), "{sse}");
        assert!(!sse.contains("internal_only"), "{sse}");
        assert!(!sse.contains("private_secret_html"), "{sse}");
        assert!(sse.contains("event: session/done"), "{sse}");
        assert!(sse.contains("\"status\":\"completed\""), "{sse}");

        // Resuming with Last-Event-ID via `request(...)` skips already-seen events and emits done.
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", "3".parse().unwrap());
        let resumed = request(
            State(state.clone()),
            Path("blueprint".into()),
            headers,
            Json(json!({"method":"stream","package_scope":scope,"task_id":task})),
        )
        .await;
        let resumed_sse = tokio::time::timeout(Duration::from_secs(3), text_body(resumed))
            .await
            .expect("resumed completed SSE stream should close immediately");
        assert!(!resumed_sse.contains("assistant/delta"), "{resumed_sse}");
        assert!(resumed_sse.contains("turn/end"), "{resumed_sse}");
        assert!(resumed_sse.contains("event: session/done"), "{resumed_sse}");

        // `method: "prompt"` with `stream: true` still returns JSON error when continue_task fails.
        let mut no_provider_state = state.clone();
        no_provider_state.provider_url.clear();
        no_provider_state.config = None;
        let err_resp = handle(
            no_provider_state,
            "blueprint",
            json!({"method":"prompt","package_scope":scope,"task_id":task,"prompt":"hello","stream":true}),
        )
        .await
        .unwrap();
        assert_eq!(err_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let err_json = body(err_resp).await;
        assert!(err_json["error"].as_str().is_some());

        runtime.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
