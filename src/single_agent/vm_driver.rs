//! VM segment execution loop (`drive_vm_segment`) and `@virtual` call spec builder.
//!
//! Proxies host tool traps via `ToolExecutor`, resolves `require()`
//! and `ActionKind::LibraryCall` via `flow_runtime`, and spawns truly concurrent
//! async `@virtual` / library calls into a bounded `tokio::task::JoinSet`.

use super::{Session, context_doc, flow_runtime, tool_executor::ToolExecutor, virtual_frame};
use crate::agent_program::{
    self, Runtime, State,
    runtime::{ActionKind, ConcurrentState},
};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_INFLIGHT_JOBS: usize = 64;

/// Execute one prepared JS segment in `runtime` until it returns or fails.
/// Returns `(terminal_state, external_call_count, segment_context_hints)`.
pub(super) async fn drive_vm_segment(
    s: &Session<'_>,
    tools: &ToolExecutor<'_, '_>,
    runtime: &mut Runtime,
    libs: &agent_program::libraries::Libraries,
    node: &str,
    step: usize,
    allow_shell: bool,
    next_ref_id: &mut usize,
    file_refs: &mut BTreeMap<String, (u64, String)>,
) -> Result<(State, usize, context_doc::SegmentContextHints)> {
    let mut external_tool_calls = 0usize;
    let mut hints = context_doc::SegmentContextHints::default();
    let mut spawned_ids = BTreeSet::<String>::new();
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
                let inflight_set: BTreeSet<String> = runtime
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
                            let (mut result, is_error) = tools.proxy_vm_tool(
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
                                context_doc::tag_resource_handle_in_turn(
                                    &mut result,
                                    next_ref_id,
                                    file_refs,
                                );
                            }
                            runtime.resume_action(&action.id, result, is_error)?;
                        }
                        ActionKind::ParallelReads { operations } => {
                            external_tool_calls += operations.len();
                            let base_id = action.id.clone();
                            let futs = operations.into_iter().enumerate().map(|(idx, (name, args))| {
                                let call_id = format!("{base_id}-p{idx}");
                                async move {
                                    tools.proxy_vm_tool(
                                        libs,
                                        node,
                                        step,
                                        &call_id,
                                        &name,
                                        args,
                                        allow_shell,
                                    )
                                    .await
                                }
                            });
                            let results = futures::future::try_join_all(futs).await?;
                            let mut items = Vec::with_capacity(results.len());
                            for (mut res, is_err) in results {
                                if !is_err {
                                    context_doc::tag_resource_handle_in_turn(
                                        &mut res,
                                        next_ref_id,
                                        file_refs,
                                    );
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
                            if library == "host" && method == "delegate" {
                                let spec = match build_delegate_spec(&action.id, args, libs) {
                                    Ok(spec) => spec,
                                    Err(error) => {
                                        runtime.resume_action(
                                            &action.id,
                                            json!({"code": "INVALID_DELEGATE", "message": format!("{error:#}")}),
                                            true,
                                        )?;
                                        continue;
                                    }
                                };
                                for (_, value) in &spec.params {
                                    context_doc::collect_delegated_markers(
                                        value,
                                        &mut hints.delegated_refs,
                                        &mut hints.delegated_content_hashes,
                                    );
                                }
                                if !is_async_inflight {
                                    let (value, is_error, receipt) =
                                        virtual_frame::execute_virtual_call(s, libs, spec, step)
                                            .await?;
                                    hints.virtual_receipts.push(receipt.format_line());
                                    runtime.resume_action(&action.id, value, is_error)?;
                                    continue;
                                }
                                guard_spawn(&mut spawned_ids, &mut inflight_jobs, &action.id)?;
                                let action_id = action.id.clone();
                                let owned = OwnedSession::from_parent(s, libs);
                                inflight_jobs.spawn(async move {
                                    let parent_s = owned.as_session();
                                    let (value, is_error, receipt) =
                                        virtual_frame::execute_virtual_call(
                                            &parent_s,
                                            &owned.libs,
                                            spec,
                                            step,
                                        )
                                        .await?;
                                    Ok((action_id, value, is_error, Some(receipt.format_line())))
                                });
                                continue;
                            }
                            if library == "host" || !is_async_inflight {
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
                            guard_spawn(&mut spawned_ids, &mut inflight_jobs, &action.id)?;
                            let action_id = action.id.clone();
                            let owned = OwnedSession::from_parent(s, libs);
                            inflight_jobs.spawn(async move {
                                let parent_s = owned.as_session();
                                let (val, is_err, receipt) = flow_runtime::execute_library_call(
                                    &parent_s,
                                    &owned.libs,
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
                                let (val, is_err, receipt) = virtual_frame::execute_virtual_call(
                                    s, libs, spec, step,
                                )
                                .await?;
                                hints.virtual_receipts.push(receipt.format_line());
                                runtime.resume_action(&action.id, val, is_err)?;
                                continue;
                            }
                            guard_spawn(&mut spawned_ids, &mut inflight_jobs, &action.id)?;
                            let action_id = spec.action_id.clone();
                            let owned = OwnedSession::from_parent(s, libs);
                            inflight_jobs.spawn(async move {
                                let parent_s = owned.as_session();
                                let (val, is_err, receipt) = virtual_frame::execute_virtual_call(
                                    &parent_s,
                                    &owned.libs,
                                    spec,
                                    step,
                                )
                                .await?;
                                Ok((action_id, val, is_err, Some(receipt.format_line())))
                            });
                        }
                        ActionKind::Task {
                            name,
                            description,
                            input,
                        } => {
                            external_tool_calls += 1;
                            let spec = build_virtual_spec_for_task(
                                runtime,
                                &action.id,
                                &name,
                                &description,
                                input,
                                scope_vars.as_ref(),
                            );
                            for (_, v) in &spec.params {
                                context_doc::collect_delegated_markers(
                                    v,
                                    &mut hints.delegated_refs,
                                    &mut hints.delegated_content_hashes,
                                );
                            }
                            for v in spec.captures.values() {
                                context_doc::collect_delegated_markers(
                                    v,
                                    &mut hints.delegated_refs,
                                    &mut hints.delegated_content_hashes,
                                );
                            }
                            for (_, v) in &spec.partial_locals {
                                context_doc::collect_delegated_markers(
                                    v,
                                    &mut hints.delegated_refs,
                                    &mut hints.delegated_content_hashes,
                                );
                            }
                            if !is_async_inflight {
                                let (val, is_err, receipt) = virtual_frame::execute_virtual_call(
                                    s, libs, spec, step,
                                )
                                .await?;
                                hints.virtual_receipts.push(receipt.format_line());
                                runtime.resume_action(&action.id, val, is_err)?;
                                continue;
                            }
                            guard_spawn(&mut spawned_ids, &mut inflight_jobs, &action.id)?;
                            let action_id = spec.action_id.clone();
                            let owned = OwnedSession::from_parent(s, libs);
                            inflight_jobs.spawn(async move {
                                let parent_s = owned.as_session();
                                let (val, is_err, receipt) = virtual_frame::execute_virtual_call(
                                    &parent_s,
                                    &owned.libs,
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

fn guard_spawn(
    spawned_ids: &mut BTreeSet<String>,
    inflight_jobs: &mut tokio::task::JoinSet<Result<(String, Value, bool, Option<String>)>>,
    action_id: &str,
) -> Result<()> {
    if !spawned_ids.insert(action_id.to_owned()) {
        inflight_jobs.abort_all();
        bail!("internal error: duplicate inflight action {action_id}");
    }
    if inflight_jobs.len() >= MAX_INFLIGHT_JOBS {
        inflight_jobs.abort_all();
        bail!("CONCURRENCY_LIMIT: exceeded {MAX_INFLIGHT_JOBS} concurrent inflight tasks");
    }
    Ok(())
}

struct OwnedSession {
    state: crate::agent_service::AgentServiceState,
    task: String,
    model: String,
    cancel: tokio_util::sync::CancellationToken,
    plugin: Option<String>,
    scope: String,
    libs: agent_program::libraries::Libraries,
    turn: usize,
    ban_execution: bool,
    input_budget: usize,
}

impl OwnedSession {
    fn from_parent(s: &Session<'_>, libs: &agent_program::libraries::Libraries) -> Self {
        Self {
            state: s.state.clone(),
            task: s.task.to_owned(),
            model: s.model.to_owned(),
            cancel: s.cancel.clone(),
            plugin: s.plugin.clone(),
            scope: s.scope.clone(),
            libs: libs.clone(),
            turn: s.turn,
            ban_execution: s.ban_execution,
            input_budget: s.input_budget,
        }
    }

    fn as_session(&self) -> Session<'_> {
        Session {
            agent_id: None,
            task_node_id: None,
            plugin: self.plugin.clone(),
            state: &self.state,
            task: &self.task,
            model: &self.model,
            turn: self.turn,
            cancel: &self.cancel,
            ban_execution: self.ban_execution,
            scope: self.scope.clone(),
            parent: None,
            input_budget: self.input_budget,
        }
    }
}

pub(super) fn build_virtual_spec(
    runtime: &Runtime,
    action_id: &str,
    fn_name: &str,
    args: Vec<Value>,
    captured: BTreeMap<String, Value>,
    _scope_vars: Option<&BTreeMap<String, Value>>,
) -> virtual_frame::VirtualCallSpec {
    let fn_def = runtime.program.functions.get(fn_name);
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
        .unwrap_or_else(|| {
            format!("Complete `{fn_name}` and return a JSON object with a concise `summary` field.")
        });
    virtual_frame::VirtualCallSpec {
        action_id: action_id.to_owned(),
        fn_name: fn_name.to_owned(),
        signature,
        contract,
        params,
        captures: captured,
        partial_locals: Vec::new(),
    }
}

fn build_delegate_spec(
    action_id: &str,
    mut args: Vec<Value>,
    parent_libs: &agent_program::libraries::Libraries,
) -> Result<virtual_frame::VirtualCallSpec> {
    let input = if args.len() == 1 {
        args.remove(0)
    } else {
        bail!("host.delegate expects one object or task string argument")
    };
    let (name, task, context) = match input {
        Value::String(task) => (None, task, Value::Null),
        Value::Object(mut object) => {
            let name = object.remove("name").and_then(|value| value.as_str().map(str::to_owned));
            let task = object
                .remove("task")
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or_else(|| anyhow::anyhow!("host.delegate requires a string `task`"))?;
            (name, task, object.remove("context").unwrap_or(Value::Null))
        }
        _ => bail!("host.delegate requires an object or non-empty task string"),
    };
    let task = task.trim().to_owned();
    if task.is_empty() {
        bail!("host.delegate requires a non-empty string `task`");
    }
    let fn_name = name
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            let candidate = crate::agent_program::libraries::namespace(value.trim());
            if crate::agent_program::libraries::identifier(&candidate)
                && !crate::agent_program::libraries::reserved(&candidate)
            {
                candidate
            } else {
                "delegate".to_owned()
            }
        })
        .unwrap_or_else(|| "delegate".to_owned());
    let params = match context {
        Value::Object(object) => {
            let context_value = Value::Object(object.clone());
            let mut params = object
                .into_iter()
                .filter_map(|(key, value)| {
                    delegate_context_key_is_safe(&key, parent_libs).then_some((key, value))
                })
                .collect::<Vec<_>>();
            // Keep the complete original object available through `context`, including
            // keys that cannot become parameters because they collide with VM bindings.
            params.push(("context".to_owned(), context_value));
            params
        }
        Value::Null => Vec::new(),
        value => vec![("context".to_owned(), value)],
    };
    let param_names = params.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>();
    Ok(virtual_frame::VirtualCallSpec {
        action_id: action_id.to_owned(),
        fn_name: fn_name.clone(),
        signature: format!("async function {fn_name}({})", param_names.join(", ")),
        contract: task,
        params,
        captures: BTreeMap::new(),
        partial_locals: Vec::new(),
    })
}

fn delegate_context_key_is_safe(
    key: &str,
    parent_libs: &agent_program::libraries::Libraries,
) -> bool {
    if !crate::agent_program::libraries::identifier(key)
        || key.starts_with("$temp")
        || is_js_keyword(key)
    {
        return false;
    }
    let leaf_libs = virtual_frame::leaf_libraries(parent_libs);
    key != "host" && key != "context" && !leaf_libs.contains_key(key)
}

fn is_js_keyword(name: &str) -> bool {
    matches!(
        name,
        "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "implements"
            | "import"
            | "in"
            | "instanceof"
            | "interface"
            | "let"
            | "new"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "try"
            | "true"
            | "typeof"
            | "var"
            | "false"
            | "void"
            | "while"
            | "with"
            | "null"
            | "yield"
    )
}

/// Build a `VirtualCallSpec` for a partial `@virtual` function whose JS prefix
/// fell through to `ActionKind::Task`. Preserves strict Stack-Frame Isolation:
/// only the function's own parameters, explicit `@uses` captures, and local
/// variables declared inside the function body enter the isolated child frame
/// (unrelated outer `executeTask()` bindings never leak).
pub(super) fn build_virtual_spec_for_task(
    runtime: &Runtime,
    action_id: &str,
    fn_name: &str,
    description: &str,
    input: Value,
    scope_vars: Option<&BTreeMap<String, Value>>,
) -> virtual_frame::VirtualCallSpec {
    let fn_def = runtime.program.functions.get(fn_name);
    let input_obj = input.as_object();

    let mut params = Vec::new();
    let mut param_display = Vec::new();
    let mut seen_names = BTreeSet::new();
    if let Some(f) = fn_def {
        for p in &f.params {
            if agent_program::libraries::identifier(p) {
                let val = input_obj
                    .and_then(|m| m.get(p))
                    .cloned()
                    .unwrap_or(Value::Null);
                params.push((p.clone(), val));
                param_display.push(p.clone());
                seen_names.insert(p.clone());
            }
        }
    } else if let Some(obj) = input_obj {
        for (k, v) in obj {
            if agent_program::libraries::identifier(k) {
                params.push((k.clone(), v.clone()));
                param_display.push(k.clone());
                seen_names.insert(k.clone());
            }
        }
    }

    let mut captures = BTreeMap::new();
    if let Some(f) = fn_def {
        for u in &f.uses {
            let val = input_obj
                .and_then(|m| m.get(u))
                .or_else(|| scope_vars.and_then(|sv| sv.get(u)))
                .cloned()
                .unwrap_or(Value::Null);
            captures.insert(u.clone(), val);
            seen_names.insert(u.clone());
        }
    }

    let mut partial_locals = Vec::new();
    if let (Some(f), Some(vars)) = (fn_def, scope_vars)
        && f.partial_virtual
    {
        for local_name in f
            .parameter_lexical
            .iter()
            .chain(f.lexical.iter())
            .chain(f.vars.iter())
        {
            if !agent_program::libraries::identifier(local_name)
                || !seen_names.insert(local_name.clone())
            {
                continue;
            }
            if let Some(val) = vars.get(local_name) {
                partial_locals.push((local_name.clone(), val.clone()));
            }
        }
    }

    let async_prefix = if fn_def.is_some_and(|f| f.is_async) {
        "async "
    } else {
        ""
    };
    let signature = format!(
        "{async_prefix}function {fn_name}({})",
        param_display.join(", ")
    );
    let contract = fn_def
        .map(|f| f.description.trim().to_owned())
        .filter(|d| !d.is_empty())
        .or_else(|| {
            let trimmed = description.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        })
        .unwrap_or_else(|| {
            format!("Complete `{fn_name}` and return a JSON object with a concise `summary` field.")
        });
    virtual_frame::VirtualCallSpec {
        action_id: action_id.to_owned(),
        fn_name: fn_name.to_owned(),
        signature,
        contract,
        params,
        captures,
        partial_locals,
    }
}
