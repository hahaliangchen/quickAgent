//! Local tool contracts, catalog filtering, and execution dispatch.

use serde_json::{Value, json};
use std::sync::Arc;

/// Return contracts of the file tools as programs receive them. They are kept
/// off the MCP list, where an outputSchema obliges structured content.
fn local_output_schema(name: &str) -> Option<Value> {
    let page = json!({
        "path":{"type":"string"},
        "content":{"type":"string","description":"This page only; read the next page while complete is false."},
        "complete":{"type":"boolean","description":"false when the requested range continues beyond this page"},
        "start_line":{"type":"integer"},
        "end_line":{"type":"integer"},
        "total_lines":{"type":"integer"},
        "next_start_line":{"type":["integer","null"],"description":"Pass as start_line to read the next page; null when nothing remains"},
        "next_column":{"type":["integer","null"],"description":"Pass as start_column with the same start_line to continue a partial long line"},
        "returned_chars":{"type":"integer"},
        "code_hash":{"type":"string"}
    });
    let required = json!([
        "path",
        "content",
        "complete",
        "start_line",
        "end_line",
        "total_lines",
        "next_start_line",
        "next_column",
        "code_hash"
    ]);
    let written = |extra: Value| {
        let mut properties = json!({"path":{"type":"string"},"bytes_written":{"type":"integer"},"changed":{"type":"boolean"},"code_hash":{"type":"string"}});
        properties
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let required: Vec<_> = properties.as_object().unwrap().keys().cloned().collect();
        json!({"type":"object","required":required,"properties":properties})
    };
    Some(match name {
        "exec_command" | "write_stdin" | "kill_session" => crate::shell_process::output_schema(),
        "browser_fill" => {
            json!({"type":"object","required":["selector","length","url"],"properties":{"selector":{"type":"string"},"length":{"type":"integer"},"url":{"type":"string"}}})
        }
        "browser_set_viewport" => {
            json!({"type":"object","required":["width","height","color_scheme"],"properties":{"width":{"type":"integer"},"height":{"type":"integer"},"color_scheme":{"type":"string"}}})
        }
        "browser_scroll" => {
            json!({"type":"object","required":["scroll_x","scroll_y","max_x","max_y"],"properties":{"scroll_x":{"type":"number"},"scroll_y":{"type":"number"},"max_x":{"type":"number"},"max_y":{"type":"number"}}})
        }
        "browser_screenshot" => {
            json!({"type":"object","required":["artifact_id","visual_artifact","captured"],"properties":{"artifact_id":{"type":"string"},"visual_artifact":{"type":"object"},"captured":{"type":"boolean"},"selector":{"type":"string"},"clip":{"type":"object"}}})
        }
        "verify_check" => {
            json!({"type":"object","required":["ecosystem","command","ok","duration_ms","outcome","truncated","error_count","warning_count","diagnostics"],"properties":{"ecosystem":{"type":"string"},"command":{"type":"array","items":{"type":"string"}},"ok":{"type":"boolean"},"duration_ms":{"type":"integer"},"outcome":{"type":"string"},"truncated":{"type":"boolean"},"asset_id":{"type":"string"},"execution_error":{"type":"string"},"asset_error":{"type":"string"},"error_count":{"type":"integer"},"warning_count":{"type":"integer"},"diagnostics":{"type":"array","items":{"properties":{"code":{"type":["string","null"]},"col":{"type":["integer","null"]},"file":{"type":["string","null"]},"line":{"type":["integer","null"]},"message":{"type":"string"},"severity":{"type":"string"}},"required":["file","line","col","severity","code","message"],"type":"object"}}}})
        }
        "verify_test" => {
            json!({"type":"object","required":["ecosystem","command","ok","duration_ms","outcome","truncated","passed","failed","skipped","failures"],"properties":{"ecosystem":{"type":"string"},"command":{"type":"array","items":{"type":"string"}},"ok":{"type":"boolean"},"duration_ms":{"type":"integer"},"outcome":{"type":"string"},"truncated":{"type":"boolean"},"asset_id":{"type":"string"},"execution_error":{"type":"string"},"asset_error":{"type":"string"},"passed":{"type":"integer"},"failed":{"type":"integer"},"skipped":{"type":"integer"},"failures":{"type":"array","items":{"properties":{"file":{"type":"string"},"line":{"type":"integer"},"message":{"type":"string"},"name":{"type":"string"}},"required":["name","message"],"type":"object"}}}})
        }
        "plan_set" => {
            json!({"type":"object","required":["steps","completed","total"],"properties":{"steps":{"type":"array","items":crate::plan_tools::step_schema()},"completed":{"type":"integer"},"total":{"type":"integer"}}})
        }
        "plan_update" => {
            json!({"type":"object","required":["step","completed","total"],"properties":{"step":crate::plan_tools::step_schema(),"completed":{"type":"integer"},"total":{"type":"integer"}}})
        }
        "git_blame" => {
            json!({"type":"array","items":{"properties":{"author":{"type":"string"},"commit":{"type":"string"},"date":{"type":["string","null"]},"line":{"type":"integer"},"summary":{"type":"string"}},"required":["line","commit","author","date","summary"],"type":"object"}})
        }
        "git_commit" => {
            json!({"type":"object","required":["commit","files_count","summary","git_metadata_changed"],"properties":{"commit":{"type":"string"},"files_count":{"type":"integer"},"summary":{"type":"string"},"git_metadata_changed":{"type":"boolean"}}})
        }
        "git_restore" => {
            json!({"type":"object","required":["paths","files_count","staged","git_metadata_changed"],"properties":{"paths":{"type":"array","items":{"type":"string"}},"files_count":{"type":"integer"},"staged":{"type":"boolean"},"git_metadata_changed":{"type":"boolean"}}})
        }
        "apply_edits" => {
            json!({"type":"object","required":["atomic","changed","failed","files"],"properties":{"atomic":{"type":"boolean"},"changed":{"type":"integer"},"failed":{"type":"integer"},"files":{"type":"array","items":{"properties":{"bytes_written":{"type":"integer"},"changed":{"type":"boolean"},"code_hash":{"type":"string"},"error":{"type":"string"},"index_refresh":{"type":"object"},"path":{"type":"string"}},"required":["path","changed"],"type":"object"}}}})
        }
        "git_status" => {
            json!({"type":"object","required":["head","branch","clean","staged","modified","untracked","total_entries","truncated"],"properties":{
            "head":{"type":["string","null"]},"branch":{"type":["string","null"]},"clean":{"type":"boolean"},
            "staged":{"type":"array","items":{"type":"string"}},"modified":{"type":"array","items":{"type":"string"}},"untracked":{"type":"array","items":{"type":"string"}},
            "total_entries":{"type":"integer"},"truncated":{"type":"boolean"}}})
        }
        "git_diff" => {
            json!({"type":"object","required":["diff","files","truncated","stat_only"],"properties":{
            "diff":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},"truncated":{"type":"boolean"},"stat_only":{"type":"boolean"}}})
        }
        "git_log" => json!({"type":"object","required":["commits","truncated"],"properties":{
            "commits":{"type":"array","items":{"type":"object","required":["commit","short_commit","author","date","subject"],"properties":{
                "commit":{"type":"string"},"short_commit":{"type":"string"},"author":{"type":"string"},"date":{"type":"string"},"subject":{"type":"string"}}}},"truncated":{"type":"boolean"}}}),
        "read_file" => json!({"type":"object","required":required,"properties":page}),
        "read_file_lines" => {
            let mut properties = page;
            properties["lines"] = json!({"type":"array","items":{"type":"object","required":["line","text"],"properties":{"line":{"type":"integer"},"text":{"type":"string"}}}});
            let mut required = required;
            required.as_array_mut().unwrap().push(json!("lines"));
            json!({"type":"object","required":required,"properties":properties})
        }
        "read_file_chunk" => {
            json!({"type":"object","required":["path","file_bytes","start_byte","end_byte","next_byte_offset","start_line","end_line","next_line","eof","line_truncated","bytes","line_count","content","lines"],"properties":{
            "path":{"type":"string"},
            "file_bytes":{"type":"integer"},
            "start_byte":{"type":"integer"},
            "end_byte":{"type":"integer"},
            "next_byte_offset":{"type":["integer","null"]},
            "start_line":{"type":["integer","null"]},
            "end_line":{"type":["integer","null"]},
            "next_line":{"type":["integer","null"]},
            "eof":{"type":"boolean"},
            "line_truncated":{"type":"boolean"},
            "bytes":{"type":"integer"},
            "line_count":{"type":"integer"},
            "content":{"type":"string"},
            "lines":{"type":"array","items":{"type":"object","required":["line","text"],"properties":{"line":{"type":["integer","null"]},"text":{"type":"string"}}}}}})
        }
        "list_dir" => {
            json!({"type":"object","required":["root","entries","truncated"],"properties":{
            "root":{"type":"string"},
            "truncated":{"type":"boolean","description":"true when more matching entries existed than max_entries returned"},
            "unexpanded_dirs":{"type":"array","items":{"type":"string"},"description":"Default noise/dependency directories seen during listing without traversing inside them"},
            "hints":{"type":"array","items":{"type":"string"},"description":"Usage or visibility hints for skipped noise/dependency directories"},
            "entries":{"type":"array","items":{"type":"object","required":["path","kind","size_bytes","modified_at","modified_ms"],"properties":{
                "path":{"type":"string","description":"Workspace-relative path, usable as a read path"},
                "kind":{"enum":["file","directory","symlink","other"]},
                "size_bytes":{"type":["integer","null"]},
                "modified_at":{"type":["string","null"],"description":"RFC3339 UTC modification timestamp"},
                "modified_ms":{"type":["integer","null"],"description":"Modification time in Unix epoch milliseconds"}}}}}})
        }
        "search_text" => {
            json!({"type":"object","required":["query","match_mode","output_mode","matches","truncated"],"properties":{
            "query":{"type":"string"},
            "match_mode":{"enum":["literal","regex"]},
            "output_mode":{"enum":["content","files","count"]},
            "matches":{"type":"array","description":"Content mode: matching lines. Empty in files and count modes.","items":{"type":"object","required":["path","line","column","text"],"properties":{
                "path":{"type":"string"},"line":{"type":"integer"},"column":{"type":"integer"},"text":{"type":"string"},
                "before":{"type":"array","items":{"type":"string"}},"after":{"type":"array","items":{"type":"string"}}}}},
            "files":{"type":"array","items":{"type":"string"},"description":"Files mode: matching files."},
            "counts":{"type":"array","items":{"type":"object","required":["path","count"],"properties":{"path":{"type":"string"},"count":{"type":"integer"}}},"description":"Count mode: matching lines per file."},
            "total":{"type":"integer","description":"Count mode: matching lines in all listed files."},
            "truncated":{"type":"boolean","description":"true when more matches (content) or files (files, count) exist than were returned"}}})
        }
        "write_file" | "edit_file" => written(json!({"created":{"type":"boolean"}})),
        "replace_range" => {
            written(json!({"start_line":{"type":"integer"},"end_line":{"type":"integer"}}))
        }
        "delete_path" => json!({"type":"object","required":["path","deleted","kind"],"properties":{
            "path":{"type":"string"},
            "deleted":{"type":"boolean"},
            "kind":{"enum":["file","directory"]}}}),
        "rename_path" => {
            json!({"type":"object","required":["path","to","overwritten","index_refresh"],"properties":{
            "path":{"type":"string"},
            "to":{"type":"string"},
            "overwritten":{"type":"boolean"},
            "index_refresh":{}}})
        }
        "create_dir" => json!({"type":"object","required":["path","created"],"properties":{
            "path":{"type":"string"},
            "created":{"type":"boolean"}}}),
        _ => return None,
    })
}

pub(crate) fn local_tools(
    _workspace: &crate::tools::Workspace,
    catalog: Option<&crate::builtin_plugins::ToolCatalog>,
    ban_run_command: bool,
) -> Vec<Value> {
    let definitions = crate::mcp::tool_definitions();
    definitions.as_array().into_iter().flatten().filter(|tool| {
        tool.get("name").and_then(Value::as_str).is_some_and(|name| {
            (!ban_run_command || crate::shell_process::is_tool(name)
                || (name != "run_program" && !crate::project_process::is_execution(name)))
                && match catalog {
                    Some(catalog) => catalog.contains(name) && !catalog.packages.declares_tool(name),
                    None => crate::tool_registry::ToolRegistry::is_in_local_fallback(name),
                }
        })
    }).map(|tool| {
        let name=tool["name"].as_str().unwrap_or_default(); let mut description=tool["description"].as_str().unwrap_or_default().to_owned();
        let mut schema=tool.get("inputSchema").cloned().unwrap_or_else(||json!({"type":"object","properties":{}}));
        if let Some(props)=schema.get_mut("properties").and_then(Value::as_object_mut) {props.remove("workspace_root");}
        if let Some(required)=schema.get_mut("required").and_then(Value::as_array_mut) {required.retain(|name| name.as_str()!=Some("workspace_root"));}
        if matches!(name, "write_file" | "replace_range" | "edit_file") {
            if let Some(props)=schema.get_mut("properties").and_then(Value::as_object_mut) { props.remove("expected_code_hash"); }
            if let Some(required)=schema.get_mut("required").and_then(Value::as_array_mut) { required.retain(|item| item.as_str()!=Some("expected_code_hash")); }
            description = match name {
                "write_file" => "Create or replace a UTF-8 file. The host manages the file baseline and stops for user action if the file changed externally.",
                "edit_file" => "Apply unique text replacements to a UTF-8 file. The host manages the file baseline and stops for user action if the file changed externally.",
                _ => "Replace an inclusive 1-indexed line range. The host manages the file baseline and stops for user action if the file changed externally.",
            }.to_owned();
            if let Some(prop)=schema.pointer_mut("/properties/expected_old_text") { prop["description"]=json!("Optional exact selected text, used to validate the requested edit."); }
        }
        let mut function = json!({"name":name,"description":description,"parameters":schema});
        if let Some(output) = local_output_schema(name) { function["outputSchema"] = output; }
        json!({"type":"function","function":function})
    }).collect()
}

pub(crate) async fn execute_tool(
    workspace: Arc<crate::tools::Workspace>,
    catalog: Option<&crate::builtin_plugins::ToolCatalog>,
    name: &str,
    mut args: Value,
) -> anyhow::Result<Value> {
    if let Some(catalog) = catalog {
        return catalog.execute(workspace, name, args).await;
    }
    if crate::browser_control::is_tool(name) {
        return crate::browser_control::execute(&workspace, name, &args).await;
    }
    if crate::http_probe::is_tool(name) {
        return crate::http_probe::execute(&args).await;
    }
    if crate::project_process::is_tool(name) {
        return crate::project_process::execute(&workspace, name, args).await;
    }
    if name == "run_command" {
        anyhow::bail!(
            "run_command has been removed and was not executed; use run_program or a dedicated tool"
        );
    }
    if crate::program_execution::is_tool(name) {
        return crate::program_execution::execute(&workspace, &args).await;
    }
    if let Some(object) = args.as_object_mut() {
        object.insert(
            "workspace_root".into(),
            json!(workspace.root().display().to_string()),
        );
    }
    let response = crate::mcp::call_tool(&workspace, json!({"name":name,"arguments":args})).await?;
    Ok(response
        .get("structuredContent")
        .cloned()
        .unwrap_or(response))
}
