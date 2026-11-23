use std::{
    collections::BTreeMap,
    future::Future,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::server::{
    builtin_plugins::ToolHandler,
    plugin_protocol::{NativeContributions, NativeTool, ToolPermission},
    skill_packages::{self, Package},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{
    client,
    credentials::Secret,
    live::{self, DisplayFilter},
    sftp::Sftp,
    store::{Action, EnvTier, Server, Store},
};

const MAX_AI_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
struct SshToolContext {
    task_id: Option<String>,
    approved_protected: bool,
}

tokio::task_local! {
    static SSH_TOOL_CONTEXT: SshToolContext;
}

pub(crate) fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "ssh__list_servers"
            | "ssh__server_history"
            | "ssh__exec"
            | "ssh__list_dir"
            | "ssh__read_file"
            | "ssh__write_file"
            | "ssh__upload"
            | "ssh__tail_log"
    )
}

pub(crate) fn is_mutating_tool(name: &str) -> bool {
    matches!(name, "ssh__exec" | "ssh__write_file" | "ssh__upload")
}

pub(crate) fn requires_approval(name: &str, args: &Value) -> bool {
    requires_approval_in(&crate::server::ai_proxy::config_dir(), name, args)
}

pub(super) fn requires_approval_in(config_dir: &Path, name: &str, args: &Value) -> bool {
    if !is_mutating_tool(name) {
        return false;
    }
    let Some(server_id) = args["server_id"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    match Store::open(config_dir).and_then(|store| store.get(server_id)) {
        Ok(Some(server)) => server.env_tier == EnvTier::Prod || server.require_approval,
        Ok(None) => false,
        Err(_) => true,
    }
}

pub(crate) async fn scoped<F: Future>(
    task_id: String,
    approved_protected: bool,
    f: F,
) -> F::Output {
    SSH_TOOL_CONTEXT
        .scope(
            SshToolContext {
                task_id: Some(task_id),
                approved_protected,
            },
            f,
        )
        .await
}

fn current_context() -> SshToolContext {
    SSH_TOOL_CONTEXT.try_with(Clone::clone).unwrap_or_default()
}

pub(crate) fn package() -> Result<Package> {
    let handler: ToolHandler = Arc::new(|workspace, name, args| {
        Box::pin(async move {
            let config_dir = crate::server::ai_proxy::config_dir();
            let workspace_root = workspace.root().to_path_buf();
            execute_tool(config_dir, workspace_root, &name, args).await
        })
    });
    let definitions: Vec<Value> = serde_json::from_str(include_str!("tools.json"))?;
    let tools = definitions
        .into_iter()
        .map(|definition| {
            let name = definition["name"]
                .as_str()
                .context("SSH tool has no name")?
                .to_owned();
            let permission = if is_mutating_tool(&name) {
                ToolPermission::Execution
            } else {
                ToolPermission::ReadOnly
            };
            Ok((
                name,
                NativeTool {
                    definition,
                    handler: handler.clone(),
                    permission,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let root = std::env::var_os("GROW_RESOURCES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    skill_packages::load_builtin(
        &root.join("src/server/builtin_plugins/ssh"),
        Arc::new(NativeContributions {
            tools,
            frontend_entries: vec!["ssh.workbench".into()],
            on_unmount: None,
        }),
    )
}

pub(super) async fn execute_tool(
    config_dir: PathBuf,
    workspace_root: PathBuf,
    name: &str,
    args: Value,
) -> Result<Value> {
    let ctx = current_context();
    let workspace_str = workspace_root.to_string_lossy().into_owned();
    match name {
        "ssh__list_servers" => {
            tokio::task::spawn_blocking(move || list_servers(&config_dir, &workspace_str)).await?
        }
        "ssh__server_history" => {
            tokio::task::spawn_blocking(move || server_history(&config_dir, &workspace_str, args))
                .await?
        }
        "ssh__list_dir" => {
            let server = load_server(&config_dir, &args, false, ctx.approved_protected).await?;
            let path = args["path"]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .or(server.default_dir.as_deref())
                .unwrap_or(".")
                .to_owned();
            let limit = args["limit"].as_u64().unwrap_or(100).clamp(1, 200) as usize;
            let (connection, secret) =
                client::connect_for_tool(config_dir, server.id.clone(), ctx.task_id.is_some())
                    .await?;
            let sftp = Sftp::open(connection).await?;
            let (resolved, mut entries) = sftp.list(&path).await?;
            if ctx.task_id.is_some() {
                emit_sftp(
                    &ctx,
                    &server,
                    secret.as_ref(),
                    "list_dir",
                    &resolved,
                    |path| format!("list {path}"),
                );
            }
            let total = entries.len();
            let truncated = total > limit;
            entries.truncate(limit);
            Ok(json!({
                "server_id": server.id,
                "path": resolved,
                "total": total,
                "truncated": truncated,
                "entries": entries,
            }))
        }
        "ssh__read_file" => {
            let server = load_server(&config_dir, &args, false, ctx.approved_protected).await?;
            let path = required_str(&args, "path")?;
            let start_line = args["start_line"].as_u64().unwrap_or(1).max(1) as usize;
            let end_line = args["end_line"].as_u64().map(|value| value as usize);
            let max_chars = args["max_chars"]
                .as_u64()
                .unwrap_or(16_384)
                .clamp(256, 65_536) as usize;
            let (connection, secret) = client::connect_for_tool(
                config_dir.clone(),
                server.id.clone(),
                ctx.task_id.is_some(),
            )
            .await?;
            let sftp = Sftp::open(connection).await?;
            let mut result = sftp
                .read_lines(&path, start_line, end_line, max_chars)
                .await?;
            record_usage(
                config_dir,
                server.id.clone(),
                Action::ReadFile,
                ctx.task_id.clone(),
                workspace_str,
                Some(path.clone()),
            )
            .await?;
            if ctx.task_id.is_some() {
                let resolved = sftp.display_path(&path).await;
                emit_sftp(
                    &ctx,
                    &server,
                    secret.as_ref(),
                    "read_file",
                    &resolved,
                    |path| format!("read {path}"),
                );
            }
            if let Some(obj) = result.as_object_mut() {
                obj.insert("server_id".into(), json!(server.id));
            }
            Ok(result)
        }
        "ssh__tail_log" => {
            let server = load_server(&config_dir, &args, false, ctx.approved_protected).await?;
            let path = required_str(&args, "path")?;
            let lines = args["lines"].as_u64().unwrap_or(100).clamp(1, 500) as usize;
            let filter = args["filter"].as_str().map(str::to_owned);
            let max_chars = args["max_chars"]
                .as_u64()
                .unwrap_or(24_000)
                .clamp(256, 65_536) as usize;
            let (connection, secret) = client::connect_for_tool(
                config_dir.clone(),
                server.id.clone(),
                ctx.task_id.is_some(),
            )
            .await?;
            let sftp = Sftp::open(connection).await?;
            let mut result = sftp
                .tail_lines(&path, lines, filter.as_deref(), max_chars)
                .await?;
            record_usage(
                config_dir,
                server.id.clone(),
                Action::ReadFile,
                ctx.task_id.clone(),
                workspace_str,
                Some(path.clone()),
            )
            .await?;
            if ctx.task_id.is_some() {
                let resolved = sftp.display_path(&path).await;
                emit_sftp(
                    &ctx,
                    &server,
                    secret.as_ref(),
                    "tail_log",
                    &resolved,
                    |path| format!("tail {path}"),
                );
            }
            if let Some(obj) = result.as_object_mut() {
                obj.insert("server_id".into(), json!(server.id));
            }
            Ok(result)
        }
        "ssh__exec" => {
            let server = load_server(&config_dir, &args, true, ctx.approved_protected).await?;
            let command = required_str(&args, "command")?;
            let cwd = args["cwd"]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .or_else(|| server.default_dir.clone());
            let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(60).clamp(1, 300);
            let (connection, secret) = client::connect_for_tool(
                config_dir.clone(),
                server.id.clone(),
                ctx.task_id.is_some(),
            )
            .await?;
            let task = ctx.task_id.as_deref();
            let secret = secret.as_ref().map(secret_bytes);
            let display_cwd =
                task.and_then(|_| cwd.as_deref().map(|dir| live::redact_text(dir, secret)));
            let (mut stdout, mut stderr) = (DisplayFilter::default(), DisplayFilter::default());
            let start = Instant::now();
            if let Some(task) = task {
                live::emit_exec_start(
                    task,
                    &server.id,
                    display_cwd.as_deref(),
                    &live::redact_text(&command, secret),
                );
            }
            let mut callback = |is_stderr, chunk: &[u8]| {
                if let Some(task) = task {
                    let filter = if is_stderr { &mut stderr } else { &mut stdout };
                    live::emit_exec_chunk(task, &server.id, &filter.push(chunk, secret));
                }
            };
            let output = client::exec(
                &connection,
                &command,
                cwd.as_deref(),
                Duration::from_secs(timeout_secs),
                task.map(|_| &mut callback as &mut client::ChunkCallback<'_>),
            )
            .await;
            let elapsed_ms = start.elapsed().as_millis();
            let recorded = if output.is_ok() {
                record_usage(
                    config_dir,
                    server.id.clone(),
                    Action::Exec,
                    ctx.task_id.clone(),
                    workspace_str,
                    cwd.clone(),
                )
                .await
            } else {
                Ok(())
            };
            if let Some(task) = task {
                live::emit_exec_chunk(task, &server.id, &stdout.finish());
                live::emit_exec_chunk(task, &server.id, &stderr.finish());
                let code = output.as_ref().ok().and_then(|output| output.exit_code);
                live::emit_exec_end(task, &server.id, display_cwd, code, elapsed_ms);
            }
            let output = output?;
            recorded?;
            Ok(json!({
                "server_id": server.id,
                "cwd": cwd,
                "exit_code": output.exit_code,
                "stdout": output.stdout,
                "stderr": output.stderr,
                "stdout_truncated": output.stdout_truncated,
                "stderr_truncated": output.stderr_truncated,
            }))
        }
        "ssh__write_file" => {
            let server = load_server(&config_dir, &args, true, ctx.approved_protected).await?;
            let path = required_str(&args, "path")?;
            let content = args["content"].as_str().context("missing content")?;
            let create_parents = args["create_parents"].as_bool().unwrap_or(false);
            let backup = args["backup"].as_bool().unwrap_or(false);
            let (connection, secret) = client::connect_for_tool(
                config_dir.clone(),
                server.id.clone(),
                ctx.task_id.is_some(),
            )
            .await?;
            let sftp = Sftp::open(connection).await?;
            let bytes_written = sftp
                .write_text_atomic(&path, content, create_parents, backup)
                .await?;
            record_usage(
                config_dir,
                server.id.clone(),
                Action::WriteFile,
                ctx.task_id.clone(),
                workspace_str,
                Some(path.clone()),
            )
            .await?;
            if ctx.task_id.is_some() {
                let resolved = sftp.display_path(&path).await;
                emit_sftp(
                    &ctx,
                    &server,
                    secret.as_ref(),
                    "write_file",
                    &resolved,
                    |path| format!("write {path}"),
                );
            }
            Ok(json!({
                "server_id": server.id,
                "path": path,
                "bytes_written": bytes_written,
                "backup": backup,
            }))
        }
        "ssh__upload" => {
            let server = load_server(&config_dir, &args, true, ctx.approved_protected).await?;
            let local_path = required_str(&args, "local_path")?;
            let remote_path = required_str(&args, "remote_path")?;
            let create_parents = args["create_parents"].as_bool().unwrap_or(false);
            let overwrite = args["overwrite"].as_bool().unwrap_or(false);
            let (std_file, size, relative_display) = tokio::task::spawn_blocking(move || {
                open_workspace_upload_file(&workspace_root, &local_path)
            })
            .await??;
            let file = tokio::fs::File::from_std(std_file);
            let (connection, secret) = client::connect_for_tool(
                config_dir.clone(),
                server.id.clone(),
                ctx.task_id.is_some(),
            )
            .await?;
            let sftp = Sftp::open(connection).await?;
            let bytes_uploaded = sftp
                .upload_with_options(&remote_path, file, size, create_parents, overwrite)
                .await?;
            record_usage(
                config_dir,
                server.id.clone(),
                Action::Upload,
                ctx.task_id.clone(),
                workspace_str,
                Some(remote_path.clone()),
            )
            .await?;
            if ctx.task_id.is_some() {
                let resolved = sftp.display_path(&remote_path).await;
                let local = live::redact_text(&relative_display, secret.as_ref().map(secret_bytes));
                emit_sftp(
                    &ctx,
                    &server,
                    secret.as_ref(),
                    "upload",
                    &resolved,
                    |path| format!("upload {local} -> {path} ({bytes_uploaded} B)"),
                );
            }
            Ok(json!({
                "server_id": server.id,
                "local_path": relative_display,
                "remote_path": remote_path,
                "bytes_uploaded": bytes_uploaded,
            }))
        }
        _ => anyhow::bail!("Unknown SSH tool: {name}"),
    }
}

fn secret_bytes(secret: &Secret) -> &[u8] {
    secret.expose().as_bytes()
}

fn emit_sftp(
    ctx: &SshToolContext,
    server: &Server,
    secret: Option<&Secret>,
    action: &'static str,
    path: &str,
    detail: impl FnOnce(&str) -> String,
) {
    if let Some(task) = ctx.task_id.as_deref() {
        // Broadcast operation metadata only, never file bodies, credential records or auth errors.
        let path = live::redact_text(path, secret.map(secret_bytes));
        let detail = detail(&path);
        live::emit_sftp_event(task, &server.id, action, path, detail);
    }
}

fn required_str(args: &Value, key: &str) -> Result<String> {
    let value = args[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("missing or empty {key}"))?;
    Ok(value.to_owned())
}

async fn load_server(
    config_dir: &Path,
    args: &Value,
    mutating: bool,
    approved_protected: bool,
) -> Result<Server> {
    let server_id = required_str(args, "server_id")?;
    let dir = config_dir.to_path_buf();
    let server = tokio::task::spawn_blocking(move || {
        Store::open(&dir)?
            .get(&server_id)?
            .context("SSH server not found")
    })
    .await??;
    if mutating
        && (server.env_tier == EnvTier::Prod || server.require_approval)
        && !approved_protected
    {
        anyhow::bail!(
            "APPROVAL_REQUIRED: protected SSH server '{}' requires explicit human approval for remote mutation",
            server.name
        );
    }
    Ok(server)
}

async fn record_usage(
    config_dir: PathBuf,
    server_id: String,
    action: Action,
    task_id: Option<String>,
    workspace_root: String,
    remote_path: Option<String>,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        Store::open(&config_dir)?.record_usage(
            &server_id,
            action,
            task_id.as_deref(),
            Some(&workspace_root),
            remote_path.as_deref(),
        )
    })
    .await?
}

fn open_workspace_upload_file(
    workspace_root: &Path,
    local_path: &str,
) -> Result<(std::fs::File, u64, String)> {
    ensure!(
        !local_path.is_empty() && !local_path.contains('\0'),
        "WORKSPACE_BOUNDARY_DENIED: invalid local_path"
    );
    let raw = Path::new(local_path);
    ensure!(
        !raw.components()
            .any(|component| matches!(component, Component::ParentDir)),
        "WORKSPACE_BOUNDARY_DENIED: local_path cannot contain parent traversal (..)"
    );
    let relative = if raw.is_absolute() {
        raw.strip_prefix(workspace_root).map_err(|_| {
            anyhow::anyhow!(
                "WORKSPACE_BOUNDARY_DENIED: local_path must be inside the active workspace"
            )
        })?
    } else {
        raw
    };
    ensure!(
        !relative.components().any(|component| {
            matches!(
                component,
                Component::RootDir | Component::Prefix(_) | Component::ParentDir
            )
        }),
        "WORKSPACE_BOUNDARY_DENIED: local_path must stay inside the active workspace"
    );
    ensure!(
        !relative.components().any(|component| matches!(
            component,
            Component::Normal(name) if name.eq_ignore_ascii_case(".codex-workspace-mcp")
        )),
        "WORKSPACE_BOUNDARY_DENIED: cannot upload internal host state"
    );
    let file = crate::server::file_stream::open_regular(workspace_root, relative).map_err(|err| {
        anyhow::anyhow!(
            "WORKSPACE_BOUNDARY_DENIED: cannot open regular workspace file '{local_path}' without following symlinks: {err}"
        )
    })?;
    let size = file.metadata()?.len();
    ensure!(
        size <= MAX_AI_UPLOAD_BYTES,
        "local file exceeds 512 MiB AI upload limit"
    );
    Ok((file, size, relative.to_string_lossy().into_owned()))
}

fn list_servers(config_dir: &Path, workspace: &str) -> Result<Value> {
    let store = Store::open(config_dir)?;
    let recent = store.recent_in_workspace(workspace)?;
    let rank: BTreeMap<_, _> = recent
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    let mut servers = store.list()?;
    servers.sort_by_key(|server| rank.get(server.id.as_str()).copied().unwrap_or(usize::MAX));
    let mut items = Vec::with_capacity(servers.len());
    for server in servers {
        let recent_usage = store
            .history(&server.id, 3)?
            .into_iter()
            .map(|entry| {
                json!({
                    "task_id": entry.task_id,
                    "workspace_root": entry.workspace_root,
                    "remote_path": entry.remote_path,
                    "action": entry.action,
                    "created_at": entry.created_at,
                })
            })
            .collect::<Vec<_>>();
        items.push(json!({
            "id": server.id,
            "name": server.name,
            "host": server.host,
            "port": server.port,
            "username": server.username,
            "env_tier": server.env_tier,
            "require_approval": server.require_approval,
            "default_dir": server.default_dir,
            "tags": server.tags,
            "notes": server.notes,
            "last_connected_at": server.last_connected_at,
            "recent_usage": recent_usage,
        }));
    }
    Ok(json!({ "servers": items }))
}

fn server_history(config_dir: &Path, workspace: &str, args: Value) -> Result<Value> {
    let store = Store::open(config_dir)?;
    let id = required_str(&args, "server_id")?;
    ensure!(store.get(&id)?.is_some(), "SSH server not found");
    let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 100) as usize;
    let all_workspaces = args["all_workspaces"].as_bool().unwrap_or(false);
    let history = store
        .history(&id, 200)?
        .into_iter()
        .filter(|entry| all_workspaces || entry.workspace_root.as_deref() == Some(workspace))
        .take(limit)
        .map(|entry| {
            json!({
                "task_id": entry.task_id,
                "workspace_root": entry.workspace_root,
                "action": entry.action,
                "remote_path": entry.remote_path,
                "summary": entry.summary,
                "created_at": entry.created_at,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "server_id": id,
        "all_workspaces": all_workspaces,
        "history": history,
    }))
}

#[test]
fn declares_all_eight_ssh_tools_with_expected_permissions() {
    package().expect("SSH package and tool definitions must agree");
    let tools: Vec<Value> = serde_json::from_str(include_str!("tools.json")).unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "ssh__list_servers",
            "ssh__server_history",
            "ssh__exec",
            "ssh__list_dir",
            "ssh__read_file",
            "ssh__write_file",
            "ssh__upload",
            "ssh__tail_log",
        ]
    );
    for read_tool in [
        "ssh__list_servers",
        "ssh__server_history",
        "ssh__list_dir",
        "ssh__read_file",
        "ssh__tail_log",
    ] {
        assert!(is_tool(read_tool));
        assert!(!is_mutating_tool(read_tool));
    }
    for write_tool in ["ssh__exec", "ssh__write_file", "ssh__upload"] {
        assert!(is_tool(write_tool));
        assert!(is_mutating_tool(write_tool));
    }
}
