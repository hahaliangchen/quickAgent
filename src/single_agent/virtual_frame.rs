//! Isolated, isomorphic `@virtual` function frame runner (Context-as-Code v2).
//!
//! Core properties:
//! - **Strict Stack-Frame Isolation**: Child frame sees ONLY its own function
//!   signature, `@virtual` contract, bound parameters, `@uses` captures, and
//!   the parent's regular host libraries and execution boundary. It NEVER
//!   inherits outer conversation history, `MAIN_PROMPT`, or `session.*`.
//! - **Isomorphic Engine**: Inside the isolated frame, the model writes raw JS
//!   with the exact same `.d.ts` + Rust proxy + `// =>` + `EPHEMERAL_PEEK` loop
//!   as the outer driver, finishing with native JS `return <json_value>;`.
//! - **Stack-Frame GC + Factual Receipt**: As soon as the child returns, its
//!   entire `Runtime` and `ContextDocument` are dropped, and a compact factual
//!   `VirtualCallReceipt` (steps, files read/edited, returned summary) is given
//!   to the parent trace so the outer AI always knows what the virtual call did.

use super::{
    Session,
    context_doc::{self, ContextDocument, INLINE_LIMIT_CHARS, SegmentContextHints},
    model_call,
    tool_executor::ToolExecutor,
};
use crate::agent_program::{Runtime, State, libraries::Libraries, runtime::ActionKind};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(super) struct VirtualCallSpec {
    pub action_id: String,
    pub fn_name: String,
    pub signature: String,
    pub contract: String,
    pub params: Vec<(String, Value)>,
    pub captures: BTreeMap<String, Value>,
    pub partial_locals: Vec<(String, Value)>,
}

#[derive(Clone, Debug)]
pub(super) struct VirtualCallReceipt {
    pub fn_name: String,
    pub args_preview: String,
    pub steps: usize,
    pub files_read: Vec<String>,
    pub files_written: Vec<String>,
    pub returned_preview: String,
    pub failed: bool,
}

impl VirtualCallReceipt {
    pub(super) fn format_line(&self) -> String {
        let mut parts = vec![format!("{} step(s)", self.steps)];
        if !self.files_read.is_empty() {
            parts.push(format!("read: {}", self.files_read.join(", ")));
        }
        if !self.files_written.is_empty() {
            parts.push(format!("edited: {}", self.files_written.join(", ")));
        }
        if self.failed {
            parts.push(format!("failed: {}", self.returned_preview));
        } else {
            parts.push(format!("returned: {}", self.returned_preview));
        }
        format!(
            "// => [virtual {}({}) | {}]",
            self.fn_name,
            self.args_preview,
            parts.join(" | ")
        )
    }
}

/// Build the child library catalog. The child keeps the parent's regular tools,
/// including process and plugin libraries, while session state and delegation
/// are reserved for the parent frame.
pub(super) fn leaf_libraries(parent_libs: &Libraries) -> Libraries {
    let mut libs = parent_libs.clone();
    libs.remove("session");
    libs.remove("previous");
    libs
}

/// Execute one isolated `@virtual` function call to completion, emitting
/// frontend-compatible collapsible frame events (`create_task` + `task/node_*`)
/// and destroying its entire VM & context frame upon returning.
pub(super) async fn execute_virtual_call(
    parent_session: &Session<'_>,
    parent_libs: &Libraries,
    spec: VirtualCallSpec,
    parent_step: usize,
) -> Result<(Value, bool, VirtualCallReceipt)> {
    let call_id = spec.action_id.clone();
    let fn_name = spec.fn_name.clone();
    let contract = spec.contract.clone();
    let args_preview = format_args_preview(&spec.params);

    // Emit frontend collapsible `@virtual` call card + running frame marker so
    // child `assistant/delta` and `tool/call` events nest cleanly inside it.
    let _ = super::append_event(
        parent_session,
        "tool/call",
        json!({
            "actor": "agent",
            "turn": parent_session.turn,
            "step": parent_step,
            "callId": call_id,
            "name": "create_task",
            "arguments": json!({
                "program_task": {
                    "name": format!("{fn_name}({args_preview})"),
                    "description": contract,
                    "module": false,
                }
            }).to_string(),
        }),
        false,
    )
    .await;
    let _ = super::append_event(
        parent_session,
        "task/node_running",
        json!({
            "node_id": call_id,
            "task_node_id": call_id,
            "status": "running",
            "metadata": {
                "role": "temp",
                "caller": {"call_id": call_id},
            },
        }),
        false,
    )
    .await;

    let outcome = run_virtual_loop(parent_session, parent_libs, spec, parent_step).await;

    match &outcome {
        Ok((val, failed, _)) => {
            let status = if *failed { "failed" } else { "done" };
            let kind = if *failed { "task/node_failed" } else { "task/node_done" };
            let text = val.to_string();
            let result_payload = if *failed {
                json!({"node_id": call_id, "error": text})
            } else {
                json!({"node_id": call_id, "result": text})
            };
            let _ = super::append_event(
                parent_session,
                kind,
                json!({
                    "node_id": call_id,
                    "task_node_id": call_id,
                    "status": status,
                    "metadata": {"role": "temp", "caller": {"call_id": call_id}},
                    "result": result_payload,
                }),
                false,
            )
            .await;
            let _ = super::append_event(
                parent_session,
                "tool/result",
                json!({
                    "actor": "agent",
                    "turn": parent_session.turn,
                    "step": parent_step,
                    "message": {
                        "role": "tool",
                        "toolCallId": call_id,
                        "isError": failed,
                        "content": [{"type": "text", "text": result_payload.to_string()}],
                    },
                }),
                false,
            )
            .await;
        }
        Err(err) => {
            let msg = format!("{err:#}");
            let result_payload = json!({"node_id": call_id, "error": msg});
            let _ = super::append_event(
                parent_session,
                "task/node_failed",
                json!({
                    "node_id": call_id,
                    "task_node_id": call_id,
                    "status": "failed",
                    "metadata": {"role": "temp", "caller": {"call_id": call_id}},
                    "result": result_payload,
                }),
                false,
            )
            .await;
            let _ = super::append_event(
                parent_session,
                "tool/result",
                json!({
                    "actor": "agent",
                    "turn": parent_session.turn,
                    "step": parent_step,
                    "message": {
                        "role": "tool",
                        "toolCallId": call_id,
                        "isError": true,
                        "content": [{"type": "text", "text": result_payload.to_string()}],
                    },
                }),
                false,
            )
            .await;
        }
    }

    outcome
}

async fn run_virtual_loop(
    parent_session: &Session<'_>,
    parent_libs: &Libraries,
    spec: VirtualCallSpec,
    parent_step: usize,
) -> Result<(Value, bool, VirtualCallReceipt)> {
    let leaf_libs = leaf_libraries(parent_libs);
    let args_preview = format_args_preview(&spec.params);

    // Optional heterogeneous routing: if `delegate_agent_settings` configures a
    // dedicated lightweight model/provider for sub-frames, use it; otherwise inherit.
    let child_state;
    let child_model: String;
    let delegate_cfg = &parent_session.state.delegate_agent_settings;
    let state_ref = if delegate_cfg.provider.is_some() || delegate_cfg.model.is_some() {
        match parent_session
            .state
            .delegate_agent_settings
            .resolve(parent_session.state, parent_session.model)
        {
            Ok(resolved) => {
                child_model = resolved.default_model.clone();
                child_state = resolved;
                &child_state
            }
            Err(_) => {
                child_model = parent_session.model.to_owned();
                parent_session.state
            }
        }
    } else {
        child_model = parent_session.model.to_owned();
        parent_session.state
    };

    let child_session = Session {
        agent_id: None,
        task_node_id: Some(spec.action_id.clone()),
        plugin: parent_session.plugin.clone(),
        state: state_ref,
        task: parent_session.task,
        model: &child_model,
        turn: parent_session.turn,
        cancel: parent_session.cancel,
        ban_execution: parent_session.ban_execution,
        scope: format!("virtual:{}:{}", spec.fn_name, spec.action_id),
        parent: Some(parent_session.scope.clone()),
        input_budget: crate::model_settings::input_budget(state_ref, &child_model),
    };

    // Pre-populate child VM scope with bound parameters, `@uses` captures,
    // and any local variables precomputed by a partial `@virtual` JS prefix.
    let mut initial_bindings: BTreeMap<String, Value> = spec.captures.clone();
    for (name, val) in &spec.params {
        initial_bindings.insert(name.clone(), val.clone());
    }
    for (name, val) in &spec.partial_locals {
        initial_bindings.insert(name.clone(), val.clone());
    }
    let mut runtime = Runtime::new_repl_with_scope(leaf_libs.clone(), initial_bindings)?;

    // Build a completely isolated single-turn ContextDocument for this frame ONLY.
    let mut doc = ContextDocument::default();
    doc.begin_virtual_frame(
        &spec.signature,
        &spec.contract,
        &spec.params,
        &spec.captures,
        &spec.partial_locals,
    );

    let mut files_read = BTreeSet::new();
    let mut files_written = BTreeSet::new();
    let mut next_ref_id = 1usize;

    for sub_step in 1usize.. {
        if child_session.cancel.is_cancelled() {
            bail!("cancelled");
        }

        let reply = model_call::call_virtual_model(
            &child_session,
            &doc,
            &leaf_libs,
            parent_step,
            sub_step,
            &spec.action_id,
        )
        .await?;

        let submission = model_call::parse_virtual_submission(&reply);
        let assistant_msg = Some(reply.assistant_message.clone());
        let call_id = reply.primary_call_id();

        let source = match submission {
            crate::model_stream::RunSubmission::Reply { .. } => {
                doc.record_rejection(
                    assistant_msg,
                    call_id,
                    "VIRTUAL_TOOL_REQUIRED",
                    "Inside a `@virtual` function, call `run_js({ code })` and finish with native JS `return { ..., summary: \"...\" };`.",
                );
                continue;
            }
            crate::model_stream::RunSubmission::Rejected { code, message } => {
                doc.record_rejection(assistant_msg, call_id, code, &message);
                continue;
            }
            crate::model_stream::RunSubmission::CodeAndReply { source, .. } => source,
            crate::model_stream::RunSubmission::Run { source } => source,
        };

        runtime.install_static_libraries(&leaf_libs)?;
        let pre_fps = runtime.user_binding_fingerprints();
        let log_start = runtime.logs_len();

        // `is_virtual_leaf = true` strictly enforces VIRTUAL_LAYER_LIMIT (no nested @virtual!).
        if let Err(compile_err) = runtime.prepare_segment(&source, leaf_libs.clone(), true) {
            let msg = format!("{compile_err:#}");
            let code = if msg.contains("VIRTUAL_LAYER_LIMIT") {
                "VIRTUAL_LAYER_LIMIT"
            } else {
                "COMPILE_ERROR"
            };
            doc.record_rejection(assistant_msg, call_id, code, &msg);
            continue;
        }

        let node = format!("v_{}_{}_{}", spec.fn_name, parent_step, sub_step);
        let (state, _ext_calls) = drive_leaf_vm(
            &child_session,
            &mut runtime,
            &leaf_libs,
            &node,
            sub_step,
            &mut next_ref_id,
            &mut files_read,
            &mut files_written,
        )
        .await?;
        let logs = runtime.logs_since(log_start);

        match state {
            State::Returned(val) => {
                if runtime.segment_returned_explicitly() {
                    if let Some(task_err) = val.get("$task_error") {
                        let receipt = VirtualCallReceipt {
                            fn_name: spec.fn_name,
                            args_preview,
                            steps: sub_step,
                            files_read: files_read.into_iter().collect(),
                            files_written: files_written.into_iter().collect(),
                            returned_preview: compact_value_preview(task_err, 220),
                            failed: true,
                        };
                        return Ok((task_err.clone(), true, receipt));
                    }
                    let receipt = VirtualCallReceipt {
                        fn_name: spec.fn_name,
                        args_preview,
                        steps: sub_step,
                        files_read: files_read.into_iter().collect(),
                        files_written: files_written.into_iter().collect(),
                        returned_preview: compact_value_preview(&val, 260),
                        failed: false,
                    };
                    // `runtime` and `doc` drop right here -> instant Stack-Frame GC!
                    return Ok((val, false, receipt));
                }
                doc.record_completed_segment(
                    source,
                    &runtime,
                    &pre_fps,
                    &logs,
                    None,
                    &SegmentContextHints::default(),
                    assistant_msg,
                    call_id,
                );
            }
            State::Failed(err) => {
                // Explicit `throw new Error(...)` or unhandled error in child frame
                // can either be recovered in the next sub-step or propagated if fatal.
                doc.record_failed_segment(
                    source,
                    &runtime,
                    &pre_fps,
                    &logs,
                    &err,
                    assistant_msg,
                    call_id,
                );
            }
            State::Action(_) | State::Waiting => {
                doc.record_rejection(
                    assistant_msg,
                    call_id,
                    "VIRTUAL_UNSUPPORTED_SUSPENSION",
                    "Nested async virtual calls are forbidden inside `@virtual` frames.",
                );
            }
        }
    }
    unreachable!("unbounded worker loop exits only via return or cancellation")
}

async fn drive_leaf_vm(
    s: &Session<'_>,
    runtime: &mut Runtime,
    leaf_libs: &Libraries,
    node: &str,
    step: usize,
    next_ref_id: &mut usize,
    files_read: &mut BTreeSet<String>,
    files_written: &mut BTreeSet<String>,
) -> Result<(State, usize)> {
    let mut external_calls = 0usize;
    loop {
        if s.cancel.is_cancelled() {
            bail!("cancelled");
        }
        match runtime.next() {
            State::Action(action) => match action.kind {
                ActionKind::Tool { name, args } => {
                    external_calls += 1;
                    record_file_touch(&name, &args, files_read, files_written);
                    let (mut result, is_error) = if name == "ask_user" {
                        (
                            json!({
                                "code": "VIRTUAL_ASK_USER_FORBIDDEN",
                                "error": "`host.ask_user` is forbidden inside `@virtual` frames; return `{ \"$task_error\": \"...\" }` to the caller instead."
                            }),
                            true,
                        )
                    } else {
                        ToolExecutor::new(s).proxy_vm_tool(
                            leaf_libs,
                            node,
                            step,
                            &action.id,
                            &name,
                            args,
                            !s.ban_execution,
                        )
                        .await?
                    };
                    if !is_error {
                        context_doc::tag_resource_handle(&mut result, next_ref_id);
                    }
                    runtime.resume(result, is_error)?;
                }
                ActionKind::ParallelReads { operations } => {
                    external_calls += operations.len();
                    for (name, args) in &operations {
                        record_file_touch(name, args, files_read, files_written);
                    }
                    let base_id = action.id.clone();
                    let futs = operations.into_iter().enumerate().map(|(idx, (name, args))| {
                        let call_id = format!("{base_id}-p{idx}");
                        async move {
                            ToolExecutor::new(s).proxy_vm_tool(
                                leaf_libs,
                                node,
                                step,
                                &call_id,
                                &name,
                                args,
                                !s.ban_execution,
                            )
                            .await
                        }
                    });
                    let results = futures::future::try_join_all(futs).await?;
                    let mut items = Vec::with_capacity(results.len());
                    for (mut res, is_err) in results {
                        if !is_err {
                            context_doc::tag_resource_handle(&mut res, next_ref_id);
                        }
                        items.push(json!({"failed": is_err, "value": res}));
                    }
                    runtime.resume(Value::Array(items), false)?;
                }
                ActionKind::Require { library } => {
                    let base = library.strip_suffix("/promises").unwrap_or(&library);
                    if let Some(methods) = leaf_libs.get(base) {
                        runtime.resume(json!({ "methods": methods }), false)?;
                    } else {
                        runtime.resume(
                            json!({
                                "code": "VIRTUAL_LIBRARY_FORBIDDEN",
                                "error": format!("Library `{library}` is unavailable inside `@virtual` frames."),
                            }),
                            true,
                        )?;
                    }
                }
                ActionKind::LibraryCall { library, method, .. }
                    if library == "host" && method == "delegate" =>
                {
                    runtime.resume(
                        json!({
                            "code": "NESTED_DELEGATE_FORBIDDEN",
                            "error": "host.delegate is only available in the parent frame; nested delegation is forbidden inside a virtual worker."
                        }),
                        true,
                    )?;
                }
                ActionKind::Call { name, .. } | ActionKind::Task { name, .. } => {
                    runtime.resume(
                        json!({
                            "code": "VIRTUAL_LAYER_LIMIT",
                            "error": format!("Cannot invoke nested @virtual function `{name}` from inside a @virtual frame.")
                        }),
                        true,
                    )?;
                }
                other => {
                    runtime.resume(
                        json!({
                            "code": "VIRTUAL_UNSUPPORTED_ACTION",
                            "error": format!("Unsupported action inside @virtual frame: {:?}", other),
                        }),
                        true,
                    )?;
                }
            },
            terminal => return Ok((terminal, external_calls)),
        }
    }
}

fn record_file_touch(
    tool_name: &str,
    args: &Value,
    files_read: &mut BTreeSet<String>,
    files_written: &mut BTreeSet<String>,
) {
    let Some(path) = args["path"].as_str().filter(|p| !p.is_empty()) else {
        return;
    };
    match tool_name {
        "read_file" | "read_file_lines" | "read_file_chunk" => {
            files_read.insert(path.to_owned());
        }
        name if crate::tool_registry::ToolRegistry::records_written_path(name) => {
            files_written.insert(path.to_owned());
        }
        _ => {}
    }
}

fn format_args_preview(params: &[(String, Value)]) -> String {
    params
        .iter()
        .map(|(_, v)| format_single_arg_preview(v))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_single_arg_preview(value: &Value) -> String {
    if let Some(obj) = value.as_object() {
        // First-class Lazy Resource Handle (`{ __ref, path, ... }` or `{ __ref, exit_code, ... }`)
        if let Some(ref_id) = obj.get("__ref").and_then(Value::as_str) {
            if let Some(path) = obj.get("path").and_then(Value::as_str) {
                return format!("{ref_id}({path:?})");
            }
            if let Some(code) = obj.get("exit_code") {
                return format!("{ref_id}(exit={code})");
            }
            return ref_id.to_owned();
        }
        // Ad-hoc `{ file/path, content }` object with large content -> never leak raw file text into receipt!
        if let Some(content) = obj.get("content").and_then(Value::as_str)
            && content.chars().count() > 80
            && let Some(path) = obj
                .get("path")
                .or_else(|| obj.get("file"))
                .and_then(Value::as_str)
        {
            return format!("{{ file: {path:?}, content: <{}c> }}", content.chars().count());
        }
    }
    compact_value_preview(value, 48)
}

fn compact_value_preview(value: &Value, max_chars: usize) -> String {
    // Prefer a concise `summary` field if present on returned object!
    if let Some(obj) = value.as_object()
        && let Some(summary) = obj.get("summary").and_then(Value::as_str)
        && value.to_string().chars().count() > max_chars
    {
        let flat = summary.replace(['\r', '\n'], " ");
        let trimmed: String = flat.chars().take(max_chars).collect();
        return format!("{{ summary: {trimmed:?}, ... }}");
    }
    let raw = match value {
        Value::String(s) => format!("{s:?}"),
        other => other.to_string(),
    };
    let flat = raw.replace(['\r', '\n'], " ");
    let cap = max_chars.min(INLINE_LIMIT_CHARS);
    if flat.chars().count() <= cap {
        flat
    } else {
        let mut s: String = flat.chars().take(cap).collect();
        s.push_str("...");
        s
    }
}
