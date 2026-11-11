//! "Ask before running" permission: file edits proceed, but commands, scripts
//! and the browser wait for the user's decision in the conversation. A user
//! can allow once, for the rest of the session, or for the project for good,
//! so the question comes up rarely.
use crate::agent_service::{self, AgentServiceState, PermissionMode};
use anyhow::{Result, bail};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path as FsPath, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const POLL: Duration = Duration::from_millis(400);
const DECISIONS: [&str; 4] = ["once", "session", "project", "deny"];

fn open(root: &FsPath) -> Result<Connection> {
    let conn = agent_service::open_db(root)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tool_approvals(task_id TEXT NOT NULL,call_id TEXT NOT NULL,decision TEXT NOT NULL,PRIMARY KEY(task_id,call_id));
         CREATE TABLE IF NOT EXISTS tool_approval_grants(scope TEXT PRIMARY KEY);
         CREATE TABLE IF NOT EXISTS tool_approval_pending(task_id TEXT NOT NULL,call_id TEXT NOT NULL,PRIMARY KEY(task_id,call_id));",
    )?;
    Ok(conn)
}

/// Tools that "ask" mode runs only after approval: exactly those that
/// workspace-write mode would refuse, i.e. process execution and the browser.
pub fn needs_approval(
    mode: PermissionMode,
    name: &str,
    catalog: Option<&crate::builtin_plugins::ToolCatalog>,
    ban_execution: bool,
) -> bool {
    if crate::shell_process::is_tool(name) {
        return name != "kill_session" && (
            (cfg!(windows) && (ban_execution || !matches!(mode, PermissionMode::FullAccess)))
            || (!cfg!(windows) && matches!(mode, PermissionMode::Ask | PermissionMode::AutoReview))
        );
    }
    matches!(mode, PermissionMode::Ask | PermissionMode::AutoReview)
        && (crate::project_process::is_execution(name) || !(PermissionMode::WorkspaceWrite.allows(name)
            && catalog.is_none_or(|c| {
                c.packages
                    .allows(name, PermissionMode::WorkspaceWrite, ban_execution)
            })))
}

fn grant_scopes(task: &str, project: Option<&FsPath>) -> Vec<String> {
    let mut scopes = vec![format!("task:{task}")];
    if let Some(project) = project {
        scopes.push(format!("project:{}", project.display()));
    }
    scopes
}

fn granted(conn: &Connection, scopes: &[String]) -> Result<bool> {
    for scope in scopes {
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM tool_approval_grants WHERE scope=?1",
                [scope],
                |r| r.get(0),
            )
            .optional()?;
        if found.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn append(root: &FsPath, task: &str, kind: &str, data: &Value) -> Result<()> {
    let mut conn = agent_service::open_db(root)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    agent_service::append_event_tx(&tx, task, kind, data, Some("append"))?;
    tx.commit()?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutsideKind {
    Read,
    Write,
}

fn resolve_for_grant(raw: &FsPath) -> (PathBuf, PathBuf) {
    let mut cur = raw.to_path_buf();
    let mut tail = Vec::new();
    while !cur.exists() {
        let Some(name) = cur.file_name() else {
            break;
        };
        tail.push(name.to_os_string());
        let Some(parent) = cur.parent() else {
            break;
        };
        cur = parent.to_path_buf();
    }
    let canon_existing = cur.canonicalize().unwrap_or(cur);
    let mut resolved = canon_existing.clone();
    for part in tail.iter().rev() {
        resolved.push(part);
    }
    let base_dir = if canon_existing.is_dir() {
        canon_existing
    } else {
        canon_existing
            .parent()
            .map(FsPath::to_path_buf)
            .unwrap_or(canon_existing)
    };
    (resolved, base_dir)
}

/// Computes the canonical scope target (`dir:<repo_or_folder>` or `file:<path>`)
/// for an outside-workspace path so approving one project never unlocks another.
pub fn approval_grant_target(path: &FsPath) -> String {
    let (resolved, base_dir) = resolve_for_grant(path);
    let mut walk = Some(base_dir.as_path());
    while let Some(dir) = walk {
        if crate::process_sandbox::is_allowed_extra_root(dir) && dir.join(".git").exists() {
            return format!("dir:{}", dir.display());
        }
        walk = dir.parent();
    }
    if crate::process_sandbox::is_allowed_extra_root(&base_dir) {
        return format!("dir:{}", base_dir.display());
    }
    format!("file:{}", resolved.display())
}

fn candidate_outside_scopes(
    kind: OutsideKind,
    raw_path: &str,
    base_scopes: &[String],
) -> Vec<String> {
    let path = FsPath::new(raw_path);
    let target = approval_grant_target(path);
    let (_resolved, base_dir) = resolve_for_grant(path);
    let prefixes: &[&str] = match kind {
        OutsideKind::Read => &["outside-read", "outside-write"],
        OutsideKind::Write => &["outside-write"],
    };
    let mut candidates = Vec::new();
    for prefix in prefixes {
        for scope in base_scopes {
            candidates.push(format!("{prefix}:{target}:{scope}"));
        }
    }
    let mut walk = Some(base_dir.as_path());
    while let Some(dir) = walk {
        if crate::process_sandbox::is_allowed_extra_root(dir) {
            for prefix in prefixes {
                for scope in base_scopes {
                    let cand = format!("{prefix}:dir:{}:{scope}", dir.display());
                    if !candidates.contains(&cand) {
                        candidates.push(cand);
                    }
                }
            }
        }
        walk = dir.parent();
    }
    candidates
}

fn outside_all_granted(
    conn: &Connection,
    kind: OutsideKind,
    paths: &[String],
    base_scopes: &[String],
) -> Result<bool> {
    if paths.is_empty() {
        return Ok(false);
    }
    for raw in paths {
        let candidates = candidate_outside_scopes(kind, raw, base_scopes);
        if !granted(conn, &candidates)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Returns canonical directories granted `outside-write` for this task or project,
/// so `bwrap` can bind-mount approved related repositories without opening the host.
pub fn approved_writable_roots(host_root: &FsPath, task: &str) -> Result<Vec<PathBuf>> {
    let conn = open(host_root)?;
    let project = crate::project_notes::task_project(host_root, task)?;
    let suffixes = grant_scopes(task, project.as_deref());
    let mut stmt =
        conn.prepare("SELECT scope FROM tool_approval_grants WHERE scope LIKE 'outside-write:dir:%'")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut roots = Vec::new();
    for row in rows {
        let scope = row?;
        let Some(rest) = scope.strip_prefix("outside-write:dir:") else {
            continue;
        };
        for suffix in &suffixes {
            let tail = format!(":{suffix}");
            if let Some(dir_str) = rest.strip_suffix(&tail)
                && let Ok(canon) = PathBuf::from(dir_str).canonicalize()
                && crate::process_sandbox::is_allowed_extra_root(&canon)
                && !roots.contains(&canon)
            {
                roots.push(canon);
            }
        }
    }
    Ok(roots)
}

/// Block a tool call until the user allows it, or fail it when they decline.
#[allow(clippy::too_many_arguments)]
pub async fn wait(
    state: &AgentServiceState,
    task: &str,
    model: &str,
    agent_id: Option<&str>,
    call_id: &str,
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<()> {
    wait_with(
        state,
        task,
        model,
        agent_id,
        call_id,
        name,
        args,
        cancel,
        None,
    )
    .await
}

/// A read of paths outside the workspace waits for the user unless the
/// permission mode is full access. Its grants are scoped per target directory/file.
#[allow(clippy::too_many_arguments)]
pub async fn wait_outside_read(
    state: &AgentServiceState,
    task: &str,
    model: &str,
    agent_id: Option<&str>,
    call_id: &str,
    name: &str,
    args: &Value,
    paths: &[String],
    cancel: &CancellationToken,
) -> Result<()> {
    wait_with(
        state,
        task,
        model,
        agent_id,
        call_id,
        name,
        args,
        cancel,
        Some((OutsideKind::Read, paths)),
    )
    .await
}

/// Editing files or running commands/scripts in a directory outside the workspace
/// waits for explicit user confirmation unless the mode is full access.
#[allow(clippy::too_many_arguments)]
pub async fn wait_outside_write(
    state: &AgentServiceState,
    task: &str,
    model: &str,
    agent_id: Option<&str>,
    call_id: &str,
    name: &str,
    args: &Value,
    paths: &[String],
    cancel: &CancellationToken,
) -> Result<()> {
    wait_with(
        state,
        task,
        model,
        agent_id,
        call_id,
        name,
        args,
        cancel,
        Some((OutsideKind::Write, paths)),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn wait_with(
    state: &AgentServiceState,
    task: &str,
    model: &str,
    agent_id: Option<&str>,
    call_id: &str,
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
    outside: Option<(OutsideKind, &[String])>,
) -> Result<()> {
    let root = state.workspace.root().to_path_buf();
    let project = crate::project_notes::task_project(&root, task)?;
    let base_scopes = grant_scopes(task, project.as_deref());
    let outside_kind = outside.map(|(k, _)| k);
    let outside_paths = outside.map(|(_, p)| p);
    let scopes_to_check: Vec<String> = if outside.is_none() {
        if crate::shell_process::is_tool(name) {
            base_scopes
                .iter()
                .map(|scope| format!("shell:{scope}"))
                .collect()
        } else {
            base_scopes.clone()
        }
    } else {
        Vec::new()
    };
    {
        let conn = open(&root)?;
        if let Some((kind, paths)) = outside {
            if outside_all_granted(&conn, kind, paths, &base_scopes)? {
                return Ok(());
            }
        } else if granted(&conn, &scopes_to_check)? {
            return Ok(());
        }
    }
    // "Approve for me": a clear allow or deny needs no card for in-workspace actions;
    // outside-write always requires human confirmation even if AI reviewer says Allow.
    let mut review_reason = None;
    if matches!(state.permission_mode, PermissionMode::AutoReview) {
        let verdict =
            crate::approval_review::review(state, task, model, name, args, outside_paths, cancel)
                .await;
        let (mut decision, mut reason) = match verdict {
            Ok(crate::approval_review::Verdict::Allow(reason)) => ("allow", reason),
            Ok(crate::approval_review::Verdict::Deny(reason)) => ("deny", reason),
            Ok(crate::approval_review::Verdict::Ask(reason)) => ("ask", reason),
            Err(error) => ("ask", format!("审查没有完成：{error:#}")),
        };
        if outside_kind == Some(OutsideKind::Write) && decision == "allow" {
            decision = "ask";
            reason = format!("跨项目写入需人工确认（AI 审查意见：{reason}）");
        }
        append(
            &root,
            task,
            "approval/review",
            &json!({"call_id":call_id,"tool":name,"decision":decision,"reason":reason,"agent_id":agent_id,"outside_paths":outside_paths,"outside_write":outside_kind == Some(OutsideKind::Write)}),
        )?;
        match decision {
            "allow" => return Ok(()),
            "deny" => bail!(
                "AI_REVIEW_DENIED: the approval reviewer declined {name}: {reason}. Do not retry it or work around it. Stop and tell the user what you need so they can allow it or choose another approach."
            ),
            _ => review_reason = Some(reason),
        }
    }
    let mut detail = args.to_string();
    if detail.len() > 2_000 {
        detail = format!("{}…", detail.chars().take(2_000).collect::<String>());
    }
    open(&root)?.execute(
        "INSERT OR IGNORE INTO tool_approval_pending VALUES (?1,?2)",
        params![task, call_id],
    )?;
    append(
        &root,
        task,
        "approval/request",
        &json!({"call_id":call_id,"tool":name,"arguments":detail,"agent_id":agent_id,
        "project":project,
        "full_arguments":crate::shell_process::is_tool(name).then_some(args),
        "unsandboxed":crate::shell_process::is_tool(name) && cfg!(windows),
        "shell":crate::shell_process::is_tool(name).then_some(if cfg!(windows) {"PowerShell 7"} else {"Bash"}),
        "outside_paths":outside_paths,
        "outside_write":outside_kind == Some(OutsideKind::Write),
        "review_reason":review_reason}),
    )?;
    let waited: Result<String> = async {
        loop {
            if cancel.is_cancelled() {
                append(
                    &root,
                    task,
                    "approval/decision",
                    &json!({"call_id":call_id,"decision":"cancelled"}),
                )?;
                bail!("cancelled");
            }
            let decision: Option<String> = open(&root)?
                .query_row(
                    "SELECT decision FROM tool_approvals WHERE task_id=?1 AND call_id=?2",
                    params![task, call_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(decision) = decision {
                return Ok(decision);
            }
            // Another call in this session may have just granted a wider scope.
            let conn = open(&root)?;
            let now_granted = if let Some((kind, paths)) = outside {
                outside_all_granted(&conn, kind, paths, &base_scopes)?
            } else {
                granted(&conn, &scopes_to_check)?
            };
            if now_granted {
                return Ok("granted".to_owned());
            }
            tokio::time::sleep(POLL).await;
        }
    }
    .await;
    // Cleared on every exit, so the task list never shows a stale "awaiting approval".
    open(&root)?.execute(
        "DELETE FROM tool_approval_pending WHERE task_id=?1 AND call_id=?2",
        params![task, call_id],
    )?;
    let decision = waited?;
    let conn = open(&root)?;
    let chosen_base = match decision.as_str() {
        "session" => Some(&base_scopes[0]),
        "project" => base_scopes.last(),
        _ => None,
    };
    if let Some(base) = chosen_base {
        if let Some((kind, paths)) = outside {
            let prefix = match kind {
                OutsideKind::Read => "outside-read",
                OutsideKind::Write => "outside-write",
            };
            for raw in paths {
                let target = approval_grant_target(FsPath::new(raw));
                let scope = format!("{prefix}:{target}:{base}");
                conn.execute(
                    "INSERT OR IGNORE INTO tool_approval_grants(scope) VALUES (?1)",
                    [&scope],
                )?;
            }
        } else {
            let scope = if crate::shell_process::is_tool(name) {
                format!("shell:{base}")
            } else {
                base.clone()
            };
            conn.execute(
                "INSERT OR IGNORE INTO tool_approval_grants(scope) VALUES (?1)",
                [&scope],
            )?;
        }
    }
    append(
        &root,
        task,
        "approval/decision",
        &json!({"call_id":call_id,"decision":decision}),
    )?;
    if decision == "deny"
        && let Some((kind, paths)) = outside
    {
        match kind {
            OutsideKind::Read => bail!(
                "USER_DENIED: the user declined reading {} outside the workspace. Do not retry it or work around it. Stop and tell the user what you need from those paths.",
                paths.join(", ")
            ),
            OutsideKind::Write => bail!(
                "USER_DENIED: the user declined modifying or executing in {} outside the workspace. Do not retry it or work around it. Stop and tell the user what changes are needed in that directory.",
                paths.join(", ")
            ),
        }
    }
    if decision == "deny" {
        bail!(
            "USER_DENIED: the user declined running {name}. Do not retry it or work around it. Stop and tell the user what they need to run or allow."
        );
    }
    Ok(())
}

/// Whether the task is paused on an approval card (for the task list).
pub fn awaiting(conn: &Connection, task: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM tool_approval_pending WHERE task_id=?1)",
        [task],
        |r| r.get(0),
    )
    .unwrap_or(false)
}

#[derive(Deserialize)]
pub struct DecisionInput {
    call_id: String,
    decision: String,
}
pub async fn decide(
    State(state): State<AgentServiceState>,
    Path(task): Path<String>,
    Json(input): Json<DecisionInput>,
) -> Response {
    let result = (|| {
        anyhow::ensure!(
            DECISIONS.contains(&input.decision.as_str()),
            "未知的审批选项"
        );
        open(state.workspace.root())?.execute(
            "INSERT OR IGNORE INTO tool_approvals(task_id,call_id,decision) VALUES (?1,?2,?3)",
            params![task, input.call_id, input.decision],
        )?;
        Ok(json!({"ok":true}))
    })();
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::CONFLICT,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_review_reviews_exactly_what_ask_mode_would_ask() {
        for name in ["exec_command", "write_stdin", "kill_session", "run_program", "run_project_script", "browser_open", "browser_close", "write_file", "read_file", "search_text"] {
            for ban in [false, true] {
                assert_eq!(needs_approval(PermissionMode::AutoReview, name, None, ban), needs_approval(PermissionMode::Ask, name, None, ban), "{name}");
            }
            assert_eq!(PermissionMode::AutoReview.allows(name), PermissionMode::Ask.allows(name), "{name}");
        }
        assert_eq!(serde_json::to_value(PermissionMode::AutoReview).unwrap(), "auto_review");
    }

    #[test]
    fn shell_approvals_reflect_missing_windows_sandbox_and_keep_cleanup_free() {
        for mode in [PermissionMode::ReadOnly, PermissionMode::WorkspaceWrite, PermissionMode::Ask, PermissionMode::FullAccess] {
            assert!(mode.allows("exec_command"));
            assert_eq!(needs_approval(mode,"exec_command",None,false), if cfg!(windows) {
                !matches!(mode,PermissionMode::FullAccess)
            } else { matches!(mode,PermissionMode::Ask) });
            assert_eq!(needs_approval(mode,"exec_command",None,true), cfg!(windows) || matches!(mode,PermissionMode::Ask));
            assert!(!needs_approval(mode,"kill_session",None,true));
        }
    }

    #[tokio::test]
    async fn shell_does_not_reuse_browser_grants_and_shows_the_complete_command() -> Result<()> {
        let root = std::env::temp_dir().join(format!("shell-approval-{}",agent_service::uuid_like()));
        std::fs::create_dir_all(&root)?;
        let state = agent_service::tests::test_state(&root,"127.0.0.1:9".parse()?);
        open(&root)?.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','','fake','running',0,0)",[])?;
        open(&root)?.execute("INSERT INTO tool_approval_grants(scope) VALUES ('task:t')",[])?;
        let command = format!("echo '{}tail'","x".repeat(2500));
        let args = json!({"cmd":command,"workdir":"."});
        let cancel = CancellationToken::new();
        let waiter = wait(&state,"t","fake",None,"shell-call","exec_command",&args,&cancel);
        let decide = async {
            for _ in 0..50 {
                let conn = open(&root)?;
                let pending: Option<String> = conn.query_row("SELECT data FROM agent_task_events WHERE task_id='t' AND kind='approval/request'",[],|r|r.get(0)).optional()?;
                if let Some(raw) = pending {
                    let event:Value = serde_json::from_str(&raw)?;
                    assert_eq!(event["full_arguments"]["cmd"],command);
                    assert_eq!(event["unsandboxed"],cfg!(windows));
                    conn.execute("INSERT INTO tool_approvals VALUES ('t','shell-call','session')",[])?;
                    return Ok::<_,anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            bail!("shell wrongly reused the existing generic approval grant")
        };
        let (a,b) = tokio::join!(waiter,decide);
        a?; b?;
        assert!(granted(&open(&root)?,&["shell:task:t".into()])?);
        Ok(())
    }

    #[test]
    fn ask_mode_gates_only_execution() {
        assert!(needs_approval(
            PermissionMode::Ask,
            "run_program",
            None,
            false
        ));
        assert!(needs_approval(
            PermissionMode::Ask,
            "browser_open",
            None,
            false
        ));
        assert!(!needs_approval(
            PermissionMode::Ask,
            "write_file",
            None,
            false
        ));
        assert!(!needs_approval(
            PermissionMode::Ask,
            "read_file",
            None,
            false
        ));
        assert!(!needs_approval(
            PermissionMode::FullAccess,
            "run_program",
            None,
            false
        ));
        assert!(PermissionMode::Ask.allows("run_program"));
        assert_eq!(PermissionMode::WorkspaceWrite.allows("run_program"), cfg!(target_os = "linux"));
    }

    #[tokio::test]
    async fn decisions_unblock_and_session_grants_skip_later_questions() {
        let root = std::env::temp_dir().join(format!("approvals-{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let state = agent_service::tests::test_state(&root, "127.0.0.1:9".parse().unwrap());
        agent_service::open_db(&root).unwrap().execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','','fake','running',0,0)",[]).unwrap();
        let cancel = CancellationToken::new();
        let decide_later = |call: &'static str, decision: &'static str| {
            let root = root.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                open(&root)
                    .unwrap()
                    .execute(
                        "INSERT INTO tool_approvals VALUES ('t',?1,?2)",
                        params![call, decision],
                    )
                    .unwrap();
            })
        };
        decide_later("c1", "deny");
        let denied = wait(&state, "t", "fake", None, "c1", "run_program", &json!({}), &cancel)
            .await
            .unwrap_err();
        assert!(denied.to_string().contains("USER_DENIED"), "{denied:#}");
        decide_later("c2", "session");
        wait(&state, "t", "fake", None, "c2", "run_program", &json!({}), &cancel)
            .await
            .unwrap();
        assert!(!awaiting(&open(&root).unwrap(), "t"));
        // Granted for the session: no question, returns at once.
        tokio::time::timeout(
            Duration::from_millis(50),
            wait(&state, "t", "fake", None, "c3", "run_program", &json!({}), &cancel),
        )
        .await
        .unwrap()
        .unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn outside_write_scopes_grants_to_target_repo_and_populates_writable_roots() {
        let base = std::env::temp_dir().join(format!(
            "outside-approvals-{}",
            agent_service::uuid_like()
        ));
        let root = base.join("work");
        let repo_b = base.join("repo-b");
        let repo_c = base.join("repo-c");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(repo_b.join(".git")).unwrap();
        std::fs::create_dir_all(repo_b.join("src")).unwrap();
        std::fs::create_dir_all(repo_c.join(".git")).unwrap();
        let state = agent_service::tests::test_state(&root, "127.0.0.1:9".parse().unwrap());
        agent_service::open_db(&root)
            .unwrap()
            .execute(
                "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','','fake','running',0,0)",
                [],
            )
            .unwrap();
        let cancel = CancellationToken::new();
        let root_clone = root.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            open(&root_clone)
                .unwrap()
                .execute(
                    "INSERT INTO tool_approvals VALUES ('t','w1','session')",
                    [],
                )
                .unwrap();
        });
        let b_file = repo_b.join("src/new_api.rs").to_string_lossy().into_owned();
        wait_outside_write(
            &state,
            "t",
            "fake",
            None,
            "w1",
            "write_file",
            &json!({"path": b_file}),
            std::slice::from_ref(&b_file),
            &cancel,
        )
        .await
        .unwrap();
        let roots = approved_writable_roots(&root, "t").unwrap();
        assert_eq!(roots, vec![repo_b.canonicalize().unwrap()]);
        // Another file inside repo_b (and reading repo_b) passes immediately without waiting.
        let b_other = repo_b.join("README.md").to_string_lossy().into_owned();
        tokio::time::timeout(
            Duration::from_millis(50),
            wait_outside_write(
                &state,
                "t",
                "fake",
                None,
                "w2",
                "edit_file",
                &json!({"path": b_other}),
                std::slice::from_ref(&b_other),
                &cancel,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(
            Duration::from_millis(50),
            wait_outside_read(
                &state,
                "t",
                "fake",
                None,
                "r1",
                "read_file",
                &json!({"path": b_other}),
                std::slice::from_ref(&b_other),
                &cancel,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        // Unrelated repo_c is NOT unlocked by repo_b's grant.
        let c_file = repo_c.join("lib.rs").to_string_lossy().into_owned();
        let ungranted = tokio::time::timeout(
            Duration::from_millis(80),
            wait_outside_write(
                &state,
                "t",
                "fake",
                None,
                "w3",
                "write_file",
                &json!({"path": c_file}),
                std::slice::from_ref(&c_file),
                &cancel,
            ),
        )
        .await;
        assert!(ungranted.is_err(), "repo_c must still require its own approval");
        std::fs::remove_dir_all(&base).unwrap();
    }
}
