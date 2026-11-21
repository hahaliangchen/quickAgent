//! Model-facing declarations, generated from the same schemas used by tools.
use super::{
    libraries::{self, Libraries},
    *,
};
use std::collections::BTreeSet;

fn text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("*/", "* /")
}
fn key(value: &str) -> String {
    if libraries::identifier(value) {
        value.into()
    } else {
        json!(value).to_string()
    }
}
fn note(schema: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(description) = schema["description"].as_str() {
        parts.push(text(description));
    }
    if schema["type"] == "integer"
        || schema["type"]
            .as_array()
            .is_some_and(|types| types.iter().any(|ty| ty == "integer"))
    {
        parts.push("integer".into());
    }
    // Constraints that TS cannot express remain explicit annotations, never dropped.
    for (name, value) in schema.as_object().into_iter().flatten() {
        if !matches!(
            name.as_str(),
            "type"
                | "description"
                | "properties"
                | "required"
                | "items"
                | "prefixItems"
                | "additionalProperties"
                | "enum"
                | "const"
                | "anyOf"
                | "oneOf"
                | "allOf"
                | "$defs"
                | "definitions"
                | "$ref"
        ) {
            parts.push(text(&format!("{name}={value}")));
        }
    }
    if schema["additionalProperties"] == false {
        parts.push("no extra properties".into());
    }
    if schema.get("oneOf").is_some() {
        parts.push("exactly one schema must match".into());
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("/** {} */ ", parts.join("; "))
    }
}
fn ty(schema: &Value, root: &Value, seen: &mut BTreeSet<String>, depth: usize) -> String {
    if depth > 32 {
        return format!("{}unknown", note(&json!({"schema":schema})));
    }
    if schema == &Value::Bool(false) {
        return "never".into();
    }
    if schema == &Value::Bool(true) || schema.is_null() {
        return "unknown".into();
    }
    if let Some(reference) = schema["$ref"].as_str() {
        let value = reference
            .strip_prefix('#')
            .and_then(|pointer| root.pointer(pointer));
        if !seen.insert(reference.into()) || value.is_none() {
            return format!("/** $ref={} */ unknown", text(reference));
        }
        let resolved = ty(value.unwrap(), root, seen, depth + 1);
        seen.remove(reference);
        let mut siblings = schema.clone();
        siblings.as_object_mut().unwrap().remove("$ref");
        return if siblings.as_object().unwrap().is_empty() {
            resolved
        } else {
            format!("({resolved}) & ({})", ty(&siblings, root, seen, depth + 1))
        };
    }
    let mut shapes = Vec::new();
    if let Some(value) = schema.get("const") {
        shapes.push(value.to_string());
    }
    if let Some(values) = schema["enum"].as_array() {
        shapes.push(if values.is_empty() {
            "never".into()
        } else {
            values
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(" | ")
        });
    }
    for (name, separator) in [("anyOf", " | "), ("oneOf", " | "), ("allOf", " & ")] {
        if let Some(items) = schema[name].as_array() {
            if items.is_empty() {
                shapes.push(if name == "allOf" {
                    "unknown".into()
                } else {
                    "never".into()
                });
                continue;
            }
            shapes.push(format!(
                "({})",
                items
                    .iter()
                    .map(|v| format!("({})", ty(v, root, seen, depth + 1)))
                    .collect::<Vec<_>>()
                    .join(separator)
            ));
        }
    }
    let type_names = schema["type"]
        .as_array()
        .map(|v| v.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_else(|| schema["type"].as_str().into_iter().collect());
    if !type_names.is_empty() || schema.get("properties").is_some() {
        let names = if type_names.is_empty() {
            vec!["object"]
        } else {
            type_names
        };
        let types = names
            .into_iter()
            .map(|name| match name {
                "null" => "null".into(),
                "boolean" => "boolean".into(),
                "number" | "integer" => "number".into(),
                "string" => "string".into(),
                "array" => {
                    if let Some(prefix) = schema["prefixItems"]
                        .as_array()
                        .or_else(|| schema["items"].as_array())
                    {
                        let mut types = prefix
                            .iter()
                            .map(|v| ty(v, root, seen, depth + 1))
                            .collect::<Vec<_>>();
                        let rest = if schema.get("prefixItems").is_some() {
                            &schema["items"]
                        } else {
                            &schema["additionalItems"]
                        };
                        if rest != &Value::Bool(false) {
                            types.push(format!("...({})[]", ty(rest, root, seen, depth + 1)));
                        }
                        // JSON Schema permits shorter tuples unless minItems requires them.
                        let minimum = schema["minItems"].as_u64().unwrap_or(0) as usize;
                        for (index, item) in types.iter_mut().take(prefix.len()).enumerate() {
                            if index >= minimum {
                                *item = format!("({item})?");
                            }
                        }
                        format!("[{}]", types.join(", "))
                    } else {
                        format!("({})[]", ty(&schema["items"], root, seen, depth + 1))
                    }
                }
                "object" => {
                    let mut fields = schema["properties"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(name, value)| {
                            let required = schema["required"]
                                .as_array()
                                .is_some_and(|list| list.iter().any(|v| v == name));
                            format!(
                                "{}{}: {}",
                                key(name),
                                if required { "" } else { "?" },
                                ty(value, root, seen, depth + 1)
                            )
                        })
                        .collect::<Vec<_>>();
                    match schema.get("additionalProperties") {
                        Some(Value::Bool(false)) => {}
                        Some(Value::Object(_)) => {
                            // Intersection keeps the additional-key constraint without
                            // incorrectly imposing it on named properties in TypeScript.
                            let extra = ty(&schema["additionalProperties"], root, seen, depth + 1);
                            fields.push(format!(
                                "/** additional keys: {} */ [key: string]: unknown",
                                text(&extra)
                            ));
                        }
                        _ => fields.push("[key: string]: unknown".into()),
                    }
                    if let Some(required) = schema["required"].as_array() {
                        for name in required.iter().filter_map(Value::as_str) {
                            if schema["properties"].get(name).is_none() {
                                fields.push(format!("{}: unknown", key(name)));
                            }
                        }
                    }
                    format!("{{ {} }}", fields.join("; "))
                }
                _ => "unknown".into(),
            })
            .collect::<Vec<String>>();
        shapes.push(types.join(" | "));
    }
    let shape = if shapes.is_empty() {
        "unknown".into()
    } else {
        shapes
            .into_iter()
            .map(|s| format!("({s})"))
            .collect::<Vec<_>>()
            .join(" & ")
    };
    format!("{}{shape}", note(schema))
}
pub fn schema_type(schema: &Value) -> String {
    ty(schema, schema, &mut BTreeSet::new(), 0)
}
pub fn headers(libraries: &Libraries, definitions: &[Value]) -> String {
    let definitions = definitions
        .iter()
        .filter_map(|d| {
            d["function"]["name"]
                .as_str()
                .map(|name| (name, &d["function"]))
        })
        .collect::<BTreeMap<_, _>>();
    let mut result = String::from(
        "// Declarations only: call these fixed names in plain JS flow source; do not copy TS types into source.\n",
    );
    for (library, methods) in libraries {
        let object = methods.keys().any(|name| libraries::reserved(name));
        result.push_str(&if object {
            format!("declare const {library}: {{\n")
        } else {
            format!("declare namespace {library} {{\n")
        });
        for (method, target) in methods {
            let Some(definition) = definitions.get(target.as_str()) else {
                continue;
            };
            // Planning and native execution must receive the complete same
            // tool contract, including requirements after its first sentence.
            // These declarations are for program code, where a source read
            // returns a whole file instead of an AI's page.
            let program_read = crate::source_read::is_source_read(target)
                && definition["parameters"]["properties"]["max_chars"].is_object();
            let mut parameters = definition["parameters"].clone();
            let mut description = definition["description"].as_str().unwrap_or("").to_owned();
            if program_read {
                description = description.replace(&crate::source_read::page_note(false), &crate::source_read::page_note(true));
                parameters["properties"]["max_chars"] = json!({"type":"integer","minimum":1000,"maximum":crate::source_read::PROGRAM_PAGE_CHARS});
            }
            let description = text(&description);
            let returns = definition
                .get("outputSchema")
                .filter(|v| !v.is_null())
                .map(schema_type)
                .unwrap_or_else(|| "unknown".into());
            let prefix = if object { "" } else { "function " };
            result.push_str(&format!(
                "  /** {description} */\n  {prefix}{method}(args: {}): {returns};\n",
                schema_type(&parameters)
            ));
        }
        result.push_str(if object { "};\n" } else { "}\n" });
    }
    result
}

/// Lightweight declaration prelude for the code-only transport. It carries
/// stable library names and typed method shapes without emitting JSON Schema
/// tool definitions on the wire.
pub fn static_library_header(libraries: &Libraries) -> String {
    let mut out = String::from(
        "// Context-as-code library declarations (pre-injected in top-level scope; import/require also supported).\n\
         // IMPORTANT: Pre-bound `io.*`, `git.*`, `process.*` methods are SYNCHRONOUS.\n\
         // For concurrent Promise.all / Promise.allSettled batches, use the async `.promises` sub-namespace (`io.promises.read`, `git.promises.status`, `process.promises.run_readonly`) or `require(\"<lib>/promises\")`.\n\
         // MANDATORY exact-text edit rule: for io.edit/io.replace/io.write, use String.raw tagged templates for snippets containing backslashes that have NO backticks (`) or `${` (put real line breaks directly because `\\n` stays literal).\n\
         // NEVER wrap a whole file or multi-line block that contains backticks (`) or `${` (such as Rust doc comments, Markdown, or TS templates) in a single String.raw`...` — unescaped backticks terminate the template literal early. Use a quoted line array `[\"line 1\", String.raw`line 2`].join(\"\\n\")` instead.\n",
    );
    out.push_str(&directory(libraries));
    out.push('\n');
    if libraries.contains_key("browser") {
        out.push_str(
            "interface BrowserConnection {\n\
               browser_session_id: string;\n\
               page_id: string;\n\
               websocket_url?: string;\n\
               display_mode?: 'visible_window' | 'headless';\n\
               page_epoch?: number;\n\
               capture_seq?: number;\n\
             }\n\n",
        );
    }
    if libraries.contains_key("io") || libraries.contains_key("process") {
        out.push_str(
            "interface ResourceHandle {\n\
               __ref?: string;\n\
               path: string;\n\
               content: string;\n\
               complete: boolean;\n\
               bytes: number;\n\
               total_lines: number;\n\
               /** 1-based inclusive line slice, e.g. f.slice(30, 55) or multi-range f.slice([[1, 10], [33, 45]]). */\n\
               slice(startLine: number | [number, number][], endLine?: number): string;\n\
               /** Fast in-memory search (case-insensitive string or RegExp) with contextLines (default 1). */\n\
               grep(pattern: string | RegExp, contextLines?: number): string;\n\
               /** Structural symbol/heading outline with 1-based line numbers. */\n\
               outline(): string;\n\
               head(lines?: number): string;\n\
               tail(lines?: number): string;\n\
             }\n\n",
        );
    }
    for (library, methods) in libraries {
        out.push_str(&format!("declare namespace {library} {{\n"));
        for (method, target) in methods {
            let sig = match target.as_str() {
                "read_file" => "args: { path: string; max_chars?: number; max_bytes?: number }): ResourceHandle",
                "read_file_lines" => "args: { path: string; start_line: number; end_line?: number; max_chars?: number; start_column?: number; force_read?: boolean }): ResourceHandle & { next_start_line: number | null; next_column?: number | null; partial_line?: boolean; returned_chars?: number; source_format?: { byte_exact: boolean; fragment: boolean; column_unit: string } }",
                "read_file_chunk" => "args: { path: string; byte_offset?: number; start_line?: number; max_bytes?: number; max_lines?: number }): ResourceHandle & { eof: boolean; next_byte_offset: number | null }",
                "list_dir" => "args: { path?: string; recursive?: boolean; max_depth?: number; respect_gitignore?: boolean; glob?: string | string[]; exclude?: string | string[]; kind?: 'file' | 'directory' | 'symlink' | 'other'; modified_after?: string; modified_before?: string; sort_by?: 'path' | 'modified_desc' | 'modified_asc' | 'size_desc' | 'size_asc'; max_entries?: number }): { root: string; entries: Array<{ path: string; kind: string; size_bytes?: number; modified_at?: string; modified_ms?: number }>; unexpanded_dirs?: string[]; hints?: string[]; truncated: boolean }",
                "search_text" => "args: { query: string | RegExp; path?: string; paths?: string[]; regex?: boolean; case_sensitive?: boolean; respect_gitignore?: boolean; glob?: string | string[]; exclude?: string | string[]; context_lines?: number; max_matches?: number; max_matches_per_file?: number; output_mode?: 'content' | 'files' | 'count' }): { query: string; match_mode: string; output_mode: string; matches: Array<{ path: string; line: number; column: number; text: string; line_chars?: number; before?: string[]; after?: string[] }>; files?: string[]; counts?: Array<{ path: string; count: number }>; total?: number; truncated: boolean }",
                "write_file" => "args: { path: string; content: string; create_parent_dirs?: boolean; expected_code_hash?: string }): { path: string; bytes_written: number; go_reindexed: boolean; changed: boolean; created: boolean; code_hash: string; index_refresh: unknown }",
                "edit_file" => "args: { path: string; edits: Array<{ old_text: string; new_text: string }>; replace_all?: boolean; expected_code_hash?: string } | { path: string; old_text: string; new_text: string; replace_all?: boolean; expected_code_hash?: string }): { path: string; bytes_written: number; go_reindexed: boolean; changed: boolean; created: boolean; code_hash: string; index_refresh: unknown; preview?: string }",
                "replace_range" => "args: { path: string; start_line: number; end_line: number; replacement: string; expected_old_text?: string; expected_code_hash?: string }): { path: string; start_line: number; end_line: number; bytes_written: number; go_reindexed: boolean; changed: boolean; created: boolean; code_hash: string; index_refresh: unknown; preview?: string }",
                "workspace_info" => "args?: { workspace_root?: string }): { workspace_root: string; platform: string; allowed_scope: string; default_ignored_dirs: string[] }",
                "git_status" => "args?: { paths?: string[]; include_untracked?: boolean; max_entries?: number }): { head: string | null; branch: string | null; clean: boolean; staged: string[]; modified: string[]; untracked: string[]; total_entries: number; truncated: boolean }",
                "git_diff" => "args?: { rev_range?: string; staged?: boolean; paths?: string[]; stat_only?: boolean; unified?: number; max_lines?: number; max_chars?: number }): { diff: string; files: string[]; truncated: boolean; stat_only: boolean }",
                "git_log" => "args?: { rev?: string; limit?: number; paths?: string[] }): { commits: unknown[]; truncated: boolean }",
                "run_readonly_command" => "args: { cmd: string; workdir?: string; timeout_ms?: number }): { exit_code: number | null; stdout: string; stderr: string }",
                "exec_command" => "args: { cmd: string; workdir?: string; yield_time_ms?: number; max_output_tokens?: number; stdin?: boolean }): { exit_code: number | null; running: boolean; session_id?: string; stdout: string; stderr: string }",
                "write_stdin" => "args: { session_id: string; chars?: string; yield_time_ms?: number; max_output_tokens?: number; stream?: 'stdout' | 'stderr'; char_offset?: number }): { exit_code: number | null; running: boolean; session_id?: string; stdout: string; stderr: string; output?: string; next_char_offset?: number | null; total_chars?: number }",
                "kill_session" => "args: { session_id: string }): { exit_code: number | null; running: boolean; session_id?: string }",
                "read_tool_contracts" => "args?: { libraries?: string[]; library?: string; methods?: string[]; overview?: boolean }): { declarations?: string; overview?: string }",
                "find_library_methods" => "args: { query: string }): { methods: Array<{ library: string; method: string; tool?: string; orchestration?: boolean; description?: string }> }",
                "read_program_guide" => "args?: { section?: string }): { guide: string; builtin_contracts: string }",
                "list_skills" => "args?: {}): Array<{ name: string; description: string }>",
                "read_skill" => "args: { name: string }): { name: string; content: string }",
                "ask_user" => "args: { question: string }): { code: string; question: string; message: string }",
                "read_session_history" => "args?: { scope?: 'conversation' | 'project'; view?: 'records' | 'index'; task_id?: string; match_offset?: number; query?: string; role?: string; node_id?: string; turn?: number; include_descendants?: boolean; request_id?: number; before_seq?: number; after_seq?: number; event_seq?: number; snapshot_seq?: number; snapshot_id?: number; new_snapshot?: boolean; audit_history_reads?: boolean; limit?: number; char_offset?: number; max_chars?: number; include_event?: boolean; include_context?: boolean; programs?: boolean; directory?: string; program?: string; start_line?: number; end_line?: number; output?: boolean }): unknown",
                "read_asset" => "args: { asset_id?: string; seq?: number; char_offset?: number; max_chars?: number }): unknown",
                "snapshot" => "args: { asset_id: string }): unknown",
                "http_probe" => "args: { url: string; timeout_ms?: number; reason?: string }): { reachable: boolean; http_status: number | null; check_passed: boolean; elapsed_ms: number; reused?: boolean }",
                "run_program" => "args: { program: 'cargo' | 'git' | 'node' | 'python'; args: string[]; project_path?: string; timeout_seconds?: number; max_output_bytes?: number }): unknown",
                "install_dependencies" => "args?: { project_path?: string; mode?: 'auto' | 'install' | 'ci'; background?: boolean; timeout_seconds?: number; max_chars?: number }): unknown",
                "run_project_script" => "args: { project_path?: string; script: string; args?: string[]; background?: boolean; timeout_seconds?: number; ready_url?: string; ready_port?: number; wait_seconds?: number; max_chars?: number }): unknown",
                "get_project_process" => "args?: { process_id?: string; project_path?: string; script?: string; after_seq?: number; max_chars?: number; ready_url?: string; ready_port?: number; wait_seconds?: number; force_refresh?: boolean; observation_purpose?: string }): unknown",
                "stop_project_process" => "args: { process_id: string; max_chars?: number }): unknown",
                "browser_open" => "args: { url: string; visible?: boolean; browser_connection?: BrowserConnection }): unknown",
                "browser_read" => "args?: { expect_text?: string; browser_connection?: BrowserConnection }): unknown",
                "browser_wait" => "args?: { selector?: string; text?: string; state?: 'visible' | 'exists' | 'hidden'; font_status?: 'loading' | 'loaded'; timeout_ms?: number; browser_connection?: BrowserConnection }): unknown",
                "browser_diagnostics" => "args?: { url_contains?: string; category?: 'all' | 'fetch' | 'xhr' | 'font' | 'resource'; browser_connection?: BrowserConnection }): unknown",
                "browser_click" => "args?: { text?: string; selector?: string; times?: number; browser_connection?: BrowserConnection }): unknown",
                "browser_press_key" => "args: { key: string; browser_connection?: BrowserConnection }): unknown",
                "browser_upload" => "args: { path: string; selector?: string; browser_connection?: BrowserConnection }): unknown",
                "browser_screenshot" => "args?: { full_page?: boolean; browser_connection?: BrowserConnection }): unknown",
                "browser_close" => "args?: { browser_connection?: BrowserConnection }): unknown",
                "view_image" => "args: { artifact_id: string; original?: boolean; checked_goal?: string }): unknown",
                "read_attachment" => "args: { attachment_id: string; char_offset?: number; max_chars?: number; page_index?: number; slide?: number; picture?: string }): unknown",
                "analyze_images" => "args: { artifact_ids: string[]; question: string; background?: boolean }): unknown",
                "image_analysis_results" => "args?: { wait_ms?: number }): unknown",
                "generate_image" => "args: { prompt: string; model?: string; reference_paths?: string[]; reference_artifact_ids?: string[]; size?: string; quality?: 'low' | 'medium' | 'high' | 'auto'; background?: 'opaque' | 'transparent' | 'auto'; output_path?: string }): { artifact_id: string; visual_artifact: Record<string, unknown>; model: string; mode: 'generate' | 'edit'; size: string; width: number; height: number; bytes: number; output_path?: string; revised_prompt?: string }",
                "record_work_memory" => "args: { summary: string; files_changed?: string[]; implementation?: string; tests?: string; risks?: string; source_task_id?: string; kind?: string; applies_to?: string; source_refs?: string[] }): { memory_path: string; recorded: unknown }",
                "list_work_memory" => "args?: { limit?: number }): { memory_path: string; memories: unknown[] }",
                "search_work_memory" => "args: { query: string; limit?: number }): { memory_path: string; query: string; matches: unknown[] }",
                "record_architecture_memory" => "args: { area: string; summary: string; key_symbols?: string[]; key_files?: string[]; boundaries?: string; common_tasks?: string[]; risks?: string }): { memory_path: string; recorded: unknown }",
                "list_architecture_memory" => "args?: { limit?: number }): { memory_path: string; memories: unknown[] }",
                "search_architecture_memory" => "args: { query: string; limit?: number }): { memory_path: string; query: string; matches: unknown[] }",
                "record_symbol_business_context" => "args: { business_role: string; symbol_id?: string; symbol_name?: string; language?: string; file_path?: string; belongs_to_area?: string; common_tasks?: string[]; read_when?: string; avoid_when?: string; risks?: string; confidence?: number }): { memory_path: string; recorded: unknown }",
                "list_symbol_business_context" => "args?: { belongs_to_area?: string; limit?: number }): { memory_path: string; contexts: unknown[] }",
                "search_symbol_business_context" => "args: { query: string; limit?: number }): { memory_path: string; query: string; matches: unknown[] }",
                _ => "args?: Record<string, unknown>): unknown",
            };
            if target == "run_readonly_command" {
                out.push_str("  /** Run strictly read-only CLI diagnostics in a read-only sandbox — prefer `io.list`/`io.read`/`io.search` and `git.*` for file/git work, and use `process.exec_command` for builds/tests that write artifacts or spawn nested sandboxes. */\n");
            }
            out.push_str(&format!("  function {method}({sig};\n"));
        }
        if library == "host" {
            out.push_str("  function context(): { task: string; turn: number; scope: string; tools: string[] };\n");
            out.push_str("  function tools(): { libraries: string[]; orchestration_libraries: Array<{ name: string; description: string; methods: string[] }> };\n");
            if super::libraries::allows_delegation(libraries) {
                out.push_str("  /** Run an isolated AI worker frame for a single lightweight task (parallelizable via `Promise.all` / `Promise.allSettled`); `context` fields are exposed as safe variables and via `context.<key>`. */\n  function delegate(task: string): Promise<unknown>;\n  function delegate(args: { name?: string; task: string; context?: Record<string, unknown> }): Promise<unknown>;\n");
            }
        }
        if matches!(library.as_str(), "io" | "git" | "process") {
            out.push_str(&format!(
                "  /** Async Promise-returning versions of all `{library}.*` methods for `Promise.all` concurrency. */\n  const promises: Record<string, (args?: Record<string, unknown>) => Promise<any>>;\n"
            ));
        }
        out.push_str("}\n");
    }
    out
}

/// The default table of contents. List only libraries available in this frame.
pub fn directory(libraries: &Libraries) -> String {
    let process_purpose = if libraries
        .get("process")
        .is_some_and(|m| m.contains_key("exec_command") || m.contains_key("run_readonly"))
    {
        "Run project commands and inspect their processes."
    } else {
        "Inspect Agent-managed project processes."
    };
    let sections: &[(&str, &[(&str, &str)])] = &[
        ("Read and navigate code", &[
            ("io", "List files, search text, read small files, tail line ranges, or stream large files chunk by chunk. Use for raw source and workspace paths."),
        ]),
        ("Inspect repository history", &[
            ("git", "Read bounded structured status, diffs, and commit history without shell parsing."),
        ]),
        ("Run and inspect applications", &[
            ("process", process_purpose),
            ("http", "Probe HTTP endpoints."),
            ("browser", "Open, inspect, and interact with browser pages."),
            ("vision", "View and analyze images."),
        ]),
        ("Conversation and saved material", &[
            ("host", "Inspect library/tool contracts, search methods, read language guide, load skills, or ask the user."),
            ("attachments", "Read user attachments."),
            ("memory", "Look up and record work, architecture, or symbol knowledge."),
            ("session", "Read conversation history, saved flows, and saved tool results."),
            ("previous", "Resume a previous program that was paused."),
        ]),
    ];
    let mut result = String::from(
        "Library documentation by task. Use read_tool_contracts with a library name and overview:true to browse its methods. Pass only qualified method names for exact signatures and return fields.\n",
    );
    for (title, entries) in sections {
        let entries = entries.iter().filter(|(name, _)| libraries.contains_key(*name)).collect::<Vec<_>>();
        if entries.is_empty() {
            continue;
        }
        result.push_str(&format!("\n{title}\n"));
        for (name, purpose) in entries {
            result.push_str(&format!("- {name} — {purpose}\n"));
        }
    }
    result
}

/// Short signatures for common source operations and shell sessions.
pub fn core_methods(libraries: &Libraries, definitions: &[Value]) -> String {
    let definitions = definitions.iter().filter_map(|d| {
        let function = &d["function"];
        Some((function["name"].as_str()?, function))
    }).collect::<BTreeMap<_, _>>();
    let mut rows = String::new();
    for (library, methods) in libraries {
        for (method, target) in methods {
            let (inputs, outputs, purpose): (&[&str], &[&str], &str) = match target.as_str() {
                "list_dir" => (
                    &["path", "recursive", "max_depth", "glob", "exclude", "kind", "modified_after", "modified_before", "sort_by", "max_entries"],
                    &["entries", "truncated"],
                    "Find and sort files, inspect layout, or stat a single file path (glob/exclude support | pipes); entries include path, kind, size_bytes, modified_at, modified_ms.",
                ),
                "read_file" => (
                    &["path", "max_bytes", "max_chars"],
                    &["content", "complete", "bytes", "total_lines"],
                    "Read a UTF-8 file. max_bytes is a file-size admission limit (default 1 MiB, hard maximum 16 MiB): files larger than it are rejected, never prefix-truncated. To limit returned text, use max_chars and normally omit max_bytes; max_chars does not bypass the admission limit. For larger files, use read_lines or read_chunk.",
                ),
                "read_file_lines" => (
                    &["path", "start_line", "end_line", "max_chars"],
                    &["content", "lines", "complete", "total_lines", "next_start_line", "next_column"],
                    "Read inclusive lines (positive 1-based, or negative from EOF e.g. start_line:-80, end_line:-1 for tail; streams files >16 MiB).",
                ),
                "read_file_chunk" => (
                    &["path", "byte_offset", "start_line", "max_bytes", "max_lines"],
                    &["content", "lines", "eof", "bytes", "file_bytes", "next_byte_offset", "next_line"],
                    "Stream a large file in bounded chunks from byte_offset; iterate until eof, summarize per chunk, and discard raw chunk text.",
                ),
                "search_text" => (
                    &["query", "path", "regex", "output_mode", "glob", "exclude", "context_lines", "max_matches", "max_matches_per_file"],
                    &["matches", "files", "counts", "truncated"],
                    "Find text or patterns (glob/exclude support | pipes; output capped at ~48 KiB inline budget); list matching files or count them.",
                ),
                "git_status" => (
                    &["paths", "include_untracked", "max_entries"],
                    &["head", "branch", "clean", "staged", "modified", "untracked", "total_entries", "truncated"],
                    "Read bounded structured working-tree status; use paths to scope a large repository.",
                ),
                "git_diff" => (
                    &["rev_range", "staged", "paths", "stat_only", "unified", "max_lines", "max_chars"],
                    &["diff", "files", "truncated", "stat_only"],
                    "Read a bounded diff; start with stat_only and request a limited patch preview when needed.",
                ),
                "git_log" => (
                    &["limit", "paths", "rev"],
                    &["commits", "truncated"],
                    "Read a bounded structured commit history.",
                ),
                "run_readonly_command" => (&["cmd", "workdir", "timeout_ms"],
                    &["exit_code", "stdout", "stderr", "duration_ms", "truncated"],
                    "Run a read-only sandboxed Bash check/build/test (source/.git read-only; gitignored target/ and private /tmp writable)."),
                "exec_command" => (&["cmd", "workdir", "yield_time_ms", "max_output_tokens", "stdin"],
                    &["session_id", "running", "exit_code", "stdout", "stderr", "duration_ms", "encoding", "truncated"],
                    "Program/module code: run a platform-shell command; output enters JS heap, not AI context."),
                "write_stdin" => (&["session_id", "chars", "yield_time_ms", "max_output_tokens", "stream", "char_offset"],
                    &["session_id", "running", "exit_code", "stdout", "stderr", "output", "next_char_offset"],
                    "Program/module code: poll with empty chars, page full saved output, or send chars to a command started with stdin:true."),
                "kill_session" => (&["session_id"], &["session_id", "running", "exit_code", "stdout", "stderr"],
                    "Program/module code: stop this process tree; settle change_interval after its last shell exits."),
                _ => continue,
            };
            let Some(definition) = definitions.get(target.as_str()) else { continue };
            let parameters = &definition["parameters"];
            let args = inputs.iter().filter(|name| parameters["properties"].get(**name).is_some())
                .map(|name| {
                    let required = parameters["required"].as_array()
                        .is_some_and(|keys| keys.iter().any(|key| key.as_str() == Some(*name)));
                    format!("{name}{}", if required { "" } else { "?" })
                }).collect::<Vec<_>>().join(", ");
            let returns = outputs.iter().copied().filter(|name| definition["outputSchema"]["properties"].get(*name).is_some())
                .collect::<Vec<_>>().join(", ");
            let returns = if returns.is_empty() { "unknown".to_owned() } else { format!("{{{returns}}}") };
            rows.push_str(&format!("- {library}.{method}({{{args}}}) → {returns}. {purpose}\n"));
        }
    }
    if rows.is_empty() {
        return rows;
    }
    format!("\nCommon source and shell methods\nKey fields only. Search for a name or literal in a focused path, then read the relevant lines. Get optional fields and exact types with read_tool_contracts.\n{rows}")
}

/// A selected library's method names and guidance. Exact signatures are fetched separately.
pub fn overview(libraries: &Libraries, definitions: &[Value]) -> String {
    let descriptions = definitions
        .iter()
        .filter_map(|d| {
            Some((
                d["function"]["name"].as_str()?,
                d["function"]["description"].as_str().unwrap_or(""),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let mut result = String::from(
        "Library methods by purpose. Use read_tool_contracts with only methods:[\"library.method\"] to get exact inputs and returns.\n",
    );
    for (library, methods) in libraries {
        result.push_str(&format!("\n{library}\n"));
        for (method, target) in methods {
            let description = descriptions.get(target.as_str()).copied().unwrap_or("");
            let summary = description.split(['.', '。', '\n']).next().unwrap_or("");
            let summary = text(&summary.chars().take(140).collect::<String>());
            result.push_str(&format!("- {library}.{method}"));
            if !summary.is_empty() {
                result.push_str(&format!(" — {summary}"));
            }
            result.push('\n');
        }
        if library == "io" && methods.contains_key("read_lines") {
            result.push_str("When a file and line range are known, start at that line with io.read_lines. A preceding full-file read is unnecessary.\n");
        }
    }
    result
}

/// Third-party libraries by name and purpose. A program loads one where it
/// needs it; its methods are read with read_tool_contracts.
pub fn third_party_index(libraries: &BTreeMap<String, String>) -> String {
    if libraries.is_empty() {
        return String::new();
    }
    let mut result = String::from(
        "\n// Third-party libraries, loaded where the code needs one: const lib = require(\"<name>\"), or require(\"<name>/promises\") for async methods. Use read_tool_contracts with the library name and overview:true to browse its methods.\n",
    );
    for (library, description) in libraries {
        let summary = description.split(['.', '。', '\n']).next().unwrap_or("");
        let summary = text(&summary.chars().take(140).collect::<String>());
        if !summary.is_empty() {
            result.push_str(&format!("/** {summary} */\n"));
        }
        result.push_str(&format!("declare module \"{library}\";\n"));
    }
    result
}

pub fn selected(libraries: &Libraries, args: &Value) -> anyhow::Result<Libraries> {
    use anyhow::{Context, ensure};
    let mut result = Libraries::new();
    ensure!(
        args.as_object()
            .is_some_and(|o| o.keys().all(|k| matches!(k.as_str(), "libraries" | "methods" | "overview"))),
        "expected libraries and/or methods, with optional overview"
    );
    ensure!(
        args.get("libraries").is_none_or(Value::is_array)
            && args.get("methods").is_none_or(Value::is_array),
        "libraries and methods must be arrays"
    );
    ensure!(args.get("overview").is_none_or(Value::is_boolean), "overview must be a boolean");
    let groups = args.get("libraries").and_then(Value::as_array);
    let methods = args.get("methods").and_then(Value::as_array);
    ensure!(
        groups.is_some_and(|x| !x.is_empty()) || methods.is_some_and(|x| !x.is_empty()),
        "select at least one library or qualified method"
    );
    for group in groups.into_iter().flatten() {
        let name = group.as_str().context("library name must be a string")?;
        let items = libraries
            .get(name)
            .with_context(|| format!("unknown or unavailable library {name}"))?;
        result
            .entry(name.into())
            .or_insert_with(BTreeMap::new)
            .extend(items.clone());
    }
    for method in methods.into_iter().flatten() {
        let name = method.as_str().context("method name must be a string")?;
        let (group, item) = name
            .split_once('.')
            .context("use a qualified method such as io.read")?;
        let target = libraries::target(libraries, name)
            .with_context(|| format!("unknown or unavailable method {name}"))?;
        result
            .entry(group.into())
            .or_insert_with(BTreeMap::new)
            .insert(item.into(), target.into());
    }
    Ok(result)
}

pub fn contract_tool() -> Value {
    json!({"type":"function","function":{"name":"read_tool_contracts",
        "description":"Browse a library's method names and uses with {libraries:[\"io\"],overview:true}. For exact signatures, pass only qualified methods, for example {methods:[\"io.read_lines\"]}. Documentation lookup does not execute tools or expand permissions.",
        "parameters":{"type":"object","properties":{
            "libraries":{"type":"array","items":{"type":"string"},"description":"Select all methods of each named library. Use overview:true for a short library page."},
            "methods":{"type":"array","items":{"type":"string"},"description":"Select individual qualified names such as io.read_lines. Omit libraries to limit the result to these methods."},
            "overview":{"type":"boolean","description":"Return a short method list and usage notes instead of full declarations. Default false."}
        },"additionalProperties":false}}})
}

/// Retired transport name, still recognized in saved histories.
pub const CALL: &str = "invoke_library";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_program::{Runtime, State, runtime::ActionKind};

    fn shell_example(receipt: Value) -> State {
        let source = include_str!("../../skills/task-program/examples/shell-in-module-implementation.js");
        let program = crate::agent_program::compile_body(source, "inspectPackage").unwrap();
        let mut runtime = Runtime::task_program(program, "inspectPackage", vec![json!(".")]).unwrap();
        let State::Action(action) = runtime.next() else { panic!("expected shell command") };
        assert!(matches!(action.kind, ActionKind::Tool { ref name, ref args }
            if name == "exec_command" && args["workdir"] == "."));
        let running = receipt["running"] == true;
        runtime.resume(receipt, false).unwrap();
        if running {
            let State::Action(action) = runtime.next() else { panic!("expected own-session cleanup") };
            assert!(matches!(action.kind, ActionKind::Tool { ref name, ref args }
                if name == "kill_session" && args["session_id"] == "own-session"));
            runtime.resume(json!({"running":false}), false).unwrap();
        }
        runtime.next()
    }

    #[test]
    fn shell_in_module_example_main_declares_the_module_and_matches_the_skill() {
        let source = include_str!("../../skills/task-program/examples/shell-in-module.js");
        let program = crate::agent_program::compile(source).unwrap();
        assert!(program.functions["inspectPackage"].virtual_task);
        let mut runtime = Runtime::new(program, crate::agent_program::ENTRY, vec![]).unwrap();
        let State::Action(action) = runtime.next() else { panic!("expected module invocation") };
        assert!(matches!(action.kind, ActionKind::Call { ref name, ref args, .. }
            if name == "inspectPackage" && args == &vec![json!(".")]));
        let skill = include_str!("../../skills/task-program/SKILL.md").replace("\r\n", "\n");
        assert!(skill.contains(source.replace("\r\n", "\n").trim()));
    }

    #[test]
    fn shell_in_module_example_parses_complete_json_and_returns_a_small_value() {
        let scripts = (0..20).map(|i| (format!("script-{i}"), json!("not returned")))
            .collect::<serde_json::Map<_, _>>();
        let manifest = json!({"name":"actual-package","scripts":scripts,"large_field":"x".repeat(100_000)});
        let state = shell_example(json!({"running":false,"exit_code":0,"truncated":false,
            "stdout":manifest.to_string(),"stderr":""}));
        let State::Returned(summary) = state else { panic!("{state:?}") };
        assert_eq!(summary["name"], "actual-package");
        assert_eq!(summary["scriptCount"], 20);
        assert_eq!(summary["scriptNames"].as_array().unwrap().len(), 20);
        assert!(summary.to_string().len() < 1000);
    }

    #[test]
    #[cfg(not(feature="vm-measurements"))]
    fn shell_library_contracts_expose_fixed_returns_instead_of_unknown() {
        let root = std::env::temp_dir();
        let workspace = crate::tools::Workspace::new(root).unwrap();
        let definitions = crate::agent_service::local_tools(&workspace,None,false);
        let catalog = libraries::catalog(&definitions,&json!([]));
        for name in ["exec_command","write_stdin","kill_session"] {
            let selected = selected(&catalog,&json!({"methods":[format!("process.{name}")]})).unwrap();
            let text = headers(&selected,&definitions);
            assert!(text.contains("stdout:") && text.contains("stderr:") && text.contains("running:") && text.contains("exit_code:"),"{text}");
            let definition = definitions.iter().find(|tool| tool["function"]["name"] == name).unwrap();
            let returns = &definition["function"]["outputSchema"]["properties"];
            assert_eq!(returns["stdout"]["type"],"string");
            assert_eq!(returns["running"]["type"],"boolean");
            assert_eq!(returns["exit_code"]["type"],json!(["integer","null"]));
            let short = core_methods(&selected,&definitions);
            assert!(short.contains("stdout") && short.contains("exit_code") && !short.contains("→ unknown"),"{short}");
        }
    }

    #[test]
    fn shell_in_module_example_rejects_failure_before_json_parse() {
        for code in [1,9] {
            let state = shell_example(json!({"running":false,"exit_code":code,"truncated":false,
                "stdout":"not JSON","stderr":"actual diagnostic"}));
            let State::Failed(error) = state else { panic!("{state:?}") };
            assert!(error["message"].as_str().unwrap().contains("exit_code"), "{error}");
        }
        assert!(matches!(shell_example(json!({"running":false,"exit_code":0,"truncated":false,
            "stdout":"not JSON","stderr":""})), State::Failed(_)));
    }

    #[test]
    fn shell_in_module_example_stops_its_session_instead_of_parsing_unfinished_output() {
        let state = shell_example(json!({"running":true,"session_id":"own-session","exit_code":null,
            "truncated":false,"stdout":"not JSON","stderr":""}));
        let State::Failed(error) = state else { panic!("{state:?}") };
        assert!(error["message"].as_str().unwrap().contains("still running"));
    }

    fn parse(source: &str) {
        use swc_common::{FileName, SourceMap, sync::Lrc};
        use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
        let map: Lrc<SourceMap> = Default::default();
        let file = map.new_source_file(
            FileName::Custom("tools.d.ts".into()).into(),
            source.to_owned(),
        );
        let lexer = Lexer::new(
            Syntax::Typescript(TsSyntax {
                dts: true,
                ..Default::default()
            }),
            Default::default(),
            StringInput::from(&*file),
            None,
        );
        let mut parser = Parser::new_from(lexer);
        parser
            .parse_typescript_module()
            .unwrap_or_else(|e| panic!("invalid declaration {e:?}: {source}"));
        assert!(parser.take_errors().is_empty(), "{source}");
    }
    #[test]
    fn signatures_preserve_json_constraints_refs_and_actual_returns() {
        let definition = json!({"function":{"name":"read_file","description":"Read a file */ safely.\nReturn actual data.",
            "parameters":{"type":"object","required":["path"],"additionalProperties":false,"$defs":{"Mode":{"enum":["text","binary"]}},"properties":{
                "path":{"type":"string","minLength":1,"pattern":"^[a-z]"},"limit":{"type":"integer","minimum":1,"maximum":50},
                "mode":{"$ref":"#/$defs/Mode"},"value":{"anyOf":[{"type":"null"},{"type":"boolean"}]}}},
            "outputSchema":{"type":"object","required":["content"],"properties":{"content":{"type":"string"}}}}});
        let libs = libraries::catalog(&[definition.clone()], &json!([]));
        let declarations = headers(&libs, &[definition]);
        for expected in [
            "declare namespace io",
            "function read(args:",
            "path:",
            "limit?:",
            "integer",
            "minimum=1",
            "maximum=50",
            "minLength=1",
            "pattern=",
            "no extra properties",
            "\"text\" | \"binary\"",
            "content:",
            "* /",
            "Return actual data.",
        ] {
            assert!(
                declarations.contains(expected),
                "{expected}: {declarations}"
            );
        }
        assert!(!declarations.contains("read_file"));
        parse(&declarations);
    }
    #[test]
    fn missing_return_contract_is_unknown_and_headers_are_stable() {
        let defs = [
            json!({"function":{"name":"read_file","parameters":{"type":"object","properties":{"odd-key":{"type":["string","null"]}}}}}),
        ];
        let libs = libraries::catalog(&defs, &json!([]));
        let declarations = headers(&libs, &defs);
        assert!(declarations.contains("): unknown;"));
        assert!(declarations.contains("\"odd-key\"?:"));
        assert_eq!(declarations, headers(&libs, &defs));
        parse(&declarations);
    }
    #[test]
    fn recursive_unresolved_and_composite_schemas_remain_explicit() {
        let schema = json!({"type":"object","properties":{"next":{"$ref":"#"},"external":{"$ref":"https://example.invalid/schema"},"tuple":{"type":"array","prefixItems":[{"type":"integer"},{"type":"boolean"}],"minItems":1,"items":false},"dictionary":{"type":"object","additionalProperties":{"type":["integer","null"],"minimum":0}},"union":{"oneOf":[{"const":"x"},{"type":"number"}]}}});
        let value = schema_type(&schema);
        assert!(value.contains("$ref=#"));
        assert!(value.contains("$ref=https://example.invalid/schema"));
        assert!(value.contains("exactly one schema must match"));
        assert!(value.contains("additional keys"));
        parse(&format!(
            "declare namespace sample {{ function run(args:{value}):unknown; }}"
        ));
    }
    #[test]
    fn compact_index_and_selected_contracts_expose_only_available_tools() {
        let defs = [
            json!({"function":{"name":"read_file","description":"Read a file","parameters":{"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}}}),
            json!({"function":{"name":"write_file","description":"Write a file","parameters":{"type":"object","required":["content"],"properties":{"content":{"type":"string"}}}}}),
        ];
        let libs = libraries::catalog(&defs, &json!([]));
        let directory = directory(&libs);
        assert!(directory.contains("Read and navigate code\n- io"));
        assert!(!directory.contains("- code_rust"));
        assert!(!directory.contains("io.read"));
        let listed = selected(&libs, &json!({"libraries":["io"],"overview":true})).unwrap();
        let page = overview(&listed, &defs);
        assert!(page.contains("io.read — Read a file"));
        assert!(page.contains("io.write — Write a file"));
        assert!(!page.contains("path:"));
        let readonly_defs = &defs[..1];
        let readonly = libraries::catalog(readonly_defs, &json!([]));
        assert!(!overview(&readonly, readonly_defs).contains("io.write"));
        let chosen = selected(&libs, &json!({"methods":["io.read"]})).unwrap();
        let signature = headers(&chosen, &defs);
        assert!(signature.contains("path:"));
        assert!(!signature.contains("function write("));
        assert!(selected(&libs, &json!({"methods":["io.missing"]})).is_err());
        assert!(selected(&libs, &json!({"libraries":["missing"]})).is_err());
        assert!(selected(&libs, &json!({"libraries":["io"],"overview":"yes"})).is_err());
        assert!(selected(&libs, &json!({})).is_err());
    }
    /// 2026-11-11: authors read "max 60000" in io.read's contract and wrote
    /// paging loops, though program code may read a whole file.
    #[test]
    fn program_declarations_state_the_whole_file_read() {
        let description = format!("Read a file. max_bytes is a file-size admission limit: oversized files are rejected, never prefix-truncated. To limit returned text, use max_chars and normally omit max_bytes. {} Reads are bounded.", crate::source_read::page_note(false));
        let defs = [json!({"function":{"name":"read_file","description":description,"parameters":{"type":"object","required":["path"],
            "properties":{"path":{"type":"string"},"max_chars":{"type":"integer","minimum":1000,"maximum":60000,"default":24000}}}}})];
        let libs = libraries::catalog(&defs, &json!([]));
        let chosen = selected(&libs, &json!({"methods":["io.read"]})).unwrap();
        let signature = headers(&chosen, &defs);
        assert!(signature.contains("whole file") && signature.contains("2000000"), "{signature}");
        assert!(!signature.contains("60000") && !signature.contains("24000"), "{signature}");
        assert!(signature.contains("file-size admission limit") && signature.contains("omit max_bytes"), "{signature}");
        assert!(signature.contains("Reads are bounded."), "the rest of the contract stays: {signature}");
    }
    #[test]
    fn original_contract_validator_preserves_local_references_and_constraints() {
        let schema = json!({"type":"object","$defs":{"Text":{"type":"string","minLength":1}},"properties":{"path":{"$ref":"#/$defs/Text"}},"required":["path"],"additionalProperties":false});
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&json!({"path":"ok"})));
        assert!(!validator.is_valid(&json!({"path":""})));
        assert!(!validator.is_valid(&json!({"path":"ok","extra":1})));
        assert!(!validator.is_valid(&json!({})));
    }
    #[test]
    #[cfg(not(feature="vm-measurements"))]
    fn all_available_host_headers_are_valid_declarations_including_keyword_methods() {
        let root = crate::tools::Workspace::new(std::env::current_dir().unwrap()).unwrap();
        // Use the actual host definitions, not a hand-written signature sample.
        let definitions = crate::agent_service::local_tools(&root, None, false);
        let mut libs = libraries::catalog(&definitions, &json!([]));
        let mut definitions = definitions;
        definitions
            .push(json!({"function":{"name":"plugin__delete","parameters":{"type":"object"}}}));
        libs.insert(
            "plugin".into(),
            BTreeMap::from([("delete".into(), "plugin__delete".into())]),
        );
        let declarations = headers(&libs, &definitions);
        let compact = overview(&libs, &definitions);
        eprintln!(
            "host catalog sizes: full_declarations={} compact_index={} bytes",
            declarations.len(),
            compact.len()
        );
        assert!(compact.len() < declarations.len());
        parse(&declarations);
    }

    #[test]
    fn context_as_code_header_contains_host_without_json_schemas() {
        let libs = libraries::builtin();
        let header = static_library_header(&libs);
        assert!(header.contains("declare namespace host"));
        assert!(header.contains("function contracts"));
        assert!(header.contains("function delegate(task: string): Promise<unknown>"));
        assert!(header.contains("context?: Record<string, unknown>"));
        assert!(header.contains("libraries?: string[]"));
        assert!(header.contains("library?: string"));
        assert!(header.contains("scope?: 'conversation' | 'project'"));
        assert!(header.contains("snapshot_seq?: number"));
        assert!(header.contains("stream?: 'stdout' | 'stderr'"));
        assert!(header.contains("MANDATORY exact-text edit rule"));
        assert!(header.contains("String.raw"));
        assert!(header.contains("query: string | RegExp"));
        assert!(header.contains("function generate_image"));
        assert!(header.contains("reference_artifact_ids?: string[]"));
        let browser = static_library_header(&std::collections::BTreeMap::from([(
            "browser".to_owned(),
            std::collections::BTreeMap::from([("open".to_owned(), "browser_open".to_owned())]),
        )]));
        assert!(browser.contains("interface BrowserConnection"));
        assert!(browser.contains("browser_connection?: BrowserConnection"));
        assert!(!header.contains("read_file"));
        assert!(!header.contains("parameters"));
    }

    #[test]
    fn edit_declarations_advertise_line_preview() {
        let header = static_library_header(&libraries::builtin());
        assert!(header.contains("index_refresh: unknown; preview?: string"));
    }
}
