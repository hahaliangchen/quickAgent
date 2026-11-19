//! Context-as-Code v2 agent driver (Step 1: minimal pure-JS + Rust proxy loop).
//!
//! - Model ABI: raw JavaScript continuation inside `async function executeTask()`,
//!   zero JSON Schema `tools`, `.d.ts` host library declarations in `system`,
//!   and `return <<<` (or native JS `return <expr>;`) to finish a turn.
//! - Host ABI: Rust JS VM (`crate::agent_program::Runtime`) traps tool calls
//!   via `ActionKind::Tool` and proxies them into `host_tools` with full
//!   permission, sandbox, and file-baseline enforcement.
//! - Context layout: immutable Append-Only code + `// =>` receipts above, and
//!   one-step `EPHEMERAL_PEEK` at the bottom (100% KV-cache friendly).
//! - Per-turn isolation: every user turn starts with a brand-new empty VM and
//!   destroys it immediately upon emitting the final `assistant` reply.

#![allow(dead_code)]

use crate::agent_program::{
    self, Runtime, State,
    runtime::{ActionKind, ConcurrentState},
};
use crate::agent_service::{self, AgentServiceState};
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use tokio_util::sync::CancellationToken;

mod context_doc;
mod external_changes;
mod flow_runtime;
mod host_folder;
mod host_tools;
mod managed_files;
mod model_call;
pub(crate) mod modules;
mod task_frames;
mod virtual_frame;
#[cfg(test)]
mod tests_step1;
#[cfg(test)]
mod tests_step2;
#[cfg(test)]
mod tests_step3;
#[cfg(test)]
mod tests_step4;

#[cfg(test)]
use host_tools::{business_tool, business_tool_inner};

/// Production loops have no step cap; tests pass small bounds of their own.
pub(crate) const NO_ROUND_LIMIT: usize = usize::MAX;

fn default_next_ref_id() -> usize {
    1
}

/// Durable runtime window for the v2 Context-as-Code driver.
#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct Window {
    #[serde(default)]
    pub(crate) context_doc: context_doc::ContextDocument,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) program: Option<Runtime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) program_task: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task_node_id: Option<String>,
    #[serde(default = "default_next_ref_id")]
    pub(crate) next_ref_id: usize,
}

/// Per-invocation host session context shared by the agent driver, host tool
/// backend, and flow libraries.
#[derive(Clone)]
pub(crate) struct Session<'a> {
    pub(super) agent_id: Option<String>,
    pub(super) task_node_id: Option<String>,
    pub(crate) plugin: Option<String>,
    pub(crate) state: &'a AgentServiceState,
    pub(crate) task: &'a str,
    pub(super) model: &'a str,
    pub(super) turn: usize,
    pub(super) cancel: &'a CancellationToken,
    pub(super) ban_execution: bool,
    pub(super) scope: String,
    pub(super) parent: Option<String>,
    pub(super) input_budget: usize,
}

impl Session<'_> {
    pub(super) fn delegate_id(&self) -> Option<&str> {
        self.agent_id.as_deref()
    }
}

async fn db<T: Send + 'static>(
    root: &Path,
    f: impl FnOnce(&mut rusqlite::Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    crate::db_writer::write_async(root, move |conn| {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_runtime_windows(
            task_id TEXT NOT NULL, scope TEXT NOT NULL, state TEXT NOT NULL,
            PRIMARY KEY(task_id,scope));",
        )?;
        f(conn)
    })
    .await
}

async fn save_window(s: &Session<'_>, w: &Window) -> Result<()> {
    let task = s.task.to_owned();
    let scope = s.scope.clone();
    let encoded = serde_json::to_string(w)?;
    db(s.state.workspace.root(), move |conn| {
        conn.execute(
            "INSERT OR REPLACE INTO agent_runtime_windows(task_id,scope,state) VALUES (?1,?2,?3)",
            params![task, scope, encoded],
        )?;
        Ok(())
    })
    .await
}

async fn append_event(s: &Session<'_>, kind: &str, mut data: Value, surface: bool) -> Result<i64> {
    if data.get("task_node_id").is_none()
        && let Some(id) = s.task_node_id.as_deref()
    {
        data["task_node_id"] = json!(id);
    }
    let task = s.task.to_owned();
    let kind = kind.to_owned();
    db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let seq = agent_service::append_event_tx(
            &tx,
            &task,
            &kind,
            &data,
            surface.then_some("append"),
        )?;
        tx.commit()?;
        Ok(seq)
    })
    .await
}

/// Static host library scope available to a compiled JS program / flow library.
pub(crate) fn library_compile_scope(
    s: &Session<'_>,
    w: &Window,
) -> agent_program::libraries::Libraries {
    let allow_shell = s.plugin.is_none()
        && !s.ban_execution
        && w.program_task
            .as_ref()
            .is_none_or(|t| t["role"].as_str() != Some("leaf"));
    let mut libs = agent_program::libraries::builtin();
    if !allow_shell {
        libs.remove("process");
    }
    if s.plugin.is_some() {
        for methods in libs.values_mut() {
            methods.retain(|_, tool| host_tools::page_allows(s, tool));
        }
        libs.retain(|_, methods| !methods.is_empty());
    }
    libs
}

/// Cancel any persisted in-flight JS VM runtimes when a task is stopped.
pub(crate) async fn cancel_saved_programs(state: &AgentServiceState, task: &str) -> Result<()> {
    let task = task.to_owned();
    db(state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let rows = tx
            .prepare("SELECT scope,state FROM agent_runtime_windows WHERE task_id=?1")?
            .query_map([&task], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (scope, raw) in rows {
            let Ok(mut window) = serde_json::from_str::<Window>(&raw) else {
                continue;
            };
            if let Some(program) = window.program.as_mut().filter(|p| !p.finished) {
                program.cancel();
                tx.execute(
                    "UPDATE agent_runtime_windows SET state=?3 WHERE task_id=?1 AND scope=?2",
                    params![task, scope, serde_json::to_string(&window)?],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

/// Primary agent entry point (convenience wrapper without explicit plugin resource).
pub(crate) async fn run_task(
    state: AgentServiceState,
    task: String,
    model: String,
    prompt: String,
    max_rounds: usize,
    cancel: CancellationToken,
    turn: usize,
    ban_execution: bool,
) -> Result<()> {
    run_task_with_resource(
        state,
        task,
        model,
        prompt,
        max_rounds,
        cancel,
        turn,
        ban_execution,
        None,
    )
    .await
}

/// Primary Context-as-Code v2 driver entry point (Step 1 minimal clean loop).
#[allow(unused_variables)]
pub(crate) async fn run_task_with_resource(
    state: AgentServiceState,
    task: String,
    model: String,
    prompt: String,
    max_rounds: usize,
    cancel: CancellationToken,
    turn: usize,
    ban_execution: bool,
    program_resource: Option<String>,
) -> Result<()> {
    let s = Session {
        agent_id: None,
        task_node_id: None,
        plugin: crate::plugin_sessions::owner(state.workspace.root(), &task)?,
        state: &state,
        task: &task,
        model: &model,
        turn,
        cancel: &cancel,
        ban_execution,
        scope: "main".into(),
        parent: None,
        input_budget: crate::model_settings::input_budget(&state, &model),
    };

    // Load prior conversation document (if any) and record rewind snapshot.
    let task_copy = task.clone();
    let saved_raw: Option<String> = db(state.workspace.root(), move |conn| {
        let main: Option<String> = conn
            .query_row(
                "SELECT state FROM agent_runtime_windows WHERE task_id=?1 AND scope='main'",
                [&task_copy],
                |r| r.get(0),
            )
            .optional()?;
        crate::rewind::snapshot(conn, &task_copy, turn, main.as_deref())?;
        Ok(main)
    })
    .await?;

    let mut w: Window = saved_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();

    // Emit durable user/message event.
    append_event(
        &s,
        "user/message",
        json!({
            "task_id": task,
            "turn": turn,
            "message": {
                "id": agent_service::uuid_like(),
                "role": "user",
                "content": [{"type": "text", "text": prompt}]
            }
        }),
        true,
    )
    .await?;

    // Every new user turn ALWAYS starts with a brand-new empty JS VM!
    let libs = library_compile_scope(&s, &w);
    w.program = Some(Runtime::new_repl(libs.clone())?);
    w.next_ref_id = 1;
    w.context_doc.begin_turn(turn, &prompt);
    save_window(&s, &w).await?;

    let allow_shell = s.plugin.is_none() && !s.ban_execution;

    for step in 1..=max_rounds {
        if cancel.is_cancelled() {
            w.program = None;
            save_window(&s, &w).await?;
            bail!("cancelled");
        }

        let reply = model_call::call_model(&s, &w, &libs, step).await?;
        #[cfg(test)]
        if std::env::var_os("LIVE_DEBUG_STEPS").is_some() {
            let head: String = reply.content.chars().take(260).collect();
            eprintln!("[LIVE step={step} finish={:?} len={}] head={head:?}", reply.finish_reason, reply.content.len());
        }
        let submission = crate::model_stream::parse_run_submission(
            &reply.content,
            reply.finish_reason.as_deref(),
        );

        match submission {
            crate::model_stream::RunSubmission::Rejected { code, message } => {
                let raw_preview: String = reply.content.chars().take(600).collect();
                append_event(
                    &s,
                    "program/source_invalid",
                    json!({
                        "actor": "agent",
                        "turn": s.turn,
                        "step": step,
                        "code": code,
                        "error": message,
                        "raw_preview": raw_preview,
                    }),
                    false,
                )
                .await?;
                w.context_doc.set_rejection(code, &message);
                save_window(&s, &w).await?;
            }

            crate::model_stream::RunSubmission::Reply { markdown } => {
                finish_turn_with_reply(&s, &mut w, step, markdown).await?;
                return Ok(());
            }

            crate::model_stream::RunSubmission::Run { source } => {
                if execute_js_step(&s, &mut w, &libs, step, allow_shell, source, None).await? {
                    return Ok(());
                }
            }
            crate::model_stream::RunSubmission::CodeAndReply { source, markdown } => {
                if execute_js_step(&s, &mut w, &libs, step, allow_shell, source, Some(markdown))
                    .await?
                {
                    return Ok(());
                }
            }
        }
    }

    // Reached max_rounds (mainly test cap); persist current state cleanly.
    save_window(&s, &w).await?;
    Ok(())
}

/// Execute one JS step (with optional trailing `return <<<` reply).
/// Returns `Ok(true)` if the turn finished and delivered the final reply.
async fn execute_js_step(
    s: &Session<'_>,
    w: &mut Window,
    libs: &agent_program::libraries::Libraries,
    step: usize,
    allow_shell: bool,
    source: String,
    pending_reply: Option<String>,
) -> Result<bool> {
    let Some(mut runtime) = w.program.take() else {
        bail!("turn VM missing");
    };
    runtime.install_static_libraries(libs)?;
    let pre_fps = runtime.user_binding_fingerprints();
    let log_start = runtime.logs_len();

    // Main turn allows declaring top-level `@virtual` functions (`is_virtual_leaf = false`).
    if let Err(compile_err) = runtime.prepare_segment(&source, libs.clone(), false) {
        let msg = format!("{compile_err:#}");
        append_event(
            s,
            "program/source_invalid",
            json!({
                "actor": "agent",
                "turn": s.turn,
                "step": step,
                "code": "COMPILE_ERROR",
                "error": msg,
                "source": source,
            }),
            false,
        )
        .await?;
        w.program = Some(runtime);
        w.context_doc.set_rejection("COMPILE_ERROR", &msg);
        save_window(s, w).await?;
        return Ok(false);
    }

    let node = format!("step_{}_{}", s.turn, step);
    if w.next_ref_id == 0 {
        w.next_ref_id = 1;
    }
    let (final_state, external_tool_calls, hints) =
        drive_vm_segment(s, &mut runtime, libs, &node, step, allow_shell, &mut w.next_ref_id)
            .await?;
    let logs = runtime.logs_since(log_start);

    match final_state {
        State::Returned(val) => {
            if runtime.segment_returned_explicitly() && external_tool_calls == 0 {
                let answer = match val {
                    Value::String(text) => text,
                    Value::Null => String::new(),
                    other => serde_json::to_string_pretty(&other).unwrap_or_default(),
                };
                w.context_doc.record_completed_segment(
                    source,
                    &runtime,
                    &pre_fps,
                    &logs,
                    None,
                    &hints,
                );
                finish_turn_with_reply(s, w, step, answer).await?;
                return Ok(true);
            }
            if let Some(reply_md) = pending_reply {
                if external_tool_calls == 0 {
                    // Pure in-memory JS projection + final reply: pin `// =>` and finish!
                    w.context_doc.record_completed_segment(
                        source,
                        &runtime,
                        &pre_fps,
                        &logs,
                        None,
                        &hints,
                    );
                    finish_turn_with_reply(s, w, step, reply_md).await?;
                    return Ok(true);
                }
                // First-Trap Truncation: external host tools/virtual calls ran, so the trailing
                // `return <<<` was written before observing real results — discard it!
                let note = format!(
                    "// [Host note: executed your JS above ({external_tool_calls} host/virtual call(s)) and discarded the premature `return <<<` so you can inspect the real result below before replying.]"
                );
                w.context_doc.record_completed_segment(
                    source,
                    &runtime,
                    &pre_fps,
                    &logs,
                    Some(note),
                    &hints,
                );
                w.program = Some(runtime);
                save_window(s, w).await?;
                return Ok(false);
            }
            let note = (runtime.segment_returned_explicitly() && external_tool_calls > 0).then(|| {
                format!(
                    "// [Host note: executed your JS above ({external_tool_calls} host/virtual call(s)); inspect the real output below before returning your final answer.]"
                )
            });
            w.context_doc.record_completed_segment(
                source,
                &runtime,
                &pre_fps,
                &logs,
                note,
                &hints,
            );
            w.program = Some(runtime);
            save_window(s, w).await?;
            Ok(false)
        }
        State::Failed(err) => {
            if agent_program::runtime::is_user_blocker(&err) {
                let question = err["question"]
                    .as_str()
                    .or_else(|| err["message"].as_str())
                    .map(str::trim)
                    .filter(|q| !q.is_empty())
                    .unwrap_or("需要你的确认后才能继续。")
                    .to_owned();
                w.context_doc
                    .record_paused_for_user(source, &runtime, &pre_fps, &logs, &hints);
                append_event(
                    s,
                    "agent/needs_user",
                    json!({
                        "actor": "agent",
                        "turn": s.turn,
                        "step": step,
                        "question": question,
                    }),
                    false,
                )
                .await?;
                finish_turn_with_reply(s, w, step, question).await?;
                return Ok(true);
            }
            w.context_doc
                .record_failed_segment(source, &runtime, &pre_fps, &logs, &err);
            w.program = Some(runtime);
            save_window(s, w).await?;
            Ok(false)
        }
        State::Action(_) | State::Waiting => {
            w.program = Some(runtime);
            save_window(s, w).await?;
            bail!("VM suspended in an unexpected waiting state");
        }
    }
}

async fn finish_turn_with_reply(
    s: &Session<'_>,
    w: &mut Window,
    step: usize,
    markdown: String,
) -> Result<()> {
    // Archive clean turn JS trace into `host_folder` (best-effort) when JS ran.
    if let Some(turn_js) = w.context_doc.render_turn_js(s.turn) {
        let _ = host_folder::write_program(
            s.state.workspace.root(),
            s.task,
            &format!("turn-{:02}", s.turn),
            &turn_js,
        );
    }
    // Strip EPHEMERAL_PEEK, pair `(user #N, assistant #N)`, and unconditionally destroy VM!
    w.context_doc.finish_turn(&markdown);
    w.program = None;
    append_event(
        s,
        "assistant/message",
        json!({
            "actor": "agent",
            "turn": s.turn,
            "step": step,
            "message": {
                "role": "assistant",
                "content": markdown,
            }
        }),
        true,
    )
    .await?;
    save_window(s, w).await
}

/// Execute one prepared JS segment in `runtime` until it returns or fails,
/// proxying host tool traps into Rust `host_tools` and spawning isolated
/// `@virtual` calls (including concurrent `Promise.all` fan-out via `JoinSet`).
/// Returns `(terminal_state, external_call_count, segment_context_hints)`.
async fn drive_vm_segment(
    s: &Session<'_>,
    runtime: &mut Runtime,
    libs: &agent_program::libraries::Libraries,
    node: &str,
    step: usize,
    allow_shell: bool,
    next_ref_id: &mut usize,
) -> Result<(State, usize, context_doc::SegmentContextHints)> {
    let mut external_tool_calls = 0usize;
    let mut hints = context_doc::SegmentContextHints::default();
    let mut spawned_ids = std::collections::BTreeSet::<String>::new();
    let mut inflight_jobs: tokio::task::JoinSet<
        Result<(String, Value, bool, Option<String>)>,
    > = tokio::task::JoinSet::new();

    loop {
        if s.cancel.is_cancelled() {
            inflight_jobs.abort_all();
            bail!("cancelled");
        }
        match runtime.next_concurrent() {
            ConcurrentState::Actions(actions) => {
                let inflight_set: std::collections::BTreeSet<String> = runtime
                    .inflight_actions()
                    .into_iter()
                    .map(|a| a.id)
                    .collect();
                for action in actions {
                    let is_async_inflight = inflight_set.contains(&action.id);
                    let scope_vars = action.scope_variables().cloned();
                    match action.kind {
                        ActionKind::Tool { name, args } => {
                            external_tool_calls += 1;
                            let (mut result, is_error) = proxy_rust_tool(
                                s,
                                libs,
                                node,
                                step,
                                &action.id,
                                &name,
                                args,
                                allow_shell,
                            )
                            .await?;
                            if !is_error {
                                context_doc::tag_resource_handle(&mut result, next_ref_id);
                            }
                            runtime.resume_action(&action.id, result, is_error)?;
                        }
                        ActionKind::ParallelReads { operations } => {
                            external_tool_calls += operations.len();
                            let mut items = Vec::with_capacity(operations.len());
                            for (idx, (name, args)) in operations.into_iter().enumerate() {
                                let call_id = format!("{}-p{idx}", action.id);
                                let (mut res, is_err) = proxy_rust_tool(
                                    s,
                                    libs,
                                    node,
                                    step,
                                    &call_id,
                                    &name,
                                    args,
                                    false,
                                )
                                .await?;
                                if !is_err {
                                    context_doc::tag_resource_handle(&mut res, next_ref_id);
                                }
                                items.push(json!({"failed": is_err, "value": res}));
                            }
                            runtime.resume_action(&action.id, Value::Array(items), false)?;
                        }
                        ActionKind::Require { library } => {
                            let (res, is_err) = flow_runtime::resolve_require(s, libs, &library);
                            runtime.resume_action(&action.id, res, is_err)?;
                        }
                        ActionKind::LibraryCall {
                            library,
                            method,
                            args,
                        } => {
                            external_tool_calls += 1;
                            if library == "host" || !is_async_inflight {
                                // Synchronous library call (e.g. `require("batch-files")` without `/promises`
                                // or `host.*`): MUST complete and resume BEFORE calling `next_concurrent()`.
                                let (val, is_err, receipt) = flow_runtime::execute_library_call(
                                    s,
                                    libs,
                                    &library,
                                    &method,
                                    args,
                                    step,
                                    allow_shell,
                                )
                                .await?;
                                if let Some(line) = receipt {
                                    hints.virtual_receipts.push(line);
                                }
                                runtime.resume_action(&action.id, val, is_err)?;
                                continue;
                            }
                            if !spawned_ids.insert(action.id.clone()) {
                                inflight_jobs.abort_all();
                                bail!("internal error: duplicate inflight library action {}", action.id);
                            }
                            if inflight_jobs.len() >= 64 {
                                inflight_jobs.abort_all();
                                bail!("CONCURRENCY_LIMIT: exceeded 64 concurrent inflight tasks");
                            }
                            let action_id = action.id.clone();
                            let state_owned = s.state.clone();
                            let task_owned = s.task.to_owned();
                            let model_owned = s.model.to_owned();
                            let cancel_owned = s.cancel.clone();
                            let plugin_owned = s.plugin.clone();
                            let scope_owned = s.scope.clone();
                            let libs_owned = libs.clone();
                            let turn = s.turn;
                            let ban_execution = s.ban_execution;
                            let input_budget = s.input_budget;

                            inflight_jobs.spawn(async move {
                                let parent_s = Session {
                                    agent_id: None,
                                    task_node_id: None,
                                    plugin: plugin_owned,
                                    state: &state_owned,
                                    task: &task_owned,
                                    model: &model_owned,
                                    turn,
                                    cancel: &cancel_owned,
                                    ban_execution,
                                    scope: scope_owned,
                                    parent: None,
                                    input_budget,
                                };
                                let (val, is_err, receipt) = flow_runtime::execute_library_call(
                                    &parent_s,
                                    &libs_owned,
                                    &library,
                                    &method,
                                    args,
                                    step,
                                    allow_shell,
                                )
                                .await?;
                                Ok((action_id, val, is_err, receipt))
                            });
                        }
                        ActionKind::Call {
                            name,
                            args,
                            captured,
                        } => {
                            external_tool_calls += 1;
                            for a in &args {
                                context_doc::collect_delegated_markers(
                                    a,
                                    &mut hints.delegated_refs,
                                    &mut hints.delegated_content_hashes,
                                );
                            }
                            for v in captured.values() {
                                context_doc::collect_delegated_markers(
                                    v,
                                    &mut hints.delegated_refs,
                                    &mut hints.delegated_content_hashes,
                                );
                            }
                            let spec = build_virtual_spec(
                                runtime,
                                &action.id,
                                &name,
                                args,
                                captured,
                                scope_vars.as_ref(),
                            );
                            if !is_async_inflight {
                                // Synchronous `@virtual` call (non-`async function`): execute & resume immediately!
                                let (val, is_err, receipt) = virtual_frame::execute_virtual_call(
                                    s,
                                    libs,
                                    spec,
                                    step,
                                )
                                .await?;
                                hints.virtual_receipts.push(receipt.format_line());
                                runtime.resume_action(&action.id, val, is_err)?;
                                continue;
                            }
                            if !spawned_ids.insert(action.id.clone()) {
                                inflight_jobs.abort_all();
                                bail!("internal error: duplicate inflight virtual action {}", action.id);
                            }
                            if inflight_jobs.len() >= 64 {
                                inflight_jobs.abort_all();
                                bail!("CONCURRENCY_LIMIT: exceeded 64 concurrent inflight tasks");
                            }
                            let action_id = spec.action_id.clone();
                            let state_owned = s.state.clone();
                            let task_owned = s.task.to_owned();
                            let model_owned = s.model.to_owned();
                            let cancel_owned = s.cancel.clone();
                            let plugin_owned = s.plugin.clone();
                            let scope_owned = s.scope.clone();
                            let libs_owned = libs.clone();
                            let turn = s.turn;
                            let ban_execution = s.ban_execution;
                            let input_budget = s.input_budget;

                            inflight_jobs.spawn(async move {
                                let parent_s = Session {
                                    agent_id: None,
                                    task_node_id: None,
                                    plugin: plugin_owned,
                                    state: &state_owned,
                                    task: &task_owned,
                                    model: &model_owned,
                                    turn,
                                    cancel: &cancel_owned,
                                    ban_execution,
                                    scope: scope_owned,
                                    parent: None,
                                    input_budget,
                                };
                                let (val, is_err, receipt) = virtual_frame::execute_virtual_call(
                                    &parent_s,
                                    &libs_owned,
                                    spec,
                                    step,
                                )
                                .await?;
                                Ok((action_id, val, is_err, Some(receipt.format_line())))
                            });
                        }
                        other => {
                            let err = json!({
                                "code": "UNSUPPORTED_ACTION",
                                "message": format!("Action {:?} is not yet wired", other),
                            });
                            runtime.resume_action(&action.id, err, true)?;
                        }
                    }
                }
            }
            ConcurrentState::Waiting => {
                let Some(joined) = inflight_jobs.join_next().await else {
                    return Ok((State::Waiting, external_tool_calls, hints));
                };
                let (action_id, value, is_error, receipt) = joined??;
                if let Some(line) = receipt {
                    hints.virtual_receipts.push(line);
                }
                runtime.resume_action(&action_id, value, is_error)?;
            }
            ConcurrentState::Returned(val) => {
                inflight_jobs.abort_all();
                return Ok((State::Returned(val), external_tool_calls, hints));
            }
            ConcurrentState::Failed(err) => {
                inflight_jobs.abort_all();
                return Ok((State::Failed(err), external_tool_calls, hints));
            }
        }
    }
}

fn build_virtual_spec(
    runtime: &Runtime,
    action_id: &str,
    fn_name: &str,
    args: Vec<Value>,
    mut captured: BTreeMap<String, Value>,
    scope_vars: Option<&BTreeMap<String, Value>>,
) -> virtual_frame::VirtualCallSpec {
    let fn_def = runtime.program.functions.get(fn_name);
    // Only merge `scope_vars` for partial `@virtual` functions that executed local
    // prefix JS statements inside their own body before suspending; for ordinary
    // `@virtual` declarations, `captured` already holds ONLY explicit `@uses` bindings.
    if fn_def.is_some_and(|f| f.partial_virtual)
        && let Some(vars) = scope_vars
    {
        for (k, v) in vars {
            captured.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    let param_names: Vec<String> = fn_def
        .map(|f| f.params.clone())
        .unwrap_or_else(|| (0..args.len()).map(|i| format!("arg{i}")).collect());
    let params: Vec<(String, Value)> = param_names
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), args.get(i).cloned().unwrap_or(Value::Null)))
        .collect();
    let async_prefix = if fn_def.is_some_and(|f| f.is_async) {
        "async "
    } else {
        ""
    };
    let signature = format!("{async_prefix}function {fn_name}({})", param_names.join(", "));
    let contract = fn_def
        .map(|f| f.description.trim().to_owned())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| format!("Complete `{fn_name}` and return a JSON object with a concise `summary` field."));
    virtual_frame::VirtualCallSpec {
        action_id: action_id.to_owned(),
        fn_name: fn_name.to_owned(),
        signature,
        contract,
        params,
        captures: captured,
    }
}

/// Proxy a single JS host-library call (`io.*`, `git.*`, `process.*`, `host.*`, ...)
/// into its Rust implementation, recording durable `tool/call` and `tool/result` events.
async fn proxy_rust_tool(
    s: &Session<'_>,
    libs: &agent_program::libraries::Libraries,
    node: &str,
    step: usize,
    call_id: &str,
    tool_name: &str,
    args: Value,
    allow_shell: bool,
) -> Result<(Value, bool)> {
    append_event(
        s,
        "tool/call",
        json!({
            "actor": "agent",
            "turn": s.turn,
            "step": step,
            "flowNodeId": node,
            "callId": call_id,
            "name": tool_name,
            "arguments": args.to_string(),
        }),
        false,
    )
    .await?;

    let args_snapshot = args.clone();
    let (mut result, is_error) = match tool_name {
        "ask_user" => {
            let question = args["question"]
                .as_str()
                .map(str::trim)
                .filter(|q| !q.is_empty());
            match question {
                Some(q) => (
                    json!({
                        "code": agent_program::runtime::NEEDS_USER,
                        "question": q,
                        "message": q,
                    }),
                    true,
                ),
                None => (
                    json!({"error": "ask_user requires a non-empty `question` string"}),
                    true,
                ),
            }
        }
        "read_asset" => match crate::context_assets::read(
            s.state.workspace.root(),
            s.task,
            &args,
            None,
        ) {
            Ok(val) => (val, false),
            Err(err) => (json!({"error": format!("{err:#}")}), true),
        },
        "read_session_history" => match crate::session_history::read(
            s.state.workspace.root(),
            s.task,
            &args,
        )
        .await
        {
            Ok(val) => (val, false),
            Err(err) => (json!({"error": format!("{err:#}")}), true),
        },
        "list_skills" => {
            let project = crate::project_notes::task_project(s.state.workspace.root(), s.task)
                .ok()
                .flatten()
                .unwrap_or_else(|| s.state.workspace.root().to_path_buf());
            let mut items: Vec<Value> = crate::flow_skills::list(&project)
                .into_iter()
                .map(|sk| json!({"name": sk.name, "description": sk.description}))
                .collect();
            for (name, desc) in crate::flow_libraries::list_libraries(s) {
                if !items.iter().any(|it| it["name"] == name) {
                    items.push(json!({"name": name, "description": desc, "library": true}));
                }
            }
            (Value::Array(items), false)
        }
        "read_skill" => {
            let name = args["name"].as_str().unwrap_or("");
            let project = crate::project_notes::task_project(s.state.workspace.root(), s.task)
                .ok()
                .flatten()
                .unwrap_or_else(|| s.state.workspace.root().to_path_buf());
            match crate::flow_skills::read(&project, name) {
                Some(Ok(val)) => (val, false),
                Some(Err(err)) => (json!({"error": format!("{err:#}")}), true),
                None => (json!({"error": format!("skill '{name}' not found")}), true),
            }
        }
        "read_tool_contracts" => (
            json!({
                "declarations": agent_program::declarations::static_library_header(libs)
            }),
            false,
        ),
        "read_program_guide" => (
            json!({
                "guide": agent_program::CAPABILITIES,
                "builtin_contracts": agent_program::BUILTIN_CONTRACTS,
            }),
            false,
        ),
        "find_library_methods" => {
            let query = args["query"].as_str().unwrap_or("").to_ascii_lowercase();
            let mut matched = Vec::new();
            for (lib, methods) in libs {
                for (method, target) in methods {
                    if query.is_empty()
                        || lib.contains(&query)
                        || method.to_ascii_lowercase().contains(&query)
                        || target.to_ascii_lowercase().contains(&query)
                    {
                        matched.push(json!({"library": lib, "method": method, "tool": target}));
                    }
                }
            }
            for (lib, desc) in crate::flow_libraries::list_libraries(s) {
                for m in crate::flow_libraries::public_method_names(s, &lib).unwrap_or_default() {
                    if query.is_empty()
                        || lib.to_ascii_lowercase().contains(&query)
                        || m.to_ascii_lowercase().contains(&query)
                        || desc.to_ascii_lowercase().contains(&query)
                    {
                        matched.push(json!({"library": lib, "method": m, "orchestration": true, "description": desc}));
                    }
                }
            }
            (json!({"methods": matched}), false)
        }
        _ => match host_tools::business_tool_inner(
            s,
            node,
            tool_name,
            args,
            call_id,
            allow_shell,
            true,
        )
        .await
        {
            Ok(val) => (val, false),
            Err(err) => (json!({"error": format!("{err:#}")}), true),
        },
    };

    let result_seq = append_event(
        s,
        "tool/result",
        json!({
            "actor": "agent",
            "turn": s.turn,
            "step": step,
            "flowNodeId": node,
            "message": {
                "role": "tool",
                "toolCallId": call_id,
                "isError": is_error,
                "content": [{"type": "text", "text": result.to_string()}],
            },
            "meta": {"result": result},
        }),
        false,
    )
    .await?;

    // Automatically archive large (> 24KB) material results into durable content-addressed
    // context assets so future turns (after VM destruction) can inspect any slice via
    // `session.read_asset({ asset_id, char_offset, max_chars })`.
    if !is_error
        && crate::context_assets::is_material_tool(tool_name)
        && result.to_string().chars().count() > context_doc::PEEK_FULL_LIMIT_CHARS
        && let Ok(reference) = crate::context_assets::register(
            s.state.workspace.root(),
            s.task,
            result_seq,
            tool_name,
            &args_snapshot,
            &result,
        )
        && let Some(asset_id) = reference["asset_id"].as_str()
        && let Some(obj) = result.as_object_mut()
    {
        obj.insert("__asset_id".into(), Value::String(asset_id.to_owned()));
    }

    Ok((result, is_error))
}
