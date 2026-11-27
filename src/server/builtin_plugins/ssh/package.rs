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
    store::{Action, AuthType, EnvTier, NewServer, Server, Store},
};

const MAX_AI_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
struct SshToolContext {
    task_id: Option<String>,
    approved_protected: bool,
    allow_outside_upload: bool,
}

tokio::task_local! {
    static SSH_TOOL_CONTEXT: SshToolContext;
}

pub(crate) fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "ssh__list_servers"
            | "ssh__server_history"
            | "ssh__add_server"
            | "ssh__exec"
            | "ssh__list_dir"
            | "ssh__read_file"
            | "ssh__write_file"
            | "ssh__edit_file"
            | "ssh__upload"
            | "ssh__download"
            | "ssh__tail_log"
    )
}

pub(crate) fn is_mutating_tool(name: &str) -> bool {
    matches!(
        name,
        "ssh__add_server" | "ssh__exec" | "ssh__write_file" | "ssh__edit_file" | "ssh__upload"
    )
}

pub(crate) fn requires_approval(name: &str, args: &Value) -> bool {
    requires_approval_in(&crate::server::ai_proxy::config_dir(), name, args)
}

pub(super) fn requires_approval_in(config_dir: &Path, name: &str, args: &Value) -> bool {
    if name == "ssh__add_server" {
        return true;
    }
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

#[cfg(test)]
pub(super) async fn scoped<F: Future>(
    task_id: String,
    approved_protected: bool,
    f: F,
) -> F::Output {
    scoped_with_policy(task_id, approved_protected, false, f).await
}

pub(crate) async fn scoped_with_policy<F: Future>(
    task_id: String,
    approved_protected: bool,
    allow_outside_upload: bool,
    f: F,
) -> F::Output {
    SSH_TOOL_CONTEXT
        .scope(
            SshToolContext {
                task_id: Some(task_id),
                approved_protected,
                allow_outside_upload,
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
        "ssh__add_server" => {
            ensure!(
                ctx.approved_protected,
                "APPROVAL_REQUIRED: registering an SSH server profile requires explicit human approval"
            );
            tokio::task::spawn_blocking(move || add_server(&config_dir, args)).await?
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
            let sort_by = args["sort_by"].as_str().unwrap_or("name");
            let (connection, secret) =
                client::connect_for_tool(config_dir, server.id.clone(), ctx.task_id.is_some())
                    .await?;
            let sftp = Sftp::open(connection).await?;
            let (resolved, mut entries) = sftp.list(&path).await?;
            if sort_by != "name" {
                super::sftp::sort_entries(&mut entries, sort_by);
            }
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
            let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(60).clamp(1, 1800);
            let idle_timeout_secs = args["idle_timeout_secs"].as_u64().unwrap_or(120).min(1800);
            let idle_timeout =
                (idle_timeout_secs > 0).then(|| Duration::from_secs(idle_timeout_secs));
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
                idle_timeout,
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
                if output.as_ref().is_ok_and(|o| o.idle_timed_out) {
                    let notice = format!(
                        "\r\n\x1b[33m[idle_timeout: no output for {idle_timeout_secs}s]\x1b[0m\r\n"
                    );
                    live::emit_exec_chunk(task, &server.id, notice.as_bytes());
                }
                let code = output.as_ref().ok().and_then(|output| output.exit_code);
                live::emit_exec_end(task, &server.id, display_cwd, code, elapsed_ms);
            }
            let output = output?;
            recorded?;
            if output.idle_timed_out {
                return Ok(json!({
                    "server_id": server.id,
                    "cwd": cwd,
                    "status": "idle_timeout",
                    "timed_out": true,
                    "idle_timeout_secs": idle_timeout_secs,
                    "exit_code": Value::Null,
                    "stdout": output.stdout,
                    "stderr": output.stderr,
                    "stdout_truncated": output.stdout_truncated,
                    "stderr_truncated": output.stderr_truncated,
                    "hint": format!(
                        "远端命令连续 {idle_timeout_secs} 秒无任何新输出已被自动中断，请检查进程是否已退出、卡死或等待交互输入。"
                    ),
                }));
            }
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
        "ssh__edit_file" => {
            let server = load_server(&config_dir, &args, true, ctx.approved_protected).await?;
            let path = required_str(&args, "path")?;
            let replace_all = args["replace_all"].as_bool().unwrap_or(false);
            let backup = args["backup"].as_bool().unwrap_or(false);
            let edits = parse_remote_edits(&args)?;
            let (connection, secret) = client::connect_for_tool(
                config_dir.clone(),
                server.id.clone(),
                ctx.task_id.is_some(),
            )
            .await?;
            let sftp = Sftp::open(connection).await?;
            let original = sftp.read_text_full(&path).await?;
            let (updated, ranges) = crate::tools::file_edit::text_edits_with_ranges_replace_all(
                &original,
                &edits,
                replace_all,
            )
            .map_err(|error| anyhow::anyhow!("{path}: {error}"))?;
            let changed = updated != original;
            let bytes_written = if changed {
                sftp.write_text_atomic(&path, &updated, false, backup)
                    .await?
            } else {
                updated.len() as u64
            };
            let preview = crate::tools::file_edit::preview_for_byte_ranges(&updated, &ranges);
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
                    "edit_file",
                    &resolved,
                    |path| format!("edit {path} ({} edits)", edits.len()),
                );
            }
            Ok(json!({
                "server_id": server.id,
                "path": path,
                "changed": changed,
                "bytes_written": bytes_written,
                "backup": backup,
                "preview": preview,
            }))
        }
        "ssh__upload" => {
            let server = load_server(&config_dir, &args, true, ctx.approved_protected).await?;
            let local_path = required_str(&args, "local_path")?;
            let remote_path = required_str(&args, "remote_path")?;
            let create_parents = args["create_parents"].as_bool().unwrap_or(false);
            let overwrite = args["overwrite"].as_bool().unwrap_or(false);
            let upload_config_dir = config_dir.clone();
            let allow_outside_upload = ctx.allow_outside_upload;
            let (std_file, size, relative_display, local_mode) =
                tokio::task::spawn_blocking(move || {
                    open_workspace_upload_file(
                        &workspace_root,
                        &upload_config_dir,
                        &local_path,
                        allow_outside_upload,
                    )
                })
                .await??;
            let preserve_mode = local_mode.filter(|mode| mode & 0o111 != 0);
            let file = tokio::fs::File::from_std(std_file);
            let (connection, secret) = client::connect_for_tool(
                config_dir.clone(),
                server.id.clone(),
                ctx.task_id.is_some(),
            )
            .await?;
            let sftp = Sftp::open(connection).await?;
            let (bytes_uploaded, sha256) = sftp
                .upload_with_options(
                    &remote_path,
                    file,
                    size,
                    create_parents,
                    overwrite,
                    preserve_mode,
                )
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
            let mut out = json!({
                "server_id": server.id,
                "local_path": relative_display,
                "remote_path": remote_path,
                "bytes_uploaded": bytes_uploaded,
                "sha256": sha256,
            });
            if let Some(mode) = preserve_mode {
                out["permissions"] = json!(format!("{mode:04o}"));
            }
            Ok(out)
        }
        "ssh__download" => {
            let server = load_server(&config_dir, &args, false, ctx.approved_protected).await?;
            let remote_path = required_str(&args, "remote_path")?;
            let local_path = required_str(&args, "local_path")?;
            let create_parents = args["create_parents"].as_bool().unwrap_or(true);
            let overwrite = args["overwrite"].as_bool().unwrap_or(false);
            let (dest, temp_path, relative_display) = tokio::task::spawn_blocking(move || {
                prepare_workspace_download_target(
                    &workspace_root,
                    &local_path,
                    create_parents,
                    overwrite,
                )
            })
            .await??;
            let (connection, secret) = client::connect_for_tool(
                config_dir.clone(),
                server.id.clone(),
                ctx.task_id.is_some(),
            )
            .await?;
            let sftp = Sftp::open(connection).await?;
            let temp_file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .await
                .with_context(|| format!("failed to create temp file {}", temp_path.display()))?;
            let download_result = async {
                let mut temp_file = temp_file;
                let (bytes, sha256) = sftp
                    .download_to_writer(&remote_path, &mut temp_file, MAX_AI_UPLOAD_BYTES)
                    .await?;
                temp_file.sync_all().await?;
                drop(temp_file);
                if dest.exists() && !overwrite {
                    anyhow::bail!("local file already exists; pass overwrite: true to replace it");
                }
                tokio::fs::rename(&temp_path, &dest).await?;
                Ok::<(u64, String), anyhow::Error>((bytes, sha256))
            }
            .await;
            let (bytes_downloaded, sha256) = match download_result {
                Ok(result) => result,
                Err(error) => {
                    let _ = tokio::fs::remove_file(&temp_path).await;
                    return Err(error);
                }
            };
            record_usage(
                config_dir,
                server.id.clone(),
                Action::Download,
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
                    "download",
                    &resolved,
                    |path| format!("download {path} -> {local} ({bytes_downloaded} B)"),
                );
            }
            Ok(json!({
                "server_id": server.id,
                "remote_path": remote_path,
                "local_path": relative_display,
                "bytes_downloaded": bytes_downloaded,
                "sha256": sha256,
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

fn has_forbidden_upload_component(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            Component::Normal(name)
                if name.eq_ignore_ascii_case(".codex-workspace-mcp")
                    || name.eq_ignore_ascii_case(".ssh")
                    || name.eq_ignore_ascii_case(".gnupg")
        )
    })
}

fn path_starts_with_normalized(path: &Path, prefix: &Path) -> bool {
    let clean_path = crate::tools::file_edit::strip_verbatim_prefix(path);
    let clean_prefix = crate::tools::file_edit::strip_verbatim_prefix(prefix);
    #[cfg(windows)]
    {
        let p = clean_path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
        let pre = clean_prefix
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase();
        p == pre || p.starts_with(&format!("{pre}/"))
    }
    #[cfg(not(windows))]
    {
        clean_path.starts_with(&clean_prefix)
    }
}

fn is_forbidden_upload_path(path: &Path, config_dir: &Path) -> bool {
    if has_forbidden_upload_component(path) {
        return true;
    }
    let clean_cfg = config_dir
        .canonicalize()
        .map(|p| crate::tools::file_edit::strip_verbatim_prefix(&p))
        .unwrap_or_else(|_| crate::tools::file_edit::strip_verbatim_prefix(config_dir));
    if path_starts_with_normalized(path, &clean_cfg) {
        return true;
    }
    if let Ok(canon) = path.canonicalize() {
        let clean_canon = crate::tools::file_edit::strip_verbatim_prefix(&canon);
        if has_forbidden_upload_component(&clean_canon)
            || path_starts_with_normalized(&clean_canon, &clean_cfg)
        {
            return true;
        }
    }
    false
}

fn open_workspace_upload_file(
    workspace_root: &Path,
    config_dir: &Path,
    local_path: &str,
    allow_outside_upload: bool,
) -> Result<(std::fs::File, u64, String, Option<u32>)> {
    ensure!(
        !local_path.is_empty() && !local_path.contains('\0'),
        "WORKSPACE_BOUNDARY_DENIED: invalid local_path"
    );
    let clean_input = crate::tools::file_edit::strip_verbatim_prefix(Path::new(local_path));
    let clean_root = crate::tools::file_edit::strip_verbatim_prefix(workspace_root);
    let raw = clean_input.as_path();
    ensure!(
        !raw.components()
            .any(|component| matches!(component, Component::ParentDir)),
        "WORKSPACE_BOUNDARY_DENIED: local_path cannot contain parent traversal (..)"
    );
    let inside_relative = if raw.is_absolute() {
        raw.strip_prefix(workspace_root)
            .or_else(|_| raw.strip_prefix(&clean_root))
            .ok()
    } else {
        Some(raw)
    };
    let (file, display_path) = if let Some(relative) = inside_relative {
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
            !has_forbidden_upload_component(relative),
            "WORKSPACE_BOUNDARY_DENIED: cannot upload internal host state or credential files"
        );
        let file = crate::server::file_stream::open_regular(workspace_root, relative).map_err(
            |err| {
                anyhow::anyhow!(
                    "WORKSPACE_BOUNDARY_DENIED: cannot open regular workspace file '{local_path}' without following symlinks: {err}"
                )
            },
        )?;
        (file, relative.to_string_lossy().replace('\\', "/"))
    } else {
        ensure!(
            allow_outside_upload,
            "WORKSPACE_BOUNDARY_DENIED: local_path must be inside the active workspace"
        );
        ensure!(
            !is_forbidden_upload_path(raw, config_dir),
            "WORKSPACE_BOUNDARY_DENIED: cannot upload internal host state or credential files"
        );
        let parent = raw.parent().filter(|p| !p.as_os_str().is_empty()).context(
            "WORKSPACE_BOUNDARY_DENIED: invalid outside local_path without parent directory",
        )?;
        let file_name = raw
            .file_name()
            .context("WORKSPACE_BOUNDARY_DENIED: invalid outside local_path filename")?;
        if let Ok(canon_parent) = parent.canonicalize() {
            ensure!(
                !is_forbidden_upload_path(&canon_parent.join(file_name), config_dir),
                "WORKSPACE_BOUNDARY_DENIED: cannot upload internal host state or credential files"
            );
        }
        let file =
            crate::server::file_stream::open_regular(parent, Path::new(file_name)).map_err(
                |err| {
                    anyhow::anyhow!(
                        "WORKSPACE_BOUNDARY_DENIED: cannot open regular file '{local_path}' without following symlinks: {err}"
                    )
                },
            )?;
        (file, raw.to_string_lossy().replace('\\', "/"))
    };
    let meta = file.metadata()?;
    let size = meta.len();
    ensure!(
        size <= MAX_AI_UPLOAD_BYTES,
        "local file exceeds 512 MiB AI upload limit"
    );
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o777)
    };
    #[cfg(not(unix))]
    let mode = None;
    Ok((file, size, display_path, mode))
}

fn prepare_workspace_download_target(
    workspace_root: &Path,
    local_path: &str,
    create_parents: bool,
    overwrite: bool,
) -> Result<(PathBuf, PathBuf, String)> {
    ensure!(
        !local_path.is_empty()
            && !local_path.contains('\0')
            && !local_path.ends_with('/')
            && !local_path.ends_with('\\'),
        "WORKSPACE_BOUNDARY_DENIED: invalid local_path"
    );
    let clean_input = crate::tools::file_edit::strip_verbatim_prefix(Path::new(local_path));
    let raw = clean_input.as_path();
    ensure!(
        !raw.components()
            .any(|component| matches!(component, Component::ParentDir)),
        "WORKSPACE_BOUNDARY_DENIED: local_path cannot contain parent traversal (..)"
    );
    let canonical_root = workspace_root.canonicalize()?;
    let clean_root = crate::tools::file_edit::strip_verbatim_prefix(&canonical_root);
    let relative = if raw.is_absolute() {
        raw.strip_prefix(&canonical_root)
            .or_else(|_| raw.strip_prefix(workspace_root))
            .or_else(|_| raw.strip_prefix(&clean_root))
            .map_err(|_| {
                anyhow::anyhow!(
                    "WORKSPACE_BOUNDARY_DENIED: local_path must be inside the active workspace"
                )
            })?
    } else {
        raw
    };
    ensure!(
        !relative.as_os_str().is_empty()
            && !relative.components().any(|component| {
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
        "WORKSPACE_BOUNDARY_DENIED: cannot write into internal host state"
    );
    // Reject symlinks anywhere along the existing portion of the target path.
    let mut cursor = canonical_root.clone();
    for component in relative.components() {
        cursor.push(component);
        if let Ok(meta) = std::fs::symlink_metadata(&cursor) {
            ensure!(
                !meta.file_type().is_symlink(),
                "WORKSPACE_BOUNDARY_DENIED: cannot write through symlink '{local_path}'"
            );
        }
    }
    let dest = crate::tools::file_edit::workspace_path(&canonical_root, local_path)
        .map_err(|err| anyhow::anyhow!("WORKSPACE_BOUNDARY_DENIED: {err}"))?;
    if let Ok(meta) = std::fs::symlink_metadata(&dest) {
        ensure!(
            !meta.file_type().is_symlink() && !meta.is_dir(),
            "WORKSPACE_BOUNDARY_DENIED: destination is a directory or symlink"
        );
        ensure!(
            overwrite,
            "local file already exists; pass overwrite: true to replace it"
        );
    }
    let parent = dest
        .parent()
        .context("WORKSPACE_BOUNDARY_DENIED: destination has no parent directory")?;
    if !parent.exists() {
        ensure!(
            create_parents,
            "local parent directory does not exist; pass create_parents: true"
        );
        std::fs::create_dir_all(parent)?;
    }
    let canonical_parent = parent.canonicalize()?;
    ensure!(
        canonical_parent.starts_with(&canonical_root),
        "WORKSPACE_BOUNDARY_DENIED: destination escapes active workspace"
    );
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|_| anyhow::anyhow!("Cannot generate temp filename"))?;
    let nonce: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let temp_path = canonical_parent.join(format!(".grow-download-{nonce}.tmp"));
    Ok((dest, temp_path, relative.to_string_lossy().replace('\\', "/")))
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

fn add_server(config_dir: &Path, args: Value) -> Result<Value> {
    let name = required_str(&args, "name")?;
    let host = required_str(&args, "host")?;
    let username = required_str(&args, "username")?;
    let port_u64 = args["port"].as_u64().unwrap_or(22);
    ensure!(
        (1..=65535).contains(&port_u64),
        "SSH port must be between 1 and 65535"
    );
    let port = port_u64 as u16;
    let key_path = args["key_path"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let credential = args["credential"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let auth_type = match args["auth_type"].as_str() {
        Some("password") => AuthType::Password,
        Some("key_file") => AuthType::KeyFile,
        Some("agent") => AuthType::Agent,
        Some(other) => anyhow::bail!("invalid auth_type: {other}"),
        None => {
            if key_path.is_some() {
                AuthType::KeyFile
            } else if credential.is_some() {
                AuthType::Password
            } else {
                AuthType::Agent
            }
        }
    };
    let env_tier = match args["env_tier"].as_str() {
        None | Some("dev") => EnvTier::Dev,
        Some("staging") => EnvTier::Staging,
        Some("prod") => EnvTier::Prod,
        Some(other) => anyhow::bail!("invalid env_tier: {other}"),
    };
    let require_approval = args["require_approval"].as_bool().unwrap_or(false);
    let default_dir = args["default_dir"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let tags = args["tags"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::trim).filter(|s| !s.is_empty()))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let notes = args["notes"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let mut store = Store::open(config_dir)?;
    if let Some(existing) = store
        .list()?
        .into_iter()
        .find(|s| s.host == host && s.port == port && s.username == username)
    {
        return Ok(json!({
            "server_id": existing.id,
            "reused": true,
            "name": existing.name,
            "host": existing.host,
            "port": existing.port,
            "username": existing.username,
            "env_tier": existing.env_tier,
            "require_approval": existing.require_approval,
            "default_dir": existing.default_dir,
        }));
    }

    let id = store.create(NewServer {
        name,
        host,
        port: Some(port),
        username,
        auth_type,
        key_path,
        env_tier,
        require_approval,
        default_dir,
        tags,
        notes,
    })?;
    if let Some(raw_secret) = credential
        && auth_type != AuthType::Agent
    {
        if let Err(err) = store.set_credential(&id, Some(Secret::new(raw_secret)?)) {
            let _ = store.delete(&id);
            return Err(err);
        }
    }
    let created = store
        .get(&id)?
        .context("created SSH server could not be reloaded")?;
    Ok(json!({
        "server_id": created.id,
        "reused": false,
        "name": created.name,
        "host": created.host,
        "port": created.port,
        "username": created.username,
        "env_tier": created.env_tier,
        "require_approval": created.require_approval,
        "default_dir": created.default_dir,
    }))
}

fn parse_remote_edits(args: &Value) -> Result<Vec<crate::tools::TextEdit>> {
    if let Some(list) = args.get("edits") {
        ensure!(
            args.get("old_text").is_none() && args.get("new_text").is_none(),
            "provide either `edits` or `old_text`/`new_text`, not both"
        );
        let edits: Vec<crate::tools::TextEdit> =
            serde_json::from_value(list.clone()).context("invalid `edits` array")?;
        ensure!(
            !edits.is_empty() && edits.len() <= 128,
            "provide between 1 and 128 edits"
        );
        return Ok(edits);
    }
    let old_text = args["old_text"]
        .as_str()
        .context("missing `old_text` (or pass an `edits` array)")?;
    let new_text = args["new_text"]
        .as_str()
        .context("missing `new_text` (or pass an `edits` array)")?;
    let line_hint = args
        .get("line_hint")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    let replace_all = args["replace_all"].as_bool().unwrap_or(false);
    Ok(vec![crate::tools::TextEdit {
        old_text: old_text.to_owned(),
        new_text: new_text.to_owned(),
        line_hint,
        replace_all,
    }])
}

#[test]
fn declares_all_eleven_ssh_tools_with_expected_permissions() {
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
            "ssh__add_server",
            "ssh__exec",
            "ssh__list_dir",
            "ssh__read_file",
            "ssh__write_file",
            "ssh__edit_file",
            "ssh__upload",
            "ssh__download",
            "ssh__tail_log",
        ]
    );
    for read_tool in [
        "ssh__list_servers",
        "ssh__server_history",
        "ssh__list_dir",
        "ssh__read_file",
        "ssh__download",
        "ssh__tail_log",
    ] {
        assert!(is_tool(read_tool));
        assert!(!is_mutating_tool(read_tool));
    }
    for write_tool in [
        "ssh__add_server",
        "ssh__exec",
        "ssh__write_file",
        "ssh__edit_file",
        "ssh__upload",
    ] {
        assert!(is_tool(write_tool));
        assert!(is_mutating_tool(write_tool));
    }
}

#[test]
fn parse_remote_edits_accepts_shorthand_and_array_and_rejects_mixing() {
    let single = parse_remote_edits(&json!({
        "old_text": "a",
        "new_text": "b",
        "line_hint": 12,
        "replace_all": true
    }))
    .unwrap();
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].old_text, "a");
    assert_eq!(single[0].line_hint, Some(12));
    assert!(single[0].replace_all);

    let batch = parse_remote_edits(&json!({
        "edits": [
            { "old_text": "x", "new_text": "y" },
            { "old_text": "y", "new_text": "z" }
        ]
    }))
    .unwrap();
    assert_eq!(batch.len(), 2);

    assert!(
        parse_remote_edits(&json!({
            "old_text": "a",
            "new_text": "b",
            "edits": [{ "old_text": "x", "new_text": "y" }]
        }))
        .is_err()
    );
}
