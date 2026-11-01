//! The flow graph's view of a conversation: its function frames, read from the
//! tables the agent already keeps. Nothing here writes, and the agent loop does
//! not know this view exists.
use anyhow::Result;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};

use crate::agent_service::{AgentServiceState, open_db};
use crate::function_frames::{self as frames, Node};
use crate::session_history::{EventQuery, load_events};

/// A frame's own record: what its AI and its program did, and its own transitions.
const STEP_KINDS: &[&str] = &[
    "assistant/delta",
    "assistant/message",
    "assistant/draft",
    "tool/call",
    "tool/result",
    "program/start",
    "program/source_invalid",
    "program/error",
    "program/paused",
    "program/result_invalid",
    "module/loaded",
    "module/fix_started",
    "task/node_planned",
    "task/node_started",
    "task/node_suspended",
    "task/node_resumed",
    "task/node_paused",
    "task/node_returned",
];

/// Argument fields short enough to name what a tool call works on.
const TARGET_FIELDS: &[&str] = &["path", "file_path", "query", "pattern", "command", "script", "program", "name", "url"];

pub async fn list(State(state): State<AgentServiceState>, Path(task_id): Path<String>) -> Response {
    let root = state.workspace.root().to_path_buf();
    match tokio::task::spawn_blocking(move || frame_list(&open_db(&root)?, &task_id)).await {
        Ok(Ok(frames)) => Json(json!({ "frames": frames })).into_response(),
        Ok(Err(error)) => failed(&format!("{error:#}")),
        Err(error) => failed(&error.to_string()),
    }
}

pub async fn get(
    State(state): State<AgentServiceState>,
    Path((task_id, node_id)): Path<(String, String)>,
) -> Response {
    let root = state.workspace.root().to_path_buf();
    match tokio::task::spawn_blocking(move || frame_detail(&open_db(&root)?, &task_id, &node_id)).await {
        Ok(Ok(Some(detail))) => Json(detail).into_response(),
        Ok(Ok(None)) => (StatusCode::NOT_FOUND, Json(json!({ "error": "frame not found" }))).into_response(),
        Ok(Err(error)) => failed(&format!("{error:#}")),
        Err(error) => failed(&error.to_string()),
    }
}

fn failed(error: &str) -> Response {
    tracing::warn!(%error, "could not read function frames");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "could not read the conversation's frames" })),
    )
        .into_response()
}

/// Every frame of a conversation, in the order they were opened.
pub(crate) fn frame_list(conn: &Connection, task: &str) -> Result<Vec<Value>> {
    frames::nodes(conn, task)?
        .iter()
        .map(|node| {
            let mut frame = summary(conn, task, node)?;
            if node.status == "running" {
                frame["activity"] = activity(conn, task, &node.node_id)?;
            }
            Ok(frame)
        })
        .collect()
}

/// One frame with its contract, arguments and result, its own steps, and the
/// program that called it.
pub(crate) fn frame_detail(conn: &Connection, task: &str, id: &str) -> Result<Option<Value>> {
    let Some(node) = stored(conn, task, id)? else {
        return Ok(None);
    };
    let mut frame = summary(conn, task, &node)?;
    let call = &node.inputs["program_task"];
    frame["goal"] = json!(node.goal);
    frame["arguments"] = call["arguments"].clone();
    frame["parameters"] = call["parameters"].clone();
    frame["captured"] = call["captured"].clone();
    frame["result"] = node.result.clone().unwrap_or(Value::Null);
    let query = EventQuery { node_id: Some(id), kinds: STEP_KINDS, ..Default::default() };
    let events = load_events(conn, task, &query)?
        .into_iter()
        .map(|event| Ok(json!({ "seq": event.seq, "time": event.time, "type": event.kind, "data": event.data()? })))
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(json!({
        "frame": frame,
        "events": events,
        "caller_program": caller_program(conn, task, &node)?,
    })))
}

fn stored(conn: &Connection, task: &str, id: &str) -> Result<Option<Node>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT data FROM agent_task_nodes WHERE task_id=?1 AND node_id=?2",
            params![task, id],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|raw| Ok(serde_json::from_str(&raw)?)).transpose()
}

/// What opened the frame: the conversation, a user's request, or a call the program made.
fn kind(node: &Node) -> &str {
    let call = &node.inputs["program_task"];
    if node.node_id == frames::ROOT {
        "conversation"
    } else if node.metadata["previous"] == true {
        "paused_program"
    } else if call["module"] == true {
        "module"
    } else if call["role"] == "body" {
        "function"
    } else if call.is_object() {
        "virtual"
    } else if frames::is_task(node) && node.metadata["role"] == "main" {
        "request"
    } else {
        node.kind.as_str()
    }
}

fn summary(conn: &Connection, task: &str, node: &Node) -> Result<Value> {
    let call = &node.inputs["program_task"];
    let ended = !matches!(node.status.as_str(), "pending" | "running" | "suspended" | "paused");
    Ok(json!({
        "id": node.node_id,
        "parent_id": node.parent_id,
        // A paused program's calls return to it rather than to the frame that made them.
        "caller_id": node.metadata["caller"]["task"].as_str().or(node.parent_id.as_deref()),
        "kind": kind(node),
        "turn": node.metadata["turn"],
        "title": node.title,
        "function": call["name"],
        "description": call["description"],
        "status": node.status,
        "call_site": node.metadata["call_site"],
        "concurrent": node.metadata["parallel"] == true,
        "started_at": time_of(conn, task, node.first_seq)?,
        "ended_at": if ended { time_of(conn, task, node.returned_seq.or(node.last_seq))? } else { None },
        "last_seq": node.last_seq,
        "error": failure(node),
    }))
}

fn time_of(conn: &Connection, task: &str, seq: Option<i64>) -> Result<Option<i64>> {
    let Some(seq) = seq else { return Ok(None) };
    Ok(conn
        .query_row(
            "SELECT timestamp FROM agent_task_events WHERE task_id=?1 AND seq=?2",
            params![task, seq],
            |r| r.get(0),
        )
        .optional()?)
}

/// Why a frame ended without a value: what its function gave up with, or the host's note.
fn failure(node: &Node) -> Option<String> {
    if !matches!(node.status.as_str(), "failed" | "interrupted" | "cancelled") {
        return None;
    }
    let result = node.result.as_ref()?;
    if let Some(text) = result["result"].as_str() {
        let given_up = serde_json::from_str::<Value>(text)
            .ok()
            .map(|value| value["$task_error"].clone())
            .filter(|error| !error.is_null());
        return Some(match given_up {
            Some(error) => error["message"]
                .as_str()
                .or(error.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string()),
            None => text.to_owned(),
        });
    }
    result["message"].as_str().or(result["error"].as_str()).map(str::to_owned)
}

/// What a running frame's AI did last: the reasoning summary of its latest
/// step, still streaming or complete, and a tool it called after it.
fn activity(conn: &Connection, task: &str, node: &str) -> Result<Value> {
    let rows = conn
        .prepare(
            "SELECT kind,timestamp,data FROM agent_task_events WHERE task_id=?1 AND node_id=?2
             AND kind IN ('assistant/delta','assistant/message','assistant/draft','tool/call')
             ORDER BY seq DESC LIMIT 200",
        )?
        .query_map(params![task, node], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let Some(at) = rows.first().map(|row| row.1) else {
        return Ok(Value::Null);
    };
    let mut pieces = Vec::new();
    let mut tool = Value::Null;
    for (kind, _, raw) in &rows {
        let data: Value = serde_json::from_str(raw)?;
        match kind.as_str() {
            "tool/call" if pieces.is_empty() && tool.is_null() => {
                tool = json!({ "name": data["name"], "arguments": targets(&data["arguments"]) });
            }
            "assistant/delta" => pieces.extend(data["reasoning"].as_str().map(str::to_owned)),
            "assistant/message" | "assistant/draft" => {
                if pieces.is_empty() {
                    pieces.extend(data["message"]["reasoning_content"].as_str().map(str::to_owned));
                }
                break;
            }
            _ => {}
        }
    }
    pieces.reverse();
    let reasoning = pieces.concat();
    let start = reasoning.char_indices().rev().nth(1999).map_or(0, |(index, _)| index);
    Ok(json!({ "reasoning": &reasoning[start..], "tool": tool, "at": at }))
}

/// The short fields of a call's arguments, enough to say what it works on.
fn targets(arguments: &Value) -> Value {
    let parsed = arguments
        .as_str()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .unwrap_or_else(|| arguments.clone());
    let fields = TARGET_FIELDS
        .iter()
        .filter_map(|field| {
            let value = parsed[*field].as_str()?;
            (value.chars().count() <= 200).then(|| (field.to_string(), json!(value)))
        })
        .collect::<Map<_, _>>();
    Value::Object(fields)
}

/// The program that made a call, as its caller was running it when the call
/// began, with the call's line.
fn caller_program(conn: &Connection, task: &str, node: &Node) -> Result<Value> {
    let Some(first) = node.first_seq else { return Ok(Value::Null) };
    if !node.metadata["call_site"].is_object() {
        return Ok(Value::Null);
    }
    // A paused program's calls return to its frame, but its source stayed in
    // the frame where it started.
    let mut owner = node.metadata["caller"]["task"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| node.parent_id.clone());
    for _ in 0..frames::MAX_DEPTH {
        let Some(id) = owner else { break };
        let started: Option<(String, String)> = conn
            .query_row(
                "SELECT kind,data FROM agent_task_events WHERE task_id=?1 AND node_id=?2 AND seq<?3
                 AND kind IN ('program/start','module/loaded') ORDER BY seq DESC LIMIT 1",
                params![task, id, first],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((kind, raw)) = started {
            let data: Value = serde_json::from_str(&raw)?;
            let source = if kind == "program/start" {
                data["source"].as_str().map(str::to_owned)
            } else {
                module_source(conn, task, &data)?
            };
            if let Some(source) = source {
                return Ok(json!({ "frame": id, "source": source, "call_site": node.metadata["call_site"] }));
            }
        }
        owner = conn
            .query_row(
                "SELECT parent_id FROM agent_task_nodes WHERE task_id=?1 AND node_id=?2",
                params![task, id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
    }
    Ok(Value::Null)
}

/// A module version's code, as the chat saved it.
fn module_source(conn: &Connection, task: &str, loaded: &Value) -> Result<Option<String>> {
    let kept: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_modules')",
        [],
        |r| r.get(0),
    )?;
    let (Some(contract), Some(version)) = (loaded["contract"].as_str(), loaded["version"].as_i64()) else {
        return Ok(None);
    };
    if !kept {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT source FROM agent_modules WHERE task_id=?1 AND contract=?2 AND version=?3",
            params![task, contract, version],
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_service::append_event_tx;
    use rusqlite::TransactionBehavior;

    struct Root(std::path::PathBuf);
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A conversation whose request ran a program; the program called
    /// `summarize` on line 4, which read a file, while `judge` gave up.
    fn conversation() -> (Root, Connection, String, String, String) {
        let root = Root(std::env::temp_dir().join(format!("frame-view-{}", crate::agent_service::uuid_like())));
        std::fs::create_dir_all(&root.0).unwrap();
        let mut conn = open_db(&root.0).unwrap();
        conn.execute(
            "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','test','fake','running',0,0)",
            [],
        )
        .unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        let mut root_node = frames::ensure_root(&tx, "task", 1).unwrap();
        let mut main = frames::planned(&root_node, &json!({"title":"Summarize notes","goal":"Summarize notes"}), 0, "task").unwrap();
        main.metadata = json!({"role":"main","turn":1});
        root_node.status = "suspended".into();
        frames::emit(&tx, "task", "task/node_suspended", &mut root_node, 1).unwrap();
        main.status = "running".into();
        frames::emit(&tx, "task", "task/node_started", &mut main, 1).unwrap();
        let source = "import * as io from \"host:io\";\nconst notes = await io.read({path:\"notes.md\"});\n\nconst short = await summarize(notes);\n";
        append_event_tx(&tx, "task", "program/start", &json!({"turn":1,"function":null,"source":source,"task_node_id":main.node_id}), None).unwrap();
        let call = |name: &str, line: u64, ordinal: usize| {
            let mut node = frames::planned(&main, &json!({"title":name,"goal":name}), ordinal, "task").unwrap();
            node.inputs = json!({"program_task":{"name":name,"description":format!("{name} the notes."),"arguments":["# notes"],"parameters":["notes"],"role":"leaf"}});
            node.metadata = json!({"role":"temp","turn":1,"parallel":true,"call_site":{"line":line,"column":21},
                "caller":{"task":main.node_id,"call_id":format!("program-{name}"),"concurrent":true}});
            node
        };
        let mut summarize = call("summarize", 4, 0);
        frames::emit(&tx, "task", "task/node_planned", &mut summarize, 1).unwrap();
        summarize.status = "running".into();
        frames::emit(&tx, "task", "task/node_started", &mut summarize, 1).unwrap();
        append_event_tx(&tx, "task", "assistant/message", &json!({"turn":1,"step":1,"task_node_id":summarize.node_id,
            "message":{"role":"assistant","content":null,"reasoning_content":"**Reading the notes**"}}), None).unwrap();
        append_event_tx(&tx, "task", "tool/call", &json!({"turn":1,"step":1,"task_node_id":summarize.node_id,
            "callId":"read-1","name":"read_file","arguments":"{\"path\":\"notes.md\",\"max_chars\":4000}"}), None).unwrap();
        let mut judge = call("judge", 5, 1);
        frames::emit(&tx, "task", "task/node_planned", &mut judge, 1).unwrap();
        judge.status = "failed".into();
        judge.result = Some(json!({"result":"{\"$task_error\":{\"message\":\"The notes are empty.\"}}","assets":[]}));
        frames::emit(&tx, "task", "task/node_returned", &mut judge, 1).unwrap();
        // The model of another frame streams; it must not count as summarize's step.
        append_event_tx(&tx, "task", "assistant/delta", &json!({"turn":1,"step":2,"task_node_id":main.node_id,"reasoning":"**Waiting**"}), None).unwrap();
        tx.commit().unwrap();
        (root, conn, main.node_id, summarize.node_id, judge.node_id)
    }

    #[test]
    fn frames_show_calls_with_their_contract_line_state_and_what_the_ai_is_doing() {
        let (_root, conn, main, summarize, judge) = conversation();
        let frames = frame_list(&conn, "task").unwrap();
        let by_id = |id: &str| frames.iter().find(|frame| frame["id"] == id).unwrap().clone();
        assert_eq!(frames[0]["kind"], "conversation");
        let request = by_id(&main);
        assert_eq!((request["kind"].as_str(), request["status"].as_str(), request["title"].as_str()), (Some("request"), Some("running"), Some("Summarize notes")));
        let call = by_id(&summarize);
        assert_eq!(call["kind"], "virtual");
        assert_eq!(call["caller_id"], main.as_str());
        assert_eq!(call["function"], "summarize");
        assert_eq!(call["description"], "summarize the notes.");
        assert_eq!(call["call_site"]["line"], 4);
        assert_eq!(call["concurrent"], true);
        assert!(call["started_at"].is_i64() && call["ended_at"].is_null());
        assert_eq!(call["activity"]["reasoning"], "**Reading the notes**");
        assert_eq!(call["activity"]["tool"], json!({"name":"read_file","arguments":{"path":"notes.md"}}));
        let gave_up = by_id(&judge);
        assert_eq!(gave_up["status"], "failed");
        assert_eq!(gave_up["error"], "The notes are empty.");
        assert!(gave_up["ended_at"].is_i64());
        assert!(gave_up.get("activity").is_none(), "only running frames report activity");
    }

    #[test]
    fn a_frame_detail_holds_its_own_steps_and_the_calling_program() {
        let (_root, conn, main, summarize, _judge) = conversation();
        let detail = frame_detail(&conn, "task", &summarize).unwrap().unwrap();
        assert_eq!(detail["frame"]["arguments"], json!(["# notes"]));
        let kinds = detail["events"].as_array().unwrap().iter().map(|event| event["type"].as_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(kinds, ["task/node_planned", "task/node_started", "assistant/message", "tool/call"]);
        assert_eq!(detail["caller_program"]["frame"], main.as_str());
        assert_eq!(detail["caller_program"]["call_site"]["line"], 4);
        assert!(detail["caller_program"]["source"].as_str().unwrap().contains("const short = await summarize(notes);"));
        assert!(frame_detail(&conn, "task", "missing").unwrap().is_none());
        let request = frame_detail(&conn, "task", &main).unwrap().unwrap();
        assert!(request["caller_program"].is_null(), "a request is not called by a program");
        assert!(request["events"].as_array().unwrap().iter().any(|event| event["type"] == "program/start"));
    }
}
