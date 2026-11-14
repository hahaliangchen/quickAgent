use super::*;
use crate::function_frames::{self as tree, Node};

fn node_id<'a>(s: &'a Session<'_>) -> &'a str {
    s.task_node_id.as_deref().unwrap_or(tree::ROOT)
}

/// The task whose context this window holds; the conversation node when none.
pub(super) fn frame_task(s: &Session<'_>, w: &Window) -> String {
    w.task_node_id
        .clone()
        .unwrap_or_else(|| node_id(s).to_owned())
}

/// Main execution at the conversation level, before it created its task.
pub(super) fn no_task(s: &Session<'_>, w: &Window) -> bool {
    s.is_primary() && frame_task(s, w) == tree::ROOT
}

fn task_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(text);
    line.chars().take(120).collect()
}

fn required_text(args: &Value, field: &str) -> Result<String> {
    let text = args[field]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .with_context(|| format!("{field} must be a nonempty string"))?;
    ensure!(
        text.chars().count() <= 16000,
        "{field} exceeds 16000 characters"
    );
    Ok(text.to_owned())
}


/// A message the user wrote: the request that started a turn or an interjection.
pub(super) fn is_user_original(m: &Value) -> bool {
    m["role"] == "user"
        && (m.get("name").is_none() || m["name"] == "user_interjection")
        && m.get(crate::context_assets::REFERENCE).is_none()
        && !m["content"]
            .as_array()
            .is_some_and(|parts| parts.iter().all(|part| part["type"] == "image_ref"))
}

/// A frame is one main request or one function call; it is its own context.
pub(super) fn new_task(
    parent: &Node,
    args: &Value,
    ordinal: usize,
    role: &str,
    turn: usize,
) -> Result<Node> {
    let goal = required_text(args, "goal")?;
    let mut inputs = json!({});
    if role == "temp" && args["program_task"].is_object() {
        inputs["program_task"] = args["program_task"].clone();
    }
    let mut task = tree::planned(
        parent,
        &json!({"title":task_title(&goal),"goal":goal,"reason":args["reason"],"inputs":inputs}),
        ordinal,
        "task",
    )?;
    task.agent_id = parent.agent_id.clone();
    task.briefing = args["briefing"]
        .as_str()
        .map(str::trim)
        .unwrap_or("")
        .to_owned();
    task.metadata = json!({"role":role,"turn":turn});
    // Where the program called this function, for showing the call in its flow.
    if !args["call_site"].is_null() {
        task.metadata["call_site"] = args["call_site"].clone();
    }
    if parent.metadata["parallel"] == true {
        task.metadata["parallel"] = json!(true);
        task.metadata["parallel_owner"] = parent.metadata["parallel_owner"].clone();
    }
    Ok(task)
}



fn record_call(
    tx: &rusqlite::Transaction<'_>,
    task: &str,
    turn: usize,
    step: usize,
    request: &str,
    call: &crate::model_stream::ToolCall,
    node: &str,
) -> Result<i64> {
    record_event(
        tx,
        task,
        "tool/call",
        json!({"actor":"agent","turn":turn,"step":step,"task_node_id":node,"flowNodeId":request,
        "callId":call.call_id,"name":call.name,"arguments":call.arguments,
        "description":function_description(&call.arguments)}),
        None,
    )
}

/// A function call is described by the function's own contract.
fn function_description(arguments: &str) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    args["briefing"].as_str().or(args["goal"].as_str()).unwrap_or("").to_owned()
}

pub(super) fn tool_description(definitions: &[Value], name: &str, arguments: &str) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    args["label"]
        .as_str()
        .filter(|label| !label.trim().is_empty())
        .or_else(|| {
            definitions
                .iter()
                .find(|tool| tool["function"]["name"] == name)
                .and_then(|tool| tool["function"]["description"].as_str())
        })
        .unwrap_or(name)
        .to_owned()
}

/// Copy returned metadata as-is; failure is never interpreted as "not applied".

fn record_event(
    tx: &rusqlite::Transaction<'_>,
    task: &str,
    kind: &str,
    mut data: Value,
    surface: Option<&str>,
) -> Result<i64> {
    if let Some(agent) = data["task_node_id"]
        .as_str()
        .map(|id| tree::get(tx, task, id))
        .transpose()?
        .and_then(|n| n.agent_id)
    {
        data["actor"] = json!("worker");
        let local:i64=tx.query_row("UPDATE agent_delegates SET event_count=event_count+1 WHERE task_id=?1 AND id=?2 RETURNING event_count-1",params![task,agent],|r|r.get(0))?;
        let data = json!({"agent_id":agent,"agent_seq":local,"turn":data["turn"],"task_node_id":data["task_node_id"],"event_type":kind,"event":data});
        return agent_service::append_event_tx(tx, task, "subagent/event", &data, surface);
    }
    agent_service::append_event_tx(tx, task, kind, &data, surface)
}

#[allow(clippy::too_many_arguments)]
fn record_result(
    tx: &rusqlite::Transaction<'_>,
    task: &str,
    turn: usize,
    step: &Value,
    request: &Value,
    call_id: &str,
    node: &str,
    result: &Value,
) -> Result<i64> {
    record_event(
        tx,
        task,
        "tool/result",
        json!({"actor":"agent","turn":turn,"step":step,"task_node_id":node,"flowNodeId":request,
        "message":{"role":"tool","toolCallId":call_id,"isError":result.get("error").is_some(),"content":[{"type":"text","text":result.to_string()}]},
        "meta":{"result":result}}),
        Some("append"),
    )
}

/// Establish the main scope, or consume the exact pending compiled function call.
/// The latter is private to the Runtime; its saved frame is the continuation.
/// Returns the active scope without creating a second task-list scheduler.
pub(super) async fn create_task(
    s: &Session<'_>,
    w: &mut Window,
    call: &crate::model_stream::ToolCall,
    request_id: &str,
    step: usize,
) -> Result<String> {
    let parent = frame_task(s, w);
    ensure!(parent != tree::ROOT, "the host opens the main task for each user request");
    create_function_task(s, w, &parent, call, request_id, step).await
}

/// Each user request is a main task. The host opens it with the request as
/// its goal before the first model request; the AI never creates it.
pub(super) async fn open_main_task(s: &Session<'_>, w: &mut Window, request: &str) -> Result<()> {
    let goal: String = request.trim().chars().take(16000).collect();
    let goal = if goal.is_empty() { "User request".to_owned() } else { goal };
    let (task, turn, scope, mut frame) = (s.task.to_owned(), s.turn, s.scope.clone(), w.clone());
    *w = db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut root = tree::get(&tx, &task, tree::ROOT)?;
        ensure!(root.status == "running", "TASK_STACK_FRAME_MISMATCH: the conversation is not at its top level");
        let ordinal = tree::children(&tx, &task, tree::ROOT)?.len();
        let mut node = new_task(&root, &json!({"goal":goal}), ordinal, "main", turn)?;
        tree::emit(&tx, &task, "task/node_planned", &mut node, turn)?;
        root.status = "suspended".into();
        tree::emit(&tx, &task, "task/node_suspended", &mut root, turn)?;
        node.status = "running".into();
        tree::emit(&tx, &task, "task/node_started", &mut node, turn)?;
        frame.task_node_id = Some(node.node_id);
        write_window(&tx, &task, &scope, &frame)?;
        tx.commit()?;
        Ok(frame)
    })
    .await?;
    Ok(())
}

/// Create an independent window for one asynchronous virtual function.
pub(super) async fn start_parallel_function(
    s: &Session<'_>, parent_frame: &Window, action: &crate::agent_program::Action, step: usize,
) -> Result<(String, Window)> {
    let crate::agent_program::runtime::ActionKind::Call { name, args, captured } = &action.kind else {
        anyhow::bail!("parallel function requires an async virtual call");
    };
    let runtime = parent_frame.program.as_ref().context("parallel caller has no program")?;
    let snapshot = runtime.async_scope(&action.id).context("parallel call has no saved scope")?.clone();
    let function = super::program_flow::scheduled_function_task(s, parent_frame, &runtime.program, name, args, captured)?;
    let arguments = json!({"goal":name,"briefing":function["description"],"program_task":function,"call_site":action.at});
    let parent_id = frame_task(s, parent_frame);
    let conn = agent_service::open_db(s.state.workspace.root())?;
    let parent = tree::get(&conn, s.task, &parent_id)?;
    ensure!(parent.status == "running", "parallel caller is not running");
    let depth = tree::path(&conn, s.task, &parent_id)?.iter().filter(|node| tree::is_temp(node)).count();
    ensure!(depth < tree::MAX_TEMP_TASKS, "TASK_DEPTH_LIMIT");
    let ordinal = tree::children(&conn, s.task, &parent_id)?.len();
    let mut node = new_task(&parent, &arguments, ordinal, "temp", s.turn)?;
    node.metadata["parallel"] = json!(true);
    node.metadata["parallel_owner"] = json!(node.node_id);
    node.metadata["file_write_clock"] = json!(super::managed_files::write_clock(s.state.workspace.root(), s.task)?);
    node.metadata["parallel_call_id"] = json!(action.id);
    // The frame knows its caller: a concurrent call records its result there.
    node.metadata["caller"] = json!({"task":parent_id,"scope":s.scope,"call_id":action.id,"step":step,"flowNodeId":action.id,"concurrent":true});
    let mut inherited = parent_frame.clone();
    inherited.program = None;
    inherited.scope_values.extend(snapshot);
    let mut child = temporary_frame(s, &inherited, &node)?;
    child.parallel_root = Some(node.node_id.clone());
    let child_scope = tree::frame_scope(&node);
    let call = crate::model_stream::ToolCall { call_id:action.id.clone(), name:"create_task".into(), arguments:arguments.to_string() };
    let (task, turn, scope, child_copy, action_id, parent_id) = (s.task.to_owned(), s.turn, child_scope.clone(), child.clone(), action.id.clone(), parent_id);
    db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut node = node;
        record_call(&tx, &task, turn, step, &action_id, &call, &parent_id)?;
        tree::emit(&tx, &task, "task/node_planned", &mut node, turn)?;
        node.status = "running".into();
        tree::emit(&tx, &task, "task/node_started", &mut node, turn)?;
        write_window(&tx, &task, &scope, &child_copy)?;
        tx.commit()?;
        Ok(())
    }).await?;
    Ok((child_scope, child))
}

pub(super) async fn interrupt_parallel_function(s: &Session<'_>, call_id: &str) -> Result<()> {
    let task = s.task.to_owned(); let call_id = call_id.to_owned(); let turn = s.turn;
    let interrupted = db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let nodes = tree::nodes(&tx, &task)?;
        let mut interrupted = Vec::new();
        if let Some(root) = nodes.iter().find(|node| node.metadata["parallel_call_id"] == call_id) {
            let root_id = root.node_id.clone();
            for mut node in nodes {
                if matches!(node.status.as_str(), "running" | "suspended" | "current")
                    && tree::path(&tx, &task, &node.node_id)?.iter().any(|ancestor| ancestor.node_id == root_id)
                {
                    node.status = "interrupted".into();
                    interrupted.push(node.node_id.clone());
                    node.result = Some(json!({"message":"execution interrupted; effects were not replayed"}));
                    tree::emit(&tx, &task, "task/node_returned", &mut node, turn)?;
                }
            }
        }
        tx.commit()?;
        Ok(interrupted)
    }).await?;
    for owner in interrupted {
        crate::shell_process::cleanup_frame(s.state.workspace.root(), s.task, &owner).await;
    }
    Ok(())
}

/// Enter exactly one compiled function. The suspended Runtime is the only
/// continuation; there is no parallel task-list/chain scheduler.
async fn create_function_task(
    s: &Session<'_>,
    w: &mut Window,
    parent_id: &str,
    call: &crate::model_stream::ToolCall,
    request_id: &str,
    step: usize,
) -> Result<String> {
    let prepared = (|| -> Result<Node> {
        let args: Value = serde_json::from_str(&call.arguments)?;
        let pending = w
            .program
            .as_ref()
            .and_then(|p| p.pending.as_ref())
            .context("function call requires a suspended flow program")?;
        ensure!(
            pending.id == call.call_id && args["program_task"].is_object(),
            "function calls are created only by the flow runtime"
        );
        let conn = agent_service::open_db(s.state.workspace.root())?;
        let parent = tree::get(&conn, s.task, parent_id)?;
        ensure!(
            tree::is_task(&parent)
                && parent.status == "running"
                && parent.agent_id.as_deref() == s.delegate_id(),
            "TASK_STACK_FRAME_MISMATCH"
        );
        let depth = tree::path(&conn, s.task, &parent.node_id)?
            .iter()
            .filter(|n| tree::is_temp(n))
            .count();
        ensure!(depth < tree::MAX_TEMP_TASKS, "TASK_DEPTH_LIMIT");
        let owner =
            parent.clone();
        let mut node = new_task(
            &owner,
            &args,
            tree::children(&conn, s.task, &owner.node_id)?.len(),
            "temp",
            s.turn,
        )?;
        // The frame knows its caller: returning resumes the caller's window.
        node.metadata["caller"] = json!({"task":parent.node_id,"scope":s.scope,"call_id":call.call_id,"step":step,"flowNodeId":request_id});
        Ok(node)
    })();
    let prepared = match prepared {
        Ok(node) => temporary_frame(s, w, &node).map(|frame| (node, frame)),
        Err(error) => Err(error),
    };
    let (task, turn, scope, parent_frame, call, request_id, parent_id) = (
        s.task.to_owned(),
        s.turn,
        s.scope.clone(),
        w.clone(),
        call.clone(),
        request_id.to_owned(),
        parent_id.to_owned(),
    );
    let (scope, frame) = db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        record_call(&tx, &task, turn, step, &request_id, &call, &parent_id)?;
        let switched = match prepared {
            Ok((mut node, frame)) => {
                tree::emit(&tx, &task, "task/node_planned", &mut node, turn)?;
                let mut parent = tree::get(&tx, &task, &parent_id)?;
                parent.status = "suspended".into();
                tree::emit(&tx, &task, "task/node_suspended", &mut parent, turn)?;
                node.status = "running".into();
                tree::emit(&tx, &task, "task/node_started", &mut node, turn)?;
                write_window(&tx, &task, &scope, &parent_frame)?;
                let child_scope = tree::frame_scope(&node);
                write_window(&tx, &task, &child_scope, &frame)?;
                (child_scope, frame)
            }
            Err(error) => {
                let result = json!({"error":format!("{error:#}")});
                // The program reads the failure from the recorded result.
                record_result(&tx, &task, turn, &json!(step), &json!(request_id), &call.call_id, &parent_id, &result)?;
                write_window(&tx, &task, &scope, &parent_frame)?;
                (scope, parent_frame)
            }
        };
        tx.commit()?;
        Ok(switched)
    }).await?;
    *w = frame;
    Ok(scope)
}

/// Function arguments are values, never reconstructed historical tool packets.
fn temporary_frame(s: &Session<'_>, parent: &Window, node: &Node) -> Result<Window> {
    let mut child = inherited_frame(s, parent, node);
    let task = node.inputs["program_task"]
        .as_object()
        .context("compiled function descriptor required")?;
    let mut task = Value::Object(task.clone());
    if task["module"] == true && !parent.module_revisions.is_empty() {
        let contract = super::modules::contract(&task)?;
        if let Some(revision) = parent.module_revisions.get(&contract.key) {
            task["revision"] = json!(revision);
        }
    }
    let input = super::program_flow::call_view(&task)?;
    let virtual_function = task["role"] != "body";
    if virtual_function {
        // A virtual function's context is its signature, contract and actual
        // arguments: nothing of its caller's conversation, scope or skills
        // comes along. The skills its own code or contract references are
        // added to each of its requests.
        child.messages = vec![json!({"role":"system","content":s.prompt()})];
        child.scope_values.clear();
        // Except what its contract names with @uses: its closure, as in JS.
        if let Some(captured) = task["captured"].as_object() {
            child.scope_values.extend(captured.iter().map(|(name, value)| (name.clone(), value.clone())));
        }
    }
    child.messages.push(json!({"role":"user","name":"program_task_input","content":input.to_string(),"_frame_input":true}));
    child.program = super::program_flow::task_runtime(&task)?;
    child.program_task = Some(task);

    let mut session = s.clone();
    session.scope = tree::frame_scope(node);
    fit_input_bodies(&session, &mut child)?;
    Ok(child)
}

fn inherited_frame(s: &Session<'_>, parent: &Window, node: &Node) -> Window {
    let mut messages = vec![json!({"role":"system","content":s.prompt()})];
    messages.extend(
        parent
            .messages
            .iter()
            .filter(|m| is_user_original(m))
            .cloned(),
    );
    Window {
        scope_values: super::function_scope::values(parent),
        messages,
        task_node_id: Some(node.node_id.clone()),
        token_calibration: parent.token_calibration.clone(),
        parallel_root: parent.parallel_root.clone(),
        ..Default::default()
    }
}

/// What a returning task saved, as handles: the latest result per invocation
/// in its scope, without the materials it was given or inherited.
fn returned_assets(s: &Session<'_>, w: &Window, node: &str) -> Value {
    let given = w
        .messages
        .iter()
        .filter(|m| m["_frame_input"] == true || m["_pinned"] == true)
        .filter_map(|m| m[crate::context_assets::REFERENCE]["asset_id"].as_str())
        .collect::<BTreeSet<_>>();
    let Ok(index) =
        crate::context_assets::frame_index(s.state.workspace.root(), s.task, node, &w.messages)
    else {
        return json!([]);
    };
    json!(
        index["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a["asset_id"].as_str().is_some_and(|id| !given.contains(id)))
            .map(|a| {
                let mut a = a.clone();
                a.as_object_mut().unwrap().remove("in_context");
                a
            })
            .collect::<Vec<_>>()
    )
}

/// A no-tool response reaches the finish node of
/// the task in view, and that reply is the task's summary. A temporary task's
/// finish resumes its suspended Runtime: the internal function call returns
/// the summary and the waiting task continues. A main task's summary is the
/// answer to the user. Returns the context to continue in; None ends the turn.
pub(super) async fn finish(
    s: &Session<'_>,
    w: &mut Window,
    message: &Value,
) -> Result<Option<(String, Window)>> {
    let id = frame_task(s, w);
    if id == tree::ROOT {
        return Ok(None);
    }
    // Calls waiting for this frame to write or fix a module may go on.
    super::modules::release_frame(w);
    let agent = s.delegate_id().map(str::to_owned);
    let saved = returned_assets(s, w, &id);
    let text = message["content"].as_str().unwrap_or("").to_owned();
    let (task, turn, old_scope, frame) = (s.task.to_owned(), s.turn, s.scope.clone(), w.clone());
    let resumed=db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut node = tree::get(&tx, &task, &id)?;
        if !tree::is_task(&node) || node.status != "running" {
            return Ok(None);
        }
        ensure!(node.agent_id==agent,"TASK_EXECUTION_FRAME_MISMATCH");
        let summary = json!({"result":text,"assets":saved});
        // A returning frame finds its caller on itself.
        let caller = node.metadata["caller"].clone();
        let (serial_caller, concurrent_caller) = if caller["concurrent"] == true {
            (Value::Null, caller)
        } else {
            (caller, Value::Null)
        };
        // A function that gave up returns $task_error: its frame failed.
        let failed = serde_json::from_str::<Value>(&text).is_ok_and(|value| value.get("$task_error").is_some());
        node.status = if failed { "failed" } else { "done" }.into();
        node.result = Some(summary);
        let returned = tree::emit(&tx, &task, "task/node_returned", &mut node, turn)?;
        if concurrent_caller.is_object() {
            let parent = concurrent_caller["task"].as_str().context("concurrent caller missing")?;
            let call_id = concurrent_caller["call_id"].as_str().context("concurrent call id missing")?;
            let answer = json!({"task":node.title,"node_id":node.node_id,"return_event_seq":returned,"result":text,"assets":saved});
            record_result(&tx, &task, turn, &concurrent_caller["step"], &concurrent_caller["flowNodeId"], call_id, parent, &answer)?;
            let mut frame = frame;
            super::function_scope::release_finished(&mut frame);
            write_window(&tx, &task, &old_scope, &frame)?;
            tx.commit()?;
            return Ok(None);
        }
        if serial_caller.is_null() && node.agent_id.is_some() {
            let mut frame=frame;
            super::function_scope::release_finished(&mut frame);
            write_window(&tx, &task, &old_scope, &frame)?;
            tx.commit()?;
            return Ok(None);
        }
        if !tree::is_temp(&node) || serial_caller.is_null() {
            // A main task: the conversation is back at its top level.
            let mut root = tree::get(&tx, &task, tree::ROOT)?;
            if root.status == "suspended" {
                root.status = "running".into();
                tree::emit(&tx, &task, "task/node_resumed", &mut root, turn)?;
            }
            let mut frame = frame;
            frame.task_node_id = Some(tree::ROOT.into());
                super::function_scope::release_finished(&mut frame);
            write_window(&tx, &task, &old_scope, &frame)?;
            tx.commit()?;
            return Ok(None);
        }
        let mut parent = tree::get(&tx, &task, serial_caller["task"].as_str().context("returning frame has no caller")?)?;
        ensure!(parent.agent_id==agent,"TASK_CALLER_AGENT_MISMATCH");
        let scope = serial_caller["scope"].as_str().context("caller scope missing")?.to_owned();
        let call_id = serial_caller["call_id"].as_str().context("caller call missing")?.to_owned();
        let raw: String = tx
            .query_row("SELECT state FROM agent_runtime_windows WHERE task_id=?1 AND scope=?2", params![task, scope], |r| r.get(0))
            .context("waiting task frame missing")?;
        let mut resumed: Window = serde_json::from_str(&raw)?;
        ensure!(resumed.task_node_id.as_deref()==Some(parent.node_id.as_str()),"TASK_CALLER_FRAME_MISMATCH");
        let answer = json!({"task":node.title,"node_id":node.node_id,"return_event_seq":returned,"result":text,"assets":saved});
        // The caller's program reads the return from the recorded result.
        record_result(&tx, &task, turn, &serial_caller["step"], &serial_caller["flowNodeId"], &call_id, &parent.node_id, &answer)?;
        // What the user said meanwhile belongs to the whole main task.
        for original in frame.messages.iter().filter(|m| is_user_original(m)) {
            if !resumed.messages.iter().any(|m| m["source"]["seq"] == original["source"]["seq"] && m["content"] == original["content"]) {
                resumed.messages.push(original.clone());
            }
        }
        parent.status = "running".into();
        tree::emit(&tx, &task, "task/node_resumed", &mut parent, turn)?;
        write_window(&tx, &task, &scope, &resumed)?;
        tx.execute("DELETE FROM agent_runtime_windows WHERE task_id=?1 AND scope=?2", params![task, old_scope])?;
        tx.commit()?;
        Ok(Some((scope, resumed)))
    })
    .await?;
    if resumed.is_none() {
        super::function_scope::release_finished(w);
    }
    Ok(resumed)
}

/// How much of the earlier chat a new turn carries, in characters. Older
/// turns beyond it leave the window whole; their files stay listed.
/// A placeholder value, to be set from real chats.
const CHAT_CHARS: usize = 40_000;
/// How much of the latest turn's files a new turn carries in full.
const LATEST_FILES_CHARS: usize = 24_000;
/// Longer than this, one file keeps its head and tail.
const FILE_CHARS: usize = 12_000;

/// A message the host builds for a new turn from the earlier chat.
pub(super) const CHAT_HISTORY: &str = "chat_history";
pub(super) const CHAT_FILES: &str = "chat_files";

/// A new turn refreshes only the main AI's direct working loop.
/// A saved VM or called function still owns its executable continuation.
pub(super) fn can_refresh_main_window(node: &tree::Node, saved: Option<&Window>) -> bool {
    node.metadata["role"] == "main" && node.agent_id.is_none()
        && saved.is_some_and(|w| w.program.is_none() && w.program_task.is_none()
            && w.resumable.is_none() && w.resumed.is_none()
            && w.parallel_root.is_none() && !w.module_probe)
}

/// What a new turn knows of the earlier ones (docs/agent-design-guide.md §4):
/// the chat itself, the user's words and the replies the user saw, as they
/// were; and the chat folder as a code project, its program files and call
/// records listed by turn, the latest turn's files in full. The work's own
/// tool exchanges and outputs stay in the files and the history.
pub(super) fn chat_context(conn: &rusqlite::Connection, task: &str, turn: usize) -> Result<Vec<Value>> {
    let turn = turn as i64;
    // Each turn's words: what the user said, and the last reply the main AI
    // gave without calling anything.
    let rows = conn
        .prepare(&format!(
            "SELECT e.seq,e.kind,json_extract(e.data,'$.turn'),json_extract(e.data,'$.message.content'),
                    json_type(e.data,'$.message.tool_calls')='array' AND json_array_length(e.data,'$.message.tool_calls')>0
             FROM agent_task_events e
             LEFT JOIN agent_task_nodes n ON n.task_id=e.task_id AND n.node_id=COALESCE(e.node_id,json_extract(e.data,'$.task_node_id'))
             WHERE e.task_id=?1 AND e.kind IN ('user/message','assistant/message') AND json_extract(e.data,'$.turn')<?2
               AND (COALESCE(e.node_id,json_extract(e.data,'$.task_node_id')) IS NULL OR n.node_id='{root}' OR n.parent_id='{root}')
             ORDER BY e.seq",
            root = tree::ROOT
        ))?
        .query_map(params![task, turn], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<bool>>(4)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut turns = BTreeMap::<i64, (Vec<String>, Option<String>)>::new();
    for (_, kind, number, content, calls) in rows {
        let (Some(number), Some(content)) = (number, content) else { continue };
        let text = crate::session_history::stored_text(&content).trim().to_owned();
        if text.is_empty() {
            continue;
        }
        let entry = turns.entry(number).or_default();
        if kind == "user/message" {
            entry.0.push(text);
        } else if calls != Some(true) {
            entry.1 = Some(text);
        }
    }
    // A turn whose answer was a program's outcome has it on its task.
    for node in tree::children(conn, task, tree::ROOT)? {
        let (Some(returned), Some(result)) = (node.returned_seq, node.result.as_ref().and_then(|r| r["result"].as_str())) else { continue };
        let number: Option<i64> = conn
            .query_row("SELECT json_extract(data,'$.turn') FROM agent_task_events WHERE task_id=?1 AND seq<=?2 AND json_type(data,'$.turn')='integer' ORDER BY seq DESC LIMIT 1",
                params![task, returned], |r| r.get(0))
            .optional()?
            .flatten();
        if let Some(entry) = number.filter(|number| *number < turn).and_then(|number| turns.get_mut(&number))
            && entry.1.is_none()
            && !result.trim().is_empty()
        {
            entry.1 = Some(result.trim().to_owned());
        }
    }
    // From the first turn on, the chat accumulates; the turn that would take
    // it past CHAT_CHARS starts it again, and everything before leaves whole,
    // never summarized. Until the next restart the chat begins at the same
    // turn, so a provider's prompt cache keeps it: dropping the oldest turn
    // each time would change its beginning every turn (2026-11-13 review).
    // The restart keeps that turn, the one the next request most likely
    // refers to. Computed afresh each turn, the same after a rewind.
    let mut kept = Vec::new();
    let mut used = 0;
    for (number, (asked, reply)) in &turns {
        let size = asked.iter().map(|text| text.chars().count()).sum::<usize>() + reply.as_ref().map_or(0, |text| text.chars().count());
        if !kept.is_empty() && used + size > CHAT_CHARS {
            kept.clear();
            used = 0;
        }
        used += size;
        kept.push((*number, asked, reply));
    }
    let left_out = turns.len() - kept.len();
    let mut messages = Vec::new();
    for (_, asked, reply) in &kept {
        for text in *asked {
            messages.push(json!({"role":"user","name":CHAT_HISTORY,"content":text}));
        }
        if let Some(reply) = reply {
            messages.push(json!({"role":"assistant","content":reply}));
        }
    }
    if let Some(files) = chat_files(conn, task, turn, left_out)? {
        messages.push(json!({"role":"user","name":CHAT_FILES,"content":files,"_frame_input":true}));
    }
    Ok(messages)
}

/// The chat folder as a project: its files by turn, then the latest turn's
/// files in full.
fn chat_files(conn: &rusqlite::Connection, task: &str, turn: i64, left_out: usize) -> Result<Option<String>> {
    use crate::session_history::turn_record;
    let programs = conn
        .prepare("SELECT seq,json_extract(data,'$.path'),json_extract(data,'$.turn'),json_extract(data,'$.source') FROM agent_task_events
            WHERE task_id=?1 AND kind='program/start' AND json_extract(data,'$.function') IS NULL
              AND json_type(data,'$.path')='text' AND json_type(data,'$.source')='text' AND json_extract(data,'$.turn')<?2 ORDER BY seq")?
        .query_map(params![task, turn], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut files = Vec::<(i64, i64, String, String, Option<String>)>::new();
    for (seq, path, number, source) in programs {
        let file = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_owned();
        let ended: Option<String> = conn
            .query_row("SELECT json_extract(data,'$.ended') FROM agent_task_events WHERE task_id=?1 AND kind='program/end'
                AND json_extract(data,'$.program')=?2 AND seq>?3 ORDER BY seq LIMIT 1", params![task, file, seq], |r| r.get(0))
            .optional()?
            .flatten();
        let note = format!("主程序，{} 行，{}", source.lines().count(), ended.as_deref().unwrap_or("没有结束"));
        files.push((number, seq, file, note, Some(source)));
    }
    for record in turn_record::records(conn, task, None)? {
        if record.turn < turn {
            files.push((record.turn, record.first_seq, record.file, format!("直接调用记录，{} 次调用", record.calls), None));
        }
    }
    if files.is_empty() {
        return Ok(None);
    }
    files.sort_by_key(|(number, seq, ..)| (*number, *seq));
    let width = files.iter().map(|(_, _, file, ..)| file.chars().count()).max().unwrap_or(0);
    // Where names are not sent (the Messages wire) this joins the user's
    // next words, so it says itself whose it is.
    let mut text = String::from(
        "【宿主提供的材料，不是用户的话】\n这个聊天的文件夹，就是一个代码项目：每段主程序是一个文件；没写程序时主 AI 直接调用的方法，宿主按顺序记成 .calls.js，每行是一次调用，行尾注释是结果。模块在 modules/ 下，契约见 chat_modules。调用记录里的退出码和末行只是线索；有 event_seq 时用 read_session_history({event_seq:编号}) 读取当时完整输入和结果。打开文件：read_session_history({programs:true, program:\"<文件名>\"})，可加 start_line、end_line；主程序加 output:true 读它当时的输出。\n",
    );
    if left_out > 0 {
        text.push_str(&format!("最早 {left_out} 轮的对话不在上面，它们的文件仍列在这里。\n"));
    }
    let mut current = None;
    for (number, _, file, note, _) in &files {
        if current != Some(*number) {
            text.push_str(&format!("\n第 {number} 轮\n"));
            current = Some(*number);
        }
        text.push_str(&format!("  {file:<width$}  {note}\n"));
    }
    let latest = files.last().map(|(number, ..)| *number).unwrap_or(0);
    text.push_str(&format!("\n第 {latest} 轮的文件全文：\n"));
    let mut room = LATEST_FILES_CHARS;
    for (number, _, file, _, source) in files.iter().filter(|(number, ..)| *number == latest) {
        let source = match source {
            Some(source) => source.clone(),
            None => turn_record::render(conn, task, *number, None)?.unwrap_or_default(),
        };
        let chars = source.chars().collect::<Vec<_>>();
        let limit = FILE_CHARS.min(room);
        let shown = if chars.len() <= limit {
            source.clone()
        } else if file.ends_with(turn_record::CALLS) {
            // Keep execution evidence and changes before exploratory reads.
            turn_record::condense(&source, limit)
        } else {
            let half = limit / 2;
            format!(
                "{}\n/* … 中间 {} 字没放进来，用 start_line、end_line 打开 … */\n{}",
                chars[..half].iter().collect::<String>(),
                chars.len() - 2 * half,
                chars[chars.len() - half..].iter().collect::<String>()
            )
        };
        room = room.saturating_sub(shown.chars().count());
        text.push_str(&format!("\n==== {file} ====\n{}\n", shown.trim_end()));
        if room == 0 {
            break;
        }
    }
    text.push_str("【宿主材料结束】");
    Ok(Some(text))
}

/// Each user request at the conversation level starts a clean main context:
/// the earlier chat and the chat's files (`chat_context`) plus the skills
/// still loaded. Earlier working records stay in the files and history.
pub(super) async fn fresh_main_window(
    s: &Session<'_>,
    previous: Option<&Window>,
    earlier: Vec<Value>,
) -> Result<Window> {
    let mut window = Window {
        task_node_id: Some(tree::ROOT.into()),
        ..Default::default()
    };
    window
        .messages
        .push(json!({"role":"system","content":MAIN_PROMPT}));
    window.messages.extend(earlier);
    let Some(previous) = previous else {
        return Ok(window);
    };
    window.active_skills = previous.active_skills.clone();
    window.active_skill_scopes = previous.active_skill_scopes.clone();
    window.token_calibration = previous.token_calibration.clone();
    window.console = previous.console.clone();
    window.console_cell = previous.console_cell;
    window.console_log_start = 0;
    for id in
        crate::context_assets::active_instruction_ids(&previous.messages, &previous.active_skills)
    {
        let loaded = previous.messages.iter().rev().find(|m| {
            m[crate::context_assets::REFERENCE]["asset_id"] == id
                && !matches!(
                    m["_asset_state"].as_str(),
                    Some("reference_only" | "not_delivered")
                )
        });
        let mut message = if let Some(message) = loaded {
            let mut message = message.clone();
            message["role"] = json!("user");
            message["name"] = json!("saved_asset");
            message.as_object_mut().unwrap().remove("tool_call_id");
            message
        } else {
            match load_saved_asset(s, &json!({"asset_id":id})).await {
                Ok(result) => crate::context_assets::tagged(
                    json!({"role":"user","name":"saved_asset","content":result.to_string()}),
                    &result["asset_reference"],
                ),
                Err(_) => continue,
            }
        };
        message["_pinned"] = json!(true);
        window.messages.push(message);
    }
    Ok(window)
}

fn fit_input_bodies(s: &Session<'_>, child: &mut Window) -> Result<()> {
    let definitions = tools(s, child);
    let mut bodies = Vec::new();
    for (index, message) in child.messages.iter_mut().enumerate() {
        if message["_frame_input"] != true
            || message["_pinned"] == true
            || crate::context_assets::complete_body(message).is_none()
        {
            continue;
        }
        let body = message.clone();
        let delivery: Value = serde_json::from_str(message["content"].as_str().unwrap_or(""))?;
        let total_chars = delivery["total_chars"].as_u64().unwrap_or_else(|| {
            delivery
                .get("original_result")
                .unwrap_or(&delivery)
                .to_string()
                .chars()
                .count() as u64
        });
        crate::context_assets::reference_only(message);
        let mut receipt: Value = serde_json::from_str(message["content"].as_str().unwrap())?;
        receipt["total_chars"] = json!(total_chars);
        receipt["delivered"] = json!(false);
        receipt["next_read"] = json!({"asset_id":message[crate::context_assets::REFERENCE]["asset_id"],"char_offset":0,"max_chars":PAGE_CHARS});
        receipt["instruction"] = json!(
            "The complete body did not fit alongside the other task inputs. Read explicit pages as needed; return only the result required by the caller."
        );
        message["content"] = json!(receipt.to_string());
        bodies.push((index, body));
    }
    let fits = |child: &Window| {
        child
            .input_tokens(s, &request(s, &child.messages, &definitions))
            .saturating_add(4096)
            < s.input_budget
    };
    ensure!(
        fits(child),
        "TASK_INPUTS_OVER_CAPACITY: current arguments and fixed task inputs exceed the model context budget; parent frame is preserved"
    );
    for (index, body) in bodies {
        let reference = std::mem::replace(&mut child.messages[index], body);
        if !fits(child) {
            child.messages[index] = reference;
        }
    }
    Ok(())
}


pub(super) fn write_window(tx: &rusqlite::Transaction<'_>, task: &str, scope: &str, w: &Window) -> Result<()> {
    tx.execute("INSERT INTO agent_runtime_windows VALUES (?1,?2,?3) ON CONFLICT(task_id,scope) DO UPDATE SET state=excluded.state",params![task,scope,serde_json::to_string(w)?])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};


    struct Harness {
        root: std::path::PathBuf,
        state: AgentServiceState,
        cancel: CancellationToken,
        w: Window,
        scope: String,
    }
    impl Harness {
        async fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("task-frames-{}", agent_service::uuid_like()));
            std::fs::create_dir_all(&root).unwrap();
            let state = agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
            let mut conn = agent_service::open_db(&root).unwrap();
            conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','','fake','running',0,0)",[]).unwrap();
            let tx = conn.transaction().unwrap();
            tree::ensure_root(&tx, "t", 1).unwrap();
            tx.commit().unwrap();
            let mut harness = Self {
                root,
                state,
                cancel: CancellationToken::new(),
                w: Window {
                    messages: vec![json!({"role":"system","content":MAIN_PROMPT})],
                    ..Default::default()
                },
                scope: "main".into(),
            };
            harness.persist().await;
            harness
        }
        fn session(&self) -> Session<'_> {
            Session {
                agent_id: None,
                task_node_id: None,
                plugin: None,
                state: &self.state,
                task: "t",
                model: "fake",
                turn: 1,
                cancel: &self.cancel,
                ban_execution: false,
                scope: self.scope.clone(),
                parent: None,
                input_budget: INPUT_BUDGET,
            }
        }
        async fn persist(&mut self) {
            save(&self.session(), &self.w).await.unwrap();
        }
        async fn user(&mut self, text: &str) -> i64 {
            let seq = append(
                &self.session(),
                "user/message",
                json!({"turn":1,"message":{"role":"user","content":text}}),
                true,
            )
            .await
            .unwrap();
            self.w
                .messages
                .push(sourced(json!({"role":"user","content":text}), "t", seq));
            self.persist().await;
            seq
        }
    }

    type Requests = Arc<Mutex<Vec<Value>>>;

    /// A scripted model: `reply(n, body)` answers the n-th request.
    fn completion_request(body: &Value) -> bool {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| {
                m["role"] == "assistant"
                    || m["role"] == "tool"
                    || m["name"] == "task_completion_check"
                    || m["name"] == "user_interjection"
                    || (m["role"] == "user" && m.get("name").is_none())
            })
            .is_some_and(|m| m["name"] == "task_completion_check")
    }

    async fn model(
        reply: impl Fn(usize, &Value) -> Value + Send + Sync + 'static,
    ) -> (std::net::SocketAddr, Requests, tokio::task::JoinHandle<()>) {
        let index = std::sync::atomic::AtomicUsize::new(0);
        raw_model(move |_, body| {
            let messages = body["messages"].as_array().unwrap();
            if completion_request(body) {
                return answer(
                    messages
                        .iter()
                        .rev()
                        .find(|m| m["role"] == "assistant")
                        .unwrap()["content"]
                        .as_str()
                        .unwrap(),
                );
            }
            reply(
                index.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                body,
            )
        })
        .await
    }

    async fn raw_model(
        reply: impl Fn(usize, &Value) -> Value + Send + Sync + 'static,
    ) -> (std::net::SocketAddr, Requests, tokio::task::JoinHandle<()>) {
        use axum::{Json, Router, routing::post};
        let seen: Requests = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let reply = Arc::new(reply);
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let (log, reply) = (log.clone(), reply.clone());
            async move {
                let n = {
                    let mut seen = log.lock().unwrap();
                    seen.push(body.clone());
                    seen.len()-1
                };
                let message = reply(n, &body);
                Json(json!({"choices":[{"finish_reason":if message["tool_calls"].is_array(){"tool_calls"}else{"stop"},"message":message}]}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (address, seen, server)
    }

    fn call(id: &str, name: &str, args: Value) -> Value {
        json!({"role":"assistant","tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
    }

    fn answer(content: &str) -> Value {
        json!({"role":"assistant","content":content})
    }






    #[tokio::test]
    async fn a_plain_answer_needs_no_task() {
        let (address, seen, server) = model(|n, _| match n {
            0 => answer("4"),
            _ => panic!("one answer ends the turn"),
        })
        .await;
        let mut h = Harness::new().await;
        h.state = agent_service::tests::test_state(&h.root, address);
        h.user("What is 2+2?").await;
        let mut window = h.w.clone();
        assert!(matches!(
            run_loop(&h.session(), &mut window, 3, true).await.unwrap(),
            LoopResult::Answer
        ));
        server.abort();
        assert_eq!(seen.lock().unwrap().len(), 1);
        let nodes = tree::nodes(&agent_service::open_db(&h.root).unwrap(), "t").unwrap();
        assert_eq!(nodes.len(), 1, "no task was created");
        assert_eq!(nodes[0].status, "running");
    }



    /// 烧水热菜 on the real loop. The main task's steps share its context; the
    /// broken pot is a temporary task under the switch-on step with its own
    /// context. Its summary is the result of the create_task call that started
    /// it, and the step continues.




    /// A direct request keeps its goal and evidence, while a new user turn
    /// gets a fresh decision context instead of the old model's live plan.
    #[tokio::test]
    async fn a_stopped_direct_request_starts_a_fresh_window_with_chat_and_calls() {
        let (address, seen, server) = model(|n, body| {
            let messages = body["messages"].as_array().unwrap();
            match n {
                0 => {
                    let mut reply = call("evidence", "read_file", json!({"path":"evidence.log"}));
                    reply["reasoning_content"] = json!("OLD_PLAN: rebuild all four snapshots next");
                    reply
                }
                1 => {
                    assert!(messages.iter().any(|m| m["name"] == CHAT_HISTORY && m["content"] == "Investigate the failures"));
                    assert!(messages.iter().any(|m| m["name"] == CHAT_FILES && m["content"].as_str().unwrap().contains("read_session_history")), "completed results remain recoverable");
                    assert!(!messages.iter().any(|m| m["role"] == "tool" || m["tool_calls"].is_array()), "old native tool exchanges are history, not a live continuation");
                    assert!(!body.to_string().contains("OLD_PLAN"), "old private reasoning must not choose the next plan");
                    assert!(messages.iter().any(|m| m["role"] == "user" && m.get("name").is_none() && m["content"] == "continue"));
                    answer("The recorded evidence is enough; no rerun.")
                }
                2 => {
                    assert!(!messages.iter().any(|m| m["name"] == "request_handoff"));
                    assert!(messages.iter().any(|m| m["content"].as_str().unwrap_or("").contains("no rerun")));
                    answer("Done")
                }
                _ => panic!("unexpected request {n}"),
            }
        }).await;
        let mut h = Harness::new().await;
        std::fs::write(h.root.join("evidence.log"), "two existing test failures").unwrap();
        h.state = agent_service::tests::test_state(&h.root, address);
        run_task(h.state.clone(), "t".into(), "fake".into(), "Investigate the failures".into(), 1, CancellationToken::new(), 1, false).await.unwrap();
        let conn = agent_service::open_db(&h.root).unwrap();
        let request = tree::active(&conn, "t").unwrap().unwrap().node_id;
        run_task(h.state.clone(), "t".into(), "fake".into(), "continue".into(), 4, CancellationToken::new(), 2, false).await.unwrap();
        assert_eq!(tree::get(&conn, "t", &request).unwrap().status, "done", "the original request frame finishes");
        let resets: i64 = conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id='t' AND kind='context/main_reset'", [], |r|r.get(0)).unwrap();
        assert_eq!(resets, 2);
        run_task(h.state.clone(), "t".into(), "fake".into(), "A separate question".into(), 4, CancellationToken::new(), 3, false).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 3);
        server.abort();
    }

    #[tokio::test]
    async fn a_new_turn_preserves_executable_function_state() {
        let h = Harness::new().await;
        let conn = agent_service::open_db(&h.root).unwrap();
        let mut node = tree::get(&conn, "t", tree::ROOT).unwrap();
        node.metadata = json!({"role":"main"});
        let mut w = Window::default();
        assert!(can_refresh_main_window(&node, Some(&w)));
        let s = h.session();
        super::super::program_flow::install_scoped(&s, &mut w, &json!({"source":"console.log(1);"}), false).unwrap();
        assert!(!can_refresh_main_window(&node, Some(&w)), "an actual VM keeps its continuation");
        w.program = None;
        w.resumable = Some("paused-vm".into());
        assert!(!can_refresh_main_window(&node, Some(&w)), "a paused VM retains its caller too");
        w.resumable = None;
        w.program_task = Some(json!({"role":"implement"}));
        assert!(!can_refresh_main_window(&node, Some(&w)));
        w.program_task = None;
        w.parallel_root = Some("child".into());
        assert!(!can_refresh_main_window(&node, Some(&w)));
    }

    /// Not a check: prints what the next turn of a real chat would be given.
    /// GROW_ROOT=<folder holding a copy of .codex-workspace-mcp/codex_state.db> GROW_TASK=<id> GROW_TURN=<n>
    #[tokio::test]
    #[ignore]
    async fn print_chat_context_of_a_real_chat() {
        let (Ok(root), Ok(task), Ok(turn)) = (std::env::var("GROW_ROOT"), std::env::var("GROW_TASK"), std::env::var("GROW_TURN")) else { return };
        let root = std::path::PathBuf::from(root);
        let conn = rusqlite::Connection::open(root.join(".codex-workspace-mcp/codex_state.db")).unwrap();
        for message in chat_context(&conn, &task, turn.parse().unwrap()).unwrap() {
            println!("---- {} {}\n{}", message["role"].as_str().unwrap(), message["name"].as_str().unwrap_or(""), message["content"].as_str().unwrap());
        }
    }

    /// "按你说的 2 和 4 做" only works when the earlier words are there as
    /// they were said. The latest turns that fit come whole; older ones leave
    /// whole, never summarized.
    #[tokio::test]
    async fn a_new_turn_has_the_chat_as_it_was_said() {
        let h = Harness::new().await;
        let mut conn = agent_service::open_db(&h.root).unwrap();
        let tx = conn.transaction().unwrap();
        let long = "长".repeat(CHAT_CHARS / 2);
        for turn in 1..=4 {
            let said = if turn == 1 { long.clone() } else { format!("Question {turn}") };
            agent_service::append_event_tx(&tx, "t", "user/message", &json!({"turn":turn,"message":{"role":"user","content":said}}), None).unwrap();
            agent_service::append_event_tx(&tx, "t", "assistant/message", &json!({"turn":turn,"message":{"role":"assistant","content":"looking","tool_calls":[{"id":"c","type":"function","function":{"name":"list_dir","arguments":"{}"}}]}}), None).unwrap();
            let reply = if turn == 2 { long.clone() } else { format!("1. first\n2. second\n{turn}. answer") };
            agent_service::append_event_tx(&tx, "t", "assistant/message", &json!({"turn":turn,"message":{"role":"assistant","content":reply}}), None).unwrap();
        }
        agent_service::append_event_tx(&tx, "t", "user/message", &json!({"turn":5,"message":{"role":"user","content":[{"type":"text","text":"按你说的 2 和 4 做"}]}}), None).unwrap();
        tx.commit().unwrap();
        let messages = chat_context(&conn, "t", 5).unwrap();
        let chat = messages.iter().map(|m| (m["role"].as_str().unwrap(), m["content"].as_str().unwrap().to_owned())).collect::<Vec<_>>();
        // Turn 1 no longer fits beside turn 2's long reply: it leaves whole.
        assert_eq!(chat, [
            ("user", "Question 2".to_owned()), ("assistant", long.clone()),
            ("user", "Question 3".to_owned()), ("assistant", "1. first\n2. second\n3. answer".to_owned()),
            ("user", "Question 4".to_owned()), ("assistant", "1. first\n2. second\n4. answer".to_owned()),
        ], "words between calls are work, not chat; the current turn is not earlier chat");
        assert!(messages.iter().all(|m| !is_user_original(m)), "earlier words are not this turn's request");
        assert!(messages.iter().all(|m| m.as_object().unwrap().keys().all(|key| !key.starts_with('_'))), "nothing private rides along");
        // The chat restarted at turn 2 and keeps starting there while later
        // turns fit: its beginning, and the provider's cache, stay put.
        let tx = conn.transaction().unwrap();
        agent_service::append_event_tx(&tx, "t", "assistant/message", &json!({"turn":5,"message":{"role":"assistant","content":"Done 2 and 4."}}), None).unwrap();
        tx.commit().unwrap();
        let next = chat_context(&conn, "t", 6).unwrap();
        assert_eq!(next[..6], messages[..6], "the same beginning a turn later");
        assert_eq!(next[7]["content"], "Done 2 and 4.");
        // A turn that takes it past the limit starts it again from itself.
        let tx = conn.transaction().unwrap();
        agent_service::append_event_tx(&tx, "t", "user/message", &json!({"turn":6,"message":{"role":"user","content":long.clone()}}), None).unwrap();
        agent_service::append_event_tx(&tx, "t", "assistant/message", &json!({"turn":6,"message":{"role":"assistant","content":"ok"}}), None).unwrap();
        tx.commit().unwrap();
        let restarted = chat_context(&conn, "t", 7).unwrap();
        assert_eq!(restarted.iter().map(|m| m["content"].as_str().unwrap().to_owned()).collect::<Vec<_>>(), [long.clone(), "ok".to_owned()]);
        // A chat with no files has no file listing.
        assert!(!messages.iter().any(|m| m["name"] == CHAT_FILES));
    }
}
