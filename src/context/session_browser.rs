//! Durable sidebar state. Removing a project never removes its directory or chats.
use crate::server::agent_service::{self, AgentServiceState};
use anyhow::{Context, Result, ensure};
use chrono::{Local, NaiveDate, TimeZone};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};

pub fn schema(conn: &Connection, root: &FsPath) -> Result<()> {
    schema_with_default_project(conn, root, !std::env::args().any(|arg| arg == "--desktop-agent"))
}

fn schema_with_default_project(conn: &Connection, root: &FsPath, default_project: bool) -> Result<()> {
    let fresh = !conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='agent_projects')",
        [],
        |r| r.get::<_, bool>(0),
    )?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS agent_projects(path TEXT PRIMARY KEY,title TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS agent_session_meta(task_id TEXT PRIMARY KEY,title TEXT,pinned INTEGER NOT NULL DEFAULT 0,archived INTEGER NOT NULL DEFAULT 0,workspace TEXT);")?;
    crate::context::database::add_column(conn, "agent_session_meta", "kind", "TEXT NOT NULL DEFAULT 'code'")?;
    if fresh && default_project {
        conn.execute(
            "INSERT OR IGNORE INTO agent_projects(path,title) VALUES (?1,?2)",
            params![
                root.to_string_lossy(),
                root.file_name().unwrap_or_default().to_string_lossy()
            ],
        )?;
    }
    if !default_project {
        // Migrate the old desktop default only when no code sessions depend on it.
        // Remember the migration so an explicitly added folder stays registered.
        conn.execute_batch("CREATE TABLE IF NOT EXISTS agent_sidebar_migrations(name TEXT PRIMARY KEY);")?;
        let done: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_sidebar_migrations WHERE name='desktop-default-project')", [], |r| r.get(0))?;
        if !done {
            conn.execute("DELETE FROM agent_projects WHERE path=?1 AND title=?2 AND NOT EXISTS (SELECT 1 FROM agent_tasks t LEFT JOIN agent_session_meta m ON m.task_id=t.id WHERE COALESCE(m.kind,'code')='code' AND (m.workspace IS NULL OR m.workspace=?1))", params![root.to_string_lossy(), root.file_name().unwrap_or_default().to_string_lossy()])?;
            conn.execute("INSERT OR IGNORE INTO agent_sidebar_migrations(name) VALUES ('desktop-default-project')", [])?;
        }
    }
    Ok(())
}

pub fn decorate(conn: &Connection, root: &FsPath, mut task: Value) -> Result<Value> {
    let id = task["task_id"].as_str().context("task id required")?;
    let meta = conn
        .query_row(
            "SELECT title,pinned,archived,workspace,kind FROM agent_session_meta WHERE task_id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let plugin: Option<String> = conn
        .query_row(
            "SELECT plugin_id FROM plugin_sessions WHERE task_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let (title, pinned, archived, workspace, kind) = meta.unwrap_or_default();
    let awaiting = crate::tools::tool_approvals::awaiting(conn, id);
    // Each chat carries its own level; one saved before levels were stored asks first.
    let permission_mode = crate::server::agent_service::permission_mode_for(conn, id)?;
    let chat = plugin.is_some() || kind == "chat";
    task["kind"] = json!(if chat { "chat" } else { "code" });
    task["title"] = json!(title);
    task["pinned"] = json!(pinned);
    task["archived"] = json!(archived);
    task["workspace"] = if chat {
        Value::Null
    } else {
        json!(workspace.unwrap_or_else(|| root.display().to_string()))
    };
    task["plugin_id"] = json!(plugin);
    task["awaiting_approval"] = json!(awaiting);
    // Each chat carries its own level; one saved before levels were stored asks first.
    task["permission_mode"] = json!(permission_mode.as_str());
    Ok(task)
}

/// Unified folder for general-chat (`kind == "chat"`) generated assets, kept
/// separate from host state (`.codex-workspace-mcp/`) and code repositories.
/// Overridable via `GROW_CHAT_ASSETS_DIR`; defaults to `<root>/chat-assets`.
pub fn chat_assets_dir(root: &FsPath) -> Result<PathBuf> {
    let dir = std::env::var_os("GROW_CHAT_ASSETS_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("chat-assets"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn is_general_chat(root: &FsPath, task: &str) -> Result<bool> {
    let conn = agent_service::open_db(root)?;
    let kind: Option<String> = conn
        .query_row(
            "SELECT kind FROM agent_session_meta WHERE task_id=?1",
            [task],
            |r| r.get(0),
        )
        .optional()?;
    Ok(kind.as_deref() == Some("chat"))
}

pub fn task_workspace(root: &FsPath, task: &str) -> Result<PathBuf> {
    let conn = agent_service::open_db(root)?;
    let row: Option<(Option<String>, String)> = conn
        .query_row(
            "SELECT workspace, COALESCE(kind, 'code') FROM agent_session_meta WHERE task_id=?1",
            [task],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((path, kind)) = row {
        let trimmed = path.filter(|p| !p.trim().is_empty());
        if kind == "chat" {
            let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            let uses_legacy_root = match trimmed.as_deref() {
                None => true,
                Some(p) => {
                    let pb = PathBuf::from(p);
                    pb == root
                        || std::fs::canonicalize(&pb).ok().as_deref()
                            == Some(canonical_root.as_path())
                }
            };
            if uses_legacy_root {
                return chat_assets_dir(root);
            }
            return Ok(PathBuf::from(trimmed.unwrap()));
        }
        if let Some(p) = trimmed {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(root.to_path_buf())
}

fn response(result: Result<Value>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::CONFLICT,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

/// Plain text of a conversation event, for content search.
fn event_text(data: &Value) -> String {
    let content = &data["message"]["content"];
    let mut text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    if let Some(extra) = data["text"].as_str() {
        text.push_str(extra);
    }
    text
}

/// Up to `radius` characters either side of the first case-insensitive match.
fn snippet(text: &str, query: &str, radius: usize) -> Option<String> {
    let lower = text.to_lowercase();
    let at = lower.find(&query.to_lowercase())?;
    let chars = lower[..at].chars().count();
    let start = chars.saturating_sub(radius);
    let length = query.chars().count() + radius * 2;
    let body: String = text
        .chars()
        .skip(start)
        .take(length)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        body,
        if start + length < text.chars().count() {
            "…"
        } else {
            ""
        }
    ))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}
/// Sessions whose messages mention the query, newest match first, with a snippet.
pub async fn search(
    State(state): State<AgentServiceState>,
    axum::extract::Query(query): axum::extract::Query<SearchQuery>,
) -> Response {
    response((|| {
        let q = query.q.trim();
        ensure!(q.chars().count() >= 2, "至少输入两个字");
        let conn = agent_service::open_db(state.workspace.root())?;
        let pattern = format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut stmt = conn.prepare(
            "SELECT task_id,data FROM agent_task_events WHERE kind IN ('user/message','assistant/message','user/interjection') AND data LIKE ?1 ESCAPE '\\' ORDER BY timestamp DESC,seq DESC LIMIT 2000",
        )?;
        let rows = stmt
            .query_map([pattern], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut seen = std::collections::HashSet::new();
        let mut matches = Vec::new();
        for (task, data) in rows {
            if seen.contains(&task) {
                continue;
            }
            let data: Value = serde_json::from_str(&data).unwrap_or_default();
            // LIKE also hits JSON keys and escapes; keep only real text matches.
            if let Some(snippet) = snippet(&event_text(&data), q, 24) {
                seen.insert(task.clone());
                matches.push(json!({"task_id":task,"snippet":snippet}));
                if matches.len() >= 50 {
                    break;
                }
            }
        }
        Ok(json!({"matches":matches}))
    })())
}

pub async fn projects(State(state): State<AgentServiceState>) -> Response {
    response((|| {
        let conn = agent_service::open_db(state.workspace.root())?;
        let mut stmt = conn.prepare("SELECT path,title FROM agent_projects ORDER BY rowid")?;
        let projects = stmt
            .query_map([], |r| {
                Ok(json!({"path":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"projects":projects}))
    })())
}
#[derive(Deserialize)]
pub struct ProjectInput {
    path: String,
    title: Option<String>,
}
pub async fn add_project(
    State(state): State<AgentServiceState>,
    Json(input): Json<ProjectInput>,
) -> Response {
    response((|| {
        let path = std::fs::canonicalize(input.path.trim()).context("项目目录不存在")?;
        ensure!(path.is_dir(), "请选择项目目录");
        let title = input
            .title
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        let conn = agent_service::open_db(state.workspace.root())?;
        conn.execute(
            "INSERT OR IGNORE INTO agent_projects(path,title) VALUES (?1,?2)",
            params![path.to_string_lossy(), title],
        )?;
        let title: String = conn.query_row(
            "SELECT title FROM agent_projects WHERE path=?1",
            [path.to_string_lossy().as_ref()],
            |row| row.get(0),
        )?;
        Ok(json!({"path":path,"title":title}))
    })())
}
pub async fn rename_project(
    State(state): State<AgentServiceState>,
    Json(input): Json<ProjectInput>,
) -> Response {
    response((|| {
        let title = input.title.context("请输入项目名称")?;
        ensure!(!title.trim().is_empty(), "请输入项目名称");
        let conn = agent_service::open_db(state.workspace.root())?;
        ensure!(
            conn.execute(
                "UPDATE agent_projects SET title=?2 WHERE path=?1",
                params![input.path, title.trim()]
            )? > 0,
            "项目不存在"
        );
        Ok(json!({"ok":true}))
    })())
}
pub async fn remove_project(
    State(state): State<AgentServiceState>,
    Json(input): Json<ProjectInput>,
) -> Response {
    response((|| {
        let conn = agent_service::open_db(state.workspace.root())?;
        conn.execute("DELETE FROM agent_projects WHERE path=?1", [input.path])?;
        Ok(json!({"ok":true}))
    })())
}
#[derive(Deserialize)]
pub struct SessionInput {
    title: Option<String>,
    pinned: Option<bool>,
    archived: Option<bool>,
}
pub async fn update_session(
    State(state): State<AgentServiceState>,
    Path(id): Path<String>,
    Json(input): Json<SessionInput>,
) -> Response {
    response((|| {
        let mut conn = agent_service::open_db(state.workspace.root())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let status: String = tx
            .query_row("SELECT status FROM agent_tasks WHERE id=?1", [&id], |r| {
                r.get(0)
            })
            .optional()?
            .context("会话不存在")?;
        ensure!(
            input.archived != Some(true) || !matches!(status.as_str(), "running" | "cancelling"),
            "请先停止会话再归档"
        );
        tx.execute(
            "INSERT OR IGNORE INTO agent_session_meta(task_id) VALUES (?1)",
            [&id],
        )?;
        if let Some(title) = input.title {
            ensure!(!title.trim().is_empty(), "请输入会话名称");
            tx.execute(
                "UPDATE agent_session_meta SET title=?2 WHERE task_id=?1",
                params![id, title.trim()],
            )?;
        }
        if let Some(pinned) = input.pinned {
            tx.execute("UPDATE agent_session_meta SET pinned=?2,archived=CASE WHEN ?2 THEN 0 ELSE archived END WHERE task_id=?1", params![id,pinned])?;
        }
        if let Some(archived) = input.archived {
            tx.execute("UPDATE agent_session_meta SET archived=?2,pinned=CASE WHEN ?2 THEN 0 ELSE pinned END WHERE task_id=?1", params![id,archived])?;
        }
        tx.commit()?;
        Ok(json!({"ok":true}))
    })())
}

/// Delete the full task family and all its local records in one transaction.
/// Running work must stop first, so background writers cannot recreate history.
pub fn delete(conn: &mut Connection, id: &str) -> Result<Vec<String>> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut stmt = tx.prepare("WITH RECURSIVE family(id) AS (SELECT id FROM agent_tasks WHERE id=?1 UNION SELECT t.id FROM agent_tasks t JOIN family f ON t.parent_task_id=f.id) SELECT id FROM family")?;
    let ids = stmt
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    ensure!(!ids.is_empty(), "会话不存在");
    for id in &ids {
        let status: String =
            tx.query_row("SELECT status FROM agent_tasks WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        ensure!(
            !matches!(status.as_str(), "running" | "cancelling"),
            "请先停止会话及其子任务再删除"
        );
    }
    // Several legacy runtime tables predate foreign keys. Remove their records
    // explicitly as well as the tables that already declare cascading deletes.
    let mut stmt = tx.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT GLOB 'sqlite_*'",
    )?;
    let tables = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    if tables.iter().any(|table| table == "agent_delegates")
        && tables
            .iter()
            .any(|table| table == "agent_delegate_executors")
    {
        for id in &ids {
            tx.execute("DELETE FROM agent_delegate_executors WHERE agent_id IN (SELECT id FROM agent_delegates WHERE task_id=?1)", [id])?;
        }
    }
    for table in tables.iter().filter(|t| t.as_str() != "agent_tasks") {
        let quoted = format!("\"{}\"", table.replace('"', "\"\""));
        let mut stmt = tx.prepare(&format!("PRAGMA table_info({quoted})"))?;
        let columns = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        for column in ["task_id", "host_task_id"] {
            if columns.iter().any(|c| c == column) {
                for id in &ids {
                    tx.execute(&format!("DELETE FROM {quoted} WHERE {column}=?1"), [id])?;
                }
            }
        }
    }
    for id in &ids {
        tx.execute("DELETE FROM agent_tasks WHERE id=?1", [id])?;
    }
    tx.commit()?;
    Ok(ids)
}
pub async fn delete_session(
    State(state): State<AgentServiceState>,
    Path(id): Path<String>,
) -> Response {
    if state.cancellations.lock().await.contains_key(&id) {
        return response(Err(anyhow::anyhow!("请等待会话停止后再删除")));
    }
    let root = state.workspace.root().to_path_buf();
    match tokio::task::spawn_blocking(move || {
        let ids = delete(&mut agent_service::open_db(&root)?, &id)?;
        // A deleted chat's module folder goes with it.
        for id in &ids {
            crate::runtime::single_agent::modules::remove(&root, id);
        }
        Ok(json!({"deleted":ids}))
    })
    .await
    {
        Ok(result) => response(result),
        Err(error) => response(Err(error.into())),
    }
}

// ---------- Chat history and material browsing ----------
/// The events a person means by "聊天记录": what they typed and what the model
/// answered. Tool traffic sits in the same table and would drown the search.
const MESSAGE_KINDS: &str = "'user/message','assistant/message','user/interjection'";
/// One dialog query never needs more candidate rows than this in memory; the
/// bound also keeps a content search over a huge state database predictable.
const SCAN_LIMIT: usize = 20_000;
/// Answers are stored whole, but a dialog row only ever shows a preview, and one
/// opened hit is read in full.
const TEXT_LIMIT: usize = 8_000;
/// Visual products that are just copies of an upload; the upload row already
/// represents them, so listing both would show one picture twice.
const UPLOAD_COPY_SOURCES: &[&str] = &["user_attachment", "user_pdf", "attachment"];

/// Plain one-line text of a message, cut to max_chars characters.
fn collapse(text: &str, chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= chars {
        return flat;
    }
    let mut out: String = flat.chars().take(chars).collect();
    out.push('…');
    out
}

/// The local day's bounds in the millisecond clock the state database uses.
fn day_bounds(date: &str) -> Result<(i64, i64)> {
    let text = date.trim();
    let day = NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .with_context(|| format!("{text} is not a YYYY-MM-DD date"))?;
    let midnight = |day: NaiveDate| {
        day.and_hms_opt(0, 0, 0)
            .and_then(|naive| naive.and_local_timezone(Local).earliest())
            .map(|moment| moment.timestamp_millis())
            .context("the local day has no midnight")
    };
    Ok((
        midnight(day)?,
        midnight(day.succ_opt().context("the calendar has no next day")?)?,
    ))
}

fn trim_option(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

/// The half-open millisecond window a dialog request asks for, if any.
fn time_window(
    date: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Option<(i64, i64)>> {
    if let Some(date) = trim_option(date) {
        return day_bounds(date).map(Some);
    }
    let start = match trim_option(from) {
        Some(from) => Some(day_bounds(from)?.0),
        None => None,
    };
    let end = match trim_option(to) {
        Some(to) => Some(day_bounds(to)?.1),
        None => None,
    };
    Ok(match (start, end) {
        (None, None) => None,
        (start, end) => Some((start.unwrap_or(i64::MIN), end.unwrap_or(i64::MAX))),
    })
}

/// A LIKE pattern that matches the query as plain text, not as wildcards.
fn like_pattern(query: &str) -> String {
    format!(
        "%{}%",
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

/// Optional tables: a state database only grows them once the feature that owns
/// them has been used, so an unfilled store must read as empty, not as an error.
fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get::<_, bool>(0),
    )?)
}
fn local_day(timestamp: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(timestamp)
        .single()
        .map(|moment| moment.format("%Y-%m-%d").to_string())
}

/// Where a row without its own message lands: the turn, the message and the
/// moment it belongs to.
#[derive(Clone, Copy)]
struct Anchor {
    seq: i64,
    turn: i64,
    time: i64,
}

/// Every message of one conversation in sequence order, for anchoring pictures,
/// files and captured images onto the turn that produced them.
fn message_anchors(conn: &Connection, task: &str) -> Result<Vec<(i64, i64, Anchor)>> {
    let mut stmt = conn.prepare(
        "SELECT seq,timestamp,data FROM agent_task_events WHERE task_id=?1 AND kind IN ('user/message','assistant/message') ORDER BY seq",
    )?;
    let rows = stmt
        .query_map([task], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(seq, time, data)| {
            let turn = serde_json::from_str::<Value>(&data)
                .ok()
                .and_then(|value| value["turn"].as_i64())
                .unwrap_or(0);
            (time, seq, Anchor { seq, turn, time })
        })
        .collect())
}

/// The last message at or before the given moment, so an artifact without a turn
/// lands next to what was being discussed when it appeared.
fn anchor_at(anchors: &[(i64, i64, Anchor)], time: i64) -> Option<Anchor> {
    let mut found = anchors.first().map(|row| row.2);
    for (at, _, anchor) in anchors {
        if *at > time {
            break;
        }
        found = Some(*anchor);
    }
    found
}

/// Loads each conversation's message list once per request.
struct Anchors<'a> {
    conn: &'a Connection,
    loaded: std::collections::HashMap<String, Vec<(i64, i64, Anchor)>>,
}

impl<'a> Anchors<'a> {
    fn new(conn: &'a Connection) -> Self {
        Self {
            conn,
            loaded: std::collections::HashMap::new(),
        }
    }

    fn list(&mut self, task: &str) -> Result<&Vec<(i64, i64, Anchor)>> {
        if !self.loaded.contains_key(task) {
            let rows = message_anchors(self.conn, task)?;
            self.loaded.insert(task.to_owned(), rows);
        }
        self.loaded.get(task).context("anchor list missing")
    }

    /// The first message of a turn: the prompt that started it.
    fn of_turn(&mut self, task: &str, turn: i64) -> Result<Option<Anchor>> {
        Ok(self
            .list(task)?
            .iter()
            .find(|row| row.2.turn == turn)
            .map(|row| row.2))
    }

    fn by_time(&mut self, task: &str, time: i64) -> Result<Option<Anchor>> {
        Ok(anchor_at(self.list(task)?, time))
    }
}

#[derive(Deserialize)]
pub struct HistoryMessagesQuery {
    task_id: Option<String>,
    q: Option<String>,
    date: Option<String>,
    from: Option<String>,
    to: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

/// One page of chat messages, newest first, optionally across every conversation.
pub async fn history_messages(
    State(state): State<AgentServiceState>,
    axum::extract::Query(query): axum::extract::Query<HistoryMessagesQuery>,
) -> Response {
    let root = state.workspace.root().to_path_buf();
    let result = tokio::task::spawn_blocking(move || messages(&root, &query)).await;
    response(result.unwrap_or_else(|error| Err(anyhow::anyhow!("history query aborted: {error}"))))
}

fn messages(root: &FsPath, query: &HistoryMessagesQuery) -> Result<Value> {
    let conn = agent_service::open_db(root)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.unwrap_or(0);
    let needle = trim_option(query.q.as_deref());
    let window = time_window(
        query.date.as_deref(),
        query.from.as_deref(),
        query.to.as_deref(),
    )?;
    let task = trim_option(query.task_id.as_deref());

    let mut clause = String::new();
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(task) = task {
        clause.push_str(" AND e.task_id=?");
        values.push(rusqlite::types::Value::Text(task.to_owned()));
    }
    if let Some((start, end)) = window {
        clause.push_str(" AND e.timestamp>=? AND e.timestamp<?");
        values.push(rusqlite::types::Value::Integer(start));
        values.push(rusqlite::types::Value::Integer(end));
    }
    if let Some(needle) = needle {
        // The pattern narrows the scan; the exact text check below decides.
        clause.push_str(" AND e.data LIKE ? ESCAPE '\\'");
        values.push(rusqlite::types::Value::Text(like_pattern(needle)));
    }
    let where_sql = format!("WHERE e.kind IN ({MESSAGE_KINDS}){clause}");

    let mut stmt = conn.prepare(&format!(
        "SELECT e.task_id,e.seq,e.timestamp,e.kind,e.data,
                COALESCE(NULLIF(m.title,''),NULLIF(t.prompt,''),'')
         FROM agent_task_events e
         LEFT JOIN agent_session_meta m ON m.task_id=e.task_id
         LEFT JOIN agent_tasks t ON t.id=e.task_id
         {where_sql}
         ORDER BY e.timestamp DESC,e.seq DESC LIMIT {}",
        if needle.is_some() {
            SCAN_LIMIT
        } else {
            limit + offset
        }
    ))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut items = Vec::new();
    let mut matched = 0;
    for (task_id, seq, timestamp, kind, data, title) in rows {
        let data: Value = serde_json::from_str(&data).unwrap_or_default();
        let text = event_text(&data);
        let snippet = match needle {
            Some(needle) => match snippet(&text, needle, 44) {
                Some(snippet) => snippet,
                // LIKE also hits JSON keys and escapes; keep only real text.
                None => continue,
            },
            None => collapse(&text, 200),
        };
        matched += 1;
        if needle.is_some() && matched <= offset {
            continue;
        }
        if items.len() >= limit {
            continue;
        }
        items.push(json!({
            "task_id": task_id,
            "task_title": title,
            // The transcript keys its rows by event kind and sequence, so the
            // dialog can name the exact row it wants to reveal.
            "key": format!("{kind}:{seq}"),
            "seq": seq,
            "turn": data["turn"].as_i64().unwrap_or(0),
            "role": if kind == "assistant/message" { "assistant" } else { "user" },
            "timestamp": timestamp,
            "snippet": snippet,
            "text": collapse(&text, TEXT_LIMIT),
            "attachments": attachment_names(&data),
        }));
    }
    let total = match needle {
        Some(_) => matched as i64,
        None => conn.query_row(
            &format!("SELECT COUNT(*) FROM agent_task_events e {where_sql}"),
            rusqlite::params_from_iter(values.iter()),
            |r| r.get::<_, i64>(0),
        )?,
    };
    Ok(json!({"total": total, "items": items}))
}

/// Attachment names carried by a user message, so a picture-only message still
/// reads as something in the dialog.
fn attachment_names(data: &Value) -> Vec<Value> {
    data["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["attachment"]["name"].as_str())
        .map(|name| json!(name))
        .collect()
}

#[derive(Deserialize)]
pub struct HistoryMediaQuery {
    task_id: Option<String>,
    kind: Option<String>,
    q: Option<String>,
    date: Option<String>,
    from: Option<String>,
    to: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

/// One page of pictures or files, newest first, across one or every conversation.
pub async fn history_media(
    State(state): State<AgentServiceState>,
    axum::extract::Query(query): axum::extract::Query<HistoryMediaQuery>,
) -> Response {
    let root = state.workspace.root().to_path_buf();
    let result = tokio::task::spawn_blocking(move || media(&root, &query)).await;
    response(result.unwrap_or_else(|error| Err(anyhow::anyhow!("history query aborted: {error}"))))
}

fn media(root: &FsPath, query: &HistoryMediaQuery) -> Result<Value> {
    let conn = agent_service::open_db(root)?;
    let files_wanted = query.kind.as_deref() == Some("files");
    let limit = query.limit.unwrap_or(80).clamp(1, 200);
    let offset = query.offset.unwrap_or(0);
    let needle = trim_option(query.q.as_deref());
    let window = time_window(
        query.date.as_deref(),
        query.from.as_deref(),
        query.to.as_deref(),
    )?;
    let task = trim_option(query.task_id.as_deref());

    let mut items = if files_wanted {
        file_items(&conn, task)?
    } else {
        image_items(&conn, task)?
    };
    if let Some((start, end)) = window {
        items.retain(|item| {
            item["timestamp"]
                .as_i64()
                .is_some_and(|time| time >= start && time < end)
        });
    }
    if let Some(needle) = needle {
        let needle = needle.to_lowercase();
        items.retain(|item| {
            ["label", "name", "path", "task_title"]
                .iter()
                .filter_map(|key| item[*key].as_str())
                .any(|text| text.to_lowercase().contains(&needle))
        });
    }
    // Newest first, so the dialog opens on what just happened.
    items.sort_by(|a, b| {
        b["timestamp"]
            .as_i64()
            .unwrap_or_default()
            .cmp(&a["timestamp"].as_i64().unwrap_or_default())
    });
    let total = items.len();
    Ok(json!({
        "total": total,
        "items": items.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),
    }))
}

/// Pictures: what the user attached plus what the model saw or drew.
fn image_items(conn: &Connection, task: Option<&str>) -> Result<Vec<Value>> {
    let mut anchors = Anchors::new(conn);
    let mut items = Vec::new();
    if has_table(conn, "agent_attachments")? {
        items.extend(uploaded_images(&mut anchors, conn, task)?);
    }
    if has_table(conn, "agent_visual_artifacts")? {
        items.extend(captured_images(&mut anchors, conn, task)?);
    }
    Ok(items)
}

/// Pictures the user attached: the bytes live under a per-conversation receipt.
fn uploaded_images(
    anchors: &mut Anchors<'_>,
    conn: &Connection,
    task: Option<&str>,
) -> Result<Vec<Value>> {
    let filter = if task.is_some() {
        " AND a.task_id=?1"
    } else {
        ""
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT a.task_id,a.receipt,a.reference,a.turn,
                COALESCE(NULLIF(m.title,''),NULLIF(t.prompt,''),'')
         FROM agent_attachments a
         LEFT JOIN agent_session_meta m ON m.task_id=a.task_id
         LEFT JOIN agent_tasks t ON t.id=a.task_id
         WHERE (a.media_type LIKE 'image/%' OR a.image_artifact IS NOT NULL){filter}
         ORDER BY a.rowid DESC"
    ))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(task.into_iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for (task_id, receipt, reference, turn, title) in rows {
        let reference: Value = serde_json::from_str(&reference).unwrap_or_default();
        let anchor = match turn {
            Some(turn) => anchors.of_turn(&task_id, turn)?,
            None => None,
        };
        let anchor = match anchor {
            Some(anchor) => Some(anchor),
            None => anchors.list(&task_id)?.last().map(|row| row.2),
        };
        let url = format!("/agent/tasks/{task_id}/attachments/{receipt}");
        items.push(json!({
            "source": "upload",
            "url": url,
            "thumb_url": url,
            "label": reference["name"].as_str().unwrap_or("图片"),
            "bytes": reference["bytes"].as_i64(),
            "timestamp": anchor.map(|anchor| anchor.time).unwrap_or_default(),
            "turn": anchor.map(|anchor| anchor.turn).unwrap_or_default(),
            "seq": anchor.map(|anchor| anchor.seq).unwrap_or_default(),
            "task_id": task_id,
            "task_title": title,
        }));
    }
    Ok(items)
}

/// Pictures the agent captured or drew: browser screenshots, generations and
/// plugin renders. Copies of an upload are skipped; that upload has its own row.
fn captured_images(
    anchors: &mut Anchors<'_>,
    conn: &Connection,
    task: Option<&str>,
) -> Result<Vec<Value>> {
    let filter = if task.is_some() {
        " AND v.task_id=?1"
    } else {
        ""
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT v.task_id,v.artifact_id,v.metadata,
                COALESCE(NULLIF(m.title,''),NULLIF(t.prompt,''),'')
         FROM agent_visual_artifacts v
         LEFT JOIN agent_session_meta m ON m.task_id=v.task_id
         LEFT JOIN agent_tasks t ON t.id=v.task_id
         WHERE 1=1{filter}
         ORDER BY v.rowid DESC"
    ))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(task.into_iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for (task_id, artifact_id, metadata, title) in rows {
        let metadata: Value = serde_json::from_str(&metadata).unwrap_or_default();
        let source = metadata["source"].as_str().unwrap_or_default();
        if UPLOAD_COPY_SOURCES.contains(&source) {
            continue;
        }
        let captured = metadata["captured_at"].as_i64().unwrap_or_default();
        let anchor = anchors.by_time(&task_id, captured)?;
        let url = metadata["image_url"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!("/agent/tasks/{task_id}/visual-artifacts/{artifact_id}/image")
            });
        items.push(json!({
            "source": "artifact",
            "url": url,
            "thumb_url": url,
            "label": artifact_label(source),
            "artifact_id": artifact_id,
            "width": metadata["width"].as_i64(),
            "height": metadata["height"].as_i64(),
            "timestamp": anchor.map(|anchor| anchor.time).unwrap_or(captured),
            "turn": anchor.map(|anchor| anchor.turn).unwrap_or_default(),
            "seq": anchor.map(|anchor| anchor.seq).unwrap_or_default(),
            "task_id": task_id,
            "task_title": title,
        }));
    }
    Ok(items)
}
/// A short Chinese name for where a picture came from: the store records the
/// tool that produced it, which is the only thing the reader needs to know.
fn artifact_label(source: &str) -> &'static str {
    if source.contains("screenshot") {
        "截图"
    } else if source.contains("generate_image") {
        "AI 生成图"
    } else if source.contains("register_image") {
        "本地图片"
    } else if source == "plugin_page" {
        "插件渲染图"
    } else if source == "analysis_sheet" {
        "拼图分析"
    } else {
        "视觉产物"
    }
}

/// Files: what the user attached plus what the agent changed in the workspace.
fn file_items(conn: &Connection, task: Option<&str>) -> Result<Vec<Value>> {
    let mut anchors = Anchors::new(conn);
    let mut items = Vec::new();
    if has_table(conn, "agent_attachments")? {
        items.extend(uploaded_files(&mut anchors, conn, task)?);
    }
    if has_table(conn, "agent_file_changes")? {
        items.extend(changed_files(&mut anchors, conn, task)?);
    }
    Ok(items)
}

/// Non-picture attachments: documents the user handed to the conversation.
fn uploaded_files(
    anchors: &mut Anchors<'_>,
    conn: &Connection,
    task: Option<&str>,
) -> Result<Vec<Value>> {
    let filter = if task.is_some() {
        " AND a.task_id=?1"
    } else {
        ""
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT a.task_id,a.receipt,a.reference,a.media_type,a.turn,
                COALESCE(NULLIF(m.title,''),NULLIF(t.prompt,''),'')
         FROM agent_attachments a
         LEFT JOIN agent_session_meta m ON m.task_id=a.task_id
         LEFT JOIN agent_tasks t ON t.id=a.task_id
         WHERE a.media_type NOT LIKE 'image/%' AND a.image_artifact IS NULL{filter}
         ORDER BY a.rowid DESC"
    ))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(task.into_iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for (task_id, receipt, reference, media_type, turn, title) in rows {
        let reference: Value = serde_json::from_str(&reference).unwrap_or_default();
        let anchor = match turn {
            Some(turn) => anchors.of_turn(&task_id, turn)?,
            None => None,
        };
        let name = reference["name"].as_str().unwrap_or("附件").to_owned();
        items.push(json!({
            "source": "upload",
            "name": name,
            "path": name,
            "media_type": media_type,
            "bytes": reference["bytes"].as_i64(),
            "download_url": format!("/agent/tasks/{task_id}/attachments/{receipt}"),
            "timestamp": anchor.map(|anchor| anchor.time).unwrap_or_default(),
            "turn": anchor.map(|anchor| anchor.turn).unwrap_or_default(),
            "seq": anchor.map(|anchor| anchor.seq).unwrap_or_default(),
            "task_id": task_id,
            "task_title": title,
        }));
    }
    Ok(items)
}

/// Workspace files the agent created, edited or deleted, with their line counts.
fn changed_files(
    anchors: &mut Anchors<'_>,
    conn: &Connection,
    task: Option<&str>,
) -> Result<Vec<Value>> {
    let filter = if task.is_some() {
        " AND c.task_id=?1"
    } else {
        ""
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT c.task_id,c.turn,c.path,c.added,c.deleted,c.binary,c.after_exists,
                COALESCE(NULLIF(m.title,''),NULLIF(t.prompt,''),'')
         FROM agent_file_changes c
         LEFT JOIN agent_session_meta m ON m.task_id=c.task_id
         LEFT JOIN agent_tasks t ON t.id=c.task_id
         WHERE 1=1{filter}
         ORDER BY c.rowid DESC"
    ))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(task.into_iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for (task_id, turn, path, added, deleted, binary, after_exists, title) in rows {
        let anchor = anchors.of_turn(&task_id, turn)?;
        let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_owned();
        let download = format!(
            "/agent/tasks/{task_id}/workspace-file?path={}",
            url::form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>()
        );
        items.push(json!({
            "source": "workspace_change",
            "name": name,
            "path": path,
            "added": added,
            "deleted": deleted,
            "binary": binary == 1,
            "removed": after_exists == 0,
            "download_url": download,
            "timestamp": anchor.map(|anchor| anchor.time).unwrap_or_default(),
            "turn": turn,
            "seq": anchor.map(|anchor| anchor.seq).unwrap_or_default(),
            "task_id": task_id,
            "task_title": title,
        }));
    }
    Ok(items)
}

#[derive(Deserialize)]
pub struct HistoryDatesQuery {
    task_id: Option<String>,
    month: Option<String>,
}

/// The calendar behind the date tab: which days hold messages, and which months
/// hold anything at all.
pub async fn history_dates(
    State(state): State<AgentServiceState>,
    axum::extract::Query(query): axum::extract::Query<HistoryDatesQuery>,
) -> Response {
    let root = state.workspace.root().to_path_buf();
    let result = tokio::task::spawn_blocking(move || dates(&root, &query)).await;
    response(result.unwrap_or_else(|error| Err(anyhow::anyhow!("history query aborted: {error}"))))
}

fn dates(root: &FsPath, query: &HistoryDatesQuery) -> Result<Value> {
    let conn = agent_service::open_db(root)?;
    let task = trim_option(query.task_id.as_deref());
    let filter = if task.is_some() { " AND task_id=?1" } else { "" };
    let mut stmt = conn.prepare(&format!(
        "SELECT timestamp FROM agent_task_events WHERE kind IN ({MESSAGE_KINDS}){filter} ORDER BY timestamp DESC LIMIT {SCAN_LIMIT}"
    ))?;
    let stamps = stmt
        .query_map(rusqlite::params_from_iter(task.into_iter()), |r| {
            r.get::<_, i64>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut days: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for stamp in stamps {
        if let Some(day) = local_day(stamp) {
            *days.entry(day).or_default() += 1;
        }
    }
    let months = days
        .keys()
        .map(|day| day[..7].to_owned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let month = match trim_option(query.month.as_deref()) {
        Some(month) => month.to_owned(),
        // A calendar with no month of its own opens on the newest activity.
        None => months
            .last()
            .cloned()
            .unwrap_or_else(|| Local::now().format("%Y-%m").to_string()),
    };
    let entries = days
        .iter()
        .filter(|(day, _)| day.starts_with(&month))
        .map(|(day, count)| json!({"date": day, "count": count}))
        .collect::<Vec<_>>();
    Ok(json!({"month": month, "days": entries, "months": months}))
}

/// Conversations the agent may read, newest activity first.
pub fn list_sessions(root: &FsPath, args: &Value) -> Result<Value> {
    let conn = agent_service::open_db(root)?;
    let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50);
    let needle = trim_option(args["query"].as_str());
    let workspace = trim_option(args["workspace"].as_str());
    let window = time_window(
        args["date"].as_str(),
        args["since"].as_str(),
        args["until"].as_str(),
    )?;
    let mut clause = String::new();
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    if let Some((start, end)) = window {
        clause.push_str(" AND t.created_at>=? AND t.created_at<?");
        values.push(rusqlite::types::Value::Integer(start));
        values.push(rusqlite::types::Value::Integer(end));
    }
    if let Some(workspace) = workspace {
        clause.push_str(" AND COALESCE(m.workspace,'')=?");
        values.push(rusqlite::types::Value::Text(workspace.to_owned()));
    }
    if let Some(needle) = needle {
        clause.push_str(" AND (t.prompt LIKE ? ESCAPE '\\' OR m.title LIKE ? ESCAPE '\\')");
        let pattern = like_pattern(needle);
        values.push(rusqlite::types::Value::Text(pattern.clone()));
        values.push(rusqlite::types::Value::Text(pattern));
    }
    // A store that has never held a picture or a tracked change reports zero
    // instead of failing the whole listing.
    let images = if has_table(&conn, "agent_attachments")? {
        "(SELECT COUNT(*) FROM agent_attachments a WHERE a.task_id=t.id AND (a.media_type LIKE 'image/%' OR a.image_artifact IS NOT NULL))"
    } else {
        "0"
    };
    let visuals = if has_table(&conn, "agent_visual_artifacts")? {
        "(SELECT COUNT(*) FROM agent_visual_artifacts v WHERE v.task_id=t.id
            AND v.metadata NOT LIKE '%\"source\":\"user_attachment\"%'
            AND v.metadata NOT LIKE '%\"source\":\"user_pdf\"%'
            AND v.metadata NOT LIKE '%\"source\":\"attachment\"%')"
    } else {
        "0"
    };
    let changes = if has_table(&conn, "agent_file_changes")? {
        "(SELECT COUNT(*) FROM agent_file_changes c WHERE c.task_id=t.id)"
    } else {
        "0"
    };
    let from = format!(
        "FROM agent_tasks t
         LEFT JOIN agent_session_meta m ON m.task_id=t.id
         WHERE 1=1{clause}"
    );
    let mut stmt = conn.prepare(&format!(
        "SELECT t.id,
                COALESCE(NULLIF(m.title,''),NULLIF(t.prompt,''),''),
                COALESCE(m.workspace,''),t.status,t.created_at,t.updated_at,
                CASE WHEN COALESCE(m.kind,'code')='chat'
                      OR EXISTS(SELECT 1 FROM plugin_sessions p WHERE p.task_id=t.id)
                     THEN 'chat' ELSE 'code' END,
                (SELECT COUNT(*) FROM agent_task_events e WHERE e.task_id=t.id AND e.kind IN ({MESSAGE_KINDS})),
                {images} + {visuals},
                {changes}
         {from}
         ORDER BY t.updated_at DESC LIMIT {limit}"
    ))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values.iter()), |r| {
            Ok(json!({
                "task_id": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "workspace": r.get::<_, String>(2)?,
                "status": r.get::<_, String>(3)?,
                "kind": r.get::<_, String>(6)?,
                "created_at": r.get::<_, i64>(4)?,
                "updated_at": r.get::<_, i64>(5)?,
                "message_count": r.get::<_, i64>(7)?,
                "image_count": r.get::<_, i64>(8)?,
                "file_count": r.get::<_, i64>(9)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) {from}"),
        rusqlite::params_from_iter(values.iter()),
        |r| r.get(0),
    )?;
    Ok(json!({"sessions": rows, "total": total}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_search_snippets_center_on_the_match() {
        let text = "上次改 pptx 导图的时候，把封面换成了蓝色背景，然后又调整了目录页。";
        assert_eq!(
            snippet(text, "PPTX 导图", 4).unwrap(),
            "上次改 pptx 导图的时候，…"
        );
        assert_eq!(snippet(text, "蓝色", 2).unwrap(), "…成了蓝色背景…");
        assert!(snippet(text, "不存在", 4).is_none());
        let data = json!({"message":{"content":[{"type":"text","text":"封面换成蓝色"}]}});
        assert_eq!(event_text(&data), "封面换成蓝色");
    }
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_tasks(id TEXT PRIMARY KEY,status TEXT,parent_task_id TEXT,permission_mode TEXT);
            CREATE TABLE plugin_sessions(task_id TEXT PRIMARY KEY,plugin_id TEXT);
            CREATE TABLE agent_task_events(task_id TEXT,seq INTEGER);
            CREATE TABLE agent_runtime_windows(task_id TEXT,scope TEXT,state TEXT);
            CREATE TABLE ppt_session_jobs(host_task_id TEXT,remote_task_id TEXT);
            CREATE TABLE agent_attachments(task_id TEXT,receipt TEXT);
            CREATE TABLE agent_delegates(id TEXT,task_id TEXT);
            CREATE TABLE agent_delegate_executors(agent_id TEXT);",
        )
        .unwrap();
        schema(&conn, FsPath::new("/projects/current")).unwrap();
        conn
    }
    #[test]
    fn desktop_removes_only_unused_legacy_default_once() {
        let conn = db();
        let root = FsPath::new("/projects/current");
        schema_with_default_project(&conn, root, false).unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM agent_projects", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        conn.execute("INSERT INTO agent_projects(path,title) VALUES ('/projects/current','current')", []).unwrap();
        schema_with_default_project(&conn, root, false).unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM agent_projects", [], |r| r.get::<_, i64>(0)).unwrap(), 1);

        let used = db();
        used.execute("INSERT INTO agent_tasks(id,status,parent_task_id) VALUES ('old','completed',NULL)", []).unwrap();
        schema_with_default_project(&used, root, false).unwrap();
        assert_eq!(used.query_row("SELECT count(*) FROM agent_projects", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }
    #[test]
    fn deletes_descendants_and_legacy_records_without_touching_other_sessions() {
        let mut conn = db();
        conn.execute_batch("INSERT INTO agent_tasks(id,status,parent_task_id) VALUES ('root','completed',NULL),('child','completed','root'),('grandchild','failed','child'),('other','draft',NULL);
            INSERT INTO plugin_sessions VALUES ('root','ppt-master');
            INSERT INTO agent_runtime_windows VALUES ('child','main','{}'),('other','main','{}');
            INSERT INTO agent_task_events VALUES ('root',1),('grandchild',1),('other',1);
            INSERT INTO ppt_session_jobs VALUES ('root','remote');
            INSERT INTO agent_attachments VALUES ('child','receipt');
            INSERT INTO agent_delegates VALUES ('delegate','root');
            INSERT INTO agent_delegate_executors VALUES ('delegate');
            INSERT INTO agent_session_meta(task_id,title) VALUES ('root','Custom name');").unwrap();
        let deleted = delete(&mut conn, "root").unwrap();
        assert_eq!(deleted.len(), 3);
        for table in [
            "plugin_sessions",
            "ppt_session_jobs",
            "agent_attachments",
            "agent_session_meta",
            "agent_delegates",
            "agent_delegate_executors",
        ] {
            assert_eq!(
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            conn.query_row("SELECT id FROM agent_tasks", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "other"
        );
        assert_eq!(
            conn.query_row("SELECT task_id FROM agent_runtime_windows", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "other"
        );
        assert_eq!(
            conn.query_row("SELECT task_id FROM agent_task_events", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "other"
        );
    }
    #[test]
    fn active_descendant_rejects_deletion_atomically() {
        let mut conn = db();
        conn.execute_batch("INSERT INTO agent_tasks(id,status,parent_task_id) VALUES ('root','completed',NULL),('child','running','root'); INSERT INTO agent_task_events VALUES ('root',1);").unwrap();
        assert!(delete(&mut conn, "root").is_err());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM agent_tasks", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM agent_task_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn removing_the_default_project_is_durable_and_does_not_delete_sessions() {
        let conn = db();
        conn.execute_batch(
            "INSERT INTO agent_tasks(id,status,parent_task_id) VALUES ('root','completed',NULL); DELETE FROM agent_projects;",
        )
        .unwrap();
        schema(&conn, FsPath::new("/projects/current")).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM agent_projects", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let task = decorate(
            &conn,
            FsPath::new("/projects/current"),
            json!({"task_id":"root"}),
        )
        .unwrap();
        assert_eq!(task["workspace"], "/projects/current");
    }
    #[test]
    fn classifies_chat_and_project_sessions_and_keeps_title_separate_from_prompt() {
        let conn = db();
        conn.execute_batch("INSERT INTO plugin_sessions VALUES ('plugin','ppt-master');
            INSERT INTO agent_session_meta(task_id,title,pinned,archived,workspace,kind) VALUES ('code','Renamed',1,0,'/projects/second','code'),('chat',NULL,0,0,NULL,'chat');").unwrap();
        let task = decorate(
            &conn,
            FsPath::new("/projects/current"),
            json!({"task_id":"code","prompt":"Original user request"}),
        )
        .unwrap();
        assert_eq!(task["title"], "Renamed");
        assert_eq!(task["prompt"], "Original user request");
        assert_eq!(task["workspace"], "/projects/second");
        assert_eq!(task["pinned"], true);
        for id in ["plugin", "chat"] {
            let chat = decorate(
                &conn,
                FsPath::new("/projects/current"),
                json!({"task_id":id}),
            )
            .unwrap();
            assert_eq!(chat["kind"], "chat");
            assert!(chat["workspace"].is_null());
        }
    }
    /// A throwaway workspace whose state database is created by the real schema.
    fn history_root(label: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("session-browser-{label}-{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Two conversations over two days, with a picture, a document and a tracked
    /// change, so every dialog tab has something real to read.
    fn seed_history(root: &FsPath) -> (i64, i64) {
        let (first, _) = day_bounds("2026-11-20").unwrap();
        let (second, _) = day_bounds("2026-11-22").unwrap();
        let conn = agent_service::open_db(root).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_attachments(receipt TEXT PRIMARY KEY, task_id TEXT NOT NULL, attachment_id TEXT NOT NULL, reference TEXT NOT NULL, image_artifact TEXT, media_type TEXT NOT NULL, turn INTEGER, remote_scope TEXT, remote_path TEXT);
             CREATE TABLE IF NOT EXISTS agent_visual_artifacts(artifact_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, metadata TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS agent_file_changes(task_id TEXT NOT NULL, turn INTEGER NOT NULL, path TEXT NOT NULL,
                before_exists INTEGER NOT NULL, before_bytes BLOB, after_exists INTEGER NOT NULL, after_bytes BLOB,
                added INTEGER NOT NULL, deleted INTEGER NOT NULL, binary INTEGER NOT NULL, coarse INTEGER NOT NULL,
                unavailable INTEGER NOT NULL, conflicted INTEGER NOT NULL DEFAULT 0, reverted INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(task_id,turn,path));",
        )
        .unwrap();
        let insert = |id: &str, prompt: &str, created: i64| {
            conn.execute(
                "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES (?1,?2,'fake','completed',?3,?3)",
                params![id, prompt, created],
            )
            .unwrap();
        };
        insert("deploy", "把服务部署到测试环境", first + 1_000);
        insert("report", "整理本季度的复盘稿", second + 1_000);
        conn.execute_batch(
            "INSERT INTO agent_session_meta(task_id,title,pinned,archived,workspace,kind) VALUES
                ('deploy','部署测试环境',0,0,'/projects/alpha','code'),
                ('report','季度复盘',0,0,'/projects/beta','code');",
        )
        .unwrap();
        let event = |task: &str, seq: i64, at: i64, kind: &str, data: Value| {
            conn.execute(
                "INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES (?1,?2,?3,?4,?5)",
                params![task, seq, at, kind, data.to_string()],
            )
            .unwrap();
        };
        event("deploy", 1, first + 2_000, "user/message",
            json!({"turn":1,"message":{"role":"user","content":"把服务部署到测试环境，顺便看看端口"}}));
        event("deploy", 2, first + 3_000, "assistant/message",
            json!({"turn":1,"message":{"content":"已经部署完成，端口 3001 在监听。"}}));
        event("deploy", 3, first + 4_000, "tool/call",
            json!({"turn":1,"name":"exec_command","arguments":"{}"}));
        event("report", 1, second + 2_000, "user/message",
            json!({"turn":1,"message":{"role":"user","content":"整理本季度的复盘稿，重点讲端口迁移"}}));
        event("report", 2, second + 3_000, "assistant/message",
            json!({"turn":1,"message":{"content":"复盘稿已经写好，分成三部分。"}}));
        conn.execute_batch(&format!(
            "INSERT INTO agent_attachments(receipt,task_id,attachment_id,reference,media_type,turn) VALUES
                ('r-img','deploy','sha256:{0}','{{\"attachmentId\":\"sha256:{0}\",\"name\":\"topology.png\",\"bytes\":2048}}','image/png',1),
                ('r-doc','report','sha256:{1}','{{\"attachmentId\":\"sha256:{1}\",\"name\":\"review.docx\",\"bytes\":4096}}','application/vnd.openxmlformats-officedocument.wordprocessingml.document',1);
             INSERT INTO agent_visual_artifacts(artifact_id,task_id,metadata) VALUES
                ('visual-shot','deploy','{{\"source\":\"browser_screenshot\",\"captured_at\":{2},\"width\":1280,\"height\":720,\"image_url\":\"/agent/tasks/deploy/visual-artifacts/visual-shot/image\"}}'),
                ('visual-copy','deploy','{{\"source\":\"user_attachment\",\"captured_at\":{2}}}');
             INSERT INTO agent_file_changes(task_id,turn,path,before_exists,before_bytes,after_exists,after_bytes,added,deleted,binary,coarse,unavailable)
                VALUES ('deploy',1,'src/server/gateway.rs',1,X'00',1,X'01',12,3,0,0,0);",
            "a".repeat(64),
            "b".repeat(64),
            first + 3_500,
        ))
        .unwrap();
        (first, second)
    }

    fn items(value: &Value) -> &Vec<Value> {
        value["items"].as_array().unwrap()
    }

    fn listed(value: &Value) -> &Vec<Value> {
        value["sessions"].as_array().unwrap()
    }

    #[test]
    fn history_messages_scope_by_task_and_search_every_conversation() {
        let root = history_root("messages");
        seed_history(&root);
        let all = messages(
            &root,
            &HistoryMessagesQuery {
                task_id: None,
                q: None,
                date: None,
                from: None,
                to: None,
                limit: None,
                offset: None,
            },
        )
        .unwrap();
        // Tool traffic is not chat: only the two questions and two answers.
        assert_eq!(all["total"], 4);
        assert_eq!(items(&all).len(), 4);
        assert_eq!(items(&all)[0]["role"], "assistant");
        assert_eq!(items(&all)[0]["task_title"], "季度复盘");

        let scoped = messages(
            &root,
            &HistoryMessagesQuery {
                task_id: Some("deploy".to_owned()),
                q: None,
                date: None,
                from: None,
                to: None,
                limit: None,
                offset: None,
            },
        )
        .unwrap();
        assert_eq!(scoped["total"], 2);
        assert!(items(&scoped).iter().all(|row| row["task_id"] == "deploy"));

        // One word across every conversation, with a snippet around each hit.
        let found = messages(
            &root,
            &HistoryMessagesQuery {
                task_id: None,
                q: Some("端口".to_owned()),
                date: None,
                from: None,
                to: None,
                limit: None,
                offset: None,
            },
        )
        .unwrap();
        // Three messages mention the word: both halves of the deployment turn and
        // the review prompt that asks about the same subject.
        assert_eq!(found["total"], 3);
        assert!(items(&found)
            .iter()
            .all(|row| row["snippet"].as_str().is_some_and(|text| text.contains("端口"))));

        // A day narrows the same search to what happened on it.
        let dated = messages(
            &root,
            &HistoryMessagesQuery {
                task_id: None,
                q: Some("端口".to_owned()),
                date: Some("2026-11-20".to_owned()),
                from: None,
                to: None,
                limit: None,
                offset: None,
            },
        )
        .unwrap();
        assert_eq!(dated["total"], 2);
        assert!(items(&dated).iter().all(|row| row["task_id"] == "deploy"));
    }

    #[test]
    fn history_images_merge_uploads_and_captures_without_repeating_a_copy() {
        let root = history_root("images");
        seed_history(&root);
        let value = media(
            &root,
            &HistoryMediaQuery {
                task_id: None,
                kind: Some("images".to_owned()),
                q: None,
                date: None,
                from: None,
                to: None,
                limit: None,
                offset: None,
            },
        )
        .unwrap();
        let rows = items(&value);
        // The uploaded picture and the browser screenshot; the artifact copy of an
        // upload is the same picture and must not appear twice.
        assert_eq!(value["total"], 2);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row["source"] == "upload" && row["label"] == "topology.png"));
        assert!(rows.iter().any(|row| row["source"] == "artifact"
            && row["label"] == "截图"
            && row["width"] == 1280));
        // Each picture points back at the turn it belongs to.
        for row in rows {
            assert_eq!(row["task_id"], "deploy");
            assert_eq!(row["turn"], 1);
            assert!(row["seq"].as_i64().unwrap() > 0);
            assert!(row["timestamp"].as_i64().unwrap() > 0);
        }
    }

    #[test]
    fn history_files_merge_uploads_and_workspace_changes() {
        let root = history_root("files");
        seed_history(&root);
        let value = media(
            &root,
            &HistoryMediaQuery {
                task_id: None,
                kind: Some("files".to_owned()),
                q: None,
                date: None,
                from: None,
                to: None,
                limit: None,
                offset: None,
            },
        )
        .unwrap();
        let rows = items(&value);
        assert_eq!(value["total"], 2);
        let upload = rows.iter().find(|row| row["source"] == "upload").unwrap();
        assert_eq!(upload["name"], "review.docx");
        assert_eq!(upload["task_id"], "report");
        assert_eq!(upload["download_url"], "/agent/tasks/report/attachments/r-doc");
        let change = rows
            .iter()
            .find(|row| row["source"] == "workspace_change")
            .unwrap();
        assert_eq!(change["name"], "gateway.rs");
        assert_eq!(change["path"], "src/server/gateway.rs");
        assert_eq!(change["added"], 12);
        assert_eq!(change["deleted"], 3);
        assert_eq!(change["removed"], false);
        assert!(change["download_url"]
            .as_str()
            .unwrap()
            .contains("workspace-file?path=src%2Fserver%2Fgateway.rs"));
    }

    #[test]
    fn history_dates_count_messages_per_local_day() {
        let root = history_root("dates");
        seed_history(&root);
        let value = dates(
            &root,
            &HistoryDatesQuery {
                task_id: None,
                month: Some("2026-11".to_owned()),
            },
        )
        .unwrap();
        assert_eq!(value["month"], "2026-11");
        assert_eq!(
            value["days"],
            json!([{"date":"2026-11-20","count":2},{"date":"2026-11-22","count":2}])
        );
        assert_eq!(value["months"], json!(["2026-11"]));

        let scoped = dates(
            &root,
            &HistoryDatesQuery {
                task_id: Some("report".to_owned()),
                month: None,
            },
        )
        .unwrap();
        assert_eq!(scoped["days"], json!([{"date":"2026-11-22","count":2}]));
    }

    #[test]
    fn list_sessions_reports_activity_and_filters_by_day_workspace_and_words() {
        let root = history_root("sessions");
        let (first, second) = seed_history(&root);
        let value = list_sessions(&root, &json!({})).unwrap();
        assert_eq!(value["total"], 2);
        let newest = listed(&value)[0].clone();
        assert_eq!(newest["task_id"], "report");
        assert_eq!(newest["title"], "季度复盘");
        assert_eq!(newest["workspace"], "/projects/beta");
        assert_eq!(newest["kind"], "code");
        assert_eq!(newest["status"], "completed");
        assert_eq!(newest["message_count"], 2);
        assert_eq!(newest["image_count"], 0);
        assert_eq!(newest["file_count"], 0);
        let deploy = listed(&value)
            .iter()
            .find(|row| row["task_id"] == "deploy")
            .unwrap();
        assert_eq!(deploy["image_count"], 2);
        assert_eq!(deploy["file_count"], 1);

        let words = list_sessions(&root, &json!({"query": "复盘"})).unwrap();
        assert_eq!(words["total"], 1);
        assert_eq!(listed(&words)[0]["task_id"], "report");

        let project = list_sessions(&root, &json!({"workspace": "/projects/alpha"})).unwrap();
        assert_eq!(project["total"], 1);
        assert_eq!(listed(&project)[0]["task_id"], "deploy");

        let day = list_sessions(&root, &json!({"date": "2026-11-20"})).unwrap();
        assert_eq!(day["total"], 1);
        assert_eq!(listed(&day)[0]["created_at"], first + 1_000);
        let window = list_sessions(&root, &json!({"since": "2026-11-21", "until": "2026-11-22"})).unwrap();
        assert_eq!(window["total"], 1);
        assert_eq!(listed(&window)[0]["created_at"], second + 1_000);

        // The limit is capped so one call cannot walk the whole state database.
        let capped = list_sessions(&root, &json!({"limit": 500})).unwrap();
        assert_eq!(listed(&capped).len(), 2);
    }
}
