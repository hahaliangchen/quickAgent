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
use host_tools::{business_tool, business_tool_inner};

/// Production loops have no step cap; tests pass small bounds of their own.
pub(crate) const NO_ROUND_LIMIT: usize = usize::MAX;

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
    let (final_state, external_tool_calls, virtual_receipts) =
        drive_vm_segment(s, &mut runtime, libs, &node, step, allow_shell).await?;
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
                    &virtual_receipts,
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
                        &virtual_receipts,
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
                    &virtual_receipts,
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
                &virtual_receipts,
            );
            w.program = Some(runtime);
            save_window(s, w).await?;
            Ok(false)
        }
        State::Failed(err) => {
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
/// Returns `(terminal_state, external_call_count, virtual_receipt_lines)`.
async fn drive_vm_segment(
    s: &Session<'_>,
    runtime: &mut Runtime,
    libs: &agent_program::libraries::Libraries,
    node: &str,
    step: usize,
    allow_shell: bool,
) -> Result<(State, usize, Vec<String>)> {
    let mut external_tool_calls = 0usize;
    let mut virtual_receipts = Vec::new();
    let mut inflight_virtual: tokio::task::JoinSet<
        Result<(String, Value, bool, virtual_frame::VirtualCallReceipt)>,
    > = tokio::task::JoinSet::new();

    loop {
        if s.cancel.is_cancelled() {
            inflight_virtual.abort_all();
            bail!("cancelled");
        }
        match runtime.next_concurrent() {
            ConcurrentState::Actions(actions) => {
                for action in actions {
                    let scope_vars = action.scope_variables().cloned();
                    match action.kind {
                        ActionKind::Tool { name, args } => {
                            external_tool_calls += 1;
                            let (result, is_error) = proxy_rust_tool(
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
                            runtime.resume_action(&action.id, result, is_error)?;
                        }
                        ActionKind::ParallelReads { operations } => {
                            external_tool_calls += operations.len();
                            let mut items = Vec::with_capacity(operations.len());
                            for (idx, (name, args)) in operations.into_iter().enumerate() {
                                let call_id = format!("{}-p{idx}", action.id);
                                let (res, is_err) = proxy_rust_tool(
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
                                items.push(json!({"failed": is_err, "value": res}));
                            }
                            runtime.resume_action(&action.id, Value::Array(items), false)?;
                        }
                        ActionKind::Call {
                            name,
                            args,
                            captured,
                        } => {
                            external_tool_calls += 1;
                            let spec = build_virtual_spec(
                                runtime,
                                &action.id,
                                &name,
                                args,
                                captured,
                                scope_vars.as_ref(),
                            );
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

                            inflight_virtual.spawn(async move {
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
                                Ok((action_id, val, is_err, receipt))
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
                let Some(joined) = inflight_virtual.join_next().await else {
                    return Ok((State::Waiting, external_tool_calls, virtual_receipts));
                };
                let (action_id, value, is_error, receipt) = joined??;
                virtual_receipts.push(receipt.format_line());
                runtime.resume_action(&action_id, value, is_error)?;
            }
            ConcurrentState::Returned(val) => {
                inflight_virtual.abort_all();
                return Ok((State::Returned(val), external_tool_calls, virtual_receipts));
            }
            ConcurrentState::Failed(err) => {
                inflight_virtual.abort_all();
                return Ok((State::Failed(err), external_tool_calls, virtual_receipts));
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

    let outcome = match tool_name {
        "read_tool_contracts" => Ok(json!({
            "declarations": agent_program::declarations::static_library_header(libs)
        })),
        "read_program_guide" => Ok(json!({
            "guide": agent_program::CAPABILITIES,
            "builtin_contracts": agent_program::BUILTIN_CONTRACTS,
        })),
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
            Ok(json!({"methods": matched}))
        }
        _ => {
            host_tools::business_tool_inner(s, node, tool_name, args, call_id, allow_shell, true)
                .await
        }
    };

    let (result, is_error) = match outcome {
        Ok(val) => (val, false),
        Err(err) => (json!({"error": format!("{err:#}")}), true),
    };

    append_event(
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

    Ok((result, is_error))
}
