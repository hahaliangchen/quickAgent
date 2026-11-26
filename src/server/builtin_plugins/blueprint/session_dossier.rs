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
        "instruction_precedence_note": "When later user_instructions (user/message or user/interjection) conflict with or relax turn-1 task.prompt constraints, ALWAYS treat the latest user_instructions as authoritative.",
        "in_flight_tools": in_flight_tools,
        "recent_tail": recent_tail.into_iter().collect::<Vec<_>>(),
        "changed_files": {
            "local": local_files,
            "remote": remote_files,
        },
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
