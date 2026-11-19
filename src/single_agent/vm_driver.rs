//! VM segment execution loop (`drive_vm_segment`) and `@virtual` call spec builder.
//!
//! Proxies host tool traps via `tool_proxy::proxy_rust_tool`, resolves `require()`
//! and `ActionKind::LibraryCall` via `flow_runtime`, and spawns truly concurrent
//! async `@virtual` / library calls into a bounded `tokio::task::JoinSet`.

use super::{Session, context_doc, flow_runtime, tool_proxy, virtual_frame};
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
    runtime: &mut Runtime,
    libs: &agent_program::libraries::Libraries,
    node: &str,
    step: usize,
    allow_shell: bool,
    next_ref_id: &mut usize,
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
                            let (mut result, is_error) = tool_proxy::proxy_rust_tool(
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
                                let (mut res, is_err) = tool_proxy::proxy_rust_tool(
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
    }
}
