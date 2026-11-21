//! Cross-session reusable JS orchestration library runtime (`require("<lib>")`
//! and `ActionKind::LibraryCall`) for Context-as-Code v2.
//!
//! Resolves:
//! - Host built-in namespaces (`io`, `git`, `process`, `host`, `session`, ...),
//! - Built-in presets (`batch-files`, `repo-verify`),
//! - Project skill libraries (`.codex-workspace-mcp/skills/<name>/modules/*.js`),
//! - Enabled plugin package libraries (`modules/*.js`).
//!
//! Each library call runs in its own isolated Layer-1 `Runtime` frame, supports
//! deterministic sibling calls as well as isolated Layer-2 `@virtual` helpers,
//! and emits a compact factual `[library ...]` receipt into the caller's trace.

use super::{Session, context_doc, tool_proxy, virtual_frame, vm_driver};
use crate::agent_program::{
    Runtime,
    libraries::Libraries,
    runtime::{ActionKind, ConcurrentState},
};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

const MAX_LIBRARY_CALL_DEPTH: usize = 4;
const MAX_LIBRARY_ACTIONS: usize = 256;

#[cfg(test)]
mod tests {
    use super::resolve_host_require;
    use crate::agent_program::libraries;
    use serde_json::json;

    #[test]
    fn required_host_contract_matches_direct_host_surface() {
        let libs = libraries::builtin();
        let value = resolve_host_require(&libs);
        assert_eq!(value["library_methods"], json!(["context", "tools", "delegate"]));
        assert_eq!(value["methods"]["contracts"], "read_tool_contracts");
    }

    #[test]
    fn required_host_surface_omits_delegate_without_parent_session() {
        let mut libs = libraries::builtin();
        libs.remove("session");
        assert_eq!(resolve_host_require(&libs)["library_methods"], json!(["context", "tools"]));
    }
}

/// Resolve `require("<library>")` (or `require("<library>/promises")`) into the
/// method descriptor expected by `Runtime::link`.
pub(super) fn resolve_require(
    s: &Session<'_>,
    libs: &Libraries,
    library: &str,
) -> (Value, bool) {
    let base = library.strip_suffix("/promises").unwrap_or(library);
    if base == "host" {
        return (resolve_host_require(libs), false);
    }
    if let Some(native_methods) = libs.get(base) {
        let mut payload = json!({ "methods": native_methods });
        if let Some(js_methods) = crate::flow_libraries::public_method_names(s, base)
            && !js_methods.is_empty()
        {
            payload["library_methods"] = json!(js_methods);
        }
        return (payload, false);
    }
    if let Some(js_methods) = crate::flow_libraries::public_method_names(s, base)
        && !js_methods.is_empty()
    {
        return (json!({"methods": {}, "library_methods": js_methods}), false);
    }
    let mut available: Vec<String> = libs.keys().cloned().collect();
    for (name, _) in crate::flow_libraries::list_libraries(s) {
        if !available.contains(&name) { available.push(name); }
    }
    available.sort();
    (json!({"code":"UNKNOWN_LIBRARY","message":format!("Cannot find library '{library}'"),"available_libraries":available}), true)
}
fn resolve_host_require(libs: &Libraries) -> Value {
    let methods = libs.get("host").cloned().unwrap_or_default();
    let mut library_methods = vec!["context", "tools"];
    if crate::agent_program::libraries::allows_delegation(libs) {
        library_methods.push("delegate");
    }
    json!({"methods": methods, "library_methods": library_methods})
}

/// Execute one `ActionKind::LibraryCall { library, method, args }` in an isolated
/// Layer-1 runtime frame. Returns `(result_value, is_error, optional_receipt_line)`.
pub(super) async fn execute_library_call(
    s: &Session<'_>,
    libs: &Libraries,
    library: &str,
    method: &str,
    args: Vec<Value>,
    parent_step: usize,
    allow_shell: bool,
) -> Result<(Value, bool, Option<String>)> {
    if library == "host" {
        return Ok(execute_host_builtin(s, libs, method));
    }
    execute_orchestration_call(s, libs, library, method, args, parent_step, allow_shell, 0).await
}

fn execute_host_builtin(
    s: &Session<'_>,
    libs: &Libraries,
    method: &str,
) -> (Value, bool, Option<String>) {
    match method {
        "context" => {
            let tools: Vec<String> = libs
                .iter()
                .flat_map(|(ns, methods)| methods.keys().map(move |m| format!("{ns}.{m}")))
                .collect();
            (
                json!({
                    "task": s.task,
                    "turn": s.turn,
                    "scope": s.scope,
                    "tools": tools,
                }),
                false,
                None,
            )
        }
        "tools" => {
            let native: Vec<String> = libs.keys().cloned().collect();
            let orchestration: Vec<Value> = crate::flow_libraries::list_libraries(s)
                .into_iter()
                .map(|(name, description)| {
                    let methods = crate::flow_libraries::public_method_names(s, &name)
                        .unwrap_or_default();
                    json!({ "name": name, "description": description, "methods": methods })
                })
                .collect();
            (
                json!({
                    "libraries": native,
                    "orchestration_libraries": orchestration,
                }),
                false,
                None,
            )
        }
        "delegate" => (
            json!({
                "code": "NESTED_DELEGATE_FORBIDDEN",
                "message": "host.delegate is only available in the parent frame; orchestration library calls cannot delegate",
            }),
            true,
            None,
        ),
        other => (
            json!({
                "code": "UNKNOWN_HOST_METHOD",
                "message": format!("host.{other} is not a function"),
            }),
            true,
            None,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_orchestration_call<'a>(
    s: &'a Session<'a>,
    libs: &'a Libraries,
    library: &'a str,
    method: &'a str,
    args: Vec<Value>,
    parent_step: usize,
    allow_shell: bool,
    depth: usize,
) -> Pin<Box<dyn Future<Output = Result<(Value, bool, Option<String>)>> + Send + 'a>> {
    Box::pin(async move {
        if depth >= MAX_LIBRARY_CALL_DEPTH {
            return Ok((
                json!({
                    "code": "LIBRARY_DEPTH_LIMIT",
                    "message": format!("Library call depth exceeded ({MAX_LIBRARY_CALL_DEPTH}) at {library}.{method}"),
                }),
                true,
                Some(format!(
                    "// => [library {library}.{method} | failed: depth limit]"
                )),
            ));
        }

        let arg_count = args.len();
        let (spec, program) =
            match crate::flow_libraries::compile_entry(s, library, method, &args, libs.clone()) {
                Ok(compiled) => compiled,
                Err(err) => {
                    let msg = format!("{err:#}");
                    return Ok((
                        json!({ "code": "LIBRARY_LOAD_ERROR", "message": msg }),
                        true,
                        Some(format!("// => [library {library}.{method} | failed: {msg}]")),
                    ));
                }
            };

        let mut runtime = match Runtime::new(program, &spec.name, args) {
            Ok(rt) => rt,
            Err(err) => {
                let msg = format!("{err:#}");
                return Ok((
                    json!({ "code": "LIBRARY_INIT_ERROR", "message": msg }),
                    true,
                    Some(format!("// => [library {library}.{method} | failed: {msg}]")),
                ));
            }
        };

        let lib_session = Session {
            agent_id: s.agent_id.clone(),
            task_node_id: s.task_node_id.clone(),
            plugin: s.plugin.clone(),
            state: s.state,
            task: s.task,
            model: s.model,
            turn: s.turn,
            cancel: s.cancel,
            ban_execution: s.ban_execution,
            scope: format!("library:{library}.{method}"),
            parent: Some(s.scope.clone()),
            input_budget: s.input_budget,
        };

        let mut files_read = BTreeSet::new();
        let mut files_written = BTreeSet::new();
        let mut next_ref_id = 1usize;
        let mut action_count = 0usize;
        let node = format!("lib_{library}_{method}_{parent_step}");

        let (val, is_err) = loop {
            if lib_session.cancel.is_cancelled() {
                bail!("cancelled");
            }
            match runtime.next_concurrent() {
                ConcurrentState::Actions(actions) => {
                    for action in actions {
                        action_count += 1;
                        if action_count > MAX_LIBRARY_ACTIONS {
                            return Ok((
                                json!({
                                    "code": "LIBRARY_ACTION_LIMIT",
                                    "message": format!("Library {library}.{method} exceeded {MAX_LIBRARY_ACTIONS} host actions"),
                                }),
                                true,
                                Some(format!("// => [library {library}.{method} | failed: action limit]")),
                            ));
                        }
                        let scope_vars = action.scope_variables().cloned();
                        match action.kind {
                            ActionKind::Tool { name, args } => {
                                record_touch(&name, &args, &mut files_read, &mut files_written);
                                let (mut res, failed) = tool_proxy::proxy_rust_tool(
                                    &lib_session,
                                    libs,
                                    &node,
                                    parent_step,
                                    &action.id,
                                    &name,
                                    args,
                                    allow_shell,
                                )
                                .await?;
                                if !failed {
                                    context_doc::tag_resource_handle(&mut res, &mut next_ref_id);
                                }
                                runtime.resume_action(&action.id, res, failed)?;
                            }
                            ActionKind::ParallelReads { operations } => {
                                for (name, args) in &operations {
                                    record_touch(name, args, &mut files_read, &mut files_written);
                                }
                                let base_id = action.id.clone();
                                let futs = operations.into_iter().enumerate().map(|(idx, (name, args))| {
                                    let call_id = format!("{base_id}-p{idx}");
                                    let lib_session = &lib_session;
                                    let node = &node;
                                    async move {
                                        tool_proxy::proxy_rust_tool(
                                            lib_session,
                                            libs,
                                            node,
                                            parent_step,
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
                                for (mut res, failed) in results {
                                    if !failed {
                                        context_doc::tag_resource_handle(&mut res, &mut next_ref_id);
                                    }
                                    items.push(json!({"failed": failed, "value": res}));
                                }
                                runtime.resume_action(&action.id, Value::Array(items), false)?;
                            }
                            ActionKind::Require { library: req_lib } => {
                                let (res, failed) = resolve_require(&lib_session, libs, &req_lib);
                                runtime.resume_action(&action.id, res, failed)?;
                            }
                            ActionKind::LibraryCall {
                                library: sub_lib,
                                method: sub_method,
                                args: sub_args,
                            } => {
                                let (res, failed, _) = if sub_lib == "host" {
                                    execute_host_builtin(&lib_session, libs, &sub_method)
                                } else {
                                    execute_orchestration_call(
                                        &lib_session,
                                        libs,
                                        &sub_lib,
                                        &sub_method,
                                        sub_args,
                                        parent_step,
                                        allow_shell,
                                        depth + 1,
                                    )
                                    .await?
                                };
                                runtime.resume_action(&action.id, res, failed)?;
                            }
                            ActionKind::Call {
                                name: call_name,
                                args: call_args,
                                captured,
                            } => {
                                // Check if `call_name` is a deterministic sibling method in the SAME library
                                // (e.g. `batch-files.summarizeFiles` calling sibling `readMany`).
                                let is_sibling = call_name != method
                                    && crate::flow_libraries::public_method_names(&lib_session, library)
                                        .is_some_and(|names| names.iter().any(|n| n == &call_name));
                                if is_sibling {
                                    let (res, failed, _) = execute_orchestration_call(
                                        &lib_session,
                                        libs,
                                        library,
                                        &call_name,
                                        call_args,
                                        parent_step,
                                        allow_shell,
                                        depth + 1,
                                    )
                                    .await?;
                                    runtime.resume_action(&action.id, res, failed)?;
                                } else {
                                    let vspec = vm_driver::build_virtual_spec(
                                        &runtime,
                                        &action.id,
                                        &call_name,
                                        call_args,
                                        captured,
                                        scope_vars.as_ref(),
                                    );
                                    let (vval, vfailed, vreceipt) =
                                        virtual_frame::execute_virtual_call(
                                            &lib_session,
                                            libs,
                                            vspec,
                                            parent_step,
                                        )
                                        .await?;
                                    for f in vreceipt.files_read {
                                        files_read.insert(f);
                                    }
                                    for f in vreceipt.files_written {
                                        files_written.insert(f);
                                    }
                                    runtime.resume_action(&action.id, vval, vfailed)?;
                                }
                            }
                            ActionKind::Task {
                                name: call_name,
                                description,
                                input,
                            } => {
                                let vspec = vm_driver::build_virtual_spec_for_task(
                                    &runtime,
                                    &action.id,
                                    &call_name,
                                    &description,
                                    input,
                                    scope_vars.as_ref(),
                                );
                                let (vval, vfailed, vreceipt) =
                                    virtual_frame::execute_virtual_call(
                                        &lib_session,
                                        libs,
                                        vspec,
                                        parent_step,
                                    )
                                    .await?;
                                for f in vreceipt.files_read {
                                    files_read.insert(f);
                                }
                                for f in vreceipt.files_written {
                                    files_written.insert(f);
                                }
                                runtime.resume_action(&action.id, vval, vfailed)?;
                            }
                            other => {
                                runtime.resume_action(
                                    &action.id,
                                    json!({
                                        "code": "LIBRARY_UNSUPPORTED_ACTION",
                                        "message": format!("Unsupported action inside library {library}.{method}: {:?}", other),
                                    }),
                                    true,
                                )?;
                            }
                        }
                    }
                }
                ConcurrentState::Waiting => {
                    break (
                        json!({"code": "LIBRARY_DEADLOCK", "message": format!("Library {library}.{method} entered waiting state with no pending jobs")}),
                        true,
                    );
                }
                ConcurrentState::Returned(val) => break (val, false),
                ConcurrentState::Failed(err) => break (err, true),
            }
        };

        let mut parts = vec![format!("{arg_count} arg(s)")];
        if !files_read.is_empty() {
            parts.push(format!(
                "read: {}",
                files_read.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        if !files_written.is_empty() {
            parts.push(format!(
                "edited: {}",
                files_written.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        if is_err {
            let preview: String = val.to_string().chars().take(180).collect();
            parts.push(format!("failed: {preview}"));
        }
        let receipt = format!("// => [library {library}.{method} | {}]", parts.join(" | "));
        Ok((val, is_err, Some(receipt)))
    })
}

fn record_touch(
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
        "write_file" | "edit_file" | "replace_range" | "delete_path" | "rename_path" | "create_dir" => {
            files_written.insert(path.to_owned());
        }
        _ => {}
    }
}
