//! Rust host-tool proxy for Context-as-Code v2:
//! dispatches JS host-library traps (`io.*`, `git.*`, `process.*`, `host.*`, `session.*`, ...)
//! into Rust `host_tools` and built-in handlers, records durable `tool/call` & `tool/result`
//! events, and automatically archives `> 24KB` material results into content-addressed assets.

use super::{Session, append_event, context_doc, host_tools};
use crate::agent_program;
use anyhow::Result;
use serde_json::{Value, json};

#[allow(clippy::too_many_arguments)]
pub(super) async fn proxy_rust_tool(
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
        "ask_user" => handle_ask_user(&args),
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
        "list_skills" => handle_list_skills(s),
        "read_skill" => handle_read_skill(s, &args),
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
        "find_library_methods" => handle_find_library_methods(s, libs, &args),
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

fn handle_read_skill(s: &Session<'_>, args: &Value) -> (Value, bool) {
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

fn handle_find_library_methods(
    s: &Session<'_>,
    libs: &agent_program::libraries::Libraries,
    args: &Value,
) -> (Value, bool) {
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
