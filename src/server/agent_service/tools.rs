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
        "exec_command" | "write_stdin" | "kill_session" => crate::tools::shell_process::output_schema(),
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
            json!({"type":"object","required":["steps","completed","total"],"properties":{"steps":{"type":"array","items":crate::tools::plan_tools::step_schema()},"completed":{"type":"integer"},"total":{"type":"integer"}}})
        }
        "plan_update" => {
            json!({"type":"object","required":["step","completed","total"],"properties":{"step":crate::tools::plan_tools::step_schema(),"completed":{"type":"integer"},"total":{"type":"integer"}}})
        }
        "plan_add_substeps" => {
            json!({"type":"object","required":["steps","added","completed","total"],"properties":{"steps":{"type":"array","items":crate::tools::plan_tools::step_schema()},"added":{"type":"integer"},"completed":{"type":"integer"},"total":{"type":"integer"}}})
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
            "diff":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},"truncated":{"type":"boolean"},"stat_only":{"type":"boolean"},
            "check_ok":{"type":"boolean"},"check_issues":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"line":{"type":"integer"},"message":{"type":"string"}},"required":["message"]}}}})
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
        "stat_paths" => json!({"type":"object","required":["entries"],"properties":{"entries":{"type":"array","items":{"type":"object","required":["path","exists"],"properties":{"path":{"type":"string"},"exists":{"type":"boolean"},"kind":{"enum":["file","directory","symlink","other"]},"size_bytes":{"type":"integer"},"modified_at":{"type":"string"},"modified_ms":{"type":"integer"},"readonly":{"type":"boolean"},"executable":{"type":"boolean"}}}}}}),
        "copy_path" => json!({"type":"object","required":["from","to","kind","files_copied","bytes_copied","overwritten"],"properties":{"from":{"type":"string"},"to":{"type":"string"},"kind":{"enum":["file","directory"]},"files_copied":{"type":"integer"},"bytes_copied":{"type":"integer"},"overwritten":{"type":"boolean"}}}),
        "sys_disk_space" | "sys_which" | "sys_listening_ports" | "sys_processes" => {
            return crate::tools::sys_info::definitions().into_iter().find(|tool| tool["name"] == name).map(|tool| tool["outputSchema"].clone());
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
    catalog: Option<&crate::server::builtin_plugins::ToolCatalog>,
    ban_run_command: bool,
) -> Vec<Value> {
    let definitions = crate::server::mcp::tool_definitions();
    definitions.as_array().into_iter().flatten().filter(|tool| {
        tool.get("name").and_then(Value::as_str).is_some_and(|name| {
            (!ban_run_command || crate::tools::shell_process::is_tool(name)
                || (name != "run_program" && !crate::tools::project_process::is_execution(name)))
                && match catalog {
                    Some(catalog) => catalog.contains(name) && !catalog.packages.declares_tool(name),
                    None => crate::tools::tool_registry::ToolRegistry::is_in_local_fallback(name),
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
                _ => "Replace an inclusive 1-indexed line range (set end_line = start_line - 1 for pure insertion before start_line). The host manages the file baseline and stops for user action if the file changed externally.",
            }.to_owned();
            if let Some(prop)=schema.pointer_mut("/properties/expected_old_text") { prop["description"]=json!("Optional exact selected text, used to validate the requested edit."); }
        }
        let mut function = json!({"name":name,"description":description,"parameters":schema});
        if let Some(output) = local_output_schema(name) { function["outputSchema"] = output; }
        json!({"type":"function","function":function})
    }).collect()
}

pub(crate) fn inject_mcp_workspace_root(
    workspace: &crate::tools::Workspace,
    name: &str,
    args: &mut Value,
) {
    let Some(object) = args.as_object_mut() else {
        return;
    };
    let default_root = crate::tools::file_edit::display_path(workspace.root());
    if matches!(name, "git_status" | "git_diff" | "git_log") {
        // `Git{Status,Diff,Log}Request.workspace_root` uses `#[serde(alias = "repo")]`.
        // Remove `repo` before writing `workspace_root` so serde never sees duplicate fields,
        // and preserve an explicit `repo` / `workspace_root` target (already vetted by host_tools).
        let explicit_repo = object
            .remove("repo")
            .and_then(|v| v.as_str().map(str::trim).map(str::to_owned))
            .filter(|s| !s.is_empty());
        let explicit_root = object
            .get("workspace_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let chosen = match explicit_repo.or(explicit_root) {
            Some(raw) => {
                let cleaned = crate::tools::file_edit::strip_verbatim_prefix(std::path::Path::new(&raw));
                let resolved = if cleaned.is_absolute() {
                    cleaned
                } else {
                    crate::tools::file_edit::strip_verbatim_prefix(&workspace.root().join(cleaned))
                };
                crate::tools::file_edit::display_path(&resolved)
            }
            None => default_root,
        };
        object.insert("workspace_root".into(), json!(chosen));
        return;
    }
    object.insert("workspace_root".into(), json!(default_root));
}

pub(crate) async fn execute_tool(
    workspace: Arc<crate::tools::Workspace>,
    catalog: Option<&crate::server::builtin_plugins::ToolCatalog>,
    name: &str,
    mut args: Value,
) -> anyhow::Result<Value> {
    if let Some(catalog) = catalog {
        return catalog.execute(workspace, name, args).await;
    }
    if crate::tools::browser_control::is_tool(name) {
        return crate::tools::browser_control::execute(&workspace, name, &args).await;
    }
    if name == "http_request" {
        return crate::tools::http_probe::execute_request(&args).await;
    }
    if crate::tools::http_probe::is_tool(name) {
        return crate::tools::http_probe::execute(&args).await;
    }
    if crate::tools::project_process::is_tool(name) {
        return crate::tools::project_process::execute(&workspace, name, args).await;
    }
    if name == "run_command" {
        anyhow::bail!(
            "run_command has been removed and was not executed; use run_program or a dedicated tool"
        );
    }
    if crate::tools::program_execution::is_tool(name) {
        return crate::tools::program_execution::execute(&workspace, &args).await;
    }
    inject_mcp_workspace_root(&workspace, name, &mut args);
    let response = crate::server::mcp::call_tool(&workspace, json!({"name":name,"arguments":args})).await?;
    Ok(response
        .get("structuredContent")
        .cloned()
        .unwrap_or(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_mcp_workspace_root_normalizes_git_repo_without_duplicate_field() {
        let root = std::env::temp_dir().join(format!("git-repo-norm-{}", super::super::uuid_like()));
        let other = root.with_extension("other");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let workspace = crate::tools::Workspace::new(&root).unwrap();

        // 1. Explicit absolute `repo` is moved into `workspace_root` with `repo` removed,
        //    so serde deserialization into GitStatusRequest / GitLogRequest / GitDiffRequest succeeds.
        let mut status_args = json!({ "repo": other.to_string_lossy(), "include_untracked": false });
        inject_mcp_workspace_root(&workspace, "git_status", &mut status_args);
        assert!(status_args.get("repo").is_none());
        let parsed: crate::tools::GitStatusRequest = serde_json::from_value(status_args).unwrap();
        assert_eq!(
            parsed.workspace_root.as_deref(),
            Some(crate::tools::file_edit::display_path(&other).as_str())
        );

        // 2. Relative `repo` resolves against the session workspace root.
        let mut log_args = json!({ "repo": "sub", "limit": 5 });
        inject_mcp_workspace_root(&workspace, "git_log", &mut log_args);
        assert!(log_args.get("repo").is_none());
        let parsed_log: crate::tools::GitLogRequest = serde_json::from_value(log_args).unwrap();
        assert_eq!(
            parsed_log.workspace_root.as_deref(),
            Some(crate::tools::file_edit::display_path(&workspace.root().join("sub")).as_str())
        );

        // 3. Empty `repo` falls back to workspace root without duplicate fields.
        let mut diff_args = json!({ "repo": "   " });
        inject_mcp_workspace_root(&workspace, "git_diff", &mut diff_args);
        assert!(diff_args.get("repo").is_none());
        let parsed_diff: crate::tools::GitDiffRequest = serde_json::from_value(diff_args).unwrap();
        assert_eq!(
            parsed_diff.workspace_root.as_deref(),
            Some(crate::tools::file_edit::display_path(workspace.root()).as_str())
        );

        // 4. Windows `\\?\` verbatim prefix is stripped cleanly.
        let mut verbatim_args = json!({ "repo": r"\\?\C:\Repos\demo" });
        inject_mcp_workspace_root(&workspace, "git_status", &mut verbatim_args);
        assert!(verbatim_args.get("repo").is_none());
        assert!(!verbatim_args["workspace_root"].as_str().unwrap().starts_with(r"\\?\"));

        // 5. Non-git tools keep workspace_root pinned to the active workspace.
        let mut read_args = json!({ "path": "a.txt", "workspace_root": other.to_string_lossy() });
        inject_mcp_workspace_root(&workspace, "read_file", &mut read_args);
        assert_eq!(
            read_args["workspace_root"].as_str(),
            Some(crate::tools::file_edit::display_path(workspace.root()).as_str())
        );

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(other);
    }
}
