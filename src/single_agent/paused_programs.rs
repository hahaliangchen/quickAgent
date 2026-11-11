//! A message from the user pauses a running program instead of ending it. The
//! rest was planned before the user spoke, so the main AI hears the message
//! and decides: its new program may resume the paused one as a Promise, run
//! beside or after the new work, or leave it, in which case it is discarded
//! when the turn ends. The paused program moves into a frame of its own and
//! its frames keep their state, so resuming continues from the same step.
use super::*;
use crate::agent_program::Action;
use crate::agent_program::runtime::ActionKind;
use crate::function_frames::{self as tree, Node};

pub(super) const RESUME: &str = "resume_previous_program";
const PAUSED: &str = "paused";

pub(super) fn resume_tool() -> Value {
    function(RESUME, "Continue the paused program from the step where it stopped, and return its return value. A program the user's message paused is discarded at the end of the turn unless resumed; one paused because it needed the user is kept through the next turn, and resuming retries the blocked step.",
        json!({}), json!([]))
}

/// The frame holding a paused program; it returns to whoever resumes it.
pub(super) fn is_previous(w: &Window) -> bool {
    w.program_task.as_ref().is_some_and(|task| task["role"] == "previous")
}

fn load(conn: &rusqlite::Connection, task: &str, scope: &str) -> Result<Option<Window>> {
    let raw: Option<String> = conn
        .query_row("SELECT state FROM agent_runtime_windows WHERE task_id=?1 AND scope=?2", params![task, scope], |r| r.get(0))
        .optional()?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into)).transpose()
}

/// The frame a program's call runs in, while that call has not returned.
fn callee<'a>(nodes: &'a [Node], call_id: &str) -> Option<&'a Node> {
    nodes.iter().find(|node| {
        node.metadata["caller"]["call_id"] == call_id && matches!(node.status.as_str(), "running" | "suspended" | PAUSED)
    })
}

/// A frame and every frame its calls opened, through any depth.
fn chain(nodes: &[Node], root: &str) -> Vec<Node> {
    let mut members = BTreeSet::from([root.to_owned()]);
    loop {
        let before = members.len();
        for node in nodes {
            if node.metadata["caller"]["task"].as_str().is_some_and(|caller| members.contains(caller)) {
                members.insert(node.node_id.clone());
            }
        }
        if members.len() == before {
            return nodes.iter().filter(|node| members.contains(&node.node_id)).cloned().collect();
        }
    }
}

/// Where a frame stopped: the line and call of each step its code had in
/// flight, followed by where that call stopped, or the AI still at work in a
/// function without code.
pub(super) fn stop_point(s: &Session<'_>, w: &Window) -> Result<String> {
    let conn = agent_service::open_db(s.state.workspace.root())?;
    let nodes = tree::nodes(&conn, s.task)?;
    describe(&conn, s.task, &nodes, w)
}
fn describe(conn: &rusqlite::Connection, task: &str, nodes: &[Node], w: &Window) -> Result<String> {
    let Some(runtime) = w.program.as_ref() else {
        return Ok(match program_flow::role(w) {
            program_flow::Role::Implement => "（AI 正在编写实现）".into(),
            _ => "（AI 处理中）".into(),
        });
    };
    let actions: Vec<Action> = runtime.pending.iter().cloned().chain(runtime.inflight_actions()).collect();
    let mut steps = Vec::new();
    for action in actions {
        let mut below = String::new();
        // A resume runs the paused program; a call runs in its own frame.
        let frame = match &action.kind {
            ActionKind::Tool { name, .. } if name == RESUME => match &w.resumable {
                Some(scope) => load(conn, task, scope)?,
                None => None,
            },
            _ => match callee(nodes, &action.id) {
                Some(node) => load(conn, task, &tree::frame_scope(node))?,
                None => None,
            },
        };
        if let Some(frame) = frame {
            let inner = describe(conn, task, nodes, &frame)?;
            below = if inner.starts_with('（') || inner.is_empty() { inner } else { format!(" → {inner}") };
        }
        steps.push(((action.at.line, action.at.column), format!("{}{below}", step_label(&action))));
    }
    // In source order; calls made side by side on one line by what they say.
    steps.sort();
    Ok(steps.into_iter().map(|(_, step)| step).collect::<Vec<_>>().join("、"))
}

/// A step as the outcome names it: its line and what it calls. Arguments tell
/// apart calls to one function made side by side.
pub(super) fn step_label(action: &Action) -> String {
    let callee = match &action.kind {
        ActionKind::Call { name, args, .. } => {
            let args = args.iter().map(Value::to_string).collect::<Vec<_>>().join(", ");
            let args = if args.chars().count() > 40 { format!("{}…", args.chars().take(40).collect::<String>()) } else { args };
            format!("{name}({args})")
        }
        ActionKind::Tool { name, .. } if name == RESUME => "previous.resume()".into(),
        ActionKind::Task { name, .. } | ActionKind::Tool { name, .. } => name.clone(),
        ActionKind::Require { library } => format!("require({library:?})"),
        _ => "并行读取".into(),
    };
    format!("第 {} 行 {callee}", action.at.line)
}

/// Move the main window's program into a paused frame of its own and leave the
/// main AI free to answer the user. Returns where the program stopped. A
/// program paused for a blocker is kept through the next turn, where the user
/// has had the chance to resolve it.
pub(super) async fn pause_main(s: &Session<'_>, w: &mut Window, blocked: bool) -> Result<String> {
    program_flow::recover(s, w).await?;
    let stopped_at = stop_point(s, w)?;
    let runtime = w.program.take().context("no running program to pause")?;
    let calls: BTreeSet<String> = runtime.pending.iter().map(|action| action.id.clone())
        .chain(runtime.inflight_actions().into_iter().map(|action| action.id)).collect();
    let main_id = task_frames::frame_task(s, w);
    let mut previous = w.clone();
    previous.program = Some(runtime);
    previous.program_task = Some(json!({"name":"previous","role":"previous"}));
    previous.messages = Vec::new();
    previous.program_value = None;
    previous.parallel_root = None;
    let (task, turn) = (s.task.to_owned(), s.turn);
    // Writes after this point are ones the paused program has not seen.
    let clock = super::managed_files::write_clock(s.state.workspace.root(), &task)?;
    let scope = db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let main = tree::get(&tx, &task, &main_id)?;
        let ordinal = tree::children(&tx, &task, &main_id)?.len();
        let mut node = task_frames::new_task(&main, &json!({"goal":"paused program"}), ordinal, "temp", turn)?;
        // It runs beside the main AI, so it never counts as the task in view.
        node.metadata["parallel"] = json!(true);
        node.metadata["parallel_owner"] = json!(node.node_id);
        node.metadata["file_write_clock"] = json!(clock);
        node.metadata["previous"] = json!(true);
        if blocked {
            node.metadata["kept_through_turn"] = json!(turn + 1);
        }
        node.status = PAUSED.into();
        tree::emit(&tx, &task, "task/node_paused", &mut node, turn)?;
        let scope = tree::frame_scope(&node);
        previous.task_node_id = Some(node.node_id.clone());
        let mut nodes = tree::nodes(&tx, &task)?;
        // The paused calls now return to the paused program's own frame.
        for callee in nodes.iter_mut() {
            if callee.metadata["caller"]["task"] == main_id.as_str()
                && callee.metadata["caller"]["call_id"].as_str().is_some_and(|id| calls.contains(id))
            {
                callee.metadata["caller"]["task"] = json!(node.node_id);
                callee.metadata["caller"]["scope"] = json!(scope);
            }
        }
        for callee in nodes.iter().filter(|callee| callee.metadata["caller"]["task"] == node.node_id.as_str()) {
            tree::emit(&tx, &task, "task/node_paused", &mut callee.clone(), turn)?;
        }
        mark_paused(&tx, &task, &node.node_id, turn)?;
        let mut main = tree::get(&tx, &task, &main_id)?;
        if main.status != "running" {
            main.status = "running".into();
            tree::emit(&tx, &task, "task/node_resumed", &mut main, turn)?;
        }
        task_frames::write_window(&tx, &task, &scope, &previous)?;
        tx.commit()?;
        Ok(scope)
    })
    .await?;
    w.resumable = Some(scope);
    w.program_value = None;
    append(s, "program/paused", json!({"turn":s.turn,"stopped_at":stopped_at,"scope":w.resumable}), false).await?;
    Ok(stopped_at)
}

/// Pause every frame under a paused program. They stopped at a safe point,
/// with every finished step recorded, so they may later run on; a frame cut
/// off any other way never does, since its last step may be unrecorded.
fn mark_paused(tx: &rusqlite::Transaction<'_>, task: &str, root: &str, turn: usize) -> Result<()> {
    for mut member in chain(&tree::nodes(tx, task)?, root) {
        if !matches!(member.status.as_str(), "running" | "suspended" | PAUSED) {
            continue;
        }
        if member.status == "running" {
            member.status = PAUSED.into();
        }
        // It runs beside the main AI, so it never counts as the task in view.
        member.metadata["parallel"] = json!(true);
        member.metadata["safe_stop"] = json!(true);
        tree::emit(tx, task, "task/node_paused", &mut member, turn)?;
    }
    Ok(())
}
fn mark_resumed(tx: &rusqlite::Transaction<'_>, task: &str, root: &str, turn: usize) -> Result<()> {
    for mut member in chain(&tree::nodes(tx, task)?, root) {
        if member.status == PAUSED {
            member.status = "running".into();
            tree::emit(tx, task, "task/node_resumed", &mut member, turn)?;
        }
    }
    Ok(())
}

/// Run the paused program this window may resume until it returns: Some(its
/// value, whether it failed), or None when the user spoke again and it paused.
pub(super) async fn resume(s: &Session<'_>, w: &Window) -> Result<Option<(Value, bool)>> {
    let scope = w.resumable.clone().context("no paused program to resume")?;
    let (task, turn) = (s.task.to_owned(), s.turn);
    let (start_scope, mut frame, previous_id) = db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let previous = load(&tx, &task, &scope)?.context("the paused program already finished or was discarded")?;
        let previous_id = previous.task_node_id.clone().context("paused program frame has no task")?;
        mark_resumed(&tx, &task, &previous_id, turn)?;
        // It continues where it stopped: in its innermost serial call.
        let nodes = tree::nodes(&tx, &task)?;
        let (mut at, mut frame) = (scope, previous);
        while let Some(action) = frame.program.as_ref().and_then(|program| program.pending.clone())
            && let Some(node) = callee(&nodes, &action.id)
            && let Some(child) = load(&tx, &task, &tree::frame_scope(node))?
        {
            at = tree::frame_scope(node);
            frame = child;
        }
        tx.commit()?;
        Ok((at, frame, previous_id))
    })
    .await?;
    // It runs as a task of its own, like a concurrent call: polled in place,
    // each resumed level would add a whole loop to the caller's stack, and a
    // program resumed inside a resumed one overflowed it. The set ends the
    // task if this call is dropped.
    let (state, task, model, cancel) = (s.state.clone(), s.task.to_owned(), s.model.to_owned(), s.cancel.clone());
    let (agent_id, plugin, parent) = (s.agent_id.clone(), s.plugin.clone(), s.parent.clone());
    let (turn, ban_execution, input_budget) = (s.turn, s.ban_execution, s.input_budget);
    let mut resumed = tokio::task::JoinSet::new();
    resumed.spawn(async move {
        let session = Session { agent_id, task_node_id: frame.task_node_id.clone(), plugin, state: &state, task: &task,
            model: &model, turn, cancel: &cancel, ban_execution, scope: start_scope, parent, input_budget };
        let result = run_loop(&session, &mut frame, 80, false).await;
        (result, frame)
    });
    let (result, frame) = resumed.join_next().await.context("resumed program task missing")?.context("resumed program task failed")?;
    // The user stopped the turn while it ran: its frames stay as they are and
    // it is kept with the program that was resuming it (design guide §7.4).
    if s.cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let (task, turn, scope) = (s.task.to_owned(), s.turn, w.resumable.clone().unwrap_or_default());
    let returned = match &result {
        Ok(LoopResult::Answer) if is_previous(&frame) => frame.program_value.clone(),
        _ => None,
    };
    if matches!(result, Ok(LoopResult::Paused)) {
        db(s.state.workspace.root(), move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            mark_paused(&tx, &task, &previous_id, turn)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        return Ok(None);
    }
    let Some(value) = returned else {
        let message = match result {
            Err(error) => format!("{error:#}"),
            _ => "the paused program did not finish".into(),
        };
        discard_frame(s, &previous_id).await?;
        return Ok(Some((json!({"message":message}), true)));
    };
    db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut node = tree::get(&tx, &task, &previous_id)?;
        node.status = "done".into();
        node.result = Some(value_summary(&tx, &task, &scope)?);
        tree::emit(&tx, &task, "task/node_returned", &mut node, turn)?;
        tx.execute("DELETE FROM agent_runtime_windows WHERE task_id=?1 AND scope=?2", params![task, scope])?;
        tx.commit()?;
        Ok(())
    })
    .await?;
    Ok(Some(match value.get("$task_error") {
        Some(error) => (error.clone(), true),
        None => (value, false),
    }))
}
fn value_summary(tx: &rusqlite::Transaction<'_>, task: &str, scope: &str) -> Result<Value> {
    Ok(load(tx, task, scope)?.and_then(|frame| frame.program_value).map(|value| json!({"result":value})).unwrap_or(Value::Null))
}

/// End a paused frame and everything under it without running any of it.
async fn discard_frame(s: &Session<'_>, root: &str) -> Result<()> {
    let (task, turn, root) = (s.task.to_owned(), s.turn, root.to_owned());
    db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for mut member in chain(&tree::nodes(&tx, &task)?, &root) {
            if matches!(member.status.as_str(), "running" | "suspended" | PAUSED) {
                member.status = "interrupted".into();
                member.result = Some(json!({"message":"paused and not resumed; its remaining steps did not run"}));
                tree::emit(&tx, &task, "task/node_returned", &mut member, turn)?;
                tx.execute("DELETE FROM agent_runtime_windows WHERE task_id=?1 AND scope=?2", params![task, tree::frame_scope(&member)])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

/// The frames of programs a blocker paused, kept for the next turn: the
/// turn's final cleanup leaves their state as it is.
pub(crate) fn kept_scopes(conn: &rusqlite::Connection, task: &str) -> Result<BTreeSet<String>> {
    let nodes = tree::nodes(conn, task)?;
    Ok(nodes.iter()
        .filter(|node| node.metadata["previous"] == true && node.metadata["kept_through_turn"].is_u64()
            && matches!(node.status.as_str(), "running" | "suspended" | PAUSED))
        .flat_map(|root| chain(&nodes, &root.node_id))
        .map(|node| tree::frame_scope(&node))
        .collect())
}

/// Whether a window's program has asked to resume the paused one and has not
/// got its result yet.
pub(super) fn resumes_previous(w: &Window) -> bool {
    w.program.as_ref().is_some_and(|runtime| {
        runtime.pending.iter().cloned().chain(runtime.inflight_actions())
            .any(|action| matches!(&action.kind, ActionKind::Tool { name, .. } if name == RESUME))
    })
}

/// Keep a paused program through the next turn, with its frames as they are.
/// A turn the user stopped does not use up the one turn it is kept for.
pub(super) async fn keep_through_next_turn(s: &Session<'_>, scope: &str) -> Result<()> {
    let (task, turn, scope) = (s.task.to_owned(), s.turn, scope.to_owned());
    db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let root = tree::nodes(&tx, &task)?.into_iter().find(|node| node.metadata["previous"] == true && tree::frame_scope(node) == scope
            && matches!(node.status.as_str(), "running" | "suspended" | PAUSED));
        if let Some(mut root) = root {
            root.metadata["kept_through_turn"] = json!(turn + 1);
            tree::emit(&tx, &task, "task/node_paused", &mut root, turn)?;
            mark_paused(&tx, &task, &root.node_id, turn)?;
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

/// A kept program this host cannot read back, because another version of the
/// host saved it, is discarded before the turn starts. Returns what the main
/// AI is told, so it does not write a program that resumes nothing.
pub(super) async fn drop_unreadable(s: &Session<'_>, w: &mut Window) -> Result<Option<String>> {
    let Some(scope) = w.resumable.clone() else { return Ok(None) };
    let task = s.task.to_owned();
    let unreadable = db(s.state.workspace.root(), move |conn| {
        let nodes = tree::nodes(conn, &task)?;
        let Some(root) = nodes.iter().find(|node| node.metadata["previous"] == true && tree::frame_scope(node) == scope) else { return Ok(None) };
        for member in chain(&nodes, &root.node_id) {
            if !matches!(member.status.as_str(), "running" | "suspended" | PAUSED) {
                continue;
            }
            let raw: Option<String> = conn
                .query_row("SELECT state FROM agent_runtime_windows WHERE task_id=?1 AND scope=?2", params![task, tree::frame_scope(&member)], |r| r.get(0))
                .optional()?;
            let readable = match raw {
                Some(raw) => serde_json::from_str::<Window>(&raw).is_ok_and(|frame| {
                    frame.program.as_ref().is_none_or(|runtime| runtime.program.version == crate::agent_program::VERSION)
                }),
                None => member.node_id != root.node_id,
            };
            if !readable {
                return Ok(Some(root.node_id.clone()));
            }
        }
        Ok(None)
    })
    .await?;
    let Some(root) = unreadable else { return Ok(None) };
    discard_frame(s, &root).await?;
    w.resumable = None;
    save(s, w).await?;
    append(s, "program/kept_unreadable", json!({"turn":s.turn,"frame":root}), false).await?;
    Ok(Some("The program kept from the previous turn was saved by a different version of the host and cannot be read back, so it was discarded: previous.resume() is not available. The steps it finished are in the conversation history; write a new program for what remains.".into()))
}

/// At the end of a turn, programs paused and not resumed are discarded; one
/// paused for a blocker this turn waits for the next.
pub(super) async fn discard_unresumed(s: &Session<'_>, w: &mut Window) -> Result<()> {
    let (task, turn) = (s.task.to_owned(), s.turn as u64);
    let (paused, kept) = db(s.state.workspace.root(), move |conn| {
        let (kept, paused): (Vec<_>, Vec<_>) = tree::nodes(conn, &task)?.into_iter()
            .filter(|node| node.metadata["previous"] == true && matches!(node.status.as_str(), "running" | "suspended" | PAUSED))
            .partition(|node| node.metadata["kept_through_turn"].as_u64().is_some_and(|last| last > turn));
        Ok((paused.into_iter().map(|node| node.node_id).collect::<Vec<_>>(), kept.into_iter().map(|node| tree::frame_scope(&node)).collect::<Vec<_>>()))
    })
    .await?;
    for root in &paused {
        discard_frame(s, root).await?;
    }
    let resumable = w.resumable.clone().filter(|scope| kept.contains(scope));
    if w.resumable != resumable {
        w.resumable = resumable;
        save(s, w).await?;
    }
    Ok(())
}

/// Concurrent calls of a resumed program that paused with it, each from its
/// innermost frame, to run again. Only a pause's safe stop qualifies, once.
pub(super) fn paused_calls(s: &Session<'_>, w: &Window) -> Result<Vec<(String, Window)>> {
    let Some(runtime) = w.program.as_ref() else { return Ok(Vec::new()) };
    let mut conn = agent_service::open_db(s.state.workspace.root())?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let nodes = tree::nodes(&tx, s.task)?;
    let mut calls = Vec::new();
    for action in runtime.inflight_actions() {
        let Some(node) = callee(&nodes, &action.id).filter(|node| node.metadata["safe_stop"] == true) else { continue };
        let mut at = tree::frame_scope(node);
        let Some(mut frame) = load(&tx, s.task, &at)? else { continue };
        let mut started = node.clone();
        started.metadata["safe_stop"] = json!(false);
        tree::emit(&tx, s.task, "task/node_resumed", &mut started, s.turn)?;
        while let Some(action) = frame.program.as_ref().and_then(|program| program.pending.clone())
            && let Some(node) = callee(&nodes, &action.id)
            && let Some(child) = load(&tx, s.task, &tree::frame_scope(node))?
        {
            at = tree::frame_scope(node);
            frame = child;
        }
        calls.push((at, frame));
    }
    tx.commit()?;
    Ok(calls)
}
