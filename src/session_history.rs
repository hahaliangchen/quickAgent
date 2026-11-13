//! Read immutable conversation events and source references; no role-specific
//! context rebuilding or execution protocol replay.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;
mod index;
pub(crate) mod turn_record;

/// Stored content as read back from the database: plain text, or the JSON
/// text of content parts.
pub(crate) fn stored_text(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(parts @ Value::Array(_)) if raw.trim_start().starts_with('[') => message_text(&parts),
        _ => raw.to_owned(),
    }
}

/// The words of a message's content: a string, or the text of its parts.
pub(crate) fn message_text(content: &Value) -> String {
    match content {
        Value::String(text) => stored_text(text),
        Value::Array(parts) => parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}
#[cfg(test)]
mod regression_tests;
mod flow_folder;
mod store;
pub use store::{EventQuery, load_events, tool_call_node};

pub fn saved_history_payload<'a>(kind: &str, data: &'a Value) -> Result<&'a Value> {
    let paths: &[&str] = match kind {
        "user/message"
        | "assistant/message"
        | "organizer/method_message"
        | "organizer/continuation_plan"
        | "organizer/context_note"
        | "visual/input_result" => &["/message"],
        "tool/result" | "tool/async_result" => &["/meta/result"],
        "worker/yield" => &[
            "/output/original_return",
            "/output/worker_return",
            "/output",
        ],
        "observer/node_review" => &[
            "/result/original_message",
            "/result/observer_return",
            "/result",
        ],
        "organizer/decision" => &["/decision"],
        "organizer/assignment" => &["/order"],
        "organizer/result_read" => &["/result"],
        "observer/advice_delivered" => &["/advice"],
        // A completed retrospective has an authoritative raw model return.
        // Unavailable/failed records may only contain status and message;
        // preserve that full record instead of making it unreadable.
        "observer/retrospective" => &["/observer_return", ""],
        _ => &[],
    };
    paths
        .iter()
        .find_map(|path| data.pointer(path).filter(|value| !value.is_null()))
        .or_else(|| {
            if kind == "tool/result" {
                data.pointer("/meta/result")
            } else {
                None
            }
        })
        .or_else(|| if paths.is_empty() { Some(data) } else { None })
        .with_context(|| format!("{kind} event is missing its saved original payload field"))
}

fn source_record(task: &str, seq: i64, time: i64, kind: &str, data: &Value) -> Value {
    let mut record = json!({"seq":seq,"time":time,"kind":kind,"turn":data["turn"],"step":data["step"],
        "identity":data.get("identity").or_else(||data.pointer("/advice/identity")),"actor":data.get("actor").cloned().unwrap_or_else(||json!(source_actor(kind))),
        "node_id":data.get("nodeId").or_else(||data.get("flowNodeId")).or_else(||data.pointer("/identity/node_id")),
        "call_id":data.get("callId").or_else(||data.pointer("/message/toolCallId")).or_else(||data.pointer("/message/source/callId")),
        "work_id":data.get("workId").or_else(||data.get("work_id")).or_else(||data.pointer("/output/id")).or_else(||data.pointer("/order/id")).or_else(||data.pointer("/identity/work_id")),
        "duration_ms":data.pointer("/meta/durationMs"),"stage":data["stage"],"failure_stage":data["failure_stage"],
        "review_id":data.get("review_id").or_else(||data.pointer("/advice/review_id")),
        "is_error":data.pointer("/message/isError").or_else(||data.get("is_error")),"review_status":data.get("status").or_else(||data.pointer("/advice/review_status")),
        "payload":saved_history_payload(kind,data).unwrap_or(data),
        "history_reference":{"scope":"conversation","task_id":task,"event_seq":seq,"include_event":true}});
    if let Some(sources) = data.get("source_event_seqs") {
        record["derived_from_event_seqs"] = sources.clone();
    }
    record
}

fn source_metadata(record: &Value) -> Value {
    let mut source = record.clone();
    source.as_object_mut().unwrap().remove("payload");
    source
}

pub(crate) fn event_source(task: &str, seq: i64, time: i64, kind: &str, data: &Value) -> Value {
    source_metadata(&source_record(task, seq, time, kind, data))
}

fn source_actor(kind: &str) -> &str {
    match kind {
        "assistant/message" | "worker/yield" | "worker/progress" => "worker",
        "user/message" => "user",
        kind if kind.starts_with("organizer/") => "organizer",
        kind if kind.starts_with("observer/") => "observer",
        kind if kind.starts_with("tool/") => "worker",
        _ => "unknown",
    }
}

fn image_message(message: &Value) -> Option<Value> {
    let images = message["content"]
        .as_array()?
        .iter()
        .filter(|block| matches!(block["type"].as_str(), Some("image_ref" | "image_url")))
        .cloned()
        .collect::<Vec<_>>();
    (!images.is_empty()).then(|| json!({"content":images}))
}

pub fn tool() -> Value {
    json!({"type":"function","function":{"name":"read_session_history",
    "description":"Read saved conversation history only when current inputs lack needed information. Set view=index for a compact, expandable table of contents: open a turn, then a task node, whose tool calls are listed with their file, search or command; read a call or its result by its seq with view=records. Reads in the current user turn share a fixed snapshot; keep snapshot_seq while navigating or paging. To inspect work done after that snapshot, explicitly start a new snapshot. History-reading calls and results are excluded even by exact event_seq unless explicitly auditing them. Default scope=conversation reads the current conversation or a returned task_id. scope=project searches workspace conversations. Historical records do not prove current resource availability.",
    "parameters":{"type":"object","properties":{
        "scope":{"type":"string","enum":["conversation","project"]},
        "view":{"type":"string","enum":["records","index"],"description":"index shows an expandable table of contents for a conversation; records reads actual historical entries."},
        "task_id":{"type":"string","description":"Conversation returned by project search; defaults to the current conversation. Only conversations saved in this workspace can be read."},
        "match_offset":{"type":"integer","minimum":0,"description":"Position in the ranked matches of a query search; use the returned next_search."},
        "query":{"type":"string","maxLength":1000,"description":"Words separated by spaces, | or commas; \"quote a phrase\" to match it whole. A record matches if it contains any word; the ones containing the most words come first, then messages and programs before tool calls before tool output, then the newest. Chinese words need spaces between them."},
        "role":{"type":"string","minLength":1,"maxLength":64,"description":"Saved actor name, e.g. agent, worker, tool or visual_tool; default all. Historical actor names remain searchable."},
        "node_id":{"type":"string"},"turn":{"type":"integer","minimum":1},
        "include_descendants":{"type":"boolean","default":false,"description":"Include events owned by descendants of node_id. Split nodes also expose their explicit source interval."},
        "request_id":{"type":"integer","minimum":1,"description":"Goal ID from task history_reference, to restrict the saved event interval."},
        "before_seq":{"type":"integer","minimum":0},"after_seq":{"type":"integer","minimum":-1,"description":"Exclusive lower bound from task history_reference; retain when paging within that human goal."},"event_seq":{"type":"integer","minimum":0},
        "snapshot_seq":{"type":"integer","minimum":0,"description":"Inclusive upper bound returned by the first conversation history read; retain for pagination and exact reads."},
        "snapshot_id":{"type":"integer","minimum":0,"description":"Project search snapshot; retain the value returned in next_search."},
        "new_snapshot":{"type":"boolean","description":"Start an independent history browse including work completed since the previous snapshot. Do not set when paging or expanding its records."},
        "audit_history_reads":{"type":"boolean","description":"Explicit audit only: include history-read calls and results. Ordinary history must leave this false."},
        "limit":{"type":"integer","minimum":1,"maximum":12},
        "char_offset":{"type":"integer","minimum":0},"max_chars":{"type":"integer","minimum":256,"maximum":12000},
        "include_event":{"type":"boolean","description":"Return the original event envelope instead of just the sender's message/return or tool output; useful for inspecting provenance."},
        "programs":{"type":"boolean","description":"Return the programs this conversation ran instead of messages: each turn's main program and the implementations written for its virtual functions, with their source. Filter with turn; read them only when earlier code matters to the current request."},
        "include_context":{"type":"boolean","description":"Include recorded model input views for audit, filterable by the receiving role. Default false. An exact event_seq also resolves a context reference. These views cite the shared history; they are not new actions or independent facts."}
    }}}})
}

/// The programs this conversation ran: each turn's main program and the
/// implementations its AI wrote for virtual functions, in the order they ran.
fn read_programs(
    conn: &rusqlite::Connection,
    task: &str,
    args: &Value,
    snapshot: i64,
) -> Result<Value> {
    let task = args["task_id"].as_str().unwrap_or(task);
    let turn = args["turn"].as_i64();
    let limit = args["limit"].as_u64().unwrap_or(12).clamp(1, 12) as usize;
    let rows = conn
        .prepare("SELECT seq,data FROM agent_task_events WHERE task_id=?1 AND kind='program/start'
            AND json_extract(data,'$.source') IS NOT NULL AND (?2 IS NULL OR json_extract(data,'$.turn')=?2) AND seq<=?3 AND seq>?4 ORDER BY seq LIMIT ?5")?
        .query_map(rusqlite::params![task, turn, snapshot, args["after_seq"].as_i64().unwrap_or(-1), (limit + 1) as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > limit;
    let programs = rows
        .into_iter()
        .take(limit)
        .map(|(seq, raw)| {
            let data: Value = serde_json::from_str(&raw).unwrap_or_default();
            json!({"event_seq":seq,"turn":data["turn"],"function":data["function"],"frame":data["task_node_id"],"source":data["source"]})
        })
        .collect::<Vec<_>>();
    let next_page = if more {
        json!({"programs":true,"scope":"conversation","task_id":task,"turn":turn,"snapshot_seq":snapshot,"after_seq":programs.last().map(|p|p["event_seq"].clone()),"limit":limit})
    } else {
        Value::Null
    };
    Ok(
        json!({"programs":programs,"more":more,"next_page":next_page,"snapshot_seq":snapshot,
        "note":"function is null for a turn's main program, else the virtual function this code implements. A program records how work was done; check current files for their present state."}),
    )
}

pub(crate) fn program_output_text(end: &Value) -> String {
    let mut text = end["console"]
        .as_array()
        .map(|lines| lines.iter().map(|line| line.as_str().map(str::to_owned).unwrap_or_else(|| line.to_string())).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    if let Some(result) = end["detail"].get("result").filter(|value| !value.is_null()) {
        text.push_str(&format!("\n返回：{}", result.as_str().map(str::to_owned).unwrap_or_else(|| result.to_string())));
    }
    text
}

#[cfg(test)]
pub async fn read(root: &Path, task: &str, args: &Value) -> Result<Value> {
    read_for_call(root, task, args, None).await
}

/// Read history as it stood when the model issued `call_id`: that call and
/// anything recorded while it runs stay outside this read and its pages.
pub async fn read_for_call(
    root: &Path,
    task: &str,
    args: &Value,
    call_id: Option<&str>,
) -> Result<Value> {
    let root = root.to_path_buf();
    let task = task.to_owned();
    let args = args.clone();
    let call_id = call_id.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        let conn = crate::agent_service::open_db(&root)?;
        let boundary = match call_id {
            Some(id) => load_events(
                &conn,
                &task,
                &EventQuery {
                    kinds: &["tool/call"],
                    call_id: Some(&id),
                    newest: Some(1),
                    ..Default::default()
                },
            )?
            .first()
            .map(|event| event.seq),
            None => None,
        };
        // Reads share the first cutoff in this user turn. Intervening tool
        // activity must not silently advance it; a new user message or an
        // explicit new_snapshot starts another browsing session.
        let first_read = if let Some(current) = boundary {
            let last_action: i64 = conn.query_row(
                "SELECT COALESCE(MAX(seq),0) FROM agent_task_events WHERE task_id=?1 AND seq<?2 AND kind='user/message'",
                rusqlite::params![task, current], |r| r.get(0))?;
            let mut query = conn.prepare("SELECT seq,COALESCE(json_extract(data,'$.arguments'),'{}') FROM agent_task_events WHERE task_id=?1 AND seq>?2 AND seq<=?3 AND kind='tool/call' AND json_extract(data,'$.name')='read_session_history' ORDER BY seq")?;
            let calls = query.query_map(rusqlite::params![task,last_action,current], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut first = calls.first().map(|call|call.0).unwrap_or(current);
            for (seq, raw) in calls {
                if serde_json::from_str::<Value>(&raw).is_ok_and(|v|v["new_snapshot"]==true) { first=seq; }
            }
            first
        } else { i64::MAX };
        let target = args["task_id"].as_str().unwrap_or(&task);
        let available: i64 = if first_read == i64::MAX {
            conn.query_row("SELECT COALESCE(MAX(seq),0) FROM agent_task_events WHERE task_id=?1", [target], |r| r.get(0))?
        } else if target == task {
            first_read.saturating_sub(1)
        } else {
            conn.query_row("SELECT COALESCE(MAX(seq),0) FROM agent_task_events WHERE task_id=?1 AND rowid < (SELECT rowid FROM agent_task_events WHERE task_id=?2 AND seq=?3)", rusqlite::params![target,task,first_read], |r| r.get(0))?
        };
        let snapshot = args["snapshot_seq"].as_i64().map_or(available, |requested| requested.min(available)).max(0);
        let cutoff: i64 = if first_read == i64::MAX {
            conn.query_row("SELECT COALESCE(MAX(rowid),0) FROM agent_task_events", [], |r| r.get(0))?
        } else {
            conn.query_row("SELECT rowid-1 FROM agent_task_events WHERE task_id=?1 AND seq=?2", rusqlite::params![task,first_read], |r| r.get(0))?
        };
        let cutoff = args["snapshot_id"].as_i64().map_or(cutoff, |t| t.min(cutoff));
        read_saved(&conn, &task, &args, boundary.map(|_| first_read), snapshot, cutoff)
    })
    .await?
}

/// The call that produced a tool result, or the latest result of a call, so a
/// match on either side shows what was called and what came back.
fn paired_event(
    conn: &rusqlite::Connection,
    task: &str,
    kind: &str,
    seq: i64,
    data: &Value,
    boundary: Option<i64>,
) -> Result<Option<(i64, String, Value)>> {
    let (sql, call_id) = match kind {
        "tool/call" => (
            "SELECT seq,kind,data FROM agent_task_events WHERE task_id=?1 AND seq>?2 AND (?4 IS NULL OR seq<?4)
               AND ((kind='tool/result' AND json_extract(data,'$.message.toolCallId')=?3)
                 OR (kind='tool/async_result' AND COALESCE(json_extract(data,'$.original_call_id'),json_extract(data,'$.meta.result.original_call_id'))=?3))
               AND NOT EXISTS (SELECT 1 FROM agent_task_events next_call WHERE next_call.task_id=?1 AND next_call.kind='tool/call' AND json_extract(next_call.data,'$.callId')=?3 AND next_call.seq>?2 AND next_call.seq<agent_task_events.seq)
             ORDER BY kind='tool/async_result' DESC,seq LIMIT 1",
            data["callId"].as_str(),
        ),
        "tool/result" => (
            "SELECT seq,kind,data FROM agent_task_events WHERE task_id=?1 AND seq<?2 AND (?4 IS NULL OR seq<?4)
               AND kind='tool/call' AND json_extract(data,'$.callId')=?3
               AND (SELECT COUNT(*) FROM agent_task_events pending WHERE pending.task_id=?1 AND pending.kind='tool/call'
                    AND json_extract(pending.data,'$.callId')=?3 AND pending.seq<?2
                    AND pending.seq>COALESCE((SELECT MAX(previous.seq) FROM agent_task_events previous WHERE previous.task_id=?1 AND previous.kind='tool/result' AND json_extract(previous.data,'$.message.toolCallId')=?3 AND previous.seq<?2),0))=1
             ORDER BY seq DESC LIMIT 1",
            data.pointer("/message/toolCallId").and_then(Value::as_str),
        ),
        _ => return Ok(None),
    };
    let Some(call_id) = call_id else {
        return Ok(None);
    };
    conn.query_row(sql, rusqlite::params![task, seq, call_id, boundary], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })
    .map(|(seq, kind, raw)| Some((seq, kind, serde_json::from_str(&raw).unwrap_or_default())))
    .or_else(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        error => Err(error.into()),
    })
}

fn read_saved(
    conn: &rusqlite::Connection,
    task: &str,
    args: &Value,
    boundary: Option<i64>,
    snapshot: i64,
    project_cutoff: i64,
) -> Result<Value> {
    if args["view"] == "index" {
        return index::read(conn, task, args, snapshot);
    }
    if args["programs"] == true {
        // A named file opens that file, as in a project.
        if args["directory"].is_string()
            || args["program"].is_string()
            || args["start_line"].is_number() || args["end_line"].is_number()
        {
            let snapshot_tx = conn.unchecked_transaction()?;
            return flow_folder::read(&snapshot_tx, task, args, boundary);
        }
        return read_programs(conn, task, args, snapshot);
    }
    let store::SearchSelection {
        rows,
        project,
        target,
        role,
        query,
        terms,
        turn,
        limit,
        max_chars,
        offset,
        project_offset,
    } = store::search_events(conn, task, args, boundary, snapshot, project_cutoff)?;
    let scope = if project { "project" } else { "conversation" };
    let ranked = !terms.is_empty();
    let more = rows.len() > limit;
    let mut records = Vec::new();
    let mut remaining = max_chars;
    // Allocate the shared excerpt budget across the selected records so an
    // oversized newest result cannot silently hide all the other matches.
    let per_record = (max_chars / rows.len().min(limit).max(1)).max(1);
    for (seq, time, kind, raw, actor, node_id, record_task, prompt, status, matched_events) in
        rows.into_iter().take(limit)
    {
        let reference_snapshot = if project {
            let max: i64 = conn.query_row(
                "SELECT COALESCE(MAX(seq),0) FROM agent_task_events WHERE task_id=?1 AND rowid<=?2",
                rusqlite::params![record_task, project_cutoff],
                |r| r.get(0),
            )?;
            json!(max)
        } else {
            json!(snapshot)
        };
        let mut data: Value = serde_json::from_str(&raw)?;
        saved_history_payload(&kind, &data)?;
        if kind == "debug/context_request" {
            data["request_context"] = if let Some(id) = data["id"].as_i64() {
                crate::request_context::saved_context(conn,&record_task,id)?
                    .unwrap_or_else(||json!({"record_kind":"model_input","unavailable":true,"reason":"This older request body was not retained."}))
            } else {
                json!({"record_kind":"model_input","unavailable":true})
            };
        }
        // A call is its name and arguments; its schema description is not history.
        let call = json!({"name":data["name"],"arguments":data["arguments"]});
        let (payload, pointer) = if kind == "tool/result" && args["include_event"] != true {
            (saved_history_payload(&kind, &data)?, "/meta/result")
        } else if kind == "tool/call" && args["include_event"] != true {
            (&call, "")
        } else {
            let paths: &[&str] = if args["include_event"] == true {
                &[""]
            } else {
                match kind.as_str() {
                    "user/message" | "assistant/message" | "visual/input_result" => {
                        &["/message/content"]
                    }
                    "organizer/context_note" | "organizer/continuation_plan" => {
                        &["/message/content"]
                    }
                    "worker/yield" => &[
                        "/output/original_return",
                        "/output/worker_return",
                        "/output",
                    ],
                    "observer/node_review" => &[
                        "/result/original_message",
                        "/result/observer_return",
                        "/result",
                    ],
                    "tool/result" => &["/meta/result"],
                    "organizer/decision" => &["/decision"],
                    "debug/context_request" => &["/request_context"],
                    _ => &[""],
                }
            };
            let (payload, pointer) = paths
                .iter()
                .find_map(|pointer| {
                    data.pointer(pointer)
                        .filter(|value| !value.is_null())
                        .map(|value| (value, *pointer))
                })
                .unwrap_or((&data, ""));
            (payload, pointer)
        };
        let text = if let Some(text) = payload.as_str() {
            text.to_owned()
        } else {
            payload.to_string()
        };
        // The excerpt opens just before the first query word it contains.
        let lowered = text.to_lowercase();
        let found = terms
            .iter()
            .filter_map(|term| lowered.find(term.as_str()).map(|index| (term, index)))
            .collect::<Vec<_>>();
        let record_offset = if args["char_offset"].is_null() {
            found
                .iter()
                .map(|(_, index)| *index)
                .min()
                .map(|index| lowered[..index].chars().count().saturating_sub(120))
                .unwrap_or(offset)
        } else {
            offset
        };
        let total = text.chars().count();
        let slice = text
            .chars()
            .skip(record_offset)
            .take(per_record.min(remaining))
            .collect::<String>();
        let returned = slice.chars().count();
        remaining = remaining.saturating_sub(returned);
        let next = record_offset.saturating_add(returned);
        let mut record = json!({"task_id":record_task,"seq":seq,"sampled_at":chrono::DateTime::from_timestamp_millis(time).map(|date|date.to_rfc3339()),
            "role":actor,"kind":kind,"node_id":node_id,"turn":data["turn"],"source":source_metadata(&source_record(&record_task,seq,time,&kind,&data)),"payload_path":pointer,
            "content":slice,"content_format":if payload.is_string(){"text"}else{"json"},
            "total_chars":total,"char_offset":record_offset,"returned_chars":returned,"omitted_chars":total.saturating_sub(returned),
            "next_char_offset":if next<total{Some(next)}else{None},
            "read_reference":{"scope":"conversation","task_id":record_task,"event_seq":seq,"snapshot_seq":reference_snapshot,"include_event":args["include_event"]==true,"audit_history_reads":args["audit_history_reads"]==true},
            "next_read":if next<total{json!({"task_id":record_task,"event_seq":seq,"snapshot_seq":reference_snapshot,"char_offset":next,"include_event":args["include_event"]==true,"audit_history_reads":args["audit_history_reads"]==true})}else{Value::Null}});
        if terms.len() > 1 {
            record["matched_terms"] = json!(found.iter().map(|(term, _)| term).collect::<Vec<_>>());
        }
        record["source"]["history_reference"]["snapshot_seq"] = reference_snapshot.clone();
        record["source"]["history_reference"]["audit_history_reads"] =
            json!(args["audit_history_reads"] == true);
        let pair_boundary = reference_snapshot.as_i64().map(|s| s.saturating_add(1));
        if args["include_event"] != true
            && let Some((pair_seq, pair_kind, pair)) =
                paired_event(conn, &record_task, &kind, seq, &data, pair_boundary)?
        {
            let reference = json!({"scope":"conversation","task_id":record_task,"event_seq":pair_seq,"snapshot_seq":reference_snapshot});
            if kind == "tool/call" {
                let result = saved_history_payload(&pair_kind, &pair).unwrap_or(&pair);
                let text = result
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| result.to_string());
                let excerpt = text
                    .chars()
                    .take(per_record.min(remaining))
                    .collect::<String>();
                let returned = excerpt.chars().count();
                remaining = remaining.saturating_sub(returned);
                record["tool_result"] = json!({"seq":pair_seq,"kind":pair_kind,
                    "is_error":pair.pointer("/message/isError"),"content":excerpt,
                    "total_chars":text.chars().count(),"returned_chars":returned,"read_reference":reference});
            } else {
                record["tool_call"] = json!({"seq":pair_seq,"name":pair["name"],
                    "arguments":pair["arguments"],"read_reference":reference});
            }
        }
        if let Some(images) = image_message(&data["message"]) {
            record["image_message"] = images;
            record["source_identity"] = data["identity"].clone();
        }
        if project {
            record["conversation"] = json!({"prompt_excerpt":prompt.unwrap_or_default().chars().take(320).collect::<String>(),
                "status":status,"matched_events":matched_events});
        }
        records.push(record);
    }
    // Ranked matches come best first and page by position; plain browsing
    // stays chronological and pages by before_seq.
    let next_before = if more && !project && !ranked {
        records.last().and_then(|record| record["seq"].as_i64())
    } else {
        None
    };
    if !project && !ranked {
        records.reverse();
    }
    let next_search = if project && more {
        json!({"scope":"project","query":args["query"],"role":role,"node_id":args["node_id"],"turn":turn,
                "match_offset":project_offset+limit as i64,"snapshot_id":project_cutoff,"limit":limit,"max_chars":max_chars,"include_event":args["include_event"]==true,"include_context":args["include_context"]==true})
    } else if ranked && more {
        json!({"scope":"conversation","task_id":target,"query":args["query"],"role":role,"node_id":args["node_id"],"turn":turn,
                "before_seq":args["before_seq"],"after_seq":args["after_seq"],"request_id":args["request_id"],
                "match_offset":project_offset+limit as i64,"snapshot_seq":snapshot,"limit":limit,"max_chars":max_chars,"include_event":args["include_event"]==true,"include_context":args["include_context"]==true})
    } else {
        Value::Null
    };
    let no_match = (records.is_empty() && !query.is_empty()).then(|| {
        // Chinese has no spaces: a long run of it is one word to this search.
        let long_cjk = terms.iter().any(|term| term.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count() > 4);
        format!(
            "No saved {scope} record before this request contains any of {terms:?}.{} History reads are not searched, and history only holds past events: it cannot show current state. To get current data, call the tool that reads it instead of searching history again.",
            if long_cjk { " Separate Chinese words with spaces; a long phrase only matches where it appears exactly." } else { "" }
        )
    });
    Ok(
        json!({"records":records,"order":if ranked {"best_match"} else {"time"},"query_terms":if ranked {json!(terms)} else {Value::Null},"has_more":more,"next_before_seq":next_before,"snapshot_seq":if project {None} else {Some(snapshot)},"next_search":next_search,"no_match":no_match,
        "scope":scope,"task_id":if project{None}else{Some(target)},"snapshot_id":if project {Some(project_cutoff)} else {None},
        "history_note":"With a query, records come best match first; without one, conversation records are chronological. Both are bounded by snapshot_seq; keep it for further pages or exact reads. History-reading calls and results are excluded unless audit_history_reads is explicitly requested. A new user message or explicit new_snapshot starts a new browsing session. Saved history does not prove current resource availability. Project search and pagination keep a fixed event cutoff.",
        "returned_chars":max_chars-remaining,"max_chars":max_chars}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ppt-trial loop: the model searched `get_design` before ever calling
    /// it, and each search returned its own call instead of saying no match.
    #[tokio::test]
    async fn history_search_skips_its_own_reads_and_pairs_tool_results() {
        let root = std::env::temp_dir().join(format!(
            "history-search-{}",
            crate::agent_service::uuid_like()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let conn = crate::agent_service::open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','ppt','test','running',1,1)",[]).unwrap();
        let description = "Read saved conversation history only when current inputs lack needed information; x get_design";
        let history_call = |call: &str, query: &str| {
            json!({"actor":"agent","turn":2,"callId":call,"name":"read_session_history",
                "arguments":json!({"query":query,"limit":1}).to_string(),"description":description})
        };
        let events = [
            (
                1,
                "assistant/message",
                json!({"actor":"agent","turn":2,"message":{"role":"assistant",
                "content":"Running the gather program now.",
                "reasoning_content":"const d = await ppt.get_design({ page: 1 });"}}),
            ),
            (2, "tool/call", history_call("h1", "get_design")),
            (
                3,
                "tool/result",
                json!({"actor":"agent","turn":2,"message":{"role":"tool","toolCallId":"h1",
                "content":[{"type":"text","text":"{\"records\":[{\"content\":\"get_design\"}]}"}]},
                "meta":{"result":{"records":[{"content":"get_design"}]}}}),
            ),
            (
                4,
                "asset/registered",
                json!({"reference":{"source_tool":"read_session_history","summary":"get_design"}}),
            ),
            (5, "tool/call", history_call("h2", "get_design")),
        ];
        let insert = |seq: i64, kind: &str, data: &Value| {
            conn.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES ('t',?1,1,?2,?3)",
                rusqlite::params![seq, kind, data.to_string()]).unwrap();
        };
        for (seq, kind, data) in &events {
            insert(*seq, kind, data);
        }
        let search = |query: &str, call: &str| {
            let args = json!({"query":query,"limit":1});
            let root = root.clone();
            let call = call.to_owned();
            async move { read_for_call(&root, "t", &args, Some(&call)).await.unwrap() }
        };

        // Neither the running search, earlier searches, their outputs nor the
        // model's private reasoning count as a past get_design.
        let none = search("get_design", "h2").await;
        assert_eq!(none["records"], json!([]));
        assert!(
            none["no_match"]
                .as_str()
                .unwrap()
                .contains("call the tool that reads it")
        );
        // A tool's schema description is not searchable text either.
        assert_eq!(search("x", "h2").await["records"], json!([]));
        // History's own reads are invisible even to ordinary exact reads;
        // explicit audit can still inspect the original event.
        let ordinary = read(&root, "t", &json!({"event_seq":2,"include_event":true}))
            .await
            .unwrap();
        assert_eq!(ordinary["records"], json!([]));
        let exact = read(
            &root,
            "t",
            &json!({"event_seq":2,"include_event":true,"audit_history_reads":true}),
        )
        .await
        .unwrap();
        assert_eq!(exact["records"][0]["seq"], 2);
        assert!(
            exact["records"][0]["content"]
                .as_str()
                .unwrap()
                .contains(description)
        );

        // A real call is found by name and returns with its latest result;
        // a later tool/result match points back to the call that produced it.
        insert(
            6,
            "tool/call",
            &json!({"actor":"agent","turn":2,"callId":"p1","name":"ppt_master__get_design",
            "arguments":"{}","description":description}),
        );
        insert(
            7,
            "tool/result",
            &json!({"actor":"agent","turn":2,"message":{"role":"tool","toolCallId":"p1","isError":true,
            "content":[{"type":"text","text":"{\"error\":\"PAGE_CLOSED\"}"}]},"meta":{"result":{"error":"PAGE_CLOSED"}}}),
        );
        let mut fresh_call = history_call("h3", "get_design");
        fresh_call["arguments"] =
            json!(json!({"query":"get_design","limit":1,"new_snapshot":true}).to_string());
        insert(8, "tool/call", &fresh_call);
        let found = search("get_design", "h3").await;
        let record = &found["records"][0];
        assert_eq!(found["records"].as_array().unwrap().len(), 1);
        assert_eq!(record["seq"], 6);
        assert_eq!(
            record["content"],
            json!({"name":"ppt_master__get_design","arguments":"{}"}).to_string()
        );
        assert_eq!(record["tool_result"]["seq"], 7);
        assert_eq!(record["tool_result"]["is_error"], true);
        assert!(
            record["tool_result"]["content"]
                .as_str()
                .unwrap()
                .contains("PAGE_CLOSED")
        );
        assert!(found["no_match"].is_null());
        let by_result = search("page_closed", "h3").await;
        assert_eq!(by_result["records"][0]["seq"], 7);
        assert_eq!(
            by_result["records"][0]["tool_call"]["name"],
            "ppt_master__get_design"
        );

        // A search issued before the real call keeps the boundary it began with.
        assert_eq!(search("get_design", "h2").await["records"], json!([]));
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_history_query_reads_like_a_search_box() {
        use store::query_terms;
        assert_eq!(query_terms("权限 保存"), ["权限", "保存"]);
        assert_eq!(query_terms("Permission|level, Save"), ["permission", "level", "save"]);
        assert_eq!(query_terms("model_round\\( \\d"), ["model_round(", "\\d"]);
        assert_eq!(query_terms("\"chat level\" chat"), ["chat level", "chat"]);
        assert_eq!(query_terms("\"open quote"), ["open", "quote"]);
        assert_eq!(query_terms(" | "), Vec::<String>::new());
    }

    /// 2026-11-13: the whole query was one literal substring and matches came
    /// newest first, so "权限 保存" found nothing and a function name returned
    /// the latest file reads that happened to contain it.
    #[tokio::test]
    async fn history_search_matches_words_and_ranks_what_was_said_first() {
        let root = std::env::temp_dir().join(format!("history-rank-{}", crate::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let conn = crate::agent_service::open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','p','test','running',1,1)",[]).unwrap();
        let message = |role: &str, text: &str| json!({"actor":"agent","turn":1,"message":{"role":role,"content":text}});
        let read_call = |call: &str, path: &str| json!({"actor":"agent","turn":1,"callId":call,"name":"read_file","arguments":json!({"path":path}).to_string()});
        let output = |call: &str, text: &str| json!({"actor":"agent","turn":1,"message":{"role":"tool","toolCallId":call,"content":text},"meta":{"result":{"content":text}}});
        let events = [
            (1, "user/message", message("user", "按聊天保存权限级别")),
            (2, "assistant/message", message("assistant", "The permission level is stored per chat in agent_tasks.")),
            (3, "tool/call", read_call("c1", "src/model_round.rs")),
            (4, "tool/result", output("c1", "fn model_round() { let permission = 1; }")),
            (5, "tool/call", read_call("c2", "src/other.rs")),
            (6, "tool/result", output("c2", "// permission checks call model_round( here")),
        ];
        for (seq, kind, data) in &events {
            conn.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES ('t',?1,1,?2,?3)",
                rusqlite::params![seq, kind, data.to_string()]).unwrap();
        }
        let search = |args: Value| {
            let root = root.clone();
            async move { read(&root, "t", &args).await.unwrap() }
        };
        let seqs = |result: &Value| result["records"].as_array().unwrap().iter().map(|r| r["seq"].as_i64().unwrap()).collect::<Vec<_>>();

        // Words apart in the query still find the message that has both.
        let chinese = search(json!({"query":"权限 保存"})).await;
        assert_eq!(seqs(&chinese), [1]);
        assert_eq!(chinese["records"][0]["matched_terms"], json!(["权限", "保存"]));
        assert!(search(json!({"query":"权限级别保存"})).await["no_match"].as_str().unwrap().contains("spaces"));

        // Most words first; at equal words, what was said beats a newer tool output.
        let ranked = search(json!({"query":"permission|level","limit":3})).await;
        assert_eq!(ranked["order"], "best_match");
        assert_eq!(seqs(&ranked), [2, 6, 4]);
        // The excerpt opens at the first word it contains.
        assert!(ranked["records"][0]["content"].as_str().unwrap().starts_with("The permission"));

        // A regex-style escape is the character it escapes; ranked pages go by position.
        let first = search(json!({"query":"model_round\\(","limit":1})).await;
        assert_eq!(seqs(&first), [6]);
        let next = search(first["next_search"].clone()).await;
        assert_eq!(seqs(&next), [4]);
        assert_eq!(next["next_search"], Value::Null);

        // Browsing without a query stays chronological.
        let browse = search(json!({"limit":3})).await;
        assert_eq!(browse["order"], "time");
        assert_eq!(seqs(&browse), [4, 5, 6]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn browsing_snapshot_excludes_new_events_and_history_results_until_new_work() {
        let root = std::env::temp_dir().join(format!(
            "history-snapshot-{}",
            crate::agent_service::uuid_like()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let conn = crate::agent_service::open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','test','test','running',1,1)", []).unwrap();
        let insert = |seq: i64, kind: &str, data: Value| {
            conn.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES ('t',?1,?1,?2,?3)", rusqlite::params![seq,kind,data.to_string()]).unwrap();
        };
        let history =
            |id: &str| json!({"name":"read_session_history","callId":id,"arguments":"{}"});
        insert(1, "user/message", json!({"message":{"content":"original"}}));
        insert(2, "tool/call", history("h1"));
        insert(
            3,
            "tool/result",
            json!({"message":{"toolCallId":"h1"},"meta":{"result":{"records":["original"]}}}),
        );
        insert(4, "context/usage", json!({"counter":123}));
        insert(5, "tool/call", history("h2"));
        let first = read_for_call(&root, "t", &json!({}), Some("h1"))
            .await
            .unwrap();
        let second = read_for_call(&root, "t", &json!({}), Some("h2"))
            .await
            .unwrap();
        assert_eq!(first["snapshot_seq"], 1);
        assert_eq!(second["snapshot_seq"], 1);
        assert_eq!(
            second["records"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["seq"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![1]
        );
        let own = read_for_call(
            &root,
            "t",
            &json!({"event_seq":3,"include_event":true}),
            Some("h2"),
        )
        .await
        .unwrap();
        assert_eq!(own["records"], json!([]));
        // An intervening tool does not move the boundary. An explicit new
        // browse does; earlier history reads stay excluded unless auditing.
        insert(6, "tool/call", json!({"name":"real_tool","callId":"work"}));
        insert(
            7,
            "tool/result",
            json!({"message":{"toolCallId":"work"},"meta":{"result":{"value":1}}}),
        );
        insert(8, "tool/call", history("h3"));
        let still_old = read_for_call(&root, "t", &json!({}), Some("h3"))
            .await
            .unwrap();
        assert_eq!(still_old["snapshot_seq"], 1);
        insert(
            9,
            "tool/call",
            json!({"name":"read_session_history","callId":"h4","arguments":json!({"new_snapshot":true}).to_string()}),
        );
        let fresh = read_for_call(&root, "t", &json!({"new_snapshot":true}), Some("h4"))
            .await
            .unwrap();
        assert_eq!(fresh["snapshot_seq"], 8);
        assert!(
            fresh["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["seq"] == 7)
        );
        let blocked = read_for_call(&root, "t", &json!({"event_seq":3}), Some("h4"))
            .await
            .unwrap();
        assert_eq!(blocked["records"], json!([]));
        let audit = read_for_call(
            &root,
            "t",
            &json!({"event_seq":3,"audit_history_reads":true}),
            Some("h4"),
        )
        .await
        .unwrap();
        assert_eq!(audit["records"][0]["seq"], 3);
        // An explicit old snapshot cannot be widened after the new work.
        let old = read_for_call(&root, "t", &json!({"snapshot_seq":1}), Some("h4"))
            .await
            .unwrap();
        assert!(
            old["records"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["seq"].as_i64().unwrap() <= 1)
        );
        insert(
            10,
            "user/message",
            json!({"message":{"content":"new turn"}}),
        );
        insert(11, "tool/call", history("h5"));
        let next_turn = read_for_call(&root, "t", &json!({}), Some("h5"))
            .await
            .unwrap();
        assert_eq!(next_turn["snapshot_seq"], 10);
        assert!(
            next_turn["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["seq"] == 10)
        );
        insert(
            12,
            "assistant/message",
            json!({"message":{"role":"assistant","content":null,"tool_calls":[{"function":{"name":"read_session_history","arguments":"{}"}}]}}),
        );
        let ordinary = read(&root, "t", &json!({"event_seq":12})).await.unwrap();
        assert_eq!(ordinary["records"], json!([]));
        let audited = read(
            &root,
            "t",
            &json!({"event_seq":12,"audit_history_reads":true}),
        )
        .await
        .unwrap();
        assert_eq!(audited["records"][0]["seq"], 12);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn history_pages_and_exact_references_keep_the_original_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "history-pages-{}",
            crate::agent_service::uuid_like()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let conn = crate::agent_service::open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','test','test','running',1,1)",[]).unwrap();
        let insert = |seq: i64, kind: &str, data: Value| {
            conn.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES ('t',?1,?1,?2,?3)",rusqlite::params![seq,kind,data.to_string()]).unwrap();
        };
        for i in 1..=20 {
            insert(
                i,
                "user/message",
                json!({"message":{"content":format!("message {i}")}}),
            );
        }
        insert(
            21,
            "tool/call",
            json!({"name":"read_session_history","callId":"h1"}),
        );
        let first = read_for_call(&root, "t", &json!({"limit":10}), Some("h1"))
            .await
            .unwrap();
        assert_eq!(first["snapshot_seq"], 20);
        assert_eq!(first["next_before_seq"], 11);
        let reference = first["records"][0]["read_reference"].clone();
        assert_eq!(reference["snapshot_seq"], 20);
        insert(
            22,
            "tool/result",
            json!({"message":{"toolCallId":"h1"},"meta":{"result":first}}),
        );
        insert(
            23,
            "tool/call",
            json!({"name":"read_session_history","callId":"h2"}),
        );
        let next = read_for_call(&root, "t", &json!({"limit":10,"before_seq":11}), Some("h2"))
            .await
            .unwrap();
        assert_eq!(next["snapshot_seq"], 20);
        assert_eq!(
            next["records"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["seq"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            (1..=10).collect::<Vec<_>>()
        );
        assert_eq!(next["has_more"], false);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn history_index_navigates_recorded_tasks_with_snapshot_and_evidence() {
        let root = std::env::temp_dir().join(format!(
            "history-index-{}",
            crate::agent_service::uuid_like()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let conn = crate::agent_service::open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','test','test','running',1,1)",[]).unwrap();
        let insert = |seq: i64, kind: &str, data: Value| {
            conn.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES ('t',?1,?1,?2,?3)",rusqlite::params![seq,kind,data.to_string()]).unwrap();
        };
        insert(
            1,
            "user/message",
            json!({"turn":1,"model_content":"请分析材料\n附加信息","message":{"content":"请分析材料"}}),
        );
        insert(
            2,
            "task/node_planned",
            json!({"turn":1,"kind":"task","node_id":"main","parent_id":"n0","title":"分析材料"}),
        );
        insert(
            3,
            "program/start",
            json!({"turn":1,"task_node_id":"main","source":"PRIVATE LONG SOURCE"}),
        );
        insert(
            4,
            "tool/call",
            json!({"turn":1,"task_node_id":"main","name":"read_file","callId":"c1","arguments":"{\"path\":\"private.txt\",\"page\":4,\"content\":\"PRIVATE LONG SOURCE\"}"}),
        );
        insert(
            5,
            "tool/result",
            json!({"turn":1,"message":{"toolCallId":"c1","isError":true},"meta":{"result":{"text":"original evidence"}}}),
        );
        insert(
            6,
            "task/node_planned",
            json!({"turn":1,"kind":"task","node_id":"child","parent_id":"main","title":"检查"}),
        );
        insert(
            7,
            "program/error",
            json!({"turn":1,"task_node_id":"child","error":{"message":"检查失败"}}),
        );
        insert(
            8,
            "tool/call",
            json!({"turn":1,"task_node_id":"main","name":"read_session_history","callId":"h1","arguments":"{}"}),
        );
        insert(
            9,
            "tool/result",
            json!({"turn":1,"message":{"toolCallId":"h1"},"meta":{"result":{"records":["self"]}}}),
        );
        insert(
            10,
            "user/message",
            json!({"turn":2,"model_content":"后来的请求","message":{"content":"后来的请求"}}),
        );
        let contents = read(&root, "t", &json!({"view":"index","snapshot_seq":9}))
            .await
            .unwrap();
        assert_eq!(contents["entries"].as_array().unwrap().len(), 1);
        assert_eq!(contents["entries"][0]["kind"], "turn");
        assert_eq!(contents["entries"][0]["label"], "请分析材料");
        let chapter = read(&root, "t", &contents["entries"][0]["expand"])
            .await
            .unwrap();
        assert_eq!(chapter["snapshot_seq"], 9);
        let task = chapter["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "task")
            .unwrap();
        assert_eq!(task["label"], "分析材料");
        let mut open = task["expand"].clone();
        open["limit"] = json!(2);
        let page = read(&root, "t", &open).await.unwrap();
        assert_eq!(page["entries"][0]["kind"], "program");
        assert_eq!(page["entries"][1]["label"], "read_file");
        // The entry is the call: which file it read and where, without the
        // file's text, and where its result is.
        assert_eq!(page["entries"][1], json!({"kind":"tool","label":"read_file","seq":4,
            "arguments":{"path":"private.txt","page":4},"outcome":"error","result_seq":5}));
        assert!(page["has_more"] == true && !page.to_string().contains("PRIVATE LONG SOURCE"));
        let mut call = page["reference"].clone();
        call["event_seq"] = page["entries"][1]["seq"].clone();
        let evidence = read(&root, "t", &call).await.unwrap();
        assert_eq!(evidence["records"][0]["seq"], 4);
        assert!(
            evidence["records"][0]["tool_result"]["content"]
                .as_str()
                .unwrap()
                .contains("original evidence")
        );
        let next = read(&root, "t", &page["next_page"]).await.unwrap();
        assert_eq!(next["entries"].as_array().unwrap().len(), 1);
        assert_eq!(next["entries"][0]["label"], "检查");
        let child = read(&root, "t", &next["entries"][0]["expand"])
            .await
            .unwrap();
        assert_eq!(child["entries"][0]["kind"], "error");
        assert_eq!(child["entries"][0]["label"], "检查失败");
        let pending = read(
            &root,
            "t",
            &json!({"view":"index","node_id":"main","snapshot_seq":4}),
        )
        .await
        .unwrap();
        assert_eq!(pending["entries"][1]["outcome"], "pending");
        assert!(pending["entries"][1].get("result_reference").is_none());
        assert!(
            read(
                &root,
                "t",
                &json!({"view":"index","node_id":"missing","snapshot_seq":9})
            )
            .await
            .is_err()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn history_index_groups_multiple_user_messages_by_turn() {
        let root = std::env::temp_dir().join(format!(
            "history-index-turns-{}",
            crate::agent_service::uuid_like()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let conn = crate::agent_service::open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','test','test','running',1,1)",[]).unwrap();
        for (seq, turn, text) in [(1, 1, "最初需求"), (2, 1, "补充要求"), (3, 2, "新的问题")]
        {
            conn.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES ('t',?1,?1,'user/message',?2)", rusqlite::params![seq,json!({"turn":turn,"model_content":text,"message":{"content":text}}).to_string()]).unwrap();
        }
        let index = read(&root, "t", &json!({"view":"index","limit":1}))
            .await
            .unwrap();
        assert_eq!(index["entries"][0]["label"], "最初需求");
        let chapter = read(&root, "t", &index["entries"][0]["expand"])
            .await
            .unwrap();
        assert_eq!(chapter["entries"].as_array().unwrap().len(), 2);
        assert_eq!(chapter["entries"][1]["label"], "补充要求");
        let next = read(&root, "t", &index["next_page"]).await.unwrap();
        assert_eq!(next["entries"][0]["label"], "新的问题");
        assert_eq!(next["has_more"], false);
        let _ = std::fs::remove_dir_all(root);
    }
}
