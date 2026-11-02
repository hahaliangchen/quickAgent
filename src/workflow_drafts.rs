//! Editable copies of a conversation's recorded programs, kept apart from its
//! history, and the read-only view of those programs that the skill workbench shows.
use crate::agent_service::{self, AgentServiceState};
use anyhow::{Context, Result, ensure};
use axum::{extract::{Path, State}, http::StatusCode, response::{IntoResponse, Response}, Json};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{json, Value};
use std::{collections::{BTreeMap, HashMap}, fs, path::{Path as FsPath, PathBuf}};

/// Characters of a program's output the view carries; the rest stays in the conversation record.
const OUTPUT_CHARS: usize = 8_000;
/// Draft files larger than this are listed without their text.
const FILE_BYTES: u64 = 512 * 1024;
/// The host's own files in a draft folder; they describe the draft and are not its code.
const HOST_FILES: &[&str] = &["draft.json", "history.json", "programs.json", "AGENTS.md", "README.md"];

fn drafts(root: &FsPath) -> PathBuf {
    root.join(".codex-workspace-mcp").join("workflow-drafts")
}

fn folder(root: &FsPath, id: &str) -> Result<PathBuf> {
    ensure!(id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()), "invalid draft id");
    Ok(drafts(root).join(id))
}

fn clip(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

/// One recorded event as the view reads it. Replies and tool calls carry only the
/// fields the view uses, so a long session's tool arguments are not loaded.
struct Event {
    seq: i64,
    time: i64,
    kind: String,
    node: String,
    data: Value,
}

fn events(conn: &Connection, task: &str) -> Result<Vec<Event>> {
    let mut statement = conn.prepare(
        "SELECT seq,timestamp,kind,COALESCE(node_id,json_extract(data,'$.task_node_id'),''),
           CASE kind
             WHEN 'tool/call' THEN json_object('name',json_extract(data,'$.name'),'callId',json_extract(data,'$.callId'),
               'step',json_extract(data,'$.step'),'arguments',
               CASE WHEN json_extract(data,'$.name') IN ('create_task','write_program') THEN json_extract(data,'$.arguments') END)
             WHEN 'assistant/message' THEN json_object('actor',json_extract(data,'$.actor'),'step',json_extract(data,'$.step'),
               'content',CASE WHEN json_type(data,'$.message.content')='text' THEN json_extract(data,'$.message.content') END)
             ELSE data END
         FROM agent_task_events WHERE task_id=?1 AND kind IN ('turn/start','user/message','assistant/message','tool/call',
           'program/start','program/source_invalid','program/result','program/error','program/runtime_error','program/paused','turn/end')
         ORDER BY seq",
    )?;
    let rows = statement.query_map([task], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?))
    })?;
    rows.map(|row| {
        let (seq, time, kind, node, raw) = row?;
        Ok(Event { seq, time, kind, node, data: serde_json::from_str(&raw).unwrap_or(Value::Null) })
    })
    .collect()
}

/// A tool call's arguments: JSON text, or an object in some older records.
fn arguments(data: &Value) -> Value {
    match &data["arguments"] {
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        value => value.clone(),
    }
}

/// The user's words of a turn. A plugin page appends what was selected on the
/// page; that is kept apart, as the page's data.
fn request(event: &Event) -> Value {
    let content = &event.data["message"]["content"];
    let mut text = String::new();
    let mut attachments = Vec::new();
    for part in content.as_array().map(|parts| parts.iter().collect()).unwrap_or_else(|| vec![content]) {
        if let Some(piece) = part.as_str().or(part["text"].as_str()) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(piece);
        } else if part["attachment"].is_object() {
            attachments.push(json!({"name":part["attachment"]["name"],"bytes":part["attachment"]["bytes"]}));
        }
    }
    let marker = crate::plugin_sessions::SELECTION;
    let (words, selection) = match text.rfind(marker) {
        Some(at) => (text[..at].to_owned(), Some(text[at + marker.len()..].to_owned())),
        None => (text, None),
    };
    json!({"seq":event.seq,"time":event.time,"text":clip(&words, 20_000),"page_selection":selection,"attachments":attachments})
}

/// A main program's run and what the host recorded about how it ended.
struct Run {
    seq: i64,
    turn: i64,
    time: i64,
    source: String,
    path: Option<String>,
    /// What its author said in the reply that handed it over.
    note: Option<String>,
    status: &'static str,
    ended_at: Option<i64>,
    output: Option<String>,
    returned: Option<String>,
    error: Option<Value>,
    rewrite_of: Option<i64>,
    rewritten_by: Option<i64>,
    calls: Vec<(String, &'static str, usize)>,
    /// Hand-overs that did not compile, just before this run.
    rejected: Vec<Value>,
}

impl Run {
    fn call(&mut self, name: &str, data: &Value) {
        let (name, kind) = if name == "create_task" {
            let args = arguments(data);
            let task = &args["program_task"];
            // The declaration decides: a @virtual is an AI loop, a @module (or a
            // virtual saved before modules existed) is AI-written code.
            let kind = match task["role"].as_str() {
                Some("leaf") => "virtual",
                Some("implement") if task["module"] == true => "module",
                Some("implement") => "virtual",
                _ => "function",
            };
            (task["name"].as_str().or(args["goal"].as_str()).unwrap_or("函数").to_owned(), kind)
        } else {
            (name.to_owned(), "tool")
        };
        match self.calls.iter_mut().find(|(known, known_kind, _)| *known == name && *known_kind == kind) {
            Some(call) => call.2 += 1,
            None => self.calls.push((name, kind, 1)),
        }
    }

    fn json(&self) -> Value {
        json!({"seq":self.seq,"turn":self.turn,"time":self.time,"ended_at":self.ended_at,"source":self.source,"path":self.path,
            "note":self.note,"status":self.status,"output":self.output.as_deref().map(|text| clip(text, OUTPUT_CHARS)),
            "output_chars":self.output.as_deref().map_or(0, |text| text.chars().count()),"returned":self.returned,"error":self.error,
            "rewrite_of":self.rewrite_of,"rewritten_by":self.rewritten_by,
            "calls":self.calls.iter().map(|(name, kind, count)| json!({"name":name,"kind":kind,"count":count})).collect::<Vec<_>>(),
            "rejected":self.rejected})
    }
}

#[derive(Default)]
struct Turn {
    request: Option<Value>,
    ended: Option<String>,
    runs: Vec<usize>,
    /// Hand-overs that did not compile and were not followed by a run.
    rejected: Vec<Value>,
}

/// The conversation's main programs by turn: each run's source, the calls it made
/// and how it ended, from the records the host keeps. A run whose end was not
/// recorded says so; nothing is inferred from what the programs or the AI said.
pub(crate) fn session_programs(conn: &Connection, task: &str) -> Result<Value> {
    let status: String = conn
        .query_row("SELECT status FROM agent_tasks WHERE id=?1", [task], |r| r.get(0))
        .optional()?
        .context("session not found")?;
    let live = matches!(status.as_str(), "running" | "cancelling");
    let mut turns: BTreeMap<i64, Turn> = BTreeMap::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut replies: Vec<(i64, Value)> = Vec::new();
    let mut turn = 0;
    // Per frame: the program running in it, its error until the host decides what
    // follows, the program that ended last, the program handed back to be
    // rewritten, the last write_program call, and compile failures awaiting a run.
    let mut open: HashMap<String, usize> = HashMap::new();
    let mut failed: HashMap<String, Value> = HashMap::new();
    let mut ended: HashMap<String, usize> = HashMap::new();
    let mut handed_back: HashMap<String, usize> = HashMap::new();
    let mut submitted: HashMap<String, (i64, String)> = HashMap::new();
    let mut rejected: HashMap<String, Vec<Value>> = HashMap::new();
    let mut said: HashMap<(String, i64), String> = HashMap::new();
    for event in events(conn, task)? {
        let data = &event.data;
        let node = event.node.clone();
        match event.kind.as_str() {
            "turn/start" => turn = data["turn"].as_i64().unwrap_or(turn),
            "user/message" => {
                turn = data["turn"].as_i64().unwrap_or(turn);
                let entry = turns.entry(turn).or_default();
                if entry.request.is_none() {
                    entry.request = Some(request(&event));
                }
            }
            "tool/call" => {
                let name = data["name"].as_str().unwrap_or_default();
                if name == "write_program" {
                    let source = arguments(data)["source"].as_str().unwrap_or_default().to_owned();
                    submitted.insert(node, (data["step"].as_i64().unwrap_or(-1), source));
                } else if data["callId"].as_str().is_some_and(|id| id.starts_with("program-")) {
                    if let Some(&index) = open.get(&node) {
                        runs[index].call(name, data);
                    }
                }
            }
            "assistant/message" => {
                let text = data["content"].as_str().unwrap_or_default();
                if data["actor"] == "program" {
                    if let Some(index) = open.remove(&node) {
                        // A program that ends its turn leaves its outcome here; before
                        // program/result existed, a finished program did too.
                        let run = &mut runs[index];
                        run.status = match failed.remove(&node) {
                            Some(error) if error["code"] == "WAITING_FOR_USER" => "waiting",
                            Some(_) => "failed",
                            None => "completed",
                        };
                        run.ended_at = Some(event.time);
                        run.output = Some(text.to_owned());
                        ended.insert(node, index);
                    } else if let Some(&index) = ended.get(&node) {
                        // A paused program's outcome follows its pause.
                        runs[index].output.get_or_insert_with(|| text.to_owned());
                    }
                } else if !text.trim().is_empty() {
                    if let Some(step) = data["step"].as_i64() {
                        said.insert((node, step), text.to_owned());
                    }
                    // Before write_program, a reply carried its program in a ```js block.
                    if !text.lines().any(|line| line.trim_end() == "```js") {
                        replies.push((turn, json!({"seq":event.seq,"time":event.time,"text":clip(text, 4_000)})));
                    }
                }
            }
            "program/start" if data["function"].is_null() => {
                if let Some(index) = open.remove(&node) {
                    runs[index].status = "unknown";
                }
                failed.remove(&node);
                let source = data["source"].as_str().unwrap_or_default().to_owned();
                let note = submitted
                    .get(&node)
                    .filter(|(_, handed)| *handed == source)
                    .and_then(|(step, _)| said.get(&(node.clone(), *step)))
                    .map(|text| clip(text, 1_000));
                let seq = event.seq;
                let rewrite_of = handed_back.remove(&node).map(|previous| {
                    runs[previous].rewritten_by = Some(seq);
                    runs[previous].seq
                });
                let run_turn = data["turn"].as_i64().unwrap_or(turn);
                turns.entry(run_turn).or_default().runs.push(runs.len());
                open.insert(node.clone(), runs.len());
                runs.push(Run {
                    seq,
                    turn: run_turn,
                    time: event.time,
                    source,
                    path: data["path"].as_str().map(str::to_owned),
                    note,
                    status: "running",
                    ended_at: None,
                    output: None,
                    returned: None,
                    error: None,
                    rewrite_of,
                    rewritten_by: None,
                    calls: Vec::new(),
                    rejected: rejected.remove(&node).unwrap_or_default(),
                });
            }
            "program/source_invalid" => {
                let diagnostic = &data["diagnostic"];
                let diagnostics: Vec<Value> = match diagnostic["diagnostics"].as_array() {
                    Some(list) => list
                        .iter()
                        .map(|item| json!({"line":item["line"],"column":item["column"],"message":clip(item["message"].as_str().unwrap_or_default(), 2_000)}))
                        .collect(),
                    None => {
                        let message = diagnostic["error"].as_str().or(diagnostic["message"].as_str()).unwrap_or_default();
                        vec![json!({"line":null,"column":null,"message":clip(message, 2_000)})]
                    }
                };
                let source = submitted.get(&node).map(|(_, source)| source.clone()).filter(|source| !source.is_empty());
                rejected
                    .entry(node)
                    .or_default()
                    .push(json!({"seq":event.seq,"time":event.time,"turn":turn,"diagnostics":diagnostics,"source":source}));
            }
            "program/result" => {
                if let Some(index) = open.remove(&node) {
                    let run = &mut runs[index];
                    run.status = "completed";
                    run.ended_at = Some(event.time);
                    run.output = data["output"].as_str().map(str::to_owned);
                    run.returned = match &data["result"] {
                        Value::Null => None,
                        Value::String(text) => Some(clip(text, 2_000)),
                        value => Some(clip(&value.to_string(), 2_000)),
                    };
                    ended.insert(node, index);
                }
            }
            "program/error" => {
                if let Some(&index) = open.get(&node) {
                    let error = data["error"].clone();
                    let message = error["message"].as_str().or(error["error"].as_str()).unwrap_or_default();
                    let run = &mut runs[index];
                    run.status = "failed";
                    run.ended_at = Some(event.time);
                    run.error = Some(json!({"message":clip(message, 2_000),"line":error["location"]["line"],"code":error["code"]}));
                    failed.insert(node, error);
                }
            }
            "program/runtime_error" => {
                // The main program heals: its author gets the error and may write the next one.
                if failed.remove(&node).is_some() {
                    if let Some(index) = open.remove(&node) {
                        let run = &mut runs[index];
                        run.status = "rewritten";
                        if let Some(lines) = data["diagnostic"]["console"].as_array().filter(|lines| !lines.is_empty()) {
                            let lines: Vec<String> = lines.iter().map(|line| line.as_str().map(str::to_owned).unwrap_or_else(|| line.to_string())).collect();
                            run.output = Some(lines.join("\n"));
                        }
                        handed_back.insert(node.clone(), index);
                        ended.insert(node, index);
                    }
                }
            }
            "program/paused" => {
                if let Some(index) = open.remove(&node) {
                    runs[index].status = "paused";
                    runs[index].ended_at = Some(event.time);
                    ended.insert(node, index);
                }
            }
            "turn/end" => {
                // A program still open when its turn ended left no outcome behind.
                for (_, index) in open.drain() {
                    runs[index].status = "unknown";
                }
                failed.clear();
                handed_back.clear();
                let entry = turns.entry(turn).or_default();
                entry.ended = data["reason"]["kind"].as_str().map(str::to_owned);
                for (_, list) in rejected.drain() {
                    entry.rejected.extend(list);
                }
            }
            _ => {}
        }
    }
    for (_, index) in open.drain() {
        runs[index].status = if live { "running" } else { "unknown" };
    }
    for (_, list) in rejected.drain() {
        for item in list {
            turns.entry(item["turn"].as_i64().unwrap_or(turn)).or_default().rejected.push(item);
        }
    }
    // Turns without programs stay: their requests are the context of the ones with programs.
    let turns: Vec<Value> = turns
        .into_iter()
        .filter(|(_, entry)| entry.request.is_some() || !entry.runs.is_empty() || !entry.rejected.is_empty())
        .map(|(number, entry)| {
            // What the AI told the user after the turn's last program.
            let last = entry.runs.last().map_or(i64::MIN, |&index| runs[index].seq);
            let reply = replies
                .iter()
                .rev()
                .find(|(at, reply)| *at == number && reply["seq"].as_i64().is_some_and(|seq| seq > last))
                .map(|(_, reply)| reply.clone());
            json!({"turn":number,"request":entry.request,"reply":reply,"ended":entry.ended,
                "programs":entry.runs.iter().map(|&index| runs[index].json()).collect::<Vec<_>>(),"rejected":entry.rejected})
        })
        .collect();
    Ok(json!({"status":status,"turns":turns}))
}

fn source_title(conn: &Connection, task: &str) -> Result<String> {
    let title: String = conn.query_row(
        "SELECT COALESCE(NULLIF(TRIM(m.title),''),t.prompt) FROM agent_tasks t LEFT JOIN agent_session_meta m ON m.task_id=t.id WHERE t.id=?1",
        [task],
        |r| r.get(0),
    )?;
    Ok(clip(title.lines().next().unwrap_or_default().trim(), 60))
}

fn status_words(status: &str) -> &'static str {
    match status {
        "completed" => "已完成",
        "rewritten" => "出错，交回 AI 改写",
        "failed" => "出错停止",
        "waiting" => "停下等用户回答",
        "paused" => "被用户的新消息暂停",
        "running" => "还在运行",
        _ => "没有找到结果记录",
    }
}

/// Standing notes for the draft's own conversation. The host reads a code
/// session's AGENTS.md into every request, so every turn knows what the draft is.
fn notes(manifest: &Value, view: &Value, title: &str) -> String {
    let mut text = format!("# 流程技能草稿\n\n这个文件夹里是从会话「{title}」复制出来的流程代码，要整理成以后能复用的技能。");
    if let Some(plugin) = manifest["plugin_id"].as_str() {
        text += &format!("那次会话属于插件 {plugin}。");
    }
    text += "\n\n## 文件\n\n- `programs/`：会话里各段主程序的副本，在这里修改。\n- `original/`：创建草稿时的原样，用来对照，保持不变。\n- `modules/`：会话里保存的模块，取的是最新版本。\n- `programs.json`：每一轮用户的原话、各段程序怎么结束的、调用过哪些方法。\n- `history.json`：原始记录，包括程序的诊断和出错信息。\n\n## 各段程序\n\n";
    for turn in view["turns"].as_array().into_iter().flatten() {
        for (index, run) in turn["programs"].as_array().into_iter().flatten().enumerate() {
            text += &format!(
                "- `programs/program-{}.js`：第 {} 轮第 {} 段，{}\n",
                run["seq"], turn["turn"], index + 1, status_words(run["status"].as_str().unwrap_or_default())
            );
        }
    }
    text += "\n## 怎么做\n\n- 先看 `programs.json`，弄清每段程序做了什么、哪些真正完成了、确认点在哪里。出错后改写过的，以跑通的版本为准。\n- 写或改流程代码之前，先用 read_program_guide 读流程语言的说明，再用 read_tool_contracts 读用到的库的方法契约。\n- 只属于这一次的值（题目、页数、模板、素材、项目编号等）改成参数；要用户确认的地方照旧停下来等确认。\n- 插件的方法会改动插件里的真实数据，这个会话只整理草稿文件，用不到它们；这里也没有插件页面的工具，做不了真实的插件测试。\n- 说结果时分开说：哪些确实检查过（读过的文件、对照过的记录），哪些还没有验证。没有真正运行过的测试，不说通过。\n";
    text
}

fn create_draft(root: &FsPath, task: &str) -> Result<Value> {
    let conn = agent_service::open_db(root)?;
    let view = session_programs(&conn, task)?;
    let owner = crate::plugin_sessions::owner(root, task)?;
    let runs: Vec<&Value> = view["turns"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|turn| turn["programs"].as_array().into_iter().flatten())
        .collect();
    ensure!(!runs.is_empty(), "this session has no recorded main programs");
    let id = agent_service::uuid_like();
    let dir = folder(root, &id)?;
    fs::create_dir_all(dir.join("original").join("modules"))?;
    fs::create_dir_all(dir.join("programs"))?;
    fs::create_dir_all(dir.join("modules"))?;
    let mut sources = Vec::new();
    for run in runs {
        let seq = run["seq"].as_i64().context("recorded program sequence is missing")?;
        let source = run["source"].as_str().context("recorded program source is missing")?;
        let filename = format!("program-{seq}.js");
        fs::write(dir.join("original").join(&filename), source)?;
        fs::write(dir.join("programs").join(&filename), source)?;
        sources.push(json!({"seq":seq,"turn":run["turn"],"file":format!("programs/{filename}"),"status":run["status"],"rewrite_of":run["rewrite_of"]}));
    }
    // Snapshot the latest saved module versions, without treating unused versions as verified.
    let mut modules = Vec::new();
    if let Ok(mut q) = conn.prepare("SELECT m.name,m.contract,m.version,m.source FROM agent_modules m WHERE m.task_id=?1 AND m.version=(SELECT MAX(v.version) FROM agent_modules v WHERE v.task_id=m.task_id AND v.contract=m.contract) ORDER BY m.name") {
        let rows = q.query_map([task], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))?;
        for row in rows { let (name, contract, version, source) = row?;
            let file = format!("module-{}.js", modules.len()+1);
            fs::write(dir.join("modules").join(&file), &source)?;
            fs::write(dir.join("original").join("modules").join(&file), &source)?;
            modules.push(json!({"name":name,"contract":contract,"version":version,"file":format!("modules/{file}")}));
        }
    }
    let mut statement = conn.prepare("SELECT seq,kind,data FROM agent_task_events WHERE task_id=?1 AND kind IN ('program/start','program/result','program/error','program/runtime_error','program/paused','user/message','turn/end','module/saved','program/source_invalid') ORDER BY seq")?;
    let records: Vec<Value> = statement.query_map([task], |r| Ok((r.get::<_,i64>(0)?, r.get::<_,String>(1)?, r.get::<_,String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter().filter_map(|(seq,kind,raw)| serde_json::from_str::<Value>(&raw).ok().map(|data| json!({"seq":seq,"kind":kind,"data":data}))).collect();
    fs::write(dir.join("history.json"), serde_json::to_vec_pretty(&records)?)?;
    fs::write(dir.join("programs.json"), serde_json::to_vec_pretty(&view)?)?;
    let manifest = json!({"id":id,"source_task":task,"plugin_id":owner,"created_at":agent_service::now(),"programs":sources,"modules":modules,"workspace":dir.to_string_lossy()});
    fs::write(dir.join("draft.json"), serde_json::to_vec_pretty(&manifest)?)?;
    fs::write(dir.join("AGENTS.md"), notes(&manifest, &view, &source_title(&conn, task)?))?;
    Ok(manifest)
}

/// The draft's conversation: the one draft.json names while it exists, else (for
/// drafts made before that was recorded) the newest session working in the folder.
fn bound_chat(conn: &Connection, dir: &FsPath, manifest: &Value) -> Result<Option<String>> {
    if let Some(id) = manifest["chat_task"].as_str() {
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_tasks WHERE id=?1)", [id], |r| r.get(0))?;
        if exists {
            return Ok(Some(id.to_owned()));
        }
    }
    let Ok(workspace) = fs::canonicalize(dir) else { return Ok(None) };
    Ok(conn
        .query_row(
            "SELECT m.task_id FROM agent_session_meta m JOIN agent_tasks t ON t.id=m.task_id WHERE m.workspace=?1 ORDER BY t.created_at DESC LIMIT 1",
            [workspace.to_string_lossy()],
            |r| r.get(0),
        )
        .optional()?)
}

/// Start the draft's own code conversation in its folder, with the model settings
/// of the session the draft came from, and record it in draft.json.
fn open_chat(root: &FsPath, dir: &FsPath, manifest: &mut Value) -> Result<String> {
    let workspace = fs::canonicalize(dir)?.to_string_lossy().into_owned();
    let source = manifest["source_task"].as_str().context("draft source is missing")?.to_owned();
    let id = format!("task_{}", agent_service::uuid_like());
    let chat = id.clone();
    crate::db_writer::write(root, move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let title = source_title(&tx, &source).unwrap_or_default();
        let (model, provider, effort, fast): (String, Option<String>, Option<String>, i64) = tx.query_row(
            "SELECT model,provider_name,reasoning_effort,fast_mode FROM agent_tasks WHERE id=?1",
            [&source],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let now = agent_service::now();
        tx.execute(
            "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at,provider_name,reasoning_effort,fast_mode) VALUES (?1,'',?2,'draft',?3,?3,?4,?5,?6)",
            params![chat, model, now, provider, effort, fast],
        )?;
        tx.execute(
            "INSERT INTO agent_session_meta(task_id,title,workspace,kind) VALUES (?1,?2,?3,'code')",
            params![chat, format!("流程草稿 · {title}"), workspace],
        )?;
        tx.commit()?;
        Ok(())
    })?;
    manifest["chat_task"] = json!(id);
    fs::write(dir.join("draft.json"), serde_json::to_vec_pretty(manifest)?)?;
    Ok(id)
}

/// Drafts made before the notes existed get them before their conversation goes on.
fn ensure_notes(conn: &Connection, dir: &FsPath, manifest: &Value) -> Result<()> {
    if dir.join("AGENTS.md").exists() {
        return Ok(());
    }
    let task = manifest["source_task"].as_str().unwrap_or_default();
    let view = session_programs(conn, task).unwrap_or_else(|_| json!({"turns":[]}));
    if !dir.join("programs.json").exists() {
        fs::write(dir.join("programs.json"), serde_json::to_vec_pretty(&view)?)?;
    }
    fs::write(dir.join("AGENTS.md"), notes(manifest, &view, &source_title(conn, task).unwrap_or_default()))?;
    Ok(())
}

fn modified(path: &FsPath) -> Option<i64> {
    let time = fs::metadata(path).ok()?.modified().ok()?;
    Some(time.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
}

/// Where a draft file started from, when it was copied from the session.
fn original_of(path: &str) -> Option<String> {
    path.strip_prefix("programs/")
        .map(|name| format!("original/{name}"))
        .or_else(|| path.strip_prefix("modules/").map(|name| format!("original/modules/{name}")))
}

enum Content {
    Missing,
    Text(String),
    /// Binary, or too large to show.
    Opaque,
}

fn content(path: &FsPath) -> Content {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= FILE_BYTES => match fs::read(path).map(String::from_utf8) {
            Ok(Ok(text)) => Content::Text(text),
            _ => Content::Opaque,
        },
        Ok(meta) if meta.is_file() => Content::Opaque,
        _ => Content::Missing,
    }
}

fn walk(base: &FsPath, dir: &FsPath, found: &mut Vec<String>, depth: usize) -> Result<()> {
    if depth > 6 || found.len() >= 300 {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(base)?.to_string_lossy().replace('\\', "/");
        if depth == 0 && (relative == "original" || relative == ".codex-workspace-mcp" || HOST_FILES.contains(&relative.as_str())) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(base, &path, found, depth + 1)?;
        } else if kind.is_file() {
            found.push(relative);
        }
    }
    Ok(())
}

/// The draft's files as they are on disk beside the snapshot they started from,
/// so whatever its conversation changed, added or removed shows.
fn draft_files(dir: &FsPath, manifest: &Value) -> Result<Vec<Value>> {
    let mut paths: Vec<String> = manifest["programs"]
        .as_array()
        .into_iter()
        .chain(manifest["modules"].as_array())
        .flatten()
        .filter_map(|item| item["file"].as_str().map(str::to_owned))
        .collect();
    let mut found = Vec::new();
    walk(dir, dir, &mut found, 0)?;
    found.sort();
    for path in found {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Ok(paths
        .into_iter()
        .map(|path| {
            let current = content(&dir.join(&path));
            let original = original_of(&path).map_or(Content::Missing, |original| content(&dir.join(original)));
            let status = match (&current, &original) {
                (Content::Missing, _) => "deleted",
                (_, Content::Missing) => "added",
                (Content::Text(now), Content::Text(before)) if now == before => "unchanged",
                _ => "modified",
            };
            let text = |content: Content| match content {
                Content::Text(text) => Some(text),
                _ => None,
            };
            let binary = matches!(current, Content::Opaque) || matches!(original, Content::Opaque);
            json!({"path":path,"status":status,"binary":binary,"current":text(current),"original":text(original)})
        })
        .filter(|file| !(file["status"] == "deleted" && file["original"].is_null() && !file["binary"].as_bool().unwrap_or(false)))
        .collect())
}

fn draft_view(root: &FsPath, id: &str) -> Result<Value> {
    let dir = folder(root, id)?;
    let mut manifest: Value = serde_json::from_slice(&fs::read(dir.join("draft.json")).context("draft not found")?)?;
    let conn = agent_service::open_db(root)?;
    manifest["chat_task"] = json!(bound_chat(&conn, &dir, &manifest)?);
    if manifest["created_at"].is_null() {
        manifest["created_at"] = json!(modified(&dir.join("draft.json")));
    }
    Ok(json!({"draft":manifest,"files":draft_files(&dir, &manifest)?}))
}

fn list_drafts(root: &FsPath) -> Result<Vec<Value>> {
    let Ok(entries) = fs::read_dir(drafts(root)) else { return Ok(Vec::new()) };
    let conn = agent_service::open_db(root)?;
    let mut list = Vec::new();
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        let Ok(dir) = folder(root, &id) else { continue };
        let Some(manifest) = fs::read(dir.join("draft.json")).ok().and_then(|raw| serde_json::from_slice::<Value>(&raw).ok()) else {
            continue;
        };
        list.push(json!({"id":id,"source_task":manifest["source_task"],"plugin_id":manifest["plugin_id"],
            "created_at":manifest["created_at"].as_i64().or_else(|| modified(&dir.join("draft.json"))),
            "programs":manifest["programs"].as_array().map_or(0, Vec::len),"chat_task":bound_chat(&conn, &dir, &manifest)?}));
    }
    list.sort_by_key(|draft| std::cmp::Reverse(draft["created_at"].as_i64().unwrap_or(0)));
    Ok(list)
}

/// The draft's conversation, started on first use.
fn draft_chat(root: &FsPath, id: &str) -> Result<Value> {
    let dir = folder(root, id)?;
    let mut manifest: Value = serde_json::from_slice(&fs::read(dir.join("draft.json")).context("draft not found")?)?;
    let conn = agent_service::open_db(root)?;
    ensure_notes(&conn, &dir, &manifest)?;
    let task = match bound_chat(&conn, &dir, &manifest)? {
        Some(task) => task,
        None => open_chat(root, &dir, &mut manifest)?,
    };
    Ok(json!({"task_id":task}))
}

fn respond(result: std::result::Result<Result<Value>, tokio::task::JoinError>, status: StatusCode) -> Response {
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (status, Json(json!({"error":format!("{error:#}")}))).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":error.to_string()}))).into_response(),
    }
}

pub async fn programs(State(state): State<AgentServiceState>, Path(task): Path<String>) -> Response {
    let root = state.workspace.root().to_path_buf();
    respond(tokio::task::spawn_blocking(move || session_programs(&agent_service::open_db(&root)?, &task)).await, StatusCode::NOT_FOUND)
}

pub async fn create(State(state): State<AgentServiceState>, Path(task): Path<String>) -> Response {
    let root = state.workspace.root().to_path_buf();
    respond(
        tokio::task::spawn_blocking(move || -> Result<Value> {
            let mut manifest = create_draft(&root, &task)?;
            let dir = folder(&root, manifest["id"].as_str().context("draft id is missing")?)?;
            open_chat(&root, &dir, &mut manifest)?;
            Ok(manifest)
        })
        .await,
        StatusCode::BAD_REQUEST,
    )
}

pub async fn list(State(state): State<AgentServiceState>) -> Response {
    let root = state.workspace.root().to_path_buf();
    respond(tokio::task::spawn_blocking(move || -> Result<Value> { Ok(json!({"drafts":list_drafts(&root)?})) }).await, StatusCode::INTERNAL_SERVER_ERROR)
}

pub async fn read(State(state): State<AgentServiceState>, Path(id): Path<String>) -> Response {
    let root = state.workspace.root().to_path_buf();
    respond(tokio::task::spawn_blocking(move || draft_view(&root, &id)).await, StatusCode::NOT_FOUND)
}

pub async fn chat(State(state): State<AgentServiceState>, Path(id): Path<String>) -> Response {
    let root = state.workspace.root().to_path_buf();
    respond(tokio::task::spawn_blocking(move || draft_chat(&root, &id)).await, StatusCode::BAD_REQUEST)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn event(db: &Connection, seq: i64, node: &str, kind: &str, data: Value) {
        db.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data,node_id) VALUES ('t',?1,?2,?3,?4,?5)", params![seq, seq * 1000, kind, data.to_string(), node]).unwrap();
    }

    #[test]
    fn copies_all_programs_and_module_versions_without_touching_the_source() {
        let root = std::env::temp_dir().join(format!("workflow-draft-{}", agent_service::uuid_like()));
        fs::create_dir_all(&root).unwrap();
        let db = agent_service::open_db(&root).unwrap();
        db.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','hello','fake','completed',1,1)", []).unwrap();
        db.execute("INSERT INTO plugin_sessions(task_id,plugin_id) VALUES ('t','ppt-master')", []).unwrap();
        for (seq, kind, value) in [(1,"user/message",json!({"turn":1,"message":{"content":"hello"}})),(2,"program/start",json!({"turn":1,"function":null,"source":"first();"})),(3,"program/start",json!({"turn":2,"function":null,"source":"second();"}))] {
            db.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES ('t',?1,1,?2,?3)", params![seq,kind,value.to_string()]).unwrap();
        }
        db.execute_batch("CREATE TABLE IF NOT EXISTS agent_modules(task_id TEXT,turn INTEGER,contract TEXT,name TEXT,signature TEXT,description TEXT,version INTEGER,source TEXT,fixed INTEGER,created_at INTEGER);").unwrap();
        db.execute("INSERT INTO agent_modules VALUES ('t',1,'abc','f','f()','test',1,'function f() {}',0,1)", []).unwrap();
        let result = create_draft(&root, "t").unwrap();
        assert_eq!(result["plugin_id"], "ppt-master");
        assert_eq!(result["programs"].as_array().unwrap().len(), 2);
        assert_eq!(result["modules"].as_array().unwrap().len(), 1);
        let dir = folder(&root, result["id"].as_str().unwrap()).unwrap();
        fs::write(dir.join("programs/program-2.js"), "changed();").unwrap();
        assert_eq!(fs::read_to_string(dir.join("original/program-2.js")).unwrap(), "first();");
        assert!(folder(&root, "../outside").is_err());
        drop(db); let _ = fs::remove_dir_all(root);
    }

    /// Turn 1: a hand-over that did not compile, a run that failed and was
    /// handed back, its rewrite that finished. Turn 2: a run that stopped on its
    /// error. Turn 3: a run recorded the way hosts did before program/result.
    #[test]
    fn programs_by_turn_tell_finished_rewritten_and_stopped_runs_apart() {
        let root = std::env::temp_dir().join(format!("workflow-view-{}", agent_service::uuid_like()));
        fs::create_dir_all(&root).unwrap();
        let db = agent_service::open_db(&root).unwrap();
        db.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','make slides','fake','completed',1,1)", []).unwrap();
        let ask = format!("make 10 slides{}{{\"page_indexes\":[]}}", crate::plugin_sessions::SELECTION);
        event(&db, 1, "n0", "turn/start", json!({"turn":1}));
        event(&db, 2, "n0", "user/message", json!({"turn":1,"message":{"content":[{"type":"text","text":ask},{"type":"file","attachment":{"name":"guide.md","bytes":10}}]}}));
        let call = |source: &str| json!({"name":"write_program","callId":"call-1","step":1,"arguments":json!({"source":source}).to_string()});
        event(&db, 3, "a", "tool/call", call("broken("));
        event(&db, 4, "a", "program/source_invalid", json!({"diagnostic":{"diagnostics":[{"line":1,"column":7,"message":"unexpected end"}]}}));
        event(&db, 5, "a", "assistant/message", json!({"actor":"agent","turn":1,"step":2,"message":{"role":"assistant","content":"I will read the template first."}}));
        event(&db, 6, "a", "tool/call", json!({"name":"write_program","callId":"call-2","step":2,"arguments":json!({"source":"const p = require('ppt');"}).to_string()}));
        event(&db, 7, "a", "program/start", json!({"turn":1,"function":null,"source":"const p = require('ppt');","path":"turn-1.js"}));
        event(&db, 8, "a", "tool/call", json!({"name":"ppt__get_plan","callId":"program-1","arguments":"{}"}));
        let virtual_call = json!({"goal":"designPage","program_task":{"name":"designPage","role":"leaf"}}).to_string();
        event(&db, 9, "a", "tool/call", json!({"name":"create_task","callId":"program-2","arguments":virtual_call}));
        event(&db, 10, "a", "tool/call", json!({"name":"create_task","callId":"program-3","arguments":virtual_call}));
        event(&db, 11, "a", "program/error", json!({"error":{"message":"page 2 failed","location":{"line":4}}}));
        event(&db, 12, "a", "program/runtime_error", json!({"turn":1,"main":true,"diagnostic":{"console":["PAGE 1 ok"]}}));
        event(&db, 13, "a", "program/start", json!({"turn":1,"function":null,"source":"retry();"}));
        event(&db, 14, "a", "program/result", json!({"turn":1,"result":null,"output":"程序已完成。\n输出：\nPAGE 2 ok"}));
        event(&db, 15, "a", "assistant/message", json!({"actor":"agent","turn":1,"step":5,"message":{"role":"assistant","content":"Ten slides are saved."}}));
        event(&db, 16, "n0", "turn/end", json!({"turn":1,"reason":{"kind":"completed"}}));
        event(&db, 17, "n0", "user/message", json!({"turn":2,"message":{"content":"export it"}}));
        event(&db, 18, "b", "program/start", json!({"turn":2,"function":null,"source":"exportIt();"}));
        event(&db, 19, "b", "program/error", json!({"error":{"message":"export failed","location":{"line":1}}}));
        event(&db, 20, "b", "assistant/message", json!({"actor":"program","turn":2,"message":{"role":"assistant","content":"程序在第 1 行出错停止：export failed"}}));
        event(&db, 21, "n0", "turn/end", json!({"turn":2,"reason":{"kind":"completed"}}));
        event(&db, 22, "n0", "user/message", json!({"turn":3,"message":{"content":"again"}}));
        event(&db, 23, "c", "program/start", json!({"turn":3,"function":null,"source":"again();"}));
        event(&db, 24, "c", "program/start", json!({"turn":3,"function":"helper","source":"function helper() {}"}));
        event(&db, 25, "c", "assistant/message", json!({"actor":"program","turn":3,"message":{"role":"assistant","content":"程序已完成。"}}));
        event(&db, 26, "n0", "user/message", json!({"turn":4,"message":{"content":"thanks"}}));
        let view = session_programs(&db, "t").unwrap();
        let turns = view["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 4);
        assert_eq!((turns[3]["request"]["text"].as_str(), turns[3]["programs"].as_array().map(Vec::len)), (Some("thanks"), Some(0)), "a turn without programs is still context");
        let first = &turns[0];
        assert_eq!(first["request"]["text"], "make 10 slides", "the page's selection is not the user's words");
        assert_eq!(first["request"]["page_selection"], "{\"page_indexes\":[]}");
        assert_eq!(first["request"]["attachments"][0]["name"], "guide.md");
        assert_eq!(first["reply"]["text"], "Ten slides are saved.");
        let runs = first["programs"].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0]["status"].as_str(), runs[0]["rewritten_by"].as_i64()), (Some("rewritten"), Some(13)));
        assert_eq!(runs[0]["error"]["message"], "page 2 failed");
        assert_eq!(runs[0]["error"]["line"], 4);
        assert_eq!(runs[0]["output"], "PAGE 1 ok");
        assert_eq!(runs[0]["note"], "I will read the template first.");
        assert_eq!(runs[0]["rejected"][0]["source"], "broken(");
        assert_eq!(runs[0]["rejected"][0]["diagnostics"][0]["message"], "unexpected end");
        assert_eq!(runs[0]["calls"], json!([{"name":"ppt__get_plan","kind":"tool","count":1},{"name":"designPage","kind":"virtual","count":2}]));
        assert_eq!((runs[1]["status"].as_str(), runs[1]["rewrite_of"].as_i64()), (Some("completed"), Some(7)));
        assert_eq!(runs[1]["ended_at"], 14_000);
        assert_eq!(runs[1]["note"], Value::Null, "only the reply that handed a program over describes it");
        let stopped = &turns[1]["programs"][0];
        assert_eq!((stopped["status"].as_str(), stopped["output"].as_str()), (Some("failed"), Some("程序在第 1 行出错停止：export failed")));
        assert!(turns[1]["reply"].is_null(), "the program's outcome is not the AI's reply");
        let legacy = turns[2]["programs"].as_array().unwrap();
        assert_eq!(legacy.len(), 1, "a function's own program is not a main program");
        assert_eq!(legacy[0]["status"], "completed");
        drop(db); let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_draft_shows_what_its_conversation_changed_and_keeps_that_conversation() {
        let root = std::env::temp_dir().join(format!("workflow-files-{}", agent_service::uuid_like()));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let db = agent_service::open_db(&root).unwrap();
        db.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at,provider_name) VALUES ('t','做 10 页 PPT\nmore','m','completed',1,1,'p')", []).unwrap();
        db.execute("INSERT INTO plugin_sessions(task_id,plugin_id) VALUES ('t','ppt-master')", []).unwrap();
        event(&db, 1, "n0", "user/message", json!({"turn":1,"message":{"content":"做 10 页 PPT"}}));
        event(&db, 2, "a", "program/start", json!({"turn":1,"function":null,"source":"one();"}));
        event(&db, 3, "a", "program/result", json!({"turn":1,"output":"程序已完成。"}));
        event(&db, 4, "a", "program/start", json!({"turn":1,"function":null,"source":"two();"}));
        event(&db, 5, "a", "program/result", json!({"turn":1,"output":"程序已完成。"}));
        let mut manifest = create_draft(&root, "t").unwrap();
        let id = manifest["id"].as_str().unwrap().to_owned();
        let dir = folder(&root, &id).unwrap();
        let notes = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(notes.contains("做 10 页 PPT") && notes.contains("`programs/program-4.js`：第 1 轮第 2 段，已完成") && notes.contains("read_program_guide"));
        assert_eq!(manifest["programs"][1]["status"], "completed");
        let chat = open_chat(&root, &dir, &mut manifest).unwrap();
        fs::write(dir.join("programs/program-2.js"), "one(1);").unwrap();
        fs::write(dir.join("entry.js"), "export function make() {}").unwrap();
        fs::remove_file(dir.join("programs/program-4.js")).unwrap();
        let view = draft_view(&root, &id).unwrap();
        assert_eq!(view["draft"]["chat_task"], chat.as_str());
        let files = view["files"].as_array().unwrap();
        let status = |path: &str| files.iter().find(|file| file["path"] == path).map(|file| file["status"].as_str().unwrap().to_owned());
        assert_eq!(status("programs/program-2.js").as_deref(), Some("modified"));
        assert_eq!(status("programs/program-4.js").as_deref(), Some("deleted"));
        assert_eq!(status("entry.js").as_deref(), Some("added"));
        assert_eq!(status("AGENTS.md"), None, "the host's notes are not the draft's code");
        assert_eq!(files[0]["original"], "one();");
        let (title, workspace): (String, String) = db.query_row("SELECT title,workspace FROM agent_session_meta WHERE task_id=?1", [&chat], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(title, "流程草稿 · 做 10 页 PPT");
        assert_eq!(workspace, fs::canonicalize(&dir).unwrap().to_string_lossy());
        let listed = list_drafts(&root).unwrap();
        assert_eq!((listed[0]["id"].as_str(), listed[0]["chat_task"].as_str(), listed[0]["programs"].as_u64()), (Some(id.as_str()), Some(chat.as_str()), Some(2)));
        assert_eq!(draft_chat(&root, &id).unwrap()["task_id"], chat.as_str(), "a draft keeps one conversation");
        drop(db); let _ = fs::remove_dir_all(root);
    }
}
