//! A turn's direct work as code. The main AI's direct calls are library
//! methods, so the host writes them, in order, the way a program calls them:
//! `<name>.calls.js`, one line per call with a comment saying what came back,
//! its words between calls as comments, and a line for each program it ran.
//! With the main program files beside it, named like a project's code files,
//! the chat folder reads like a code project (docs/agent-design-guide.md §4).
//! The file is a record, never run; it is rendered from the conversation
//! records, so it follows rewinds.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

const ROOT: &str = crate::function_frames::ROOT;
/// A program arrives as its file, not as a call.
const WRITE_PROGRAM: &str = "write_program";
/// The host's record of a program calling one of its functions; the
/// program file has that call.
const PROGRAM_CALL: &str = "create_task";

pub(crate) const CALLS: &str = ".calls.js";

/// A file name stem, as a project names its code files: the ASCII words of
/// `words` (camelCase split, snake_case and other separators read as gaps),
/// lowercased, joined by dashes, each word once, at most `limit` characters
/// cut at a word boundary. Code files have no Chinese names; words in other
/// scripts are left out, and nothing left is no name.
pub(crate) fn stem(words: &str, limit: usize) -> Option<String> {
    let words = words.trim();
    let words = words.strip_suffix(".js").unwrap_or(words);
    let mut found = Vec::<String>::new();
    let mut word = String::new();
    let mut previous: Option<char> = None;
    let mut push = |word: &mut String, found: &mut Vec<String>| {
        if !word.is_empty() && !found.contains(word) {
            found.push(std::mem::take(word));
        }
        word.clear();
    };
    for c in words.chars() {
        if c.is_ascii_alphanumeric() {
            // A capital after a small letter or digit starts a word: permissionMode.
            if c.is_ascii_uppercase() && previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit()) {
                push(&mut word, &mut found);
            }
            word.push(c.to_ascii_lowercase());
        } else {
            push(&mut word, &mut found);
        }
        previous = Some(c);
    }
    push(&mut word, &mut found);
    let mut out = String::new();
    for word in found {
        let next = if out.is_empty() { word.len() } else { out.len() + 1 + word.len() };
        if next > limit {
            if out.is_empty() {
                out = word[..limit].to_owned();
            }
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&word);
    }
    (!out.is_empty()).then_some(out)
}

/// A name from the English words of what the user asked, if it has any.
pub(crate) fn request_stem(request: &str) -> Option<String> {
    stem(first_line(request), 32)
}

/// A name from the files a turn worked on, as their file names read.
fn files_stem(paths: &[String]) -> Option<String> {
    let names = paths
        .iter()
        .filter_map(|path| std::path::Path::new(path).file_stem().map(|name| name.to_string_lossy().into_owned()))
        .collect::<Vec<_>>();
    stem(&names.join(" "), 32)
}

/// The files a turn's direct calls changed, and those it read.
fn touched(conn: &Connection, task: &str, turn: i64, before: Option<i64>) -> Result<(Vec<String>, Vec<String>)> {
    let calls = conn
        .prepare(&format!(
            "SELECT json_extract(e.data,'$.name'),json_extract(e.data,'$.arguments') FROM agent_task_events e
             JOIN agent_task_nodes n ON n.task_id=e.task_id AND n.parent_id='{ROOT}'
               AND n.node_id=COALESCE(e.node_id,json_extract(e.data,'$.task_node_id'))
             WHERE e.task_id=?1 AND e.kind='tool/call' AND json_extract(e.data,'$.turn')=?2
               AND json_extract(e.data,'$.program_location') IS NULL AND (?3 IS NULL OR e.seq<?3) ORDER BY e.seq"
        ))?
        .query_map(params![task, turn, before], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (mut changed, mut read) = (Vec::new(), Vec::new());
    for (name, arguments) in calls {
        let Some(path) = arguments
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|args| args["path"].as_str().map(str::to_owned))
            .filter(|path| !matches!(path.as_str(), "" | "."))
        else {
            continue;
        };
        let list = match name.as_deref() {
            Some("write_file" | "edit_file" | "replace_range") => &mut changed,
            Some("read_file" | "read_file_lines") => &mut read,
            _ => continue,
        };
        if !list.contains(&path) {
            list.push(path);
        }
    }
    Ok((changed, read))
}

/// A call record in the chat folder: the turn it records, its file name,
/// its first call and how many calls.
pub(crate) struct Record {
    pub turn: i64,
    pub file: String,
    pub first_seq: i64,
    pub calls: i64,
}

/// The call records of a chat, in turn order. A turn that ran a program
/// shares its first program's name; one without is named after the files it
/// changed, else those it read, else the English words of its request, with
/// -2, -3 when an earlier record has the name.
pub(crate) fn records(conn: &Connection, task: &str, before: Option<i64>) -> Result<Vec<Record>> {
    let mut taken = std::collections::HashSet::new();
    let mut records = Vec::new();
    for (turn, first_seq, calls) in turns(conn, task, before)? {
        let program: Option<String> = conn
            .query_row(
                "SELECT json_extract(data,'$.path') FROM agent_task_events WHERE task_id=?1 AND kind='program/start'
                   AND json_extract(data,'$.function') IS NULL AND json_type(data,'$.path')='text'
                   AND json_extract(data,'$.turn')=?2 AND (?3 IS NULL OR seq<?3) ORDER BY seq LIMIT 1",
                params![task, turn, before],
                |r| r.get(0),
            )
            .optional()?;
        let base = match program {
            Some(path) => {
                let file = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_owned();
                file.strip_suffix(".js").unwrap_or(&file).to_owned()
            }
            None => {
                let (changed, read) = touched(conn, task, turn, before)?;
                let request: Option<String> = conn
                    .query_row(
                        "SELECT json_extract(data,'$.message.content') FROM agent_task_events
                         WHERE task_id=?1 AND kind='user/message' AND json_extract(data,'$.turn')=?2 ORDER BY seq LIMIT 1",
                        params![task, turn],
                        |r| r.get(0),
                    )
                    .optional()?
                    .flatten();
                files_stem(&changed)
                    .or_else(|| files_stem(&read))
                    .or_else(|| request_stem(&request.map(|raw| super::stored_text(&raw)).unwrap_or_default()))
                    .unwrap_or_else(|| "chat".into())
            }
        };
        let mut file = format!("{base}{CALLS}");
        let mut attempt = 1;
        while !taken.insert(file.clone()) {
            attempt += 1;
            file = format!("{base}-{attempt}{CALLS}");
        }
        records.push(Record { turn, file, first_seq, calls });
    }
    Ok(records)
}

/// Turns with direct calls by the main AI: (turn, first call seq, calls).
fn turns(conn: &Connection, task: &str, before: Option<i64>) -> Result<Vec<(i64, i64, i64)>> {
    let mut statement = conn.prepare(&format!(
        "SELECT json_extract(e.data,'$.turn') AS turn, MIN(e.seq), COUNT(*) FROM agent_task_events e
         JOIN agent_task_nodes n ON n.task_id=e.task_id AND n.parent_id='{ROOT}'
           AND n.node_id=COALESCE(e.node_id,json_extract(e.data,'$.task_node_id'))
         WHERE e.task_id=?1 AND e.kind='tool/call' AND json_extract(e.data,'$.program_location') IS NULL
           AND COALESCE(json_extract(e.data,'$.name'),'') NOT IN ('{WRITE_PROGRAM}','{PROGRAM_CALL}')
           AND json_type(e.data,'$.turn')='integer' AND (?2 IS NULL OR e.seq<?2)
         GROUP BY turn ORDER BY turn"
    ))?;
    let rows = statement
        .query_map(params![task, before], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    format!("{}…", text.chars().take(limit).collect::<String>())
}

fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("")
}

/// Words as `//` lines.
fn comment(text: &str, limit: usize) -> String {
    clip(text.trim(), limit)
        .lines()
        .map(|line| format!("// {}", line.trim_end()).trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fields that carry a body: written files, edits, sources.
const BODIES: [&str; 12] = [
    "content", "old_string", "new_string", "old_text", "new_text", "text", "source", "replacement", "data", "body", "patch", "diff",
];

/// A JSON value as a JS literal; a body becomes a length comment.
fn literal(key: Option<&str>, value: &Value) -> String {
    match value {
        Value::String(text) => {
            let chars = text.chars().count();
            if key.is_some_and(|key| BODIES.contains(&key)) && chars > 60 {
                format!("/* {chars} 字 */")
            } else if chars > 300 {
                // A long command or query still says what it was: its opening.
                let head = text.chars().take(240).collect::<String>();
                let quoted = serde_json::to_string(&head).unwrap_or_default();
                format!("{}…\" /* 共 {chars} 字 */", &quoted[..quoted.len() - 1])
            } else {
                serde_json::to_string(text).unwrap_or_default()
            }
        }
        Value::Array(items) => {
            let inner = items.iter().map(|item| literal(None, item)).collect::<Vec<_>>().join(", ");
            if inner.chars().count() > 300 {
                format!("/* {} 项 */", items.len())
            } else {
                format!("[{inner}]")
            }
        }
        Value::Object(map) => {
            // What a call is about (path, command, query) reads first.
            let mut entries = map.iter().collect::<Vec<_>>();
            const FIRST: [&str; 5] = ["path", "cmd", "query", "start_line", "end_line"];
            entries.sort_by_key(|(name, value)| {
                (FIRST.iter().position(|first| first == name).unwrap_or(FIRST.len()), value.is_array() || value.is_object())
            });
            let fields = entries
                .into_iter()
                .map(|(name, value)| {
                    let name = if crate::agent_program::libraries::identifier(name) {
                        name.clone()
                    } else {
                        serde_json::to_string(name).unwrap_or_default()
                    };
                    format!("{name}: {}", literal(Some(name.as_str()), value))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{fields}}}")
        }
        other => other.to_string(),
    }
}

/// The call as a program writes it: its library method where it has one.
fn call_line(name: &str, arguments: Option<&str>) -> String {
    let method = crate::agent_program::libraries::public_name(&crate::agent_program::libraries::builtin(), name)
        .unwrap_or_else(|| name.to_owned());
    let arguments = match arguments.map(|raw| serde_json::from_str::<Value>(raw)) {
        None => String::new(),
        Some(Ok(Value::Object(map))) if map.is_empty() => String::new(),
        Some(Ok(value)) => literal(None, &value),
        Some(Err(_)) => format!("/* 参数不是完整的 JSON，{} 字 */", arguments.unwrap_or("").chars().count()),
    };
    format!("{method}({arguments});")
}

fn error_text(result: &Value) -> Option<String> {
    let error = &result["error"];
    let message = result["message"].as_str().filter(|_| result["error_code"].is_string() || !error.is_null());
    if error.is_null() && result["error_code"].is_null() && result["ok"] != false {
        return None;
    }
    let text = message
        .map(str::to_owned)
        .or_else(|| error.as_str().map(str::to_owned))
        .or_else(|| error["message"].as_str().map(str::to_owned))
        .or_else(|| result["error_code"].as_str().map(str::to_owned))
        .unwrap_or_else(|| error.to_string());
    Some(text)
}

/// What came back, in a few words.
fn outcome(name: &str, result: &Value, failed: bool) -> String {
    let result = match result {
        Value::String(text) => serde_json::from_str::<Value>(text).unwrap_or_else(|_| result.clone()),
        other => other.clone(),
    };
    if let Some(error) = error_text(&result).or_else(|| failed.then(|| result.as_str().map(str::to_owned).unwrap_or_else(|| result.to_string()))) {
        return format!("出错：{}", clip(first_line(&error), 120));
    }
    if !result["external_change"].is_null() {
        return "文件已被别人改过，没有写入".into();
    }
    let count = |key: &str| result[key].as_array().map(Vec::len);
    match name {
        "read_file" | "read_file_lines" => match result["content"].as_str() {
            Some(text) => {
                let unread = if result["complete"] == false { "，没读完" } else { "" };
                format!("{} 行{unread}", text.lines().count())
            }
            None => "完成".into(),
        },
        "search_text" => {
            if let Some(total) = result["total"].as_u64() {
                format!("共 {total} 处")
            } else if let Some(files) = count("files").filter(|_| result["matches"].as_array().is_none_or(Vec::is_empty)) {
                if files == 0 { "没有匹配".into() } else { format!("{files} 个文件") }
            } else {
                match count("matches") {
                    Some(0) | None => "没有匹配".into(),
                    Some(matches) => format!("{matches} 处"),
                }
            }
        }
        "list_dir" => count("entries").map_or_else(|| "完成".into(), |entries| format!("{entries} 项")),
        "write_file" | "edit_file" | "replace_range" => "已写入".into(),
        _ if result["exit_code"].is_number() => {
            let code = &result["exit_code"];
            let stream = if code == 0 { "stdout" } else { "stderr" };
            let last = result[stream]
                .as_str()
                .filter(|text| !text.trim().is_empty())
                .or_else(|| result["stdout"].as_str())
                .and_then(|text| text.lines().rev().map(str::trim).find(|line| !line.is_empty()))
                .map(|line| format!("：{}", clip(line, 100)))
                .unwrap_or_default();
            format!("退出 {code}{last}")
        }
        _ => match &result {
            Value::String(text) if !text.trim().is_empty() => clip(first_line(text), 80),
            Value::Object(map) if map.get("status").is_some_and(Value::is_string) => map["status"].as_str().unwrap_or("").to_owned(),
            _ => "完成".into(),
        },
    }
}

/// Why a handed-over program did not run: its first diagnostic and where.
fn rejected(result: &Value) -> String {
    let result = match result {
        Value::String(text) => serde_json::from_str::<Value>(text).unwrap_or_else(|_| result.clone()),
        other => other.clone(),
    };
    let first = &result["diagnostics"][0];
    let message = first["message"]
        .as_str()
        .or_else(|| result["error"].as_str())
        .or_else(|| result["message"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| result.to_string());
    let code = result["code"].as_str().map(|code| format!("{code} ")).unwrap_or_default();
    match first["line"].as_u64() {
        Some(line) => format!("{code}第 {line} 行：{}", clip(first_line(&message), 160)),
        None => format!("{code}{}", clip(first_line(&message), 160)),
    }
}

/// The program file a main program ran from, and how it ended.
fn program_line(conn: &Connection, task: &str, seq: i64, data: &Value, before: Option<i64>) -> Result<Option<String>> {
    let Some(path) = data["path"].as_str() else {
        return Ok(None);
    };
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let end: Option<String> = conn
        .query_row(
            "SELECT data FROM agent_task_events WHERE task_id=?1 AND kind='program/end'
               AND json_extract(data,'$.program')=?2 AND seq>?3 AND (?4 IS NULL OR seq<?4) ORDER BY seq LIMIT 1",
            params![task, file, seq, before],
            |r| r.get(0),
        )
        .optional()?;
    let ended = match end.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()) {
        Some(end) => {
            let status = end["ended"].as_str().unwrap_or("ended").to_owned();
            let error = end["detail"]["error"].as_str().or_else(|| end["error"].as_str()).or_else(|| end["detail"]["message"].as_str());
            match error {
                Some(error) if status != "completed" => format!("{status}：{}", clip(first_line(error), 120)),
                _ => status,
            }
        }
        None => "没有结束（停止或暂停）".into(),
    };
    Ok(Some(format!("// 运行程序 {file} → {ended}")))
}

/// `turn-<n>-calls.js` for one turn, or nothing when its main AI called no
/// method directly. Only records before `before` count.
pub(crate) fn render(conn: &Connection, task: &str, turn: i64, before: Option<i64>) -> Result<Option<String>> {
    let Some((_, first, calls)) = turns(conn, task, before)?.into_iter().find(|(number, _, _)| *number == turn) else {
        return Ok(None);
    };
    // The turn runs from its first event to the next turn's first event;
    // results carry no turn of their own.
    let end: Option<i64> = conn.query_row(
        "SELECT MIN(seq) FROM agent_task_events WHERE task_id=?1 AND json_extract(data,'$.turn')>?2",
        params![task, turn],
        |r| r.get(0),
    )?;
    let end = match (end, before) {
        (Some(end), Some(before)) => Some(end.min(before)),
        (end, before) => end.or(before),
    };
    let start: i64 = conn.query_row(
        "SELECT COALESCE(MIN(seq),?3) FROM agent_task_events WHERE task_id=?1 AND json_extract(data,'$.turn')=?2",
        params![task, turn, first],
        |r| r.get(0),
    )?;
    let rows = conn
        .prepare(&format!(
            "SELECT e.seq,e.kind,e.data FROM agent_task_events e
             JOIN agent_task_nodes n ON n.task_id=e.task_id AND n.parent_id='{ROOT}'
               AND n.node_id=COALESCE(e.node_id,json_extract(e.data,'$.task_node_id'))
             WHERE e.task_id=?1 AND e.seq>=?2 AND (?3 IS NULL OR e.seq<?3)
               AND e.kind IN ('user/message','assistant/message','tool/call','tool/result','program/start')
             ORDER BY e.seq"
        ))?
        .query_map(params![task, start, end], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let request: Option<String> = conn
        .query_row(
            "SELECT json_extract(data,'$.message.content') FROM agent_task_events
             WHERE task_id=?1 AND kind='user/message' AND json_extract(data,'$.turn')=?2 ORDER BY seq LIMIT 1",
            params![task, turn],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let mut results = HashMap::<String, (bool, Value)>::new();
    let mut events = Vec::new();
    for (seq, kind, raw) in rows {
        let Ok(data) = serde_json::from_str::<Value>(&raw) else { continue };
        if kind == "tool/result" {
            if let Some(call) = data.pointer("/message/toolCallId").and_then(Value::as_str) {
                let payload = super::saved_history_payload(&kind, &data).cloned().unwrap_or(Value::Null);
                results.insert(call.to_owned(), (data.pointer("/message/isError") == Some(&Value::Bool(true)), payload));
            }
            continue;
        }
        events.push((seq, kind, data));
    }
    let mut lines = vec![
        format!("// 第 {turn} 轮：主 AI 直接调用的方法，共 {calls} 次。宿主按顺序写成代码，只是记录，不能运行。"),
    ];
    if let Some(request) = request.map(|raw| super::stored_text(&raw)).filter(|text| !text.trim().is_empty()) {
        lines.push(format!("// 用户：{}", clip(first_line(&request), 120)));
    }
    for (seq, kind, data) in events {
        match kind.as_str() {
            "assistant/message" => {
                // Words between calls say why; the final answer is in the chat.
                if let Some(text) = data.pointer("/message/content").and_then(Value::as_str).filter(|text| !text.trim().is_empty())
                    && data.pointer("/message/tool_calls").is_some_and(Value::is_array)
                {
                    lines.push(String::new());
                    lines.push(comment(text, 600));
                }
            }
            "tool/call" if data["program_location"].is_null() => {
                let name = data["name"].as_str().unwrap_or("");
                if name == PROGRAM_CALL {
                    continue;
                }
                let result = data["callId"].as_str().and_then(|call| results.get(call));
                if name == WRITE_PROGRAM {
                    // A program that ran shows as its file; one that did not compile says where.
                    if let Some((failed, result)) = result
                        && (*failed || error_text(result).is_some())
                    {
                        lines.push(format!("// 交出的程序没有运行：{}", rejected(result)));
                    }
                    continue;
                }
                let line = call_line(name, data["arguments"].as_str());
                lines.push(match result {
                    Some((failed, result)) => format!("{line}  // → {}", outcome(name, result, *failed)),
                    None => format!("{line}  // → 没有结果（被打断）"),
                });
            }
            "program/start" if data["function"].is_null() => {
                if let Some(line) = program_line(conn, task, seq, &data, end)? {
                    lines.push(line);
                }
            }
            _ => {}
        }
    }
    Ok(Some(lines.join("\n") + "\n"))
}

/// A record cut to `limit` characters for a request. Its opening lines stay,
/// then what changed things (writes, edits, programs, errors), then the
/// AI's words, then the latest of the other calls; what is left out shows
/// as one line per gap. Cutting the middle instead dropped 23 of a turn's
/// 24 edits and kept the reads before them (2026-11-13 review).
pub(crate) fn condense(source: &str, limit: usize) -> String {
    let lines = source.lines().collect::<Vec<_>>();
    let rank = |line: &str| {
        if line.starts_with("io.write(") || line.starts_with("io.edit(") || line.starts_with("io.replace(")
            || line.starts_with("// 运行程序") || line.starts_with("// 交出的程序") || line.contains("// → 出错")
        {
            1
        } else if line.starts_with("//") {
            2
        } else {
            3
        }
    };
    let opening = lines.iter().take_while(|line| line.starts_with("// 第 ") || line.starts_with("// 用户：")).count();
    let mut keep = vec![false; lines.len()];
    let mut used = 0;
    let mut take = |at: usize, keep: &mut Vec<bool>, used: &mut usize| {
        let size = lines[at].chars().count() + 1;
        if *used + size <= limit {
            keep[at] = true;
            *used += size;
        }
    };
    for at in 0..opening {
        take(at, &mut keep, &mut used);
    }
    for tier in 1..=3 {
        for at in (opening..lines.len()).rev().filter(|at| rank(lines[*at]) == tier) {
            take(at, &mut keep, &mut used);
        }
    }
    let mut out = Vec::new();
    let mut gap = 0;
    for (at, line) in lines.iter().enumerate() {
        if keep[at] {
            if gap > 0 {
                out.push(format!("// … 这里省略 {gap} 行，用 start_line、end_line 打开 …"));
                gap = 0;
            }
            out.push((*line).to_owned());
        } else if !line.trim().is_empty() {
            gap += 1;
        }
    }
    if gap > 0 {
        out.push(format!("// … 这里省略 {gap} 行，用 start_line、end_line 打开 …"));
    }
    out.join("\n") + "\n"
}

/// Make the chat folder's top-level files match the conversation records:
/// each turn with direct calls has its record, and a file no record names
/// any more (a rewound turn's program or record, a renamed record) is gone.
/// A program that never compiled has no record either: its file goes too.
pub(crate) fn sync(conn: &Connection, task: &str, dir: &Path) -> Result<()> {
    let recorded = records(conn, task, None)?;
    let programs = conn
        .prepare("SELECT json_extract(data,'$.path') FROM agent_task_events WHERE task_id=?1 AND kind='program/start'
            AND json_extract(data,'$.function') IS NULL AND json_type(data,'$.path')='text'")?
        .query_map([task], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let kept = programs
        .iter()
        .map(|path| path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned())
        .chain(recorded.iter().map(|record| record.file.clone()))
        .collect::<std::collections::HashSet<_>>();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_file() && name.ends_with(".js") && !kept.contains(&name) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    if recorded.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    for record in recorded {
        if let Some(source) = render(conn, task, record.turn, None)? {
            let path = dir.join(&record.file);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(source.as_str()) {
                std::fs::write(path, source)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_call_reads_like_the_program_line_that_would_make_it() {
        assert_eq!(
            call_line("read_file", Some(r#"{"path":"src/a.rs","start_line":100,"end_line":175}"#)),
            r#"io.read({path: "src/a.rs", start_line: 100, end_line: 175});"#
        );
        let edit = call_line("edit_file", Some(&json!({"path":"a.rs","old_string":"x".repeat(80),"new_string":"y"}).to_string()));
        assert_eq!(edit, r#"io.edit({path: "a.rs", new_string: "y", old_string: /* 80 字 */});"#);
        let edits = call_line("edit_file", Some(&json!({"edits":[{"old_text":"a".repeat(90),"new_text":"b"}],"path":"a.rs"}).to_string()));
        assert_eq!(edits, r#"io.edit({path: "a.rs", edits: [{new_text: "b", old_text: /* 90 字 */}]});"#);
        assert_eq!(call_line("workspace_info", Some("{}")), "io.workspace_info();");
        let mut record = String::from("// 第 2 轮：主 AI 直接调用的方法\n// 用户：fix it\n");
        for at in 0..40 {
            record.push_str(&format!("io.read({{path: \"src/file{at}.rs\"}});  // → 120 行\n"));
        }
        record.push_str("// Now the fix.\n");
        for at in 0..5 {
            record.push_str(&format!("io.edit({{path: \"src/file{at}.rs\", old_string: /* 80 字 */}});  // → 已写入\n"));
        }
        record.push_str("run_readonly_command({cmd: \"cargo test\"});  // → 退出 0：ok\n");
        let cut = condense(&record, 600);
        assert!(cut.chars().count() <= 700, "{cut}");
        assert!(cut.starts_with("// 第 2 轮") && cut.contains("// 用户：fix it"), "{cut}");
        assert_eq!(cut.matches("io.edit(").count(), 5, "every edit stays: {cut}");
        assert!(cut.contains("// Now the fix.") && cut.contains("cargo test"), "{cut}");
        assert!(cut.contains("这里省略") && !cut.contains("file0.rs\"});  // → 120"), "the earliest reads go first: {cut}");
        assert_eq!(
            rejected(&json!({"code":"PROGRAM_SOURCE_INVALID","diagnostics":[{"column":29,"line":12,"message":"TS_PROGRAM 12:29: TS1005"}]})),
            "PROGRAM_SOURCE_INVALID 第 12 行：TS_PROGRAM 12:29: TS1005"
        );
        let long = call_line("run_readonly_command", Some(&json!({"cmd":format!("git status; {}", "x".repeat(400))}).to_string()));
        assert!(long.starts_with(r#"run_readonly_command({cmd: "git status; xx"#) && long.ends_with("…\" /* 共 412 字 */});"), "{long}");
        assert_eq!(call_line("run_readonly_command", Some(r#"{"cmd":"git status"}"#)), r#"run_readonly_command({cmd: "git status"});"#);
        assert_eq!(outcome("search_text", &json!({"matches":[1,2,3]}), false), "3 处");
        assert_eq!(outcome("search_text", &json!({"files":["a"],"matches":[]}), false), "1 个文件");
        assert_eq!(outcome("read_file", &json!({"content":"a\nb\n","complete":false}), false), "2 行，没读完");
        assert_eq!(outcome("run_readonly_command", &json!({"exit_code":0,"stdout":"ok\ntest result: ok. 18 passed\n","stderr":""}), false), "退出 0：test result: ok. 18 passed");
        assert_eq!(outcome("edit_file", &json!({"error_code":"NOT_FOUND","message":"old_string not found\nmore"}), false), "出错：old_string not found");
        assert_eq!(stem("Save permission per chat.js", 60).as_deref(), Some("save-permission-per-chat"));
        assert_eq!(stem("  ", 60), None);
        assert_eq!(request_stem("What did the last program do?").as_deref(), Some("what-did-the-last-program-do"));
        // Code files have no Chinese names: only the English words count.
        assert_eq!(request_stem("按聊天保存权限级别，然后刷新也保留"), None);
        assert_eq!(request_stem("把 permissionMode 改成按聊天保存").as_deref(), Some("permission-mode"));
        assert_eq!(
            files_stem(&["frontend/src/permissionMode.ts".into(), "src/agent_service.rs".into(), "/w/permission_mode.test.mjs".into()]).as_deref(),
            Some("permission-mode-agent-service"),
            "at most 32 characters, whole words"
        );
    }
}
