//! Minimal chat module folder helpers retained for `rewind`, `session_browser`,
//! and `session_history::flow_folder`. Legacy `@module` author/fixer loops were
//! removed in the Context-as-Code v2 driver reset.

use super::host_folder;
use crate::agent_service;
use anyhow::Result;
use std::path::Path;

fn schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_modules(
            task_id TEXT NOT NULL, turn INTEGER NOT NULL, contract TEXT NOT NULL,
            name TEXT NOT NULL, signature TEXT NOT NULL, description TEXT NOT NULL,
            version INTEGER NOT NULL, source TEXT NOT NULL, fixed INTEGER NOT NULL,
            created_at INTEGER NOT NULL, PRIMARY KEY(task_id, contract, version));",
    )?;
    Ok(())
}

pub(crate) fn file_name(name: &str, key: &str) -> String {
    format!("{name}-{key}.js")
}

fn write_file(root: &Path, task: &str, name: &str, key: &str, source: &str) -> Result<()> {
    let dir = host_folder::chat_dir(root, task)?.join("modules");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(file_name(name, key)), source)?;
    Ok(())
}

fn latest_all(conn: &rusqlite::Connection, task: &str) -> Result<Vec<(String, String, String)>> {
    schema(conn)?;
    Ok(conn
        .prepare(
            "SELECT m.contract,m.name,m.source FROM agent_modules m
             WHERE m.task_id=?1 AND m.version=(SELECT MAX(version) FROM agent_modules WHERE task_id=m.task_id AND contract=m.contract)
             ORDER BY m.created_at, m.name",
        )?
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Make the chat folder hold the latest surviving version of each saved module
/// and surviving turn records after a rewind.
pub(crate) fn rebuild(root: &Path, task: &str) -> Result<()> {
    if let Some(dir) = host_folder::existing_chat_dir(root, task).map(|dir| dir.join("modules"))
        && dir.exists()
    {
        std::fs::remove_dir_all(&dir)?;
    }
    let conn = agent_service::open_db(root)?;
    for (key, name, source) in latest_all(&conn, task)? {
        write_file(root, task, &name, &key, &source)?;
    }
    if let Some(dir) = host_folder::existing_chat_dir(root, task) {
        crate::session_history::turn_record::sync(&conn, task, &dir)?;
    }
    Ok(())
}

/// Remove the chat's host folder when a conversation is deleted.
pub(crate) fn remove(root: &Path, task: &str) {
    if let Some(dir) = host_folder::existing_chat_dir(root, task) {
        let _ = std::fs::remove_dir_all(dir);
    }
}
