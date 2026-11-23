//! Rust host-tool proxy for Context-as-Code v2:
//! dispatches JS host-library traps (`io.*`, `git.*`, `process.*`, `host.*`, `session.*`, ...)
//! into Rust `host_tools` and built-in handlers, records durable `tool/call` & `tool/result`
//! events, and automatically archives `> 24KB` material results into content-addressed assets.

use super::{Session, append_event, context_doc, host_tools, tool_executor::ToolExecutor};
use crate::runtime::agent_program;
use anyhow::{Result, ensure};
use serde_json::{Value, json};

#[allow(clippy::too_many_arguments)]
pub(super) async fn proxy_rust_tool(
    s: &Session<'_>,
    libs: &agent_program::libraries::Libraries,
    node: &str,
    step: usize,
    call_id: &str,
    tool_name: &str,
    mut args: Value,
    allow_shell: bool,
) -> Result<(Value, bool)> {
    if tool_name == "edit_file" {
        normalize_edit_file_args(&mut args);
    }
    let call_seq = append_event(
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
        "ask_user" => handle_ask_user(&args),
        "read_asset" => match crate::context::context_assets::read(
            s.state.workspace.root(),
            s.task,
            &args,
            None,
        ) {
            Ok(val) => (val, false),
            Err(err) => (json!({"error": format!("{err:#}")}), true),
        },
        "snapshot" => match crate::context::context_assets::snapshot(
            s.state.workspace.root(),
            s.task,
            &args,
        ) {
            Ok(val) => (val, false),
            Err(err) => (json!({"error": format!("{err:#}")}), true),
        },
        "read_attachment" => match crate::media::attachments::read(s.state, s.task, &args).await {
            Ok(val) => (val, false),
            Err(err) => (json!({"error": format!("{err:#}")}), true),
        },
        "generate_image" => {
            let context = host_tools::visual_context(s, node)?;
            match crate::media::image_generation::execute(s.state, &context, &s.state.workspace, &args, s.cancel).await {
                Ok(value) => (value, false),
                Err(error) => (json!({"error": format!("{error:#}")}), true),
            }
        }
        "register_image" => {
            // Deliberately absent from host_tools::page_allows: a plugin page
            // may not reach local files that the main conversation owns.
            ensure!(
                host_tools::page_allows(s, "register_image"),
                "PLUGIN_SCOPE_DENIED: register_image"
            );
            let context = host_tools::visual_context(s, node)?;
            match crate::media::register_image::execute(s.state, &context, &args).await {
                Ok(value) => (value, false),
                Err(error) => (json!({"error": format!("{error:#}")}), true),
            }
        }
        "analyze_images" => {
            let context = host_tools::visual_context(s, node)?;
            let result = if args["background"] == true {
                crate::media::visual_jobs::submit(s.state, &context, &args, call_id, s.model, s.cancel)
            } else {
                crate::media::image_analysis::execute(s.state, &context, &args, call_id, s.model, s.cancel).await
            };
            match result {
                Ok(val) => (val, false),
                Err(err) => (json!({"error": format!("{err:#}")}), true),
            }
        }
        "image_analysis_results" => match crate::media::visual_jobs::results(
            s.state.workspace.root(), s.task, &args, s.cancel,
        ).await {
            Ok(val) => (val, false),
            Err(err) => (json!({"error": format!("{err:#}")}), true),
        },
        "read_session_history" => match crate::context::session_history::read(
            s.state.workspace.root(),
            s.task,
            &args,
        )
        .await
        {
            Ok(val) => (val, false),
            Err(err) => (json!({"error": format!("{err:#}")}), true),
        },
        "list_skills" => handle_list_skills(s),
        "read_skill" => handle_read_skill(s, &args),
        "read_tool_contracts" => handle_read_tool_contracts_with_defs(libs, &args, || {
            {
                let mut definitions = crate::server::agent_service::local_tools(&s.state.workspace, s.state.tool_catalog.as_deref(), s.ban_execution);
                if let Some(catalog) = &s.state.tool_catalog {
                    let active = catalog.packages.catalog().as_array().into_iter().flatten()
                        .filter(|p| p["enabled"] == true)
                        .filter_map(|p| p["id"].as_str().map(str::to_owned)).collect();
                    definitions.extend(catalog.packages.tools(&active));
                }
                definitions
            }
        }),
        "read_program_guide" => (
            json!({
                "guide": agent_program::CAPABILITIES,
                "builtin_contracts": agent_program::BUILTIN_CONTRACTS,
            }),
            false,
        ),
        "find_library_methods" => handle_find_library_methods(s, libs, &args),
        _ => match ToolExecutor::new(s).dispatch_host_call(
            node,
            tool_name,
            args,
            call_id,
            allow_shell,
            true,
        )
        .await
        {
            Ok(value) if value["host_denied"] == true => (
                json!({
                    "code": agent_program::runtime::NEEDS_USER,
                    "question": value["question"],
                    "message": value["message"],
                }),
                true,
            ),
            Ok(val) => (val, false),
            Err(err) => {
                let mut value = json!({"error": format!("{err:#}")});
                if let Some(failure) = err.downcast_ref::<crate::tools::batch_edits::PreflightFailure>() {
                    value["failed_files"] = json!([{
                        "file_index": failure.file_index, "path": failure.path, "edit_index": failure.edit_index,
                    }]);
                }
                (value, true)
            },
        },
    };

    if crate::tools::verify_tools::is_tool(tool_name)
        && let Some(raw) = result.as_object_mut().and_then(|o| o.remove("__verify_raw"))
    {
        match crate::context::context_assets::register(s.state.workspace.root(), s.task, call_seq, tool_name, &args_snapshot, &raw) {
            Ok(reference) => result["asset_id"] = reference["asset_id"].clone(),
            Err(error) => result["asset_error"] = json!(format!("could not archive verification output: {error:#}")),
        }
    }

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
        && crate::context::context_assets::is_material_tool(tool_name)
        && result.to_string().chars().count() > context_doc::PEEK_FULL_LIMIT_CHARS
        && let Ok(reference) = crate::context::context_assets::register(
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

fn handle_ask_user(args: &Value) -> (Value, bool) {
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

fn handle_list_skills(s: &Session<'_>) -> (Value, bool) {
    let project = crate::context::project_notes::task_project(s.state.workspace.root(), s.task)
        .ok()
        .flatten()
        .unwrap_or_else(|| s.state.workspace.root().to_path_buf());
    let mut items: Vec<Value> = crate::runtime::flow_skills::list(&project)
        .into_iter()
        .map(|sk| json!({"name": sk.name, "description": sk.description}))
        .collect();
    for (name, desc) in crate::runtime::flow_libraries::list_libraries(s) {
        if !items.iter().any(|it| it["name"] == name) {
            items.push(json!({"name": name, "description": desc, "library": true}));
        }
    }
    (Value::Array(items), false)
}

fn handle_read_skill(s: &Session<'_>, args: &Value) -> (Value, bool) {
    let name = args["name"].as_str().unwrap_or("");
    let project = crate::context::project_notes::task_project(s.state.workspace.root(), s.task)
        .ok()
        .flatten()
        .unwrap_or_else(|| s.state.workspace.root().to_path_buf());
    match crate::runtime::flow_skills::read(&project, name) {
        Some(Ok(val)) => (val, false),
        Some(Err(err)) => (json!({"error": format!("{err:#}")}), true),
        None => (json!({"error": format!("skill '{name}' not found")}), true),
    }
}

#[cfg(test)]
fn handle_read_tool_contracts(libs: &agent_program::libraries::Libraries, args: &Value) -> (Value, bool) {
    handle_read_tool_contracts_with_defs(libs, args, Vec::new)
}

fn handle_read_tool_contracts_with_defs(
    libs: &agent_program::libraries::Libraries,
    args: &Value,
    definitions: impl FnOnce() -> Vec<Value>,
) -> (Value, bool) {
    let mut selection = args.clone();
    if let Some(library) = selection.get("library").and_then(Value::as_str).map(str::to_owned) {
        let mut libraries = selection.get("libraries").and_then(Value::as_array).cloned().unwrap_or_default();
        libraries.push(Value::String(library));
        selection["libraries"] = Value::Array(libraries);
        selection.as_object_mut().unwrap().remove("library");
    }
    let has_selection = selection.get("libraries").and_then(Value::as_array).is_some_and(|v| !v.is_empty())
        || selection.get("methods").and_then(Value::as_array).is_some_and(|v| !v.is_empty());
    let selected = if has_selection {
        match agent_program::declarations::selected(libs, &selection) {
            Ok(value) => value,
            Err(error) => return (json!({"code":"INVALID_CONTRACT_SELECTION","message":format!("{error:#}")}), true),
        }
    } else {
        libs.iter().filter(|(name, _)| !name.starts_with("mcp_")).map(|(name, methods)| (name.clone(), methods.clone())).collect()
    };
    if selection.get("overview").and_then(Value::as_bool).unwrap_or(false) {
        return (json!({"overview": agent_program::declarations::overview(&selected, &definitions())}), false);
    }
    let lazy = selected.iter().filter(|(name, _)| agent_program::declarations::lazy_contract_library(name))
        .map(|(name, methods)| (name.clone(), methods.clone())).collect::<agent_program::libraries::Libraries>();
    if lazy.is_empty() {
        return (json!({"declarations": agent_program::declarations::static_library_header(&selected)}), false);
    }
    let regular = selected.into_iter().filter(|(name, _)| !agent_program::declarations::lazy_contract_library(name)).collect();
    let mut header = if agent_program::libraries::Libraries::is_empty(&regular) { String::new() }
        else { agent_program::declarations::static_library_header(&regular) };
    let defs = definitions();
    header.push_str(&agent_program::declarations::headers(&lazy, &defs));
    let methods = lazy.values().flat_map(|methods| methods.values()).collect::<std::collections::BTreeSet<_>>();
    let schemas = defs.iter().filter_map(|definition| {
        let function = &definition["function"];
        let name = function["name"].as_str()?;
        methods.iter().any(|target| target.as_str() == name).then(|| json!({
            "tool":name,"inputSchema":function["parameters"],"structuredContentSchema":function["mcpOutputSchema"],
        }))
    }).collect::<Vec<_>>();
    (json!({"declarations":header,"schemas":schemas}), false)
}

fn handle_find_library_methods(
    s: &Session<'_>,
    libs: &agent_program::libraries::Libraries,
    args: &Value,
) -> (Value, bool) {
    let query = args["query"].as_str().unwrap_or("").to_ascii_lowercase();
    let library = args["library"].as_str();
    let mut matched = Vec::new();
    for (lib, methods) in libs {
        if library.is_some_and(|name| name != lib) { continue; }
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
    for (lib, desc) in crate::runtime::flow_libraries::list_libraries(s) {
        if library.is_some_and(|name| name != lib) { continue; }
        for m in crate::runtime::flow_libraries::public_method_names(s, &lib).unwrap_or_default() {
            if query.is_empty()
                || lib.to_ascii_lowercase().contains(&query)
                || m.to_ascii_lowercase().contains(&query)
                || desc.to_ascii_lowercase().contains(&query)
            {
                matched.push(json!({
                    "library": lib,
                    "method": m,
                    "orchestration": true,
                    "description": desc,
                }));
            }
        }
    }
    (json!({"methods": matched}), false)
}

/// Accept ergonomic single-edit shorthand `{ path, old_text, new_text }` in
/// addition to `{ path, edits: [{ old_text, new_text }] }` for `io.edit`.
fn normalize_edit_file_args(args: &mut Value) {
    let Some(obj) = args.as_object_mut() else {
        return;
    };
    let needs_edits = match obj.get("edits") {
        None | Some(Value::Null) => true,
        Some(_) => false,
    };
    if !needs_edits {
        return;
    }
    if obj.get("old_text").is_some_and(Value::is_string)
        && obj.get("new_text").is_some_and(Value::is_string)
    {
        let old_text = obj.remove("old_text").unwrap();
        let new_text = obj.remove("new_text").unwrap();
        let mut edit = json!({"old_text": old_text, "new_text": new_text});
        if let Some(hint) = obj.remove("line_hint") { edit["line_hint"] = hint; }
        obj.insert("edits".into(), json!([edit]));
    }
}

#[cfg(test)]
mod tests {
    use super::{handle_read_tool_contracts, normalize_edit_file_args};
    use crate::runtime::agent_program::libraries;
    use serde_json::json;

    #[test]
    fn read_tool_contracts_filters_libraries_methods_and_legacy_library() {
        let libs = libraries::builtin();
        let (io_only, failed) = handle_read_tool_contracts(&libs, &json!({"libraries":["io"]}));
        assert!(!failed);
        assert!(io_only["declarations"].as_str().unwrap().contains("declare namespace io"));
        assert!(!io_only["declarations"].as_str().unwrap().contains("declare namespace git"));
        let (one, failed) = handle_read_tool_contracts(&libs, &json!({"library":"git","overview":false}));
        assert!(!failed);
        let text = one["declarations"].as_str().unwrap();
        assert!(text.contains("function status"));
        let (method_only, failed) = handle_read_tool_contracts(&libs, &json!({"methods":["git.status"]}));
        assert!(!failed);
        let method_text = method_only["declarations"].as_str().unwrap();
        assert!(method_text.contains("function status"));
        assert!(!method_text.contains("function diff"));
        let (overview, failed) = handle_read_tool_contracts(&libs, &json!({"libraries":["io"],"overview":true}));
        assert!(!failed);
        assert!(overview["overview"].as_str().unwrap().contains("io"));
    }

    #[test]
    fn read_tool_contracts_returns_full_schema_for_plugin_libraries_like_ssh() {
        let mut libs = libraries::builtin();
        libs.insert(
            "ssh".into(),
            std::collections::BTreeMap::from([
                ("add_server".into(), "ssh__add_server".into()),
                ("exec".into(), "ssh__exec".into()),
            ]),
        );
        let defs = vec![json!({
            "type": "function",
            "function": {
                "name": "ssh__add_server",
                "description": "Register a new SSH server profile.",
                "parameters": {
                    "type": "object",
                    "required": ["name", "host", "username"],
                    "properties": {
                        "name": { "type": "string" },
                        "host": { "type": "string" },
                        "port": { "type": "integer" },
                        "username": { "type": "string" },
                        "credential": { "type": "string" }
                    }
                }
            }
        })];
        let (result, failed) = super::handle_read_tool_contracts_with_defs(
            &libs,
            &json!({"methods": ["ssh.add_server"]}),
            || defs,
        );
        assert!(!failed);
        let decl = result["declarations"].as_str().unwrap();
        assert!(decl.contains("declare namespace ssh"), "{decl}");
        assert!(decl.contains("function add_server(args:"), "{decl}");
        assert!(decl.contains("host: (string)"), "{decl}");
        assert!(decl.contains("credential?: (string)"), "{decl}");
        assert_eq!(result["schemas"][0]["tool"], "ssh__add_server");
    }

    #[test]
    fn edit_file_shorthand_normalizes_to_edits_array() {
        let mut args = json!({
            "path": "src/a.ts",
            "old_text": "foo",
            "new_text": "bar",
        });
        normalize_edit_file_args(&mut args);
        assert_eq!(
            args,
            json!({
                "path": "src/a.ts",
                "edits": [{"old_text": "foo", "new_text": "bar"}],
            })
        );

        let mut already = json!({
            "path": "src/a.ts",
            "edits": [{"old_text": "a", "new_text": "b"}],
        });
        let expected = already.clone();
        normalize_edit_file_args(&mut already);
        assert_eq!(already, expected);
    }
}
