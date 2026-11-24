//! SQLite schema, task state, and durable event writes.

use super::{AgentServiceState, PermissionMode};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub(crate) fn open_db(root: &std::path::Path) -> anyhow::Result<rusqlite::Connection> {
    crate::context::database::open_initialized(root, Some(agent_schema))
}

fn agent_schema(conn: &rusqlite::Connection, root: &std::path::Path) -> anyhow::Result<()> {
    use crate::context::database::add_column;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_tasks (
            id TEXT PRIMARY KEY, prompt TEXT NOT NULL, model TEXT NOT NULL,
            status TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            parent_task_id TEXT, provider_name TEXT, reasoning_effort TEXT,
            fast_mode INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS agent_task_events (
            task_id TEXT NOT NULL, seq INTEGER NOT NULL, timestamp INTEGER NOT NULL,
            kind TEXT NOT NULL, data TEXT NOT NULL, surface_op TEXT, PRIMARY KEY(task_id, seq),
            FOREIGN KEY(task_id) REFERENCES agent_tasks(id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS plugin_sessions (
            task_id TEXT PRIMARY KEY REFERENCES agent_tasks(id) ON DELETE CASCADE,
            plugin_id TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS agent_observations (
            task_id TEXT NOT NULL, review_id TEXT NOT NULL, request_id INTEGER NOT NULL,
            input TEXT NOT NULL, status TEXT NOT NULL, result TEXT,
            PRIMARY KEY(task_id,review_id), FOREIGN KEY(task_id) REFERENCES agent_tasks(id) ON DELETE CASCADE
         );",
    )?;
    for column in ["surface_op", "node_id"] {
        add_column(conn, "agent_task_events", column, "TEXT")?;
    }
    crate::tools::plan_tools::schema(conn)?;
    crate::tools::workspace_changes::schema(conn)?;
    crate::runtime::function_frames::schema(conn)?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS task_events_node ON agent_task_events(task_id,node_id,seq);")?;
    for (column, definition) in [
        ("parent_task_id", "TEXT"),
        ("provider_name", "TEXT"),
        ("reasoning_effort", "TEXT"),
        ("fast_mode", "INTEGER NOT NULL DEFAULT 0"),
        ("permission_mode", "TEXT"),
    ] {
        add_column(conn, "agent_tasks", column, definition)?;
    }
    if add_column(conn, "agent_tasks", "event_high_water", "INTEGER NOT NULL DEFAULT -1")? {
        conn.execute("UPDATE agent_tasks SET event_high_water=COALESCE((SELECT MAX(seq) FROM agent_task_events WHERE task_id=agent_tasks.id),-1)", [])?;
    }
    crate::context::session_browser::schema(conn, root)?;
    crate::context::request_context::schema(conn)?;
    Ok(())
}

/// The level a chat runs at: the one it stored, and otherwise asking first.
/// A chat saved before levels were stored therefore asks first, never full
/// access, and an unreadable value can only reduce access.
pub(crate) fn permission_mode_for(
    conn: &rusqlite::Connection,
    task_id: &str,
) -> anyhow::Result<PermissionMode> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT permission_mode FROM agent_tasks WHERE id=?1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(stored
        .as_deref()
        .and_then(PermissionMode::parse)
        .unwrap_or_default())
}

/// Remember a chat's level. `updated_at` is left alone: choosing a level is not
/// activity, so it must not reorder the sidebar or retitle the chat.
pub(crate) fn save_permission_mode(
    conn: &rusqlite::Connection,
    task_id: &str,
    mode: PermissionMode,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE agent_tasks SET permission_mode=?2 WHERE id=?1",
        rusqlite::params![task_id, mode.as_str()],
    )?;
    Ok(())
}

/// Advisory process lock held for the lifetime of an agent service instance so
/// another process sharing the same workspace state directory never mistakes
/// live running tasks for crashed orphans.
pub struct ServiceLock(#[allow(dead_code)] std::fs::File);

/// Try to claim exclusive ownership of `<workspace>/.codex-workspace-mcp/service.lock`.
/// Returns `Ok(None)` when another live agent process already holds it.
pub fn try_acquire_service_lock(workspace_root: &std::path::Path) -> anyhow::Result<Option<ServiceLock>> {
    let dir = workspace_root.join(".codex-workspace-mcp");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("service.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(ServiceLock(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Acquire the workspace service lock and recover orphaned tasks only when no
/// other live instance owns the workspace state directory.
pub async fn acquire_service_lock_and_recover(
    workspace_root: &std::path::Path,
) -> anyhow::Result<Option<ServiceLock>> {
    let lock = try_acquire_service_lock(workspace_root)?;
    if lock.is_some() {
        recover_orphaned_tasks(workspace_root).await?;
    }
    Ok(lock)
}

pub async fn recover_orphaned_tasks(workspace_root: &std::path::Path) -> anyhow::Result<()> {
    let root = workspace_root.to_path_buf();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut conn = open_db(&root)?;
        let ids = {
            let mut stmt = conn.prepare("SELECT id FROM agent_tasks WHERE status='running'")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for task_id in ids {
            crate::context::request_context::recover_interrupted(&mut conn, &task_id)?;
            if let Some(event) = crate::context::session_history::load_events(
                &conn,
                &task_id,
                &crate::context::session_history::EventQuery {
                    kinds: &["step/start", "step/end"],
                    newest: Some(1),
                    ..Default::default()
                },
            )?
            .pop()
                && event.kind == "step/start"
            {
                let step_data = event.data()?;
                let tx =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                append_event_tx(&tx, &task_id, "step/end", &step_data, None)?;
                tx.commit()?;
            }
            let turn = crate::context::session_history::load_events(
                &conn,
                &task_id,
                &crate::context::session_history::EventQuery {
                    kinds: &["turn/start"],
                    newest: Some(1),
                    ..Default::default()
                },
            )?
            .pop()
            .map(|event| event.data())
            .transpose()?
            .and_then(|data| data["turn"].as_u64())
            .unwrap_or(1) as usize;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            append_event_tx(
                &tx,
                &task_id,
                "turn/end",
                &json!({"turn":turn,"reason":{"kind":"interrupted"}}),
                None,
            )?;
            tx.execute(
                "UPDATE agent_tasks SET status='interrupted',updated_at=?2 WHERE id=?1",
                params![task_id, now()],
            )?;
            tx.commit()?;
        }
        cleanup_terminal_task_data(&mut conn, None, true)?;
        Ok(())
    })
    .await??;
    Ok(())
}

/// Replace repeated execution snapshots with one resumable Flow record after a run ends.
/// The execution history (tools, progress, Flow and messages), task metadata,
/// Observer conclusions, memories and code indexes are retained.
pub(super) fn cleanup_terminal_task_data(
    conn: &mut rusqlite::Connection,
    task_id: Option<&str>,
    compact: bool,
) -> anyhow::Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let task_filter = "status IN ('completed','failed','cancelled','max_steps','interrupted') AND (?1 IS NULL OR id=?1)";
    if let Some(task_id) = task_id {
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM agent_tasks WHERE id=?1",
                [task_id],
                |r| r.get(0),
            )
            .optional()?;
        if !status.as_deref().is_some_and(|s| {
            matches!(
                s,
                "completed" | "failed" | "cancelled" | "max_steps" | "interrupted"
            )
        }) {
            return Ok(());
        }
    }
    let task_selector = format!("SELECT id FROM agent_tasks WHERE {task_filter}");
    // One continuation record replaces the repeated execution snapshots. Keep
    // the exact current frame so a paused node can resume after cleanup.
    let sessions = {
        let mut stmt=tx.prepare(&format!("SELECT task_id,data FROM agent_task_events e WHERE task_id IN ({task_selector}) AND kind='execution/commit' AND seq=(SELECT MAX(seq) FROM agent_task_events WHERE task_id=e.task_id AND kind='execution/commit')"))?;
        let rows = stmt.query_map([task_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, raw) in sessions {
        let mut data: Value = serde_json::from_str(&raw)?;
        let worker=tx.query_row("SELECT data FROM agent_task_events WHERE task_id=?1 AND kind='worker/work_state' ORDER BY seq DESC LIMIT 1",
            [&id],|row|row.get::<_,String>(0)).optional()?;
        if let Some(state) = worker
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|data| data.get("state").cloned())
        {
            data["worker_state"] = state;
        }
        tx.execute(
            "DELETE FROM agent_task_events WHERE task_id=?1 AND kind='flow/session'",
            [&id],
        )?;
        append_event_tx(&tx, &id, "flow/session", &data, None)?;
    }
    let sql = format!(
        "DELETE FROM agent_task_events WHERE task_id IN ({task_selector}) AND (
            kind IN ('scheduler/state','worker/work_state',
                     'worker/source_working_set','execution/tick',
                     'organizer/request_metrics')
            OR kind LIKE 'execution/%'
        )"
    );
    tx.execute(&sql, [task_id])?;

    // Actual role inputs and their conversation references are durable audit
    // records. Completion and startup recovery must not erase them.
    tx.commit()?;

    if compact {
        let active_tasks: i64 = conn.query_row(
            "SELECT COUNT(*) FROM agent_tasks WHERE status='running'",
            [],
            |r| r.get(0),
        )?;
        let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let free_pages: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        // Avoid blocking concurrent work. A one-time compaction after startup or
        // a large purge returns the reclaimed pages to the filesystem.
        if active_tasks == 0 && free_pages.saturating_mul(page_size) >= 16 * 1024 * 1024 {
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        }
    }
    Ok(())
}

pub(super) async fn cleanup_finished_task_data(
    root: &std::path::Path,
    task_id: &str,
) -> anyhow::Result<()> {
    let root = root.to_path_buf();
    let task_id = task_id.to_owned();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut conn = open_db(&root)?;
        cleanup_terminal_task_data(&mut conn, Some(&task_id), true)
    })
    .await??;
    Ok(())
}

pub(super) fn append_event(
    root: &std::path::Path,
    task_id: &str,
    kind: &str,
    data: Value,
    surface_op: Option<&str>,
) -> anyhow::Result<()> {
    let (task_id, kind, surface_op) = (
        task_id.to_owned(),
        kind.to_owned(),
        surface_op.map(str::to_owned),
    );
    crate::context::db_writer::write(root, move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        append_event_tx(&tx, &task_id, &kind, &data, surface_op.as_deref())?;
        tx.commit()?;
        Ok(())
    })
}

/// Preview the next identity inside the same write transaction that appends it.
pub(crate) fn next_event_seq(tx: &rusqlite::Transaction, task_id: &str) -> anyhow::Result<i64> {
    Ok(tx.query_row(
        "SELECT MAX(event_high_water,COALESCE((SELECT MAX(seq) FROM agent_task_events WHERE task_id=?1),-1))+1 FROM agent_tasks WHERE id=?1",
        [task_id], |row| row.get(0),
    )?)
}

pub(crate) fn append_event_tx(
    tx: &rusqlite::Transaction,
    task_id: &str,
    kind: &str,
    data: &Value,
    surface_op: Option<&str>,
) -> anyhow::Result<i64> {
    let seq = next_event_seq(tx, task_id)?;
    let node = if let Some(id) = data["task_node_id"].as_str() {
        Some(id.to_owned())
    } else if let Some(agent) = data["agent_id"].as_str() {
        tx.query_row("SELECT node_id FROM agent_task_nodes WHERE task_id=?1 AND json_extract(data,'$.agent_id')=?2", params![task_id,agent], |r|r.get::<_,String>(0)).optional()?
    } else {
        crate::runtime::function_frames::active(tx, task_id)?.map(|n| n.node_id)
    };
    // Work done inside a task belongs to the step it is executing. Task graph
    // events name their own node.
    let node = match node {
        Some(id) if !kind.starts_with("task/") => tx
            .query_row(
                "SELECT node_id FROM agent_task_nodes WHERE task_id=?1 AND parent_id=?2 AND status='current' AND json_extract(data,'$.kind')='step' LIMIT 1",
                params![task_id, id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .or(Some(id)),
        other => other,
    };
    tx.execute(
        "INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data,surface_op,node_id) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![task_id, seq, now(), kind, serde_json::to_string(&data)?, surface_op,node],
    )?;
    tx.execute(
        "UPDATE agent_tasks SET updated_at=?2,event_high_water=?3 WHERE id=?1",
        params![task_id, now(), seq],
    )?;
    Ok(seq)
}

async fn write_db_retry<F>(action: &str, write: F) -> anyhow::Result<()>
where
    F: Fn() -> anyhow::Result<()> + Send + Sync + 'static,
{
    let write = Arc::new(write);
    let mut last_error = None;
    for attempt in 0..3 {
        let write = write.clone();
        match tokio::task::spawn_blocking(move || write()).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => last_error = Some(error),
            Err(error) => last_error = Some(error.into()),
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await;
        }
    }
    let error = last_error.unwrap_or_else(|| anyhow::anyhow!("unknown database error"));
    anyhow::bail!("{action} failed after retries: {error}")
}

pub(crate) async fn emit(
    root: &std::path::Path,
    task_id: &str,
    kind: &str,
    data: Value,
) -> anyhow::Result<()> {
    emit_surface(root, task_id, kind, data, None).await
}

async fn emit_surface(
    root: &std::path::Path,
    task_id: &str,
    kind: &str,
    data: Value,
    surface_op: Option<&'static str>,
) -> anyhow::Result<()> {
    let root = root.to_path_buf();
    let task_id = task_id.to_owned();
    let kind = kind.to_owned();
    write_db_retry("append task event", move || {
        append_event(&root, &task_id, &kind, data.clone(), surface_op)
    })
    .await
}

pub(crate) async fn finish(
    state: &AgentServiceState,
    task_id: &str,
    status: &str,
) -> anyhow::Result<()> {
    crate::runtime::single_agent::cancel_saved_programs(state, task_id).await?;
    crate::tools::shell_process::cleanup_task(state.workspace.root(), task_id).await;
    // A single-agent conversation owns durable resources across user turns.
    // Closing its browser is an explicit tool action, not failure cleanup.
    let lookup_root = state.workspace.root().to_path_buf();
    let lookup_task = task_id.to_owned();
    let single_agent = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let conn = open_db(&lookup_root)?;
        Ok(crate::context::session_history::load_events(
            &conn,
            &lookup_task,
            &crate::context::session_history::EventQuery {
                kinds: &["turn/start"],
                newest: Some(1),
                ..Default::default()
            },
        )?
        .pop()
        .map(|event| event.data())
        .transpose()?
        .is_some_and(|data| data["runtime"] == "single_agent"))
    })
    .await??;
    if !single_agent {
        crate::tools::browser_control::cleanup_after_finish(state.workspace.root(), task_id, status).await;
    }
    let root = state.workspace.root().to_path_buf();
    let id = task_id.to_owned();
    let status = status.to_owned();
    write_db_retry("finish task", move || {
        let conn = open_db(&root)?;
        let changed = conn.execute(
            "UPDATE agent_tasks SET status=?2,updated_at=?3 WHERE id=?1",
            params![id, status, now()],
        )?;
        anyhow::ensure!(changed == 1, "task no longer exists");
        Ok(())
    })
    .await
}
