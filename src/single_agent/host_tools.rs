//! Host-side tool execution backend (permissions, approvals, sandbox, managed
//! file baselines, workspace change tracking, and plugin/page tool dispatch).
//!
//! Kept strictly decoupled from the agent driver loop so the Context-as-Code v2
//! driver can invoke any host capability cleanly without legacy ReAct baggage.

use super::{Session, managed_files};
use crate::agent_service::{self, AgentServiceState};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const SHELL_MODULE_ONLY_MESSAGE: &str = "SHELL_MODULE_ONLY: Shell execution is disabled in this session or restricted plugin context (allow_shell=false).";

/// Which host capabilities a plugin-owned session may reach. `register_image`
/// is deliberately missing: it reads local files, and a plugin page owns none
/// outside its own workspace.
pub(super) fn page_allows(s: &Session<'_>, name: &str) -> bool {
    let Some(owner) = &s.plugin else {
        return true;
    };
    matches!(
        name,
        "read_tool_contracts"
            | "read_program_guide"
            | "read_attachment"
            | "analyze_images"
            | "generate_image"
            | "image_analysis_results"
            | "view_image"
            | "list_skills"
            | "read_skill"
            | "unload_skill"
            | "read_session_history"
            | "read_asset"
            | "snapshot"
            | "list_context_assets"
            | "find_library_methods"
            | "ask_user"
    ) || crate::function_frames::is_tool(name)
        || s.state
            .tool_catalog
            .as_ref()
            .is_some_and(|c| c.packages.owns_tool(owner, name) || c.packages.builtin_tool(name))
}

pub(super) fn visual_context(
    s: &Session<'_>,
    node: &str,
) -> Result<crate::visual_artifacts::VisualContext> {
    Ok(crate::visual_artifacts::VisualContext {
        turn: Some(s.turn),
        identity: json!({
            "task_id": s.task,
            "request_id": s.turn,
            "node_id": node,
            "runtime": "single_agent",
            "agent_id": s.delegate_id(),
            "task_node_id": s.task_node_id.as_ref()
        }),
        allowed_artifact_ids: crate::visual_artifacts::task_artifact_ids(
            s.state.workspace.root(),
            s.task,
        )?,
        ..Default::default()
    })
}

async fn page_tool_result(
    s: &Session<'_>,
    node: &str,
    call_id: &str,
    name: &str,
    args: &Value,
    answer: Value,
) -> Result<Value> {
    use base64::Engine;
    let source = json!({
        "source": "plugin_page",
        "tool": name,
        "arguments": args,
        "document": answer["document"],
        "project_id": answer["document"]["project_id"],
        "source_revision": answer["document"]["revision"]
    });
    let parts = answer["content"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let metas = parts
        .iter()
        .filter(|part| part["type"] == "image")
        .map(|part| part["meta"].clone())
        .collect::<Vec<_>>();
    let mut review = None;
    let mut review_unavailable = None;
    if !metas.is_empty() {
        match crate::plugin_io::review_brief(
            s.state,
            s.task,
            name,
            args,
            &answer["document"],
            &json!(metas),
        )
        .await
        {
            Ok(brief) => review = brief,
            Err(error) => review_unavailable = Some(format!("{error:#}")),
        }
    }
    let mut data = Vec::new();
    let mut images = Vec::new();
    for part in parts {
        match part["type"].as_str() {
            Some("text") => {
                let text = part["text"].as_str().unwrap_or_default();
                data.push(serde_json::from_str(text).unwrap_or_else(|_| json!(text)));
            }
            Some("image") => {
                let mime = part["mimeType"].as_str().unwrap_or("image/png");
                ensure!(
                    matches!(mime, "image/png" | "image/jpeg" | "image/webp"),
                    "unsupported page image type"
                );
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(part["data"].as_str().unwrap_or_default())
                    .context("page image is not base64")?;
                ensure!(bytes.len() <= 16 * 1024 * 1024, "page image too large");
                let mut visual = visual_context(s, node)?;
                visual.source_tool_call_id = call_id.to_owned();
                let mut metadata = source.clone();
                metadata["image_meta"] = part["meta"].clone();
                if let Some(review) = &review {
                    metadata["review"] = json!({
                        "instructions": review["instructions"],
                        "subject": review["subjects"][images.len()]
                    });
                }
                let image = crate::visual_artifacts::save(
                    s.state.workspace.root(),
                    &visual,
                    metadata,
                    &bytes,
                )?;
                let mut entry = json!({"artifact_id": image["artifact_id"], "mime_type": mime});
                if !part["meta"].is_null() {
                    entry["meta"] = part["meta"].clone();
                }
                images.push(entry);
            }
            _ => {}
        }
    }
    let ok = answer["ok"].as_bool().unwrap_or(false);
    let mut result = json!({
        "status": if ok { "succeeded" } else { "failed" },
        "data": if data.len() == 1 { data.pop().unwrap() } else { json!(data) },
    });
    if let Some(document) = answer.get("document") {
        result["document"] = document.clone();
    }
    if let Some(first) = images.first() {
        result["artifact_id"] = first["artifact_id"].clone();
        result["images"] = json!(images);
    }
    if let Some(error) = review_unavailable {
        result["review_instructions_unavailable"] = json!(error);
    }
    Ok(result)
}

#[allow(dead_code)]
pub(super) async fn business_tool(
    s: &Session<'_>,
    node: &str,
    name: &str,
    args: Value,
    call_id: &str,
) -> Result<Value> {
    business_tool_inner(s, node, name, args, call_id, false, false).await
}

/// Execute one host capability call with full permission, approval, sandbox,
/// file-baseline, and plugin routing enforcement.
#[allow(dead_code)]
pub(super) async fn business_tool_inner(
    s: &Session<'_>,
    node: &str,
    name: &str,
    args: Value,
    call_id: &str,
    allow_shell: bool,
    program_read: bool,
) -> Result<Value> {
    ensure!(
        !crate::project_process::is_execution(name) || allow_shell,
        SHELL_MODULE_ONLY_MESSAGE
    );
    ensure!(page_allows(s, name), "PLUGIN_SCOPE_DENIED: {name}");
    if name == "read_session_history" {
        let mut args = args;
        if s.delegate_id().is_some() || s.plugin.is_some() {
            ensure!(
                args["task_id"].as_str().is_none_or(|task| task == s.task),
                "this session may only read this conversation"
            );
            ensure!(
                args["scope"]
                    .as_str()
                    .is_none_or(|scope| scope == "conversation"),
                "this session cannot search other tasks"
            );
            args["task_id"] = json!(s.task);
        }
        return crate::session_history::read_for_call(
            s.state.workspace.root(),
            s.task,
            &args,
            Some(call_id),
        )
        .await;
    }
    ensure!(
        s.state.permission_mode.allows(name)
            && s.state
                .tool_catalog
                .as_ref()
                .is_none_or(|catalog| catalog.packages.allows(
                    name,
                    s.state.permission_mode,
                    s.ban_execution
                )),
        "permission_denied: {name}"
    );
    if crate::process_sandbox::ExecutionBoundary::for_mode(s.state.permission_mode)
        != crate::process_sandbox::ExecutionBoundary::Host
        && s.state
            .tool_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.packages.is_command_tool(name))
    {
        anyhow::bail!(
            "SANDBOX_UNSUPPORTED: package command tools cannot yet run under the workspace sandbox"
        );
    }
    if !(name == "write_stdin" && args["chars"].as_str().is_none_or(str::is_empty))
        && crate::tool_approvals::needs_approval(
            s.state.permission_mode,
            name,
            s.state.tool_catalog.as_deref(),
            s.ban_execution,
        )
    {
        crate::tool_approvals::wait(
            s.state,
            s.task,
            s.model,
            s.delegate_id(),
            call_id,
            name,
            &args,
            s.cancel,
        )
        .await?;
    }
    if crate::plan_tools::is_tool(name) {
        let work = s.state.workspace.clone();
        return crate::plan_tools::scoped(work.root().to_path_buf(), s.task.to_owned(), s.turn, async {
            if let Some(catalog) = s.state.tool_catalog.as_deref() {
                catalog.execute(work.clone(), name, args).await
            } else {
                crate::plan_tools::execute(name,args).await
            }
        }).await;
    }

    let tool_workspace = if name == "view_image" {
        s.state.workspace.clone()
    } else {
        let path = if let Some(plugin) =
            crate::plugin_sessions::owner(s.state.workspace.root(), s.task)?
        {
            crate::plugin_sessions::workspace_dir(s.state.workspace.root(), &plugin)?
        } else {
            crate::session_browser::task_workspace(s.state.workspace.root(), s.task)?
        };
        std::sync::Arc::new(crate::tools::Workspace::new(path)?)
    };
    let mut extra_roots =
        crate::tool_approvals::approved_writable_roots(s.state.workspace.root(), s.task)?;
    let outside = outside_read_paths(tool_workspace.root(), &extra_roots, name, &args);
    if !outside.is_empty()
        && !matches!(
            s.state.permission_mode,
            crate::agent_service::PermissionMode::FullAccess
        )
    {
        crate::tool_approvals::wait_outside_read(
            s.state,
            s.task,
            s.model,
            s.delegate_id(),
            call_id,
            name,
            &args,
            &outside,
            s.cancel,
        )
        .await?;
    }
    let outside_exec = outside_exec_or_index_paths(tool_workspace.root(), name, &args);
    if !outside_exec.is_empty() {
        ensure!(
            !matches!(
                s.state.permission_mode,
                crate::agent_service::PermissionMode::ReadOnly
            ) && !s.ban_execution,
            "OUTSIDE_WORKSPACE_DENIED: {name} cannot target paths outside the workspace ({}) in read-only mode",
            outside_exec.join(", ")
        );
        if !matches!(
            s.state.permission_mode,
            crate::agent_service::PermissionMode::FullAccess
        ) {
            crate::tool_approvals::wait_outside_write(
                s.state,
                s.task,
                s.model,
                s.delegate_id(),
                call_id,
                name,
                &args,
                &outside_exec,
                s.cancel,
            )
            .await?;
            extra_roots =
                crate::tool_approvals::approved_writable_roots(s.state.workspace.root(), s.task)?;
            for raw in &outside_exec {
                if let Ok(canon) = std::path::Path::new(raw).canonicalize()
                    && crate::process_sandbox::is_allowed_extra_root(&canon)
                    && !extra_roots.contains(&canon)
                {
                    extra_roots.push(canon);
                }
            }
        }
    }
    let detected_edit = detect_outside_edit(&tool_workspace, name, &args)?;
    let (tool_workspace, args, is_outside_edit) = if let Some(edit_target) = detected_edit {
        ensure!(
            !matches!(
                s.state.permission_mode,
                crate::agent_service::PermissionMode::ReadOnly
            ) && !s.ban_execution,
            "OUTSIDE_WORKSPACE_EDIT: editing {} outside the project is not allowed in read-only mode",
            edit_target.display_path
        );
        if !matches!(
            s.state.permission_mode,
            crate::agent_service::PermissionMode::FullAccess
        ) {
            let paths = [edit_target.display_path.clone()];
            crate::tool_approvals::wait_outside_write(
                s.state,
                s.task,
                s.model,
                s.delegate_id(),
                call_id,
                name,
                &args,
                &paths,
                s.cancel,
            )
            .await?;
            extra_roots =
                crate::tool_approvals::approved_writable_roots(s.state.workspace.root(), s.task)?;
            if crate::process_sandbox::is_allowed_extra_root(&edit_target.work_root)
                && !extra_roots.contains(&edit_target.work_root)
            {
                extra_roots.push(edit_target.work_root.clone());
            }
        }
        let mut normalized = args.clone();
        normalized["path"] = json!(edit_target.rel_path);
        normalized["workspace_root"] = json!(edit_target.work_root.to_string_lossy());
        (
            std::sync::Arc::new(crate::tools::Workspace::new(&edit_target.work_root)?),
            normalized,
            true,
        )
    } else {
        (tool_workspace, args, false)
    };
    if name == crate::shell_process::READ_ONLY {
        return crate::shell_process::run_read_only(&tool_workspace, &args, s.cancel).await;
    }
    let sandbox_boundary = crate::shell_process::boundary(s.state.permission_mode, s.ban_execution);
    if crate::shell_process::is_tool(name) {
        return Box::pin(execute_sandboxed_shell_tool(
            sandbox_boundary,
            extra_roots,
            s.state,
            &tool_workspace,
            s.task,
            s.turn,
            s.task_node_id.as_deref(),
            name,
            &args,
            s.cancel,
            s.ban_execution,
        ))
        .await;
    }
    crate::process_sandbox::scope_tool_sandbox(sandbox_boundary, extra_roots, async {
        if crate::workspace_changes::is_mutation(name) {
            crate::shell_process::ensure_idle(tool_workspace.root()).await?;
        }
        if s.delegate_id().is_some()
            && tool_workspace.root() != s.state.workspace.root()
            && crate::workspace_changes::is_mutation(name)
            && args["background"] == true
        {
            anyhow::bail!(
                "GIT_DELEGATE_BACKGROUND_WRITE_DENIED: finish foreground processes before the branch is snapshotted"
            );
        }
        let _mutation_guard = if crate::workspace_changes::is_mutation(name)
            && s.delegate_id().is_none()
            && s.plugin.is_none()
        {
            Some(
                crate::workspace_changes::mutation_guard(
                    tool_workspace.root(),
                    name,
                    &args,
                    s.cancel,
                )
                .await?,
            )
        } else {
            None
        };
        let _claim = if let Some(resource) =
            crate::workspace_changes::resource_key(tool_workspace.root(), name, &args)?
        {
            Some(crate::workspace_changes::claim_resource(
                s.state.workspace.root(),
                s.task,
                call_id,
                resource,
            )?)
        } else {
            None
        };
        let parallel_owner = s.task_node_id.as_deref().and_then(|id| {
            let conn = agent_service::open_db(s.state.workspace.root()).ok()?;
            let node = crate::function_frames::get(&conn, s.task, id).ok()?;
            node.metadata["parallel_owner"].as_str().map(str::to_owned)
        });
        let managed = if matches!(name, "write_file" | "edit_file" | "replace_range") {
            let prepared = managed_files::prepare(
                s.state.workspace.root(),
                s.task,
                tool_workspace.root(),
                name,
                args.clone(),
            )?;
            if prepared.conflict()? {
                return prepared.preserve(s.state.workspace.root(), s.task);
            }
            if name == "write_file"
                && let Some(owner) = &parallel_owner
            {
                let conn = agent_service::open_db(s.state.workspace.root())?;
                let frame = crate::function_frames::get(&conn, s.task, owner)?;
                let started = frame.metadata["file_write_clock"].as_i64().unwrap_or(0);
                if let Some(conflict) = prepared.parallel_conflict_result(
                    s.state.workspace.root(),
                    s.task,
                    owner,
                    started,
                )? {
                    return Ok(conflict);
                }
            }
            Some(prepared)
        } else {
            None
        };
        let untouched = if crate::project_process::may_change_files(name) || crate::git_tools::mutates(name) || name == crate::batch_edits::NAME {
            Some(managed_files::untouched(
                s.state.workspace.root(),
                s.task,
                tool_workspace.root(),
            )?)
        } else {
            None
        };
        let mut args = managed.as_ref().map(|p| p.args.clone()).unwrap_or(args);
        if name == crate::batch_edits::NAME {
            crate::batch_edits::paths(tool_workspace.root(), &args)?;
            managed_files::guard_batch(s.state.workspace.root(),s.task,tool_workspace.root(),&mut args)?;
        }
        let targets_external_project = is_outside_edit || !outside_exec.is_empty();
        let result = if crate::workspace_changes::is_mutation(name)
            && s.delegate_id().is_none()
            && s.plugin.is_none()
            && !targets_external_project
        {
            crate::workspace_changes::execute_tracked(
                s.state.clone(),
                tool_workspace.clone(),
                s.task.to_owned(),
                s.turn,
                name.to_owned(),
                args.clone(),
                s.cancel.clone(),
            )
            .await
        } else if crate::workspace_changes::is_mutation(name) {
            let boundary =
                crate::process_sandbox::ExecutionBoundary::for_mode(s.state.permission_mode);
            if crate::project_process::is_tool(name) {
                crate::project_process::execute_with_boundary(
                    &tool_workspace,
                    name,
                    args.clone(),
                    s.cancel,
                    boundary,
                )
                .await
            } else if crate::git_tools::is_tool(name) {
                crate::git_tools::execute(&tool_workspace,name,&args,boundary,s.cancel).await
            } else if crate::verify_tools::is_tool(name) {
                crate::verify_tools::execute(&tool_workspace, name, &args, s.cancel, boundary).await
            } else if crate::program_execution::is_tool(name) {
                crate::program_execution::execute_with_boundary(
                    &tool_workspace,
                    &args,
                    s.cancel,
                    boundary,
                )
                .await
            } else {
                agent_service::execute_tool(
                    tool_workspace.clone(),
                    s.state.tool_catalog.as_deref(),
                    name,
                    args.clone(),
                )
                .await
            }
        } else if crate::git_tools::is_tool(name) {
            let boundary = if cfg!(target_os = "linux") && s.state.permission_mode == crate::agent_service::PermissionMode::ReadOnly {
                crate::process_sandbox::ExecutionBoundary::ReadOnly
            } else { crate::process_sandbox::ExecutionBoundary::for_mode(s.state.permission_mode) };
            crate::git_tools::execute(&tool_workspace,name,&args,boundary,s.cancel).await
        } else if crate::browser_control::is_tool(name) {
            let mut visual = visual_context(s, node)?;
            visual.source_tool_call_id = call_id.to_owned();
            visual.source_event_id = format!("{}:tool:{call_id}", s.task);
            crate::browser_control::execute_scoped(&tool_workspace, &visual, name, &args).await
        } else {
            if let Some(catalog) = &s.state.tool_catalog
                && let Some(page_name) = catalog.packages.page_tool(name)
            {
                if let Some(decision) =
                    crate::plugin_io::authorize_page_tool(s.state, s.task, name, &args).await?
                    && decision["allowed"] == false
                {
                    return Ok(json!({
                        "host_denied": true,
                        "code": decision["code"],
                        "message": decision["message"],
                        "question": decision["question"]
                    }));
                }
                let supplied = args.clone();
                let answer = match crate::page_tools::call(s.task, &page_name, args, s.cancel).await
                {
                    Ok(answer) => answer,
                    Err(error) => {
                        if error
                            .to_string()
                            .starts_with(crate::page_tools::PAGE_CLOSED)
                        {
                            s.cancel.cancel();
                        }
                        return Err(error);
                    }
                };
                return page_tool_result(s, node, call_id, name, &supplied, answer).await;
            }
            if let Some(catalog) = &s.state.tool_catalog
                && catalog.packages.declares_tool(name)
            {
                let queried = args["task_id"].as_str().map(str::to_owned);
                let mut output = tokio::select! {
                    result = crate::plugin_io::execute(s.state, s.task, tool_workspace.clone(), name, args.clone()) => result?,
                    _ = s.cancel.cancelled() => anyhow::bail!("task cancelled during skill tool execution"),
                };
                let root = s.state.workspace.root();
                if output["status"] == "accepted"
                    && let Some(job) = output["task_id"].as_str()
                    && crate::plugin_jobs::poll_tool(name).is_some()
                {
                    crate::plugin_jobs::record(
                        root,
                        s.task,
                        name,
                        job,
                        call_id,
                        s.task_node_id.as_deref(),
                    )?;
                    output["host"] = json!(
                        "Accepted as a background task. The host delivers its result when it finishes."
                    );
                }
                if let Some(job) = queried
                    && name.ends_with("__task")
                    && output["status"]
                        .as_str()
                        .is_some_and(crate::plugin_jobs::finished)
                {
                    crate::plugin_jobs::complete(root, s.task, &job, "delivered")?;
                }
                return Ok(output);
            }
            let mut args = args.clone();
            if program_read {
                crate::source_read::mark_program_read(name, &mut args);
            }
            agent_service::execute_tool(
                tool_workspace.clone(),
                s.state.tool_catalog.as_deref(),
                name,
                args,
            )
            .await
        };
        if let Some(untouched) = untouched
            && !result.as_ref().is_ok_and(|value| value["running"] == true)
        {
            untouched.adopt(
                s.state.workspace.root(),
                s.task,
                parallel_owner.as_deref(),
                s.turn,
            )?;
        }
        // A deleted or moved path keeps no baseline: the next write must compare
        // against what is on disk then, not against bytes that are gone.
        if result.is_ok()
            && matches!(name, "delete_path" | "rename_path")
        {
            for raw in [args["path"].as_str(), args["to"].as_str()].into_iter().flatten() {
                managed_files::forget(s.state.workspace.root(), s.task, tool_workspace.root(), raw)?;
            }
        }
        if let Some(prepared) = managed {
            return match result {
                Ok(value) => {
                    if prepared.committed(s.state.workspace.root(), s.task)? {
                        if value["changed"] != false {
                            prepared.record_writer(
                                s.state.workspace.root(),
                                s.task,
                                parallel_owner.as_deref().unwrap_or("@main"),
                                s.turn,
                            )?;
                        }
                        Ok(value)
                    } else {
                        prepared.preserve(s.state.workspace.root(), s.task)
                    }
                }
                Err(error)
                    if format!("{error:#}").contains("write_conflict:")
                        || prepared.conflict()? =>
                {
                    prepared.preserve(s.state.workspace.root(), s.task)
                }
                Err(error) => Err(error),
            };
        }
        if matches!(name, "read_file" | "read_file_lines")
            && let Ok(value) = &result
            && let Some(conflict) = managed_files::observe(
                s.state.workspace.root(),
                s.task,
                tool_workspace.root(),
                &args,
                value,
            )?
        {
            return Ok(conflict);
        }
        result
    })
    .await
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
async fn execute_sandboxed_shell_tool(
    sandbox_boundary: crate::process_sandbox::ExecutionBoundary,
    extra_roots: Vec<std::path::PathBuf>,
    state: &AgentServiceState,
    workspace: &crate::tools::Workspace,
    task: &str,
    turn: usize,
    owner: Option<&str>,
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
    ban_execution: bool,
) -> Result<Value> {
    crate::process_sandbox::scope_tool_sandbox(
        sandbox_boundary,
        extra_roots,
        crate::shell_process::execute_program(
            state,
            workspace,
            task,
            turn,
            owner,
            name,
            args,
            cancel,
            ban_execution,
        ),
    )
    .await
}

#[derive(Clone, Debug)]
struct OutsideEditTarget {
    display_path: String,
    work_root: std::path::PathBuf,
    rel_path: String,
}

fn detect_outside_edit(
    workspace: &crate::tools::Workspace,
    name: &str,
    args: &Value,
) -> Result<Option<OutsideEditTarget>> {
    use std::path::{Component, Path, PathBuf};
    if !matches!(name, "write_file" | "edit_file" | "replace_range") {
        return Ok(None);
    }
    let Some(raw_path) = args["path"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let explicit_ws = args["workspace_root"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let root = workspace
        .root()
        .canonicalize()
        .unwrap_or_else(|_| workspace.root().to_path_buf());
    let base = match explicit_ws {
        Some(ws) => {
            let ws_path = Path::new(ws);
            if ws_path.is_absolute() {
                ws_path.to_path_buf()
            } else {
                root.join(ws_path)
            }
        }
        None => root.clone(),
    };
    let candidate = if Path::new(raw_path).is_absolute() {
        PathBuf::from(raw_path)
    } else {
        base.join(raw_path)
    };
    for comp in Path::new(raw_path).components() {
        if matches!(comp, Component::ParentDir) {
            return Ok(None);
        }
    }
    let mut cur = candidate.clone();
    let mut tail = Vec::new();
    while !cur.exists() {
        ensure!(
            std::fs::symlink_metadata(&cur).is_err(),
            "refusing to write through a dangling symbolic link: {}",
            cur.display()
        );
        let Some(name_part) = cur.file_name() else {
            return Ok(None);
        };
        tail.push(name_part.to_os_string());
        let Some(parent) = cur.parent() else {
            return Ok(None);
        };
        cur = parent.to_path_buf();
    }
    let Ok(canon_cur) = cur.canonicalize() else {
        return Ok(None);
    };
    let mut full_target = canon_cur.clone();
    for part in tail.iter().rev() {
        full_target.push(part);
    }
    if full_target.starts_with(&root) {
        return Ok(None);
    }
    crate::database::check_source_access(&full_target)?;
    let (work_root, rel_path) = if let Some(ws) = explicit_ws
        && let Ok(canon_ws) = (if Path::new(ws).is_absolute() {
            PathBuf::from(ws)
        } else {
            root.join(ws)
        })
        .canonicalize()
        && canon_ws.is_dir()
        && full_target.starts_with(&canon_ws)
        && let Ok(rel) = full_target.strip_prefix(&canon_ws)
        && !rel.as_os_str().is_empty()
    {
        (canon_ws, rel.to_string_lossy().replace('\\', "/"))
    } else if canon_cur.is_dir() && !tail.is_empty() {
        let rel = tail
            .iter()
            .rev()
            .collect::<PathBuf>()
            .to_string_lossy()
            .replace('\\', "/");
        (canon_cur, rel)
    } else {
        let (Some(parent), Some(file)) = (full_target.parent(), full_target.file_name()) else {
            return Ok(None);
        };
        (
            parent.to_path_buf(),
            file.to_string_lossy().replace('\\', "/"),
        )
    };
    Ok(Some(OutsideEditTarget {
        display_path: full_target.to_string_lossy().into_owned(),
        work_root,
        rel_path,
    }))
}

fn outside_exec_or_index_paths(
    workspace: &std::path::Path,
    name: &str,
    args: &Value,
) -> Vec<String> {
    let root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut raw_candidates = Vec::new();
    match name {
        "exec_command" => {
            if let Some(w) = args["workdir"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                raw_candidates.push(w);
            }
        }
        "run_program" | "run_project_script" | "install_dependencies" => {
            if let Some(p) = args["project_path"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                raw_candidates.push(p);
            }
        }
        "sync_git"
        | "code_index__index"
        | "index_rust_workspace"
        | "index_ts_workspace"
        | "index_go_workspace"
        | "index_python_workspace" => {
            if let Some(w) = args["workspace_root"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                raw_candidates.push(w);
            }
        }
        _ => {}
    }
    let mut found = Vec::new();
    for raw in raw_candidates {
        let p = std::path::Path::new(raw);
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        };
        let resolved = full.canonicalize().unwrap_or(full);
        if !resolved.starts_with(&root) {
            let s = resolved.to_string_lossy().into_owned();
            if !found.contains(&s) {
                found.push(s);
            }
        }
    }
    found
}

fn outside_read_paths(
    workspace: &std::path::Path,
    extra_roots: &[std::path::PathBuf],
    name: &str,
    args: &Value,
) -> Vec<String> {
    let is_read = matches!(
        name,
        "list_dir"
            | "search_text"
            | "read_git"
            | "read_code_flow"
            | "workspace_info"
            | "git_status"
            | "git_diff"
            | "git_log"
            | "code_index__status"
            | "code_index__list"
            | "code_index__search"
            | "code_index__read"
            | "rust_index_status"
            | "ts_index_status"
            | "go_index_status"
            | "python_index_status"
            | "list_rust_symbols"
            | "list_ts_symbols"
            | "list_go_symbols"
            | "list_python_symbols"
            | "search_rust_symbols"
            | "search_ts_symbols"
            | "search_go_symbols"
            | "search_python_symbols"
    ) || crate::source_read::is_source_read(name);
    if !is_read {
        return Vec::new();
    }
    let root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let base = args["workspace_root"]
        .as_str()
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| std::path::PathBuf::from(raw.trim()));
    let outside = |path: std::path::PathBuf| {
        let resolved = path.canonicalize().unwrap_or(path);
        !resolved.starts_with(&root)
            && !extra_roots.iter().any(|extra| resolved.starts_with(extra))
    };
    let mut found = Vec::new();
    if let Some(base) = &base
        && outside(base.clone())
    {
        found.push(base.to_string_lossy().into_owned());
    }
    if !matches!(name, "read_git" | "read_code_flow") {
        for raw in args["path"].as_str().into_iter().chain(
            args["paths"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        ) {
            let path = std::path::Path::new(raw);
            let full = if path.is_absolute() {
                path.to_path_buf()
            } else {
                base.clone().unwrap_or_else(|| root.clone()).join(path)
            };
            if outside(full) && !found.iter().any(|known| known == raw) {
                found.push(raw.to_owned());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::SHELL_MODULE_ONLY_MESSAGE;

    #[test]
    fn shell_denial_message_describes_the_allow_shell_boundary() {
        assert!(SHELL_MODULE_ONLY_MESSAGE.contains("allow_shell=false"));
        assert!(!SHELL_MODULE_ONLY_MESSAGE.contains("@virtual helpers cannot run shell"));
    }
}
