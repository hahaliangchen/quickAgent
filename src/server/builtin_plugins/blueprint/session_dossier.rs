//! Read-only SQLite panoramic X-ray for Blueprint AI (`blueprint__inspect_session`).
//! Reads `codex_state.db` directly to give the Blueprint agent full-session visibility
//! (task goal & red-lines, plan & pinned facts, local/remote changed files, failure
//! clusters, environment cues, and winning commands) in one bounded call.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_USER_INSTRUCTIONS: usize = 20;
const OLD_TURN_INSTRUCTION_CHARS: usize = 800;
const RECENT_TURN_INSTRUCTION_CHARS: usize = 2_500;
const IN_FLIGHT_TAIL_LINES: usize = 30;
const TAIL_LINE_MAX_CHARS: usize = 240;

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn extract_user_text(data: &Value) -> String {
    let from_msg = crate::context::session_history::message_text(&data["message"]["content"]);
    if !from_msg.trim().is_empty() {
        return from_msg;
    }
    if let Some(text) = data["text"].as_str().filter(|s| !s.trim().is_empty()) {
        return text.to_owned();
    }
    crate::context::session_history::message_text(&data["content"])
}

fn append_tail_lines(ring: &mut VecDeque<String>, raw: &str, cap: usize) {
    let clean = crate::server::builtin_plugins::ssh::live::strip_ansi_csi(raw);
    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if ring.len() >= cap {
            ring.pop_front();
        }
        ring.push_back(clip(line.trim_end(), TAIL_LINE_MAX_CHARS));
    }
}

struct InFlightCall {
    call_id: String,
    seq: i64,
    step: i64,
    tool: String,
    target: String,
    server_id: Option<String>,
    started_at_ms: i64,
    last_output_at_ms: i64,
    tail_lines: VecDeque<String>,
}

fn clip(raw: &str, max_chars: usize) -> String {
    let clean = raw.trim();
    let mut chars = clean.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn clip_inline(raw: &str, max_chars: usize) -> String {
    let joined = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&joined, max_chars)
}

fn has_env_cue(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("venv")
        || lower.contains("pythonpath")
        || lower.contains("python3.")
        || lower.contains("127.0.0.1:")
        || lower.contains("localhost:")
        || lower.contains(".env")
        || lower.contains("--features")
        || lower.contains("cargo build")
        || lower.contains("pytest")
        || lower.contains("go test")
        || lower.contains("docker")
        || lower.contains("systemctl")
}

fn push_unique_capped(list: &mut Vec<String>, seen: &mut BTreeSet<String>, item: String, cap: usize) {
    if list.len() >= cap || item.is_empty() {
        return;
    }
    if seen.insert(item.clone()) {
        list.push(item);
    }
}

fn parse_tool_args(raw: &Value) -> Value {
    match raw {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!({})),
        Value::Object(_) => raw.clone(),
        _ => json!({}),
    }
}

fn target_of_tool(name: &str, args: &Value) -> String {
    if let Some(p) = args["path"].as_str().filter(|s| !s.is_empty()) {
        return p.to_owned();
    }
    if let Some(cmd) = args["cmd"]
        .as_str()
        .or_else(|| args["command"].as_str())
        .filter(|s| !s.is_empty())
    {
        return clip_inline(cmd, 120);
    }
    if let Some(files) = args["files"].as_array() {
        let paths: Vec<&str> = files
            .iter()
            .filter_map(|f| f["path"].as_str())
            .take(4)
            .collect();
        if !paths.is_empty() {
            return paths.join(", ");
        }
    }
    if let Some(paths) = args["paths"].as_array() {
        let items: Vec<&str> = paths.iter().filter_map(Value::as_str).take(4).collect();
        if !items.is_empty() {
            return items.join(", ");
        }
    }
    if let Some(server) = args["server_id"].as_str().filter(|s| !s.is_empty()) {
        return format!("server:{server}");
    }
    name.to_owned()
}

fn load_latest_plan(conn: &Connection, task_id: &str) -> (Value, Vec<String>) {
    let steps = conn
        .query_row(
            "SELECT steps_json FROM agent_task_plans WHERE task_id=?1 ORDER BY turn DESC LIMIT 1",
            params![task_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!([]));

    let pinned_facts = conn
        .query_row(
            "SELECT facts_json FROM agent_task_pinned_facts WHERE task_id=?1 ORDER BY turn DESC LIMIT 1",
            params![task_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default();

    (steps, pinned_facts)
}

pub(crate) fn inspect_session(state_root: &Path, args: &Value) -> Result<Value> {
    let task_id = args["task_id"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .context("task_id is required")?;
    let max_failures = args["max_failures"].as_u64().unwrap_or(25).clamp(1, 60) as usize;
    let max_commands = args["max_commands"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
    let max_recent = args["max_recent"].as_u64().unwrap_or(20).clamp(1, 60) as usize;

    let conn = crate::server::agent_service::open_db(state_root)?;
    let task_row: Option<(String, String, String, String, i64, i64, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT t.prompt, t.status, COALESCE(t.model, ''), COALESCE(t.provider_name, ''),
                    t.created_at, t.updated_at, m.title, m.workspace
             FROM agent_tasks t
             LEFT JOIN agent_session_meta m ON m.task_id = t.id
             WHERE t.id = ?1",
            params![task_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()?;

    let Some((prompt, status, model, provider, created_at, updated_at, title, workspace)) = task_row
    else {
        ensure!(false, "session task_id not found in state database: {task_id}");
        unreachable!()
    };

    let (plan_steps, pinned_facts) = load_latest_plan(&conn, task_id);

    let mut stmt = conn.prepare(
        "SELECT seq, timestamp, kind, data FROM agent_task_events WHERE task_id = ?1 ORDER BY seq ASC",
    )?;
    let mut rows = stmt.query(params![task_id])?;

    let mut total_events = 0usize;
    let mut total_steps = 0usize;
    let mut total_programs = 0usize;
    let mut total_tool_calls = 0usize;
    let mut total_cost_usd = 0.0f64;

    let mut current_turn: i64 = 1;
    let mut max_event_ts_ms: i64 = 0;
    let mut raw_user_instructions: Vec<(i64, i64, &'static str, String)> = Vec::new();
    let mut in_flight_calls: BTreeMap<String, InFlightCall> = BTreeMap::new();
    let mut last_in_flight_id: Option<String> = None;

    let mut calls_by_id: BTreeMap<String, (i64, String, Value)> = BTreeMap::new();
    let mut local_files = Vec::<String>::new();
    let mut local_files_seen = BTreeSet::<String>::new();
    let mut remote_files = Vec::<Value>::new();
    let mut remote_files_seen = BTreeSet::<String>::new();
    let mut env_files = Vec::<String>::new();
    let mut env_files_seen = BTreeSet::<String>::new();
    let mut env_cues = Vec::<String>::new();
    let mut env_cues_seen = BTreeSet::<String>::new();
    let mut remote_servers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut file_activity: BTreeMap<String, FileActivity> = BTreeMap::new();
    let mut step_files: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    let ws_prefix = workspace
        .as_deref()
        .map(|w| w.trim().replace('\\', "/").trim_end_matches('/').to_owned())
        .filter(|w| !w.is_empty());

    let mut failures = Vec::<Value>::new();
    let mut cluster_map: BTreeMap<(String, String), (usize, i64, i64, String)> = BTreeMap::new();
    let mut winning_commands = Vec::<Value>::new();
    let mut winning_cmd_seen = BTreeSet::<String>::new();
    let mut last_assistant_text = String::new();
    let mut latest_seq: i64 = 0;
    let mut recent_tail: VecDeque<Value> = VecDeque::with_capacity(max_recent + 1);
    let push_recent = |tail: &mut VecDeque<Value>, entry: Value| {
        if tail.len() >= max_recent {
            tail.pop_front();
        }
        tail.push_back(entry);
    };

    while let Some(row) = rows.next()? {
        total_events += 1;
        let seq: i64 = row.get(0)?;
        latest_seq = latest_seq.max(seq);
        let timestamp_ms: i64 = row.get::<_, Option<i64>>(1)?.unwrap_or(0);
        max_event_ts_ms = max_event_ts_ms.max(timestamp_ms);
        let kind: String = row.get(2)?;
        let raw_data: String = row.get(3)?;

        // Fast-skip high-volume streaming delta/debug events before JSON parsing.
        // Keep `shell/output` so in-flight local commands retain their latest 30-line tail.
        if kind == "assistant/delta" || kind == "debug/context_request" || kind == "debug/context_end" {
            continue;
        }

        let data: Value = match serde_json::from_str(&raw_data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let step = data["step"].as_i64().unwrap_or(0);

        match kind.as_str() {
            "turn/start" => {
                let turn = data["turn"].as_i64().unwrap_or(0);
                if turn > 0 {
                    current_turn = turn;
                }
                push_recent(&mut recent_tail, json!({
                    "seq": seq,
                    "kind": "turn/start",
                    "turn": turn,
                }));
            }
            "turn/end" => {
                in_flight_calls.clear();
                last_in_flight_id = None;
                let reason = data["reason"]["kind"].as_str().unwrap_or("completed");
                let err = data["reason"]["error"]["message"]
                    .as_str()
                    .or_else(|| data["reason"]["message"].as_str())
                    .map(|s| clip_inline(s, 180));
                push_recent(&mut recent_tail, json!({
                    "seq": seq,
                    "kind": "turn/end",
                    "reason": reason,
                    "error": err,
                }));
            }
            "user/message" => {
                let turn = data["turn"]
                    .as_i64()
                    .filter(|&t| t > 0)
                    .unwrap_or(current_turn);
                current_turn = turn;
                let text = extract_user_text(&data);
                if !text.trim().is_empty() {
                    push_recent(&mut recent_tail, json!({
                        "seq": seq,
                        "turn": turn,
                        "kind": "user/message",
                        "summary": clip_inline(&text, 200),
                    }));
                    raw_user_instructions.push((turn, seq, "user/message", text));
                }
            }
            "user/interjection" => {
                let turn = data["turn"]
                    .as_i64()
                    .filter(|&t| t > 0)
                    .unwrap_or(current_turn);
                let text = extract_user_text(&data);
                if !text.trim().is_empty() {
                    push_recent(&mut recent_tail, json!({
                        "seq": seq,
                        "turn": turn,
                        "kind": "user/interjection",
                        "summary": clip_inline(&text, 200),
                    }));
                    raw_user_instructions.push((turn, seq, "user/interjection", text));
                }
            }
            "shell/output" => {
                let target_id = data["callId"]
                    .as_str()
                    .or_else(|| data["call_id"].as_str())
                    .filter(|cid| in_flight_calls.contains_key(*cid))
                    .map(str::to_owned)
                    .or_else(|| {
                        in_flight_calls
                            .values()
                            .filter(|c| !c.tool.starts_with("ssh__"))
                            .max_by_key(|c| c.seq)
                            .map(|c| c.call_id.clone())
                    })
                    .or_else(|| last_in_flight_id.clone());
                if let Some(cid) = target_id
                    && let Some(call) = in_flight_calls.get_mut(&cid)
                {
                    let mut saw_output = false;
                    for field in ["text", "chunk", "output", "stdout", "stderr"] {
                        if let Some(text) = data[field].as_str().filter(|s| !s.is_empty()) {
                            append_tail_lines(&mut call.tail_lines, text, IN_FLIGHT_TAIL_LINES);
                            saw_output = true;
                        }
                    }
                    if saw_output || timestamp_ms > 0 {
                        call.last_output_at_ms = call.last_output_at_ms.max(timestamp_ms);
                    }
                }
            }
            "plan/updated" => {
                let active_step = data["steps"].as_array().and_then(|steps| {
                    steps.iter().find_map(|s| {
                        (s["status"].as_str() == Some("in_progress"))
                            .then(|| s["title"].as_str().map(|t| clip_inline(t, 120)))
                            .flatten()
                    })
                });
                let done_count = data["steps"]
                    .as_array()
                    .map(|steps| steps.iter().filter(|s| s["status"].as_str() == Some("done")).count())
                    .unwrap_or(0);
                let total_plan = data["steps"].as_array().map(|s| s.len()).unwrap_or(0);
                push_recent(&mut recent_tail, json!({
                    "seq": seq,
                    "kind": "plan/updated",
                    "done": done_count,
                    "total": total_plan,
                    "in_progress": active_step,
                }));
            }
            "step/start" => {
                total_steps += 1;
            }
            "program/start" => {
                total_programs += 1;
            }
            "context/usage" => {
                if let Some(cost) = data["cost_amount"].as_f64() {
                    total_cost_usd += cost;
                }
            }
            "assistant/message" => {
                let text = crate::context::session_history::message_text(&data["message"]["content"]);
                if !text.trim().is_empty() {
                    push_recent(&mut recent_tail, json!({
                        "seq": seq,
                        "step": step,
                        "kind": "assistant/message",
                        "summary": clip_inline(&text, 200),
                    }));
                    last_assistant_text = text;
                }
            }
            "workspace/changes" => {
                if let Some(files) = data["files"].as_array() {
                    for f in files {
                        if let Some(p) = f["path"].as_str().filter(|s| !s.is_empty()) {
                            push_unique_capped(&mut local_files, &mut local_files_seen, p.to_owned(), 60);
                        }
                    }
                }
            }
            "tool/call" => {
                total_tool_calls += 1;
                let name = data["name"].as_str().unwrap_or_default().to_owned();
                let args = parse_tool_args(&data["arguments"]);
                let server_id = args["server_id"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                if let Some(cid) = data["callId"].as_str().or_else(|| data["call_id"].as_str()) {
                    calls_by_id.insert(cid.to_owned(), (step, name.clone(), args.clone()));
                    let base_target = target_of_tool(&name, &args);
                    let in_flight_target = match server_id.as_deref() {
                        Some(sid) if name.starts_with("ssh__") && !base_target.starts_with("server:") => {
                            if base_target == name {
                                format!("server:{sid}")
                            } else {
                                format!("server:{sid} · {base_target}")
                            }
                        }
                        _ => base_target,
                    };
                    in_flight_calls.insert(
                        cid.to_owned(),
                        InFlightCall {
                            call_id: cid.to_owned(),
                            seq,
                            step,
                            tool: name.clone(),
                            target: in_flight_target,
                            server_id: server_id.clone(),
                            started_at_ms: timestamp_ms,
                            last_output_at_ms: timestamp_ms,
                            tail_lines: VecDeque::new(),
                        },
                    );
                    last_in_flight_id = Some(cid.to_owned());
                }
                if name.starts_with("ssh__") {
                    if let Some(sid) = server_id.as_deref() {
                        let cwds = remote_servers.entry(sid.to_owned()).or_default();
                        if let Some(cwd) = args["cwd"].as_str().filter(|s| !s.is_empty()) {
                            cwds.insert(cwd.to_owned());
                        }
                    }
                }
                if let Some(path) = args["path"].as_str().or_else(|| args["remote_path"].as_str()) {
                    if path.contains(".env") {
                        push_unique_capped(&mut env_files, &mut env_files_seen, path.to_owned(), 12);
                    }
                }
                if let Some(cmd) = args["cmd"].as_str().or_else(|| args["command"].as_str()) {
                    if has_env_cue(cmd) {
                        push_unique_capped(&mut env_cues, &mut env_cues_seen, clip_inline(cmd, 180), 15);
                    }
                }
                // Record coarse file-level read/edit activity & symbol cues for Blueprint class extraction.
                record_tool_call_activity(
                    &name,
                    &args,
                    step,
                    ws_prefix.as_deref(),
                    &mut file_activity,
                    &mut step_files,
                );
            }
            "program/error" => {
                let msg = data["error"]["error"]
                    .as_str()
                    .or_else(|| data["error"]["message"].as_str())
                    .or_else(|| data["message"].as_str())
                    .unwrap_or("program error");
                let preview = clip_inline(msg, 220);
                push_recent(&mut recent_tail, json!({
                    "seq": seq,
                    "step": step,
                    "kind": "program/error",
                    "error": preview,
                }));
                let cluster_key = ("program/error".to_owned(), clip_inline(msg, 64));
                cluster_map
                    .entry(cluster_key)
                    .and_modify(|entry| {
                        entry.0 += 1;
                        entry.2 = entry.2.max(step);
                    })
                    .or_insert((1, step, step, preview.clone()));
                if failures.len() < max_failures {
                    failures.push(json!({
                        "step": step,
                        "event_seq": seq,
                        "kind": "program/error",
                        "tool": "run_js",
                        "target": "",
                        "error": preview,
                    }));
                }
            }
            "tool/result" => {
                let msg = &data["message"];
                let cid = msg["toolCallId"]
                    .as_str()
                    .or_else(|| data["callId"].as_str())
                    .or_else(|| data["call_id"].as_str())
                    .unwrap_or_default();
                in_flight_calls.remove(cid);
                if last_in_flight_id.as_deref() == Some(cid) {
                    last_in_flight_id = in_flight_calls
                        .values()
                        .max_by_key(|c| c.seq)
                        .map(|c| c.call_id.clone());
                }
                let (call_step, tool_name, call_args) = calls_by_id
                    .get(cid)
                    .cloned()
                    .unwrap_or_else(|| (step, "unknown".to_owned(), json!({})));
                let res = &data["meta"]["result"];
                let is_err = msg["isError"].as_bool() == Some(true);
                let exit_code = res["exit_code"].as_i64();
                let nonzero_exit = exit_code.is_some_and(|code| code != 0);

                // Extract environment cues from tool stdout/stderr (e.g. `ps` showing venv PYTHONPATH).
                for field in ["stdout", "stderr"] {
                    if let Some(text) = res[field].as_str() {
                        for line in text.lines() {
                            if has_env_cue(line) {
                                push_unique_capped(
                                    &mut env_cues,
                                    &mut env_cues_seen,
                                    clip_inline(line, 180),
                                    15,
                                );
                            }
                        }
                    }
                }

                // Track successful file writes (local & remote).
                if !is_err {
                    match tool_name.as_str() {
                        "edit_file" | "write_file" | "replace_range" => {
                            if res["changed"].as_bool() == Some(true)
                                && let Some(p) = res["path"].as_str()
                            {
                                push_unique_capped(&mut local_files, &mut local_files_seen, p.to_owned(), 60);
                            }
                        }
                        "apply_edits" => {
                            if let Some(files) = res["files"].as_array() {
                                for f in files {
                                    if f["changed"].as_bool() == Some(true)
                                        && let Some(p) = f["path"].as_str()
                                    {
                                        push_unique_capped(&mut local_files, &mut local_files_seen, p.to_owned(), 60);
                                    }
                                }
                            }
                        }
                        "ssh__edit_file" | "ssh__write_file" | "ssh__upload" => {
                            let sid = call_args["server_id"].as_str().unwrap_or_default();
                            let rpath = call_args["path"]
                                .as_str()
                                .or_else(|| call_args["remote_path"].as_str())
                                .unwrap_or_default();
                            if !rpath.is_empty() {
                                let key = format!("{sid}:{rpath}:{tool_name}");
                                if remote_files.len() < 40 && remote_files_seen.insert(key) {
                                    remote_files.push(json!({
                                        "server_id": sid,
                                        "path": rpath,
                                        "op": tool_name,
                                    }));
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let target_preview = target_of_tool(&tool_name, &call_args);
                let tail_err = if is_err || nonzero_exit {
                    res["error"]
                        .as_str()
                        .or_else(|| res["stderr"].as_str().filter(|s| !s.trim().is_empty()))
                        .or_else(|| msg["content"][0]["text"].as_str())
                        .map(|s| clip_inline(s, 180))
                } else {
                    None
                };
                push_recent(&mut recent_tail, json!({
                    "seq": seq,
                    "step": call_step,
                    "kind": "tool/result",
                    "tool": tool_name,
                    "target": clip_inline(&target_preview, 120),
                    "ok": !is_err && !nonzero_exit,
                    "exit_code": exit_code,
                    "error": tail_err,
                }));

                // Track failures & non-zero shell/SSH commands.
                if is_err || nonzero_exit {
                    let target = target_of_tool(&tool_name, &call_args);
                    let err_text = res["error"]
                        .as_str()
                        .or_else(|| res["stderr"].as_str().filter(|s| !s.trim().is_empty()))
                        .or_else(|| res["stdout"].as_str().filter(|s| !s.trim().is_empty()))
                        .or_else(|| msg["content"][0]["text"].as_str())
                        .unwrap_or("tool error");
                    let preview = clip_inline(err_text, 220);
                    let key = (tool_name.clone(), clip(&target, 80));
                    cluster_map
                        .entry(key)
                        .and_modify(|entry| {
                            entry.0 += 1;
                            entry.2 = entry.2.max(call_step);
                        })
                        .or_insert((1, call_step, call_step, preview.clone()));
                    if failures.len() < max_failures {
                        failures.push(json!({
                            "step": call_step,
                            "event_seq": seq,
                            "kind": if nonzero_exit { "nonzero_exit" } else { "tool_error" },
                            "tool": tool_name,
                            "target": target,
                            "exit_code": exit_code,
                            "error": preview,
                        }));
                    }
                } else if matches!(
                    tool_name.as_str(),
                    "exec_command" | "ssh__exec" | "verify_test" | "verify_check"
                ) && exit_code.unwrap_or(0) == 0
                {
                    let cmd = call_args["cmd"]
                        .as_str()
                        .or_else(|| call_args["command"].as_str())
                        .or_else(|| res["cmd"].as_str())
                        .unwrap_or(tool_name.as_str());
                    let scope = if tool_name == "ssh__exec" { "remote" } else { "local" };
                    let cwd = call_args["cwd"]
                        .as_str()
                        .or_else(|| call_args["workdir"].as_str())
                        .unwrap_or("");
                    let dedup_key = format!("{scope}:{cwd}:{}", clip_inline(cmd, 140));
                    if winning_cmd_seen.insert(dedup_key) {
                        winning_commands.push(json!({
                            "step": call_step,
                            "event_seq": seq,
                            "scope": scope,
                            "cwd": cwd,
                            "cmd": clip_inline(cmd, 180),
                        }));
                    }
                }
            }
            _ => {}
        }
    }

    // Keep the most recent `max_commands` winning commands (end of run usually holds final build/verify).
    if winning_commands.len() > max_commands {
        let drop_n = winning_commands.len() - max_commands;
        winning_commands.drain(0..drop_n);
    }

    // Build chronological multi-turn user instruction timeline (user/message + user/interjection).
    if raw_user_instructions.is_empty() && !prompt.trim().is_empty() {
        raw_user_instructions.push((1, 0, "user/message", prompt.clone()));
    }
    if raw_user_instructions.len() > MAX_USER_INSTRUCTIONS {
        let drop_n = raw_user_instructions.len() - MAX_USER_INSTRUCTIONS;
        raw_user_instructions.drain(0..drop_n);
    }
    let distinct_turns: Vec<i64> = raw_user_instructions
        .iter()
        .map(|(turn, _, _, _)| *turn)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let recent_turns: BTreeSet<i64> = distinct_turns.into_iter().rev().take(2).collect();
    let user_instructions: Vec<Value> = raw_user_instructions
        .into_iter()
        .map(|(turn, seq, kind, text)| {
            let limit = if recent_turns.contains(&turn) {
                RECENT_TURN_INSTRUCTION_CHARS
            } else {
                OLD_TURN_INSTRUCTION_CHARS
            };
            json!({
                "turn": turn,
                "seq": seq,
                "kind": kind,
                "text": clip(&text, limit),
            })
        })
        .collect();
    let latest_user_instruction = user_instructions.last().cloned().unwrap_or(Value::Null);

    // Collect currently in-flight tool calls with live SSH / shell terminal tails.
    let now_ms = now_epoch_ms().max(max_event_ts_ms).max(updated_at);
    let mut in_flight_vec: Vec<InFlightCall> = in_flight_calls.into_values().collect();
    in_flight_vec.sort_by_key(|c| c.seq);
    let in_flight_tools: Vec<Value> = in_flight_vec
        .into_iter()
        .map(|call| {
            let mut last_out_ms = call.last_output_at_ms;
            let mut tail_lines: Vec<String> = call.tail_lines.into_iter().collect();
            if call.tool.starts_with("ssh__")
                && let Some(snap) = crate::server::builtin_plugins::ssh::live::tail_lines_for_task(
                    task_id,
                    call.server_id.as_deref(),
                    IN_FLIGHT_TAIL_LINES,
                )
            {
                if !snap.tail_lines.is_empty() {
                    tail_lines = snap.tail_lines;
                }
                if snap.last_output_at_ms > 0 {
                    last_out_ms = last_out_ms.max(snap.last_output_at_ms);
                }
            }
            let started_ms = if call.started_at_ms > 0 {
                call.started_at_ms
            } else {
                now_ms
            };
            let last_active_ms = if last_out_ms > 0 {
                last_out_ms.max(started_ms)
            } else {
                started_ms
            };
            let elapsed_sec = ((now_ms - started_ms).max(0)) / 1000;
            let idle_sec = ((now_ms - last_active_ms).max(0)) / 1000;
            json!({
                "call_id": call.call_id,
                "seq": call.seq,
                "step": call.step,
                "tool": call.tool,
                "target": call.target,
                "elapsed_sec": elapsed_sec,
                "idle_sec": idle_sec,
                "tail_lines": tail_lines,
            })
        })
        .collect();

    let mut failure_clusters: Vec<Value> = cluster_map
        .into_iter()
        .filter(|(_, (count, _, _, _))| *count >= 2)
        .map(|((tool, target), (count, first_step, last_step, sample))| {
            json!({
                "tool": tool,
                "target": target,
                "count": count,
                "first_step": first_step,
                "last_step": last_step,
                "sample_error": sample,
            })
        })
        .collect();
    failure_clusters.sort_by(|a, b| b["count"].as_u64().cmp(&a["count"].as_u64()));
    failure_clusters.truncate(12);

    let servers_summary: Vec<Value> = remote_servers
        .into_iter()
        .map(|(id, cwds)| {
            json!({
                "server_id": id,
                "cwds": cwds.into_iter().take(6).collect::<Vec<_>>(),
            })
        })
        .collect();

    let duration_sec = ((updated_at - created_at).max(0)) / 1000;
    let (class_candidates, candidate_chains) = build_class_candidates(
        &file_activity,
        &step_files,
        &local_files,
        &remote_files,
        &env_files,
        &pinned_facts,
    );

    Ok(json!({
        "task": {
            "task_id": task_id,
            "title": title.unwrap_or_else(|| clip_inline(&prompt, 80)),
            "workspace": workspace.unwrap_or_default(),
            "status": status,
            "model": model,
            "provider": provider,
            "prompt": clip(&prompt, 3_500),
            "duration_sec": duration_sec,
            "total_events": total_events,
            "latest_seq": latest_seq,
            "total_steps": total_steps,
            "total_programs": total_programs,
            "total_tool_calls": total_tool_calls,
            "estimated_cost_usd": (total_cost_usd * 10_000.0).round() / 10_000.0,
        },
        "plan": {
            "steps": plan_steps,
            "pinned_facts": pinned_facts,
        },
        "user_instructions": user_instructions,
        "latest_user_instruction": latest_user_instruction,
        "instruction_precedence_note": "Read `user_instructions` chronologically from turn 1 to the latest turn (or from the tail backwards). Later turns supersede earlier conflicting constraints; a short follow-up like '继续' or '再连一下' inherits the previous substantive task and authorizations — NEVER revert to turn-1 `task.prompt` constraints that were already overridden.",
        "in_flight_tools": in_flight_tools,
        "recent_tail": recent_tail.into_iter().collect::<Vec<_>>(),
        "changed_files": {
            "local": local_files,
            "remote": remote_files,
        },
        "class_candidates": class_candidates,
        "candidate_chains": candidate_chains,
        "failure_clusters": failure_clusters,
        "failures": failures,
        "environment_signals": {
            "remote_servers": servers_summary,
            "env_files_touched": env_files,
            "cues": env_cues,
        },
        "winning_commands": winning_commands,
        "final_outcome": clip(&last_assistant_text, 1_500),
        "drilldown_hint": "Call `session.read_session_history({ task_id, event_seq })` to inspect any specific failure/step in full, or `{ task_id, programs: true }` for raw JS code.",
    }))
}

#[derive(Default)]
struct FileActivity {
    reads: usize,
    edits: usize,
    first_step: i64,
    last_step: i64,
    symbols: BTreeSet<String>,
    is_remote: bool,
    server_id: Option<String>,
}

fn normalize_session_path(raw: &str, ws_prefix: Option<&str>, is_remote: bool) -> Option<String> {
    let mut p = raw.trim().replace('\\', "/");
    if p.is_empty() {
        return None;
    }
    if !is_remote {
        if let Some(ws) = ws_prefix {
            if let Some(rest) = p.strip_prefix(ws).and_then(|s| s.strip_prefix('/')) {
                p = rest.to_owned();
            }
        }
        while let Some(rest) = p.strip_prefix("./") {
            p = rest.to_owned();
        }
        // Ignore obvious noise/build artifacts/scratch/logs outside source tree.
        let lower = p.to_ascii_lowercase();
        if p.starts_with('/')
            || lower.starts_with("target/")
            || lower.starts_with("tmp/")
            || lower.starts_with("temp/")
            || lower.starts_with("node_modules/")
            || lower.starts_with(".git/")
            || lower.starts_with(".codex-workspace-mcp/")
            || lower.starts_with(".quickagent/")
            || lower.contains("/node_modules/")
            || lower.contains("/dist/")
            || lower.contains("/.staging/")
            || lower.ends_with(".log")
            || lower.ends_with("package-lock.json")
            || lower.ends_with("cargo.lock")
            || lower.ends_with(".png")
            || lower.ends_with(".jpg")
            || lower.ends_with(".svg")
        {
            return None;
        }
    } else {
        // Ignore remote upload staging chunks / tmp logs.
        let lower = p.to_ascii_lowercase();
        if lower.contains("/.staging/")
            || lower.contains("/part-")
            || lower.ends_with(".tar.gz")
            || lower.ends_with(".gz")
            || lower.ends_with(".log")
        {
            return None;
        }
    }
    Some(p)
}

fn is_boilerplate_symbol(sym: &str) -> bool {
    let lower = sym.to_ascii_lowercase();
    sym.len() < 4
        || sym.len() > 48
        || sym.starts_with("__")
        || matches!(
            lower.as_str(),
            "new"
                | "main"
                | "drop"
                | "fmt"
                | "clone"
                | "from"
                | "into"
                | "default"
                | "init"
                | "test"
                | "setup"
                | "teardown"
                | "call"
                | "server"
                | "source"
                | "agent"
                | "helper"
        )
}

fn extract_symbols_from_snippet(text: &str, out: &mut BTreeSet<String>) {
    if out.len() >= 8 {
        return;
    }
    for line in text.lines().take(60) {
        let t = line.trim();
        let sym = if let Some(rest) = t
            .strip_prefix("pub async fn ")
            .or_else(|| t.strip_prefix("pub(crate) async fn "))
            .or_else(|| t.strip_prefix("pub(super) async fn "))
            .or_else(|| t.strip_prefix("pub fn "))
            .or_else(|| t.strip_prefix("pub(crate) fn "))
            .or_else(|| t.strip_prefix("pub(super) fn "))
            .or_else(|| t.strip_prefix("async fn "))
            .or_else(|| t.strip_prefix("fn "))
            .or_else(|| t.strip_prefix("export async function "))
            .or_else(|| t.strip_prefix("export function "))
            .or_else(|| t.strip_prefix("async function "))
            .or_else(|| t.strip_prefix("function "))
            .or_else(|| t.strip_prefix("async def "))
            .or_else(|| t.strip_prefix("def "))
        {
            rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .next()
        } else if let Some(rest) = t.strip_prefix("func ") {
            // Only strip receiver when line actually starts with `func (recv ...)` (not `func foo(...) (bool, error)`).
            let after_recv = if rest.starts_with('(')
                && let Some(idx) = rest.find(") ")
            {
                &rest[idx + 2..]
            } else {
                rest
            };
            after_recv
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .next()
        } else {
            None
        };
        if let Some(s) = sym.filter(|s| {
            !is_boilerplate_symbol(s)
                && !matches!(*s, "bool" | "string" | "error" | "int64" | "uint64" | "float64")
        }) {
            out.insert(s.to_owned());
            if out.len() >= 8 {
                break;
            }
        }
    }
}

fn collect_edit_snippets(obj: &Value) -> String {
    let mut buf = String::new();
    for key in ["new_text", "content", "old_text"] {
        if let Some(s) = obj[key].as_str() {
            buf.push_str(s);
            buf.push('\n');
        }
    }
    if let Some(edits) = obj["edits"].as_array() {
        for edit in edits.iter().take(6) {
            for key in ["new_text", "old_text"] {
                if let Some(s) = edit[key].as_str() {
                    buf.push_str(s);
                    buf.push('\n');
                }
            }
        }
    }
    buf
}

fn record_tool_call_activity(
    name: &str,
    args: &Value,
    step: i64,
    ws_prefix: Option<&str>,
    activity: &mut BTreeMap<String, FileActivity>,
    step_files: &mut BTreeMap<i64, Vec<String>>,
) {
    let mut touch = |raw: &str, is_edit: bool, is_remote: bool, server_id: Option<&str>, snippet: Option<&str>| {
        let Some(norm) = normalize_session_path(raw, ws_prefix, is_remote) else {
            return;
        };
        let key = if is_remote {
            format!("remote:{}:{}", server_id.unwrap_or("ssh"), norm)
        } else {
            norm
        };
        let entry = activity.entry(key.clone()).or_insert_with(|| FileActivity {
            first_step: step,
            last_step: step,
            is_remote,
            server_id: server_id.map(str::to_owned),
            ..Default::default()
        });
        if is_edit {
            entry.edits += 1;
        } else {
            entry.reads += 1;
        }
        if entry.first_step == 0 || (step > 0 && step < entry.first_step) {
            entry.first_step = step;
        }
        entry.last_step = entry.last_step.max(step);
        if let Some(code) = snippet {
            extract_symbols_from_snippet(code, &mut entry.symbols);
        }
        let list = step_files.entry(step).or_default();
        if !list.contains(&key) && list.len() < 16 {
            list.push(key);
        }
    };

    match name {
        "read_file" | "read_file_lines" | "read_file_chunk" => {
            if let Some(p) = args["path"].as_str() {
                touch(p, false, false, None, None);
            }
        }
        "parallel_read_batch" => {
            if let Some(paths) = args["paths"].as_array() {
                for p in paths.iter().filter_map(Value::as_str) {
                    touch(p, false, false, None, None);
                }
            }
        }
        "edit_file" | "write_file" | "replace_range" => {
            if let Some(p) = args["path"].as_str() {
                let snippet = collect_edit_snippets(args);
                touch(p, true, false, None, (!snippet.is_empty()).then_some(snippet.as_str()));
            }
        }
        "apply_edits" => {
            if let Some(files) = args["files"].as_array() {
                for f in files {
                    if let Some(p) = f["path"].as_str() {
                        let snippet = collect_edit_snippets(f);
                        touch(p, true, false, None, (!snippet.is_empty()).then_some(snippet.as_str()));
                    }
                }
            }
        }
        "ssh__read_file" => {
            let sid = args["server_id"].as_str();
            if let Some(p) = args["path"].as_str().or_else(|| args["remote_path"].as_str()) {
                touch(p, false, true, sid, None);
            }
        }
        "ssh__edit_file" | "ssh__write_file" | "ssh__upload" => {
            let sid = args["server_id"].as_str();
            if let Some(p) = args["path"].as_str().or_else(|| args["remote_path"].as_str()) {
                let snippet = collect_edit_snippets(args);
                touch(p, true, true, sid, (!snippet.is_empty()).then_some(snippet.as_str()));
            }
        }
        _ => {}
    }
}

fn to_pascal(raw: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            if cap {
                out.push(ch.to_ascii_uppercase());
                cap = false;
            } else {
                out.push(ch);
            }
        } else {
            cap = true;
        }
    }
    if out.is_empty() { "Module".into() } else { out }
}

fn to_slug(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_sep = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_sep = false;
        } else if !prev_sep && !out.is_empty() {
            out.push('_');
            prev_sep = true;
        }
    }
    out.trim_end_matches('_').to_owned()
}

fn classify_file_module(path: &str, is_remote: bool, server_id: Option<&str>) -> (String, String, &'static str) {
    if is_remote {
        let sid = server_id.unwrap_or("remote");
        let name = format!("Remote{}", to_pascal(sid));
        return (format!("ext.{}", to_slug(sid)), name, "external");
    }
    let lower = path.to_ascii_lowercase();
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let file_name = parts.last().copied().unwrap_or(path);
    let stem = file_name.split('.').next().unwrap_or(file_name);

    let is_test = lower.contains("/tests/")
        || lower.starts_with("tests/")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_tests.rs")
        || lower.ends_with("_tests.inc.rs")
        || lower.ends_with("_test.inc.rs")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.tsx")
        || lower.ends_with(".test.mjs")
        || lower.ends_with(".spec.mjs")
        || lower.ends_with(".spec.ts")
        || lower.starts_with("test_")
        || lower.contains("/test_");
    if is_test {
        let dir = parts.get(parts.len().saturating_sub(2)).copied().unwrap_or("suite");
        let area = if matches!(dir, "tests" | "parts") {
            parts.get(parts.len().saturating_sub(3)).copied().unwrap_or(stem)
        } else {
            dir
        };
        let cls = format!("{}Tests", to_pascal(area));
        return (format!("test.{}", to_slug(&cls)), cls, "test_guard");
    }

    let is_frontend = lower.starts_with("frontend/")
        || lower.ends_with(".tsx")
        || lower.ends_with(".jsx")
        || lower.ends_with(".vue");
    if is_frontend {
        if let Some(pos) = parts.iter().position(|&p| p == "builtin_plugins")
            && let Some(&plugin) = parts.get(pos + 1)
        {
            let cls = format!("{}Ui", to_pascal(plugin));
            return (format!("ui.{}", to_slug(plugin)), cls, "trigger_ui");
        }
        let cls = format!("{}View", to_pascal(stem));
        return (format!("ui.{}", to_slug(stem)), cls, "trigger_ui");
    }

    let is_api = lower.contains("route")
        || lower.contains("handler")
        || lower.contains("controller")
        || lower.contains("endpoint")
        || lower.contains("proxy")
        || lower.contains("/api/");
    let is_store = lower.contains("store")
        || lower.contains("database")
        || lower.contains("sqlite")
        || lower.contains("repo")
        || lower.contains("ledger")
        || lower.contains("schema")
        || lower.contains("migration")
        || lower.contains(".env")
        || lower.contains("config");

    let kind = if is_api {
        "trigger_api"
    } else if is_store {
        "store"
    } else {
        "service"
    };

    // Coarse-grained business module grouping (cover business areas rather than 1-file-per-class fragmentation).
    let group_label = if let Some(pos) = parts.iter().position(|&p| p == "builtin_plugins")
        && let Some(&plugin) = parts.get(pos + 1)
    {
        format!("{}_{}", plugin, if is_api { "api" } else if is_store { "store" } else { "service" })
    } else if parts.len() == 2 && !matches!(parts[0], "src" | "internal" | "pkg" | "cmd" | "app" | "lib" | "docs") {
        // Feature top-level package like `lightrag_service/{service.py, kg_engine.py, main.py, SKILL.md}` -> one cohesive class!
        parts[0].to_owned()
    } else if parts.len() >= 3 {
        let dir = parts[parts.len() - 2];
        if matches!(dir, "src" | "internal" | "pkg" | "cmd" | "app" | "lib" | "parts") {
            stem.to_owned()
        } else if let Some((prefix, _)) = stem.split_once('_')
            && prefix.len() >= 2
            && matches!(prefix, "kg" | "es" | "ssh" | "db" | "auth" | "session" | "workspace" | "agent")
        {
            // Group sibling feature files like `internal/server/kg_{wire,agent,backlink,tools}.go` -> `ServerKg`.
            format!("{dir}_{prefix}")
        } else {
            format!("{dir}_{stem}")
        }
    } else {
        stem.to_owned()
    };

    let cls = to_pascal(&group_label);
    let prefix = match kind {
        "trigger_api" => "api",
        "store" => "store",
        _ => "svc",
    };
    (format!("{prefix}.{}", to_slug(&group_label)), cls, kind)
}

fn layer_order(kind: &str) -> u8 {
    match kind {
        "trigger_ui" => 0,
        "trigger_api" | "trigger_job" => 1,
        "service" => 2,
        "store" | "external" => 3,
        "test_guard" => 4,
        _ => 2,
    }
}

fn build_class_candidates(
    activity: &BTreeMap<String, FileActivity>,
    step_files: &BTreeMap<i64, Vec<String>>,
    local_changed: &[String],
    remote_changed: &[Value],
    env_files: &[String],
    pinned_facts: &[String],
) -> (Vec<Value>, Vec<Value>) {
    struct Cluster {
        id: String,
        name: String,
        kind: &'static str,
        files: BTreeSet<String>,
        symbols: BTreeSet<String>,
        reads: usize,
        edits: usize,
        first_step: i64,
        last_step: i64,
    }

    let mut clusters: BTreeMap<String, Cluster> = BTreeMap::new();
    let mut file_to_cluster: BTreeMap<String, String> = BTreeMap::new();

    for (key, act) in activity {
        let raw_path = if act.is_remote {
            key.splitn(3, ':').nth(2).unwrap_or(key)
        } else {
            key.as_str()
        };
        let (cid, cname, ckind) = classify_file_module(raw_path, act.is_remote, act.server_id.as_deref());
        file_to_cluster.insert(key.clone(), cid.clone());
        let entry = clusters.entry(cid.clone()).or_insert_with(|| Cluster {
            id: cid,
            name: cname,
            kind: ckind,
            files: BTreeSet::new(),
            symbols: BTreeSet::new(),
            reads: 0,
            edits: 0,
            first_step: act.first_step,
            last_step: act.last_step,
        });
        entry.files.insert(raw_path.to_owned());
        for sym in &act.symbols {
            entry.symbols.insert(format!("{raw_path}#{sym}"));
        }
        entry.reads += act.reads;
        entry.edits += act.edits;
        if act.first_step > 0 && (entry.first_step == 0 || act.first_step < entry.first_step) {
            entry.first_step = act.first_step;
        }
        entry.last_step = entry.last_step.max(act.last_step);
    }

    // Also ensure changed local/remote files appear even if they came only from workspace/changes.
    for p in local_changed {
        if let Some(norm) = normalize_session_path(p, None, false)
            && !file_to_cluster.contains_key(&norm)
        {
            let (cid, cname, ckind) = classify_file_module(&norm, false, None);
            file_to_cluster.insert(norm.clone(), cid.clone());
            let entry = clusters.entry(cid.clone()).or_insert_with(|| Cluster {
                id: cid,
                name: cname,
                kind: ckind,
                files: BTreeSet::new(),
                symbols: BTreeSet::new(),
                reads: 0,
                edits: 1,
                first_step: 0,
                last_step: 0,
            });
            entry.files.insert(norm);
        }
    }
    for r in remote_changed {
        if let Some(p) = r["path"].as_str()
            && let Some(norm) = normalize_session_path(p, None, true)
        {
            let sid = r["server_id"].as_str();
            let key = format!("remote:{}:{norm}", sid.unwrap_or("ssh"));
            if !file_to_cluster.contains_key(&key) {
                let (cid, cname, ckind) = classify_file_module(&norm, true, sid);
                file_to_cluster.insert(key, cid.clone());
                let entry = clusters.entry(cid.clone()).or_insert_with(|| Cluster {
                    id: cid,
                    name: cname,
                    kind: ckind,
                    files: BTreeSet::new(),
                    symbols: BTreeSet::new(),
                    reads: 0,
                    edits: 1,
                    first_step: 0,
                    last_step: 0,
                });
                entry.files.insert(norm);
            }
        }
    }

    let mut ranked: Vec<Cluster> = clusters.into_values().collect();
    ranked.sort_by(|a, b| {
        let score_a = a.edits * 5 + a.reads;
        let score_b = b.edits * 5 + b.reads;
        score_b
            .cmp(&score_a)
            .then_with(|| layer_order(a.kind).cmp(&layer_order(b.kind)))
            .then_with(|| a.id.cmp(&b.id))
    });
    ranked.truncate(10);

    let kept_ids: BTreeSet<String> = ranked.iter().map(|c| c.id.clone()).collect();
    let cluster_meta: BTreeMap<String, (String, &'static str)> = ranked
        .iter()
        .map(|c| (c.id.clone(), (c.name.clone(), c.kind)))
        .collect();

    // Observe cross-class co-access & step transitions to propose coarse method chains.
    let mut pair_weights: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut prev_step_clusters: Vec<String> = Vec::new();
    for files in step_files.values() {
        let mut cur_clusters: Vec<String> = Vec::new();
        for f in files {
            if let Some(cid) = file_to_cluster.get(f)
                && kept_ids.contains(cid)
                && !cur_clusters.contains(cid)
            {
                cur_clusters.push(cid.clone());
            }
        }
        // Within-step pairs & consecutive-step pairs, oriented by architectural layer.
        for a in &cur_clusters {
            for b in cur_clusters.iter().chain(&prev_step_clusters) {
                if a == b {
                    continue;
                }
                let ka = layer_order(cluster_meta[a].1);
                let kb = layer_order(cluster_meta[b].1);
                let edge = if ka < kb || (ka == kb && a < b) {
                    (a.clone(), b.clone())
                } else {
                    (b.clone(), a.clone())
                };
                *pair_weights.entry(edge).or_insert(0) += 1;
            }
        }
        if !cur_clusters.is_empty() {
            prev_step_clusters = cur_clusters;
        }
    }

    let mut edges: Vec<((String, String), usize)> = pair_weights.into_iter().collect();
    edges.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    edges.truncate(14);

    let mut candidates = Vec::with_capacity(ranked.len());
    let mut first_method_by_cluster: BTreeMap<String, String> = BTreeMap::new();
    let mut attached_pinned_rules = false;
    for c in &ranked {
        let mut properties = Vec::new();
        for (idx, f) in c.files.iter().take(5).enumerate() {
            properties.push(json!({
                "id": format!("file_{}", idx + 1),
                "name": if idx == 0 { "source".to_owned() } else { format!("source_{}", idx + 1) },
                "kind": if c.kind == "external" { "config" } else { "file" },
                "value": f,
            }));
        }
        for (idx, sym) in c.symbols.iter().take(3).enumerate() {
            properties.push(json!({
                "id": format!("sym_{}", idx + 1),
                "name": sym.split('#').nth(1).unwrap_or("symbol"),
                "kind": "symbol",
                "value": sym,
            }));
        }
        if c.kind == "store" || c.kind == "external" {
            for (idx, ef) in env_files.iter().take(2).enumerate() {
                properties.push(json!({
                    "id": format!("env_{}", idx + 1),
                    "name": "env_file",
                    "kind": "env",
                    "value": ef,
                }));
            }
        }
        // Attach pinned facts once on the primary service/external class instead of duplicating onto every class.
        if !attached_pinned_rules && !pinned_facts.is_empty() && matches!(c.kind, "service" | "external") {
            attached_pinned_rules = true;
            for (idx, fact) in pinned_facts.iter().take(3).enumerate() {
                properties.push(json!({
                    "id": format!("rule_{}", idx + 1),
                    "name": format!("constraint_{}", idx + 1),
                    "kind": "rule",
                    "value": clip_inline(fact, 160),
                }));
            }
        }

        let mut methods = Vec::new();
        for sym_ref in c.symbols.iter().take(3) {
            if let Some(sym) = sym_ref.split('#').nth(1) {
                let mid = to_slug(sym);
                if !mid.is_empty() {
                    methods.push(json!({
                        "id": mid,
                        "name": sym,
                        "symbol": sym,
                        "reads": ["source"],
                    }));
                }
            }
        }
        if methods.is_empty() {
            let default_m = match c.kind {
                "trigger_ui" => ("trigger", "触发交互"),
                "trigger_api" => ("handle", "处理接口请求"),
                "store" => ("persist", "读写存储"),
                "external" => ("invoke", "远端交互"),
                "test_guard" => ("verify", "执行验证"),
                _ => ("execute", "核心处理"),
            };
            methods.push(json!({
                "id": default_m.0,
                "name": default_m.1,
                "reads": ["source"],
            }));
        }
        first_method_by_cluster.insert(
            c.id.clone(),
            methods[0]["id"].as_str().unwrap_or("execute").to_owned(),
        );
        candidates.push(json!({
            "id": c.id,
            "name": c.name,
            "kind": c.kind,
            "file_path": if c.kind == "external" { Value::Null } else { json!(c.files.iter().next()) },
            "properties": properties,
            "methods": methods,
            "activity": {
                "reads": c.reads,
                "edits": c.edits,
                "first_step": c.first_step,
                "last_step": c.last_step,
            }
        }));
    }

    let chains: Vec<Value> = edges
        .into_iter()
        .map(|((from_id, to_id), weight)| {
            let from_name = &cluster_meta[&from_id].0;
            let to_name = &cluster_meta[&to_id].0;
            let from_m = first_method_by_cluster
                .get(&from_id)
                .map(String::as_str)
                .unwrap_or("execute");
            let to_m = first_method_by_cluster
                .get(&to_id)
                .map(String::as_str)
                .unwrap_or("execute");
            json!({
                "source_class": from_id,
                "source_class_name": from_name,
                "source_method": from_m,
                "target_class": to_id,
                "target_class_name": to_name,
                "target_method": to_m,
                "expression": format!("bp.{from_name}.{from_m}().{to_m}()"),
                "co_access_weight": weight,
            })
        })
        .collect();

    (candidates, chains)
}
