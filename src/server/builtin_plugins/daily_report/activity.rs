//! Today's real work, read from the QuickAgent state database and the projects'
//! own git history. Everything here is a fact the report can point at: no
//! summarising, no invented tasks.
use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// Files listed per project before the list becomes noise in a report.
const FILE_LIMIT: usize = 40;
/// Commits listed per project.
const COMMIT_LIMIT: usize = 20;

/// The local day's bounds in the millisecond clock the state database uses.
/// Both ends matter: a report is about one day, not everything since then.
fn day_bounds(date: Option<&str>) -> Result<(String, i64, i64)> {
    let day = match date.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .with_context(|| format!("{text} is not a YYYY-MM-DD date"))?,
        None => Local::now().date_naive(),
    };
    let midnight = |day: NaiveDate| {
        day.and_hms_opt(0, 0, 0)
            .and_then(|naive| naive.and_local_timezone(Local).earliest())
            .context("the local day has no midnight")
            .map(|moment| moment.timestamp_millis())
    };
    let start = midnight(day)?;
    let end = midnight(day.succ_opt().context("the calendar has no next day")?)?;
    Ok((day.format("%Y-%m-%d").to_string(), start, end))
}

/// One project's share of a day's work.
#[derive(Default)]
struct ProjectDay {
    sessions: Vec<Value>,
    files: Vec<String>,
    commits: Vec<String>,
}

fn project_name(workspace: &str) -> String {
    Path::new(workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(workspace)
        .to_owned()
}

fn sessions(root: &Path, start: i64, end: i64) -> Result<Vec<Value>> {
    let conn = crate::server::agent_service::open_db(root)?;
    let mut statement = conn.prepare(
        "SELECT t.id, COALESCE(m.title, t.prompt), t.status, t.created_at, COALESCE(m.workspace, '')
         FROM agent_tasks t LEFT JOIN agent_session_meta m ON m.task_id = t.id
         WHERE t.created_at >= ?1 AND t.created_at < ?2
         ORDER BY t.created_at ASC",
    )?;
    let rows = statement.query_map([start, end], |row| {
        Ok(json!({
            "task_id": row.get::<_, String>(0)?,
            "title": row.get::<_, String>(1)?,
            "status": row.get::<_, String>(2)?,
            "created_at": row.get::<_, i64>(3)?,
            "workspace": row.get::<_, String>(4)?,
        }))
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn changed_files(root: &Path, start: i64, end: i64) -> Result<Vec<Value>> {
    let conn = crate::server::agent_service::open_db(root)?;
    let mut statement = conn.prepare(
        "SELECT workspace, path FROM agent_change_ledger WHERE created_at >= ?1 AND created_at < ?2 GROUP BY workspace, path ORDER BY workspace, path",
    )?;
    let rows = statement.query_map([start, end], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows
        .filter_map(Result::ok)
        .map(|(workspace, path)| json!({ "workspace": workspace, "path": path }))
        .collect())
}

/// Commits made today, or nothing when the project is not a git checkout.
fn commits(workspace: &str, date: &str) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args([
            "log",
            &format!("--since={date} 00:00:00"),
            "--pretty=%h %s",
            &format!("-n{COMMIT_LIMIT}"),
        ])
        .current_dir(workspace)
        .output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Aggregate one local day into per-project facts. A failing source is reported
/// as empty rather than failing the whole page: a report without git is still
/// worth writing.
pub(super) fn today(state_root: &Path, date: Option<&str>) -> Result<Value> {
    let (day, start, end) = day_bounds(date)?;
    let mut grouped: BTreeMap<String, ProjectDay> = BTreeMap::new();
    let mut warnings: Vec<String> = Vec::new();

    match sessions(state_root, start, end) {
        Ok(rows) => {
            for row in rows {
                let workspace = row["workspace"].as_str().unwrap_or_default().to_owned();
                let entry = grouped.entry(workspace).or_default();
                // The task id lets the model follow a session into
                // session.read_session_history instead of guessing at details.
                entry.sessions.push(json!({
                    "task_id": row["task_id"],
                    "title": row["title"],
                    "status": row["status"],
                    "created_at": row["created_at"],
                }));
            }
        }
        Err(error) => warnings.push(format!("sessions unavailable: {error}")),
    }
    match changed_files(state_root, start, end) {
        Ok(rows) => {
            for row in rows {
                let workspace = row["workspace"].as_str().unwrap_or_default().to_owned();
                let path = row["path"].as_str().unwrap_or_default().to_owned();
                let entry = grouped.entry(workspace).or_default();
                if entry.files.len() < FILE_LIMIT && !entry.files.contains(&path) {
                    entry.files.push(path);
                }
            }
        }
        Err(error) => warnings.push(format!("changed files unavailable: {error}")),
    }

    let projects = grouped
        .into_iter()
        .map(|(workspace, mut day_work)| {
            if !workspace.is_empty() && Path::new(&workspace).is_dir() {
                day_work.commits = commits(&workspace, &day);
            }
            json!({
                "project": if workspace.is_empty() { "未归属项目".to_owned() } else { project_name(&workspace) },
                "workspace": workspace,
                "sessions": day_work.sessions,
                "files": day_work.files,
                "commits": day_work.commits,
            })
        })
        .collect::<Vec<_>>();

    let mut total_sessions = 0;
    let mut total_files = 0;
    let mut total_commits = 0;
    for project in &projects {
        total_sessions += project["sessions"].as_array().map(Vec::len).unwrap_or_default();
        total_files += project["files"].as_array().map(Vec::len).unwrap_or_default();
        total_commits += project["commits"].as_array().map(Vec::len).unwrap_or_default();
    }
    Ok(json!({
        "date": day,
        "projects": projects,
        "totals": { "sessions": total_sessions, "files": total_files, "commits": total_commits },
        "warnings": warnings,
    }))
}
