//! A bounded table of contents over persisted events, not an AI summary.
use anyhow::{Result, ensure};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn clip(text: &str, limit: usize) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut chars = line.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

/// Directory entries are built exclusively from events that existed at the
/// snapshot: mutable task-node state can describe actions from the future.
pub(super) fn read(conn: &Connection, task: &str, args: &Value, snapshot: i64) -> Result<Value> {
    ensure!(
        args["scope"].as_str().is_none_or(|s| s == "conversation"),
        "history index requires conversation scope"
    );
    ensure!(
        args["event_seq"].is_null()
            && args["before_seq"].is_null()
            && args["query"].is_null()
            && args["programs"] != true,
        "history index uses turn, node_id and after_seq; use records view for searches and exact events"
    );
    let target = args["task_id"].as_str().unwrap_or(task);
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_tasks WHERE id=?1)",
        [target],
        |r| r.get(0),
    )?;
    ensure!(exists, "Conversation not found in this workspace: {target}");
    let node = args["node_id"].as_str();
    let turn = args["turn"].as_i64();
    ensure!(turn.is_none_or(|n| n > 0), "invalid history turn");
    if let Some(id) = node {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_task_events WHERE task_id=?1 AND seq<=?2 AND kind='task/node_planned' AND json_extract(data,'$.node_id')=?3)",
            params![target,snapshot,id], |r| r.get(0))?;
        ensure!(exists, "Task node not found in this history snapshot: {id}");
    }
    // A page holds a whole turn of ordinary length: at 12 entries a page, a
    // turn of 89 calls took 8 reads to list (2026-11-13).
    let project = conn
        .query_row("SELECT workspace FROM agent_session_meta WHERE task_id=?1", [target], |r| r.get::<_, String>(0))
        .map(|path| path.trim_end_matches('/').to_owned())
        .unwrap_or_default();
    let limit = args["limit"].as_u64().unwrap_or(120).clamp(1, 200) as usize;
    let after = args["after_seq"].as_i64().unwrap_or(-1);
    let level = if node.is_some() {
        "node"
    } else if turn.is_some() {
        "turn"
    } else {
        "root"
    };
    let owner = node.unwrap_or("n0");
    // Only compact projections are selected; never load a full tool result,
    // source program, HTML document or model context to build the directory.
    let sql = "WITH RECURSIVE edges AS (
        SELECT task_id,seq,json_extract(data,'$.node_id') AS node_id,
          json_extract(data,'$.parent_id') AS parent_id,data
        FROM agent_task_events WHERE task_id=?1 AND seq<=?2
          AND kind='task/node_planned' AND json_extract(data,'$.kind')='task'
    ), ancestry(task_id,descendant,ancestor) AS (
        SELECT task_id,node_id,node_id FROM edges
        UNION
        SELECT a.task_id,a.descendant,p.parent_id FROM ancestry a
          JOIN edges p ON p.task_id=a.task_id AND p.node_id=a.ancestor
          JOIN edges parent ON parent.task_id=p.task_id AND parent.node_id=p.parent_id
    ), inferred AS (
        SELECT a.task_id, MIN(a.seq) AS seq, 'task/node_inferred' AS kind,
          json_patch(p.data,json_object('turn',json_extract(a.data,'$.turn'))) AS data
        FROM agent_task_events a JOIN ancestry tree ON tree.task_id=a.task_id
          AND tree.descendant=json_extract(a.data,'$.task_node_id')
        JOIN edges p ON p.task_id=tree.task_id AND p.node_id=tree.ancestor AND p.seq<=a.seq
        WHERE a.task_id=?1 AND a.seq<=?2 AND json_extract(a.data,'$.turn') IS NOT NULL
          AND a.kind IN ('program/start','program/error','tool/call','assistant/message','worker/yield','task/node_returned')
          AND (a.kind!='tool/call' OR COALESCE(json_extract(a.data,'$.name'),'')!='read_session_history')
          AND NOT EXISTS (SELECT 1 FROM agent_task_events explicit WHERE explicit.task_id=a.task_id
            AND explicit.seq<=?2 AND explicit.kind IN ('task/node_planned','task/node_resumed')
            AND json_extract(explicit.data,'$.node_id')=tree.ancestor
            AND json_extract(explicit.data,'$.turn')=json_extract(a.data,'$.turn'))
        GROUP BY a.task_id,tree.ancestor,json_extract(a.data,'$.turn')
    ), source AS (
        SELECT task_id,seq,kind,data FROM agent_task_events WHERE task_id=?1 AND seq<=?2
        UNION ALL SELECT task_id,seq,kind,data FROM inferred
    ) SELECT seq,kind,
        COALESCE(json_extract(data,'$.turn'),0),
        COALESCE(json_extract(data,'$.node_id'),''),
        COALESCE(json_extract(data,'$.title'),''),
        COALESCE(json_extract(data,'$.name'),''),
        COALESCE(json_extract(data,'$.function'),''),
        substr(COALESCE(json_extract(data,'$.model_content'),json_extract(data,'$.message.content'),''),1,600),
        substr(COALESCE(json_extract(data,'$.error.message'),''),1,300),
        CASE WHEN kind='tool/call' AND json_valid(json_extract(data,'$.arguments'))
          AND json_type(json_extract(data,'$.arguments'))='object'
          THEN (SELECT json_group_object(key,value) FROM
              (SELECT key,value FROM json_each(json_extract(data,'$.arguments')) WHERE type IN ('integer','real') LIMIT 5))
          ELSE '{}' END,
        CASE WHEN kind='tool/call' THEN (SELECT r.seq FROM agent_task_events r
          WHERE r.task_id=e.task_id AND r.seq>e.seq AND r.seq<=?2
            AND ((r.kind='tool/result' AND json_extract(r.data,'$.message.toolCallId')=json_extract(e.data,'$.callId'))
              OR (r.kind='tool/async_result' AND COALESCE(json_extract(r.data,'$.original_call_id'),json_extract(r.data,'$.meta.result.original_call_id'))=json_extract(e.data,'$.callId')))
            AND NOT EXISTS (SELECT 1 FROM agent_task_events later WHERE later.task_id=e.task_id AND later.kind='tool/call' AND later.seq>e.seq AND later.seq<r.seq AND json_extract(later.data,'$.callId')=json_extract(e.data,'$.callId'))
          ORDER BY r.kind='tool/async_result' DESC,r.seq LIMIT 1) END,
        CASE WHEN kind='tool/call' THEN (SELECT CASE
          WHEN json_extract(r.data,'$.message.isError')=1 OR json_extract(r.data,'$.meta.result.status')='failed'
            OR json_extract(r.data,'$.meta.result.ok')=0 THEN 'error'
          WHEN json_extract(r.data,'$.meta.result.status')='pending' THEN 'pending' ELSE 'returned' END
          FROM agent_task_events r WHERE r.task_id=e.task_id AND r.seq>e.seq AND r.seq<=?2
            AND ((r.kind='tool/result' AND json_extract(r.data,'$.message.toolCallId')=json_extract(e.data,'$.callId'))
              OR (r.kind='tool/async_result' AND COALESCE(json_extract(r.data,'$.original_call_id'),json_extract(r.data,'$.meta.result.original_call_id'))=json_extract(e.data,'$.callId')))
            AND NOT EXISTS (SELECT 1 FROM agent_task_events later WHERE later.task_id=e.task_id AND later.kind='tool/call' AND later.seq>e.seq AND later.seq<r.seq AND json_extract(later.data,'$.callId')=json_extract(e.data,'$.callId'))
          ORDER BY r.kind='tool/async_result' DESC,r.seq LIMIT 1) END,
        CASE WHEN kind='tool/call' AND json_valid(json_extract(data,'$.arguments'))
          AND json_type(json_extract(data,'$.arguments'))='object'
          THEN (SELECT json_group_object(key,substr(value,1,160)) FROM
              (SELECT key,value FROM json_each(json_extract(data,'$.arguments')) WHERE type='text'
                AND key IN ('path','query','cmd','workdir','glob','name','url','output_mode','asset_id') LIMIT 6))
          ELSE '{}' END
      FROM source e WHERE task_id=?1 AND seq<=?2 AND seq>?3 AND (
        (?4='root' AND kind='user/message' AND NOT EXISTS (
            SELECT 1 FROM agent_task_events prior WHERE prior.task_id=e.task_id AND prior.kind='user/message'
              AND prior.seq<e.seq AND json_extract(prior.data,'$.turn')=json_extract(e.data,'$.turn'))) OR
        (?4='turn' AND json_extract(data,'$.turn')=?5 AND (
            kind='user/message' OR
            (kind IN ('task/node_planned','task/node_resumed','task/node_inferred') AND json_extract(data,'$.parent_id')='n0' AND json_extract(data,'$.kind')='task' AND NOT EXISTS (SELECT 1 FROM agent_task_events p WHERE p.task_id=e.task_id AND p.seq<e.seq AND p.kind IN ('task/node_planned','task/node_resumed') AND json_extract(p.data,'$.turn')=json_extract(e.data,'$.turn') AND json_extract(p.data,'$.node_id')=json_extract(e.data,'$.node_id'))) OR
            (kind IN ('tool/call','program/start','program/error','assistant/message') AND COALESCE(json_extract(data,'$.task_node_id'),'n0')='n0'))) OR
        (?4='node' AND (?5 IS NULL OR json_extract(data,'$.turn')=?5) AND (
            (kind IN ('task/node_planned','task/node_resumed','task/node_inferred') AND json_extract(data,'$.parent_id')=?6 AND json_extract(data,'$.kind')='task' AND NOT EXISTS (SELECT 1 FROM agent_task_events p WHERE p.task_id=e.task_id AND p.seq<e.seq AND p.kind IN ('task/node_planned','task/node_resumed') AND json_extract(p.data,'$.node_id')=json_extract(e.data,'$.node_id') AND (?5 IS NULL OR json_extract(p.data,'$.turn')=?5))) OR
            (kind IN ('tool/call','program/start','program/error','assistant/message') AND json_extract(data,'$.task_node_id')=?6)))
      ) AND (kind!='tool/call' OR COALESCE(json_extract(data,'$.name'),'') NOT IN ('read_session_history','write_program'))
        AND (kind!='assistant/message' OR (json_type(data,'$.message.content')='text' AND length(json_extract(data,'$.message.content'))>0))
      ORDER BY seq ASC LIMIT ?7";
    let mut query = conn.prepare(sql)?;
    let rows = query
        .query_map(
            params![
                target,
                snapshot,
                after,
                level,
                turn,
                owner,
                (limit + 1) as i64
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                    r.get::<_, Option<String>>(11)?,
                    r.get::<_, String>(12)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = rows.len() > limit;
    let mut entries = Vec::new();
    for (
        seq,
        kind,
        event_turn,
        node_id,
        title,
        name,
        function,
        text,
        error,
        numeric_args,
        result_seq,
        outcome,
        named_args,
    ) in rows.into_iter().take(limit)
    {
        let (entry_kind, label) = match kind.as_str() {
            "user/message" if level == "root" => ("turn", clip(&text, 100)),
            "user/message" => ("user", clip(&text, 100)),
            "task/node_planned" | "task/node_resumed" | "task/node_inferred" => {
                ("task", clip(&title, 100))
            }
            "tool/call" => ("tool", clip(&name, 100)),
            "program/start" => (
                "program",
                if function.is_empty() {
                    "流程程序".into()
                } else {
                    clip(&function, 100)
                },
            ),
            "program/error" => (
                "error",
                if error.is_empty() {
                    "流程执行出错".into()
                } else {
                    clip(&error, 100)
                },
            ),
            "assistant/message" => ("assistant", clip(&text, 100)),
            _ => continue,
        };
        let mut entry = json!({"kind":entry_kind,"label":label,"seq":seq,"turn":event_turn,
            "read_reference":{"scope":"conversation","task_id":target,"event_seq":seq,"snapshot_seq":snapshot}});
        if kind == "task/node_inferred" {
            entry["inferred_from_activity"] = json!(true);
        }
        if entry_kind == "tool" {
            // A call reads like a line of the work's script: what was called
            // and with which file, search or command. Without these an entry
            // read "read_file_lines 100-175" with no file, and the next turn
            // searched again for what the last one had found (2026-11-13).
            // File contents and edits stay out; so does the reference object
            // every entry used to repeat, now given once for the page.
            let mut arguments = serde_json::from_str::<Value>(&named_args)?;
            // Paths and commands are shown from the chat's project, not with
            // its absolute folder repeated in every entry.
            if let Some(named) = arguments.as_object_mut().filter(|_| !project.is_empty()) {
                for value in named.values_mut() {
                    if let Some(text) = value.as_str() {
                        let short = text.replace(&format!("cd {project} && "), "").replace(&format!("{project}/"), "");
                        *value = json!(short);
                    }
                }
            }
            if let (Some(arguments), Some(numbers)) = (arguments.as_object_mut(), serde_json::from_str::<Value>(&numeric_args)?.as_object()) {
                arguments.extend(numbers.clone());
            }
            let mut call = json!({"kind":"tool","label":label,"seq":seq});
            if arguments.as_object().is_some_and(|object| !object.is_empty()) {
                call["arguments"] = arguments;
            }
            match outcome.as_deref() {
                Some("returned") => {}
                other => call["outcome"] = json!(other.unwrap_or("pending")),
            }
            if let Some(result_seq) = result_seq {
                call["result_seq"] = json!(result_seq);
            }
            entries.push(call);
            continue;
        }
        if entry_kind == "turn" && event_turn > 0 {
            entry["expand"] = json!({"view":"index","scope":"conversation","task_id":target,"turn":event_turn,"snapshot_seq":snapshot});
        } else if entry_kind == "task" && !node_id.is_empty() {
            entry["node_id"] = json!(node_id);
            entry["expand"] = json!({"view":"index","scope":"conversation","task_id":target,"node_id":node_id,"snapshot_seq":snapshot});
            if event_turn > 0 {
                entry["expand_this_turn"] = json!({"view":"index","scope":"conversation","task_id":target,"node_id":node_id,"turn":event_turn,"snapshot_seq":snapshot});
            }
        }
        entries.push(entry);
    }
    let next_page = if has_more {
        let mut next = json!({"view":"index","scope":"conversation","task_id":target,"snapshot_seq":snapshot,"after_seq":entries.last().map(|entry|entry["seq"].clone()),"limit":limit});
        if let Some(id) = node {
            next["node_id"] = json!(id);
        }
        if let Some(number) = turn {
            next["turn"] = json!(number);
        }
        next
    } else {
        Value::Null
    };
    Ok(
        json!({"view":"index","scope":"conversation","task_id":target,"snapshot_seq":snapshot,"level":level,
        "entries":entries,"has_more":has_more,"next_page":next_page,
        "reference":{"scope":"conversation","task_id":target,"snapshot_seq":snapshot},
        "note":"Directory from recorded events, not an AI summary. A tool entry is the call itself: its name and its file, search or command; outcome appears only when it did not return normally. To read a call or its result, use view=records with reference plus event_seq set to the entry's seq or result_seq. Follow expand to navigate and read_reference for other entries. History reads are excluded. Keep snapshot_seq when navigating."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Opt-in smoke test on a real saved conversation; the database is opened
    /// read-only and neither its contents nor its state are modified.
    #[test]
    #[ignore = "set HISTORY_INDEX_DATABASE and HISTORY_INDEX_TASK to inspect a saved conversation"]
    fn real_saved_conversation_is_navigable() {
        let path = std::env::var("HISTORY_INDEX_DATABASE").expect("HISTORY_INDEX_DATABASE");
        let task = std::env::var("HISTORY_INDEX_TASK").expect("HISTORY_INDEX_TASK");
        let conn =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let snapshot: i64 = conn
            .query_row(
                "SELECT MAX(seq) FROM agent_task_events WHERE task_id=?1",
                [&task],
                |r| r.get(0),
            )
            .unwrap();
        let root = read(&conn, &task, &json!({"view":"index"}), snapshot).unwrap();
        let chapter = root["entries"]
            .as_array()
            .unwrap()
            .last()
            .expect("conversation has a user turn");
        let turn = read(&conn, &task, &chapter["expand"], snapshot).unwrap();
        let task_node = turn["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["kind"] == "task")
            .expect("turn has a task node");
        let details = read(&conn, &task, &task_node["expand"], snapshot).unwrap();
        assert!(
            details["entries"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
        );
        assert_eq!(details["snapshot_seq"], snapshot);
        println!(
            "real history index: {}",
            json!({"snapshot":snapshot,"turns":root["entries"].as_array().unwrap().len(),"tasks":turn["entries"].as_array().unwrap().iter().filter(|e|e["kind"]=="task").count(),"node_entries":details["entries"],"has_more":details["has_more"]})
        );
    }
}
