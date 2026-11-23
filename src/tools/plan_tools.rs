//! Task-scoped progress. A row is the latest full snapshot in a turn.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, future::Future, path::PathBuf};

pub const TOOLS: &[&str] = &["plan_set", "plan_update", "plan_add_substeps"];
const MAX_STEPS: usize = 100;
const MAX_SUBSTEP_BATCH: usize = 50;

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    InProgress,
    Done,
    Blocked,
}

#[derive(Clone, Deserialize, Serialize)]
struct Step {
    id: String,
    title: String,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// Set on sub-steps only. Historical snapshots without the key read back as
    /// a top-level step, which is exactly what they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
}

#[derive(Deserialize)]
struct Update {
    id: String,
    status: Status,
    note: Option<String>,
}

#[derive(Deserialize)]
struct AddSubsteps {
    parent_id: String,
    steps: Vec<Step>,
}

struct Scope {
    root: PathBuf,
    task: String,
    turn: usize,
}
tokio::task_local! { static SCOPE: Scope; }

pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}

pub async fn scoped<F: Future>(root: PathBuf, task: String, turn: usize, f: F) -> F::Output {
    SCOPE.scope(Scope { root, task, turn }, f).await
}

pub fn schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_task_plans(
        task_id TEXT NOT NULL REFERENCES agent_tasks(id) ON DELETE CASCADE,
        turn INTEGER NOT NULL, steps_json TEXT NOT NULL, updated_at INTEGER NOT NULL,
        PRIMARY KEY(task_id,turn));",
    )?;
    Ok(())
}

pub async fn execute(name: &str, args: Value) -> Result<Value> {
    let (root, task, turn) = SCOPE
        .try_with(|s| (s.root.clone(), s.task.clone(), s.turn))
        .context("plan tools require a task session")?;
    let name = name.to_owned();
    crate::context::db_writer::write_async(&root, move |conn| apply(conn, &task, turn, &name, &args)).await
}

fn latest(conn: &Connection, task: &str, turn: usize) -> Result<Vec<Step>> {
    let snapshot: Option<String> = conn.query_row("SELECT steps_json FROM agent_task_plans WHERE task_id=?1 AND turn<=?2 ORDER BY turn DESC LIMIT 1",params![task,turn],|r|r.get(0)).optional()?;
    Ok(snapshot
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default())
}

/// `completed` and `total` keep counting top-level steps so older clients,
/// stored events and rewind snapshots stay comparable. Sub-steps only refine
/// the percentage the panel renders.
fn progress(steps: &[Step]) -> (usize, usize) {
    let total = steps.iter().filter(|s| s.parent_id.is_none()).count();
    let completed = steps
        .iter()
        .filter(|s| s.parent_id.is_none() && s.status == Status::Done)
        .count();
    (completed, total)
}

fn complete_children_flag(args: &Value) -> Result<bool> {
    match args.get("complete_children") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => anyhow::bail!("complete_children must be a boolean"),
    }
}

/// A plan has at most two levels: every `parent_id` must name another step that
/// is itself top-level. A third level, an orphan or a self reference is an
/// authoring mistake that would render as an invisible item.
fn validate_steps(steps: &[Step]) -> Result<()> {
    ensure!(
        steps.len() <= MAX_STEPS,
        "plan accepts at most {MAX_STEPS} steps"
    );
    let mut ids = BTreeSet::new();
    for step in steps {
        ensure!(
            !step.id.trim().is_empty() && step.id.len() <= 96 && ids.insert(&step.id),
            "step IDs must be non-empty, unique and at most 96 bytes"
        );
        ensure!(
            !step.title.trim().is_empty() && step.title.chars().count() <= 200,
            "step titles must contain 1 to 200 characters"
        );
        ensure!(
            step.note.as_ref().is_none_or(|n| n.chars().count() <= 500),
            "step note exceeds 500 characters"
        );
        ensure!(
            step.parent_id
                .as_ref()
                .is_none_or(|p| !p.trim().is_empty() && p.len() <= 96 && p != &step.id),
            "step parent_id must be the non-empty ID of another step"
        );
    }
    for step in steps {
        let Some(parent) = step.parent_id.as_deref() else {
            continue;
        };
        let target = steps
            .iter()
            .find(|s| s.id == parent)
            .with_context(|| format!("step {} points at unknown parent {parent}", step.id))?;
        ensure!(
            target.parent_id.is_none(),
            "plans support at most two levels: {parent} cannot be the parent of {}",
            step.id
        );
    }
    Ok(())
}

/// Keep the two visible layers consistent with what the panel shows: a sub-step
/// that starts lifts a pending parent, and a parent marked done may not leave
/// unfinished sub-steps underneath it.
fn settle_children(steps: &mut [Step], complete_children: bool) -> Result<()> {
    let promoted = steps
        .iter()
        .filter(|s| s.parent_id.is_none() && s.status == Status::Pending)
        .map(|s| s.id.clone())
        .filter(|parent| {
            steps.iter().any(|child| {
                child.parent_id.as_deref() == Some(parent.as_str())
                    && child.status == Status::InProgress
            })
        })
        .collect::<Vec<_>>();
    for parent in promoted {
        if let Some(step) = steps.iter_mut().find(|s| s.id == parent) {
            step.status = Status::InProgress;
        }
    }
    let done = steps
        .iter()
        .filter(|s| s.parent_id.is_none() && s.status == Status::Done)
        .map(|s| s.id.clone())
        .collect::<Vec<_>>();
    for parent in done {
        let open = steps
            .iter()
            .filter(|c| c.parent_id.as_deref() == Some(parent.as_str()) && c.status != Status::Done)
            .count();
        if open == 0 {
            continue;
        }
        ensure!(
            complete_children,
            "parent step {parent} still has {open} unfinished sub-step(s): update the sub-steps first, or pass complete_children: true"
        );
        for child in steps
            .iter_mut()
            .filter(|c| c.parent_id.as_deref() == Some(parent.as_str()))
        {
            child.status = Status::Done;
        }
    }
    Ok(())
}

/// Append under one top-level step instead of forcing the model to resend the
/// whole plan. New sub-steps land after the parent's existing sub-steps so the
/// group stays compact in the order the plan is read.
fn append_substeps(mut steps: Vec<Step>, add: AddSubsteps) -> Result<(Vec<Step>, usize)> {
    ensure!(
        !add.steps.is_empty(),
        "plan_add_substeps needs at least one sub-step"
    );
    ensure!(
        add.steps.len() <= MAX_SUBSTEP_BATCH,
        "plan_add_substeps accepts at most {MAX_SUBSTEP_BATCH} sub-steps"
    );
    ensure!(
        !steps.is_empty(),
        "plan_add_substeps needs an existing plan: call plan_set first"
    );
    let parent_id = add.parent_id.trim().to_owned();
    let index = steps
        .iter()
        .position(|s| s.id == parent_id)
        .with_context(|| format!("parent step {parent_id} does not exist"))?;
    ensure!(
        steps[index].parent_id.is_none(),
        "step {parent_id} is already a sub-step: plans support at most two levels"
    );
    let added = add.steps.len();
    let at = steps
        .iter()
        .rposition(|s| s.parent_id.as_deref() == Some(parent_id.as_str()))
        .map_or(index + 1, |last| last + 1);
    steps.splice(
        at..at,
        add.steps.into_iter().map(|mut child| {
            child.parent_id = Some(parent_id.clone());
            child
        }),
    );
    Ok((steps, added))
}

fn apply(
    conn: &mut Connection,
    task: &str,
    turn: usize,
    name: &str,
    args: &Value,
) -> Result<Value> {
    ensure!(turn > 0, "plan tools require an active turn");
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let complete_children = complete_children_flag(args)?;
    let (mut steps, updated_id, added) = match name {
        "plan_set" => (
            serde_json::from_value::<Vec<Step>>(args["steps"].clone()).context("invalid steps")?,
            None,
            None,
        ),
        "plan_update" => {
            let update: Update =
                serde_json::from_value(args.clone()).context("invalid plan update")?;
            let mut steps = latest(&tx, task, turn)?;
            let step = steps
                .iter_mut()
                .find(|s| s.id == update.id)
                .context("plan step id not found")?;
            step.status = update.status;
            if update.note.is_some() {
                step.note = update.note.filter(|s| !s.is_empty());
            }
            (steps, Some(update.id), None)
        }
        "plan_add_substeps" => {
            let add: AddSubsteps =
                serde_json::from_value(args.clone()).context("invalid plan sub-steps")?;
            let (steps, added) = append_substeps(latest(&tx, task, turn)?, add)?;
            (steps, None, Some(added))
        }
        _ => anyhow::bail!("unknown plan tool"),
    };
    validate_steps(&steps)?;
    settle_children(&mut steps, complete_children)?;
    let (completed, total) = progress(&steps);
    let updated = updated_id.and_then(|id| steps.iter().find(|s| s.id == id).cloned());
    tx.execute("INSERT INTO agent_task_plans(task_id,turn,steps_json,updated_at) VALUES (?1,?2,?3,?4)
        ON CONFLICT(task_id,turn) DO UPDATE SET steps_json=excluded.steps_json,updated_at=excluded.updated_at",
        params![task,turn,serde_json::to_string(&steps)?,crate::server::agent_service::now()])?;
    crate::server::agent_service::append_event_tx(
        &tx,
        task,
        "plan/updated",
        &json!({"turn":turn,"steps":steps,"completed":completed,"total":total}),
        None,
    )?;
    tx.commit()?;
    Ok(match (updated, added) {
        (Some(step), _) => json!({"step":step,"completed":completed,"total":total}),
        (None, Some(added)) => {
            json!({"steps":steps,"added":added,"completed":completed,"total":total})
        }
        (None, None) => json!({"steps":steps,"completed":completed,"total":total}),
    })
}

pub fn step_schema() -> Value {
    json!({"type":"object","required":["id","title","status"],"properties":{
        "id":{"type":"string","minLength":1,"maxLength":96},"title":{"type":"string","minLength":1,"maxLength":200},
        "status":{"enum":["pending","in_progress","done","blocked"]},"note":{"type":"string","maxLength":500},
        "parent_id":{"type":"string","minLength":1,"maxLength":96,"description":"ID of the top-level step this sub-step belongs to. A plan has at most two levels."}}})
}

pub fn definitions() -> Vec<Value> {
    let step = step_schema();
    vec![
        json!({"name":"plan_set","description":"Replace this task's progress plan. Pass an empty list to clear it. A step with parent_id becomes a sub-step of that top-level step; plans have at most two levels.","inputSchema":{"type":"object","required":["steps"],"properties":{"steps":{"type":"array","maxItems":MAX_STEPS,"items":step},"complete_children":{"type":"boolean","description":"Mark the sub-steps of a done parent done instead of rejecting the write."}}}}),
        json!({"name":"plan_update","description":"Update a step by ID. Omit note to keep it or pass an empty string to clear it. A sub-step set to in_progress lifts a pending parent; a parent cannot be done while its sub-steps are unfinished unless complete_children is true.","inputSchema":{"type":"object","required":["id","status"],"properties":{"id":step["properties"]["id"],"status":step["properties"]["status"],"note":step["properties"]["note"],"complete_children":{"type":"boolean","description":"Close the unfinished sub-steps of this parent instead of rejecting the write."}}}}),
        json!({"name":"plan_add_substeps","description":"Append sub-steps under an existing top-level step without resending the whole plan. They are inserted after that step's existing sub-steps.","inputSchema":{"type":"object","required":["parent_id","steps"],"properties":{"parent_id":step["properties"]["id"],"steps":{"type":"array","minItems":1,"maxItems":MAX_SUBSTEP_BATCH,"items":step}}}}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(conn: &Connection) {
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','first','fake','completed',0,0)", []).unwrap();
        conn.execute_batch("CREATE TABLE agent_runtime_windows(task_id TEXT NOT NULL,scope TEXT NOT NULL,state TEXT NOT NULL,PRIMARY KEY(task_id,scope));").unwrap();
    }

    #[test]
    fn snapshots_and_sse_events_rewind_together() {
        let root = std::env::temp_dir().join(format!("plan-{}", crate::server::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let mut conn = crate::server::agent_service::open_db(&root).unwrap();
        fixture(&conn);
        for turn in 1..=2 {
            crate::context::rewind::snapshot(&conn, "t", turn, None).unwrap();
            let tx = conn.transaction().unwrap();
            crate::server::agent_service::append_event_tx(
                &tx,
                "t",
                "user/message",
                &json!({"turn":turn,"message":{"role":"user","content":"continue"}}),
                None,
            )
            .unwrap();
            tx.commit().unwrap();
            let value = if turn == 1 { apply(&mut conn,"t",turn,"plan_set",&json!({"steps":[{"id":"check","title":"Check","status":"pending","note":"Keep"}]})) }
                else { apply(&mut conn,"t",turn,"plan_update",&json!({"id":"check","status":"done"})) }.unwrap();
            assert_eq!(value["completed"], turn - 1);
            assert_eq!(value["total"], 1);
        }
        assert_eq!(
            latest(&conn, "t", 2).unwrap()[0].note.as_deref(),
            Some("Keep")
        );
        crate::context::rewind::rewind(&root, "t", 2).unwrap();
        assert!(latest(&conn, "t", 2).unwrap()[0].status == Status::Pending);
        let s: String = conn.query_row("SELECT data FROM agent_task_events WHERE task_id='t' AND kind='plan/updated' ORDER BY seq DESC LIMIT 1",[],|r|r.get(0)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&s).unwrap()["steps"][0]["status"],
            "pending"
        );
        assert!(apply(&mut conn,"t",2,"plan_set",&json!({"steps":[{"id":"dup","title":"One","status":"pending"},{"id":"dup","title":"Two","status":"done"}]})).is_err());
        assert!(
            apply(
                &mut conn,
                "t",
                2,
                "plan_update",
                &json!({"id":"missing","status":"done"})
            )
            .is_err()
        );
        let count: i64 = conn
            .query_row("SELECT count(*) FROM agent_task_plans", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn setup() -> (PathBuf, Connection) {
        let root = std::env::temp_dir().join(format!("plan-{}", crate::server::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let conn = crate::server::agent_service::open_db(&root).unwrap();
        fixture(&conn);
        (root, conn)
    }

    fn stored(conn: &Connection, turn: usize) -> Value {
        serde_json::to_value(latest(conn, "t", turn).unwrap()).unwrap()
    }

    fn ids(steps: &Value) -> Vec<String> {
        steps
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn two_level_plans_accept_substeps_and_reject_broken_parent_links() {
        let (root, mut conn) = setup();
        let value = apply(
            &mut conn,
            "t",
            1,
            "plan_set",
            &json!({"steps":[
                {"id":"a","title":"Design","status":"in_progress"},
                {"id":"a1","title":"Sketch","status":"done","parent_id":"a"},
                {"id":"b","title":"Build","status":"pending"}]}),
        )
        .unwrap();
        // Counts stay on top-level steps so old clients keep reading them.
        assert_eq!(value["total"], 2);
        assert_eq!(value["completed"], 0);
        let plan = stored(&conn, 1);
        assert_eq!(ids(&plan), vec!["a", "a1", "b"]);
        assert_eq!(plan[1]["parent_id"], "a");
        assert!(plan[0].get("parent_id").is_none());
        // A third level, an orphan, a self reference and a duplicate ID are refused.
        for broken in [
            json!({"steps":[
                {"id":"a","title":"Design","status":"pending"},
                {"id":"a1","title":"Sketch","status":"pending","parent_id":"a"},
                {"id":"deep","title":"Pixel","status":"pending","parent_id":"a1"}]}),
            json!({"steps":[
                {"id":"a","title":"Design","status":"pending"},
                {"id":"a1","title":"Sketch","status":"pending","parent_id":"ghost"}]}),
            json!({"steps":[{"id":"a","title":"Design","status":"pending","parent_id":"a"}]}),
            json!({"steps":[
                {"id":"a","title":"Design","status":"pending"},
                {"id":"a","title":"Again","status":"pending"}]}),
        ] {
            assert!(
                apply(&mut conn, "t", 1, "plan_set", &broken).is_err(),
                "{broken} should be rejected"
            );
        }
        // Rejected writes never replaced the stored snapshot.
        assert_eq!(ids(&stored(&conn, 1)), vec!["a", "a1", "b"]);
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn add_substeps_appends_inside_the_parent_group_and_promotes_it() {
        let (root, mut conn) = setup();
        apply(
            &mut conn,
            "t",
            1,
            "plan_set",
            &json!({"steps":[
                {"id":"a","title":"Design","status":"pending"},
                {"id":"b","title":"Build","status":"pending"}]}),
        )
        .unwrap();
        let value = apply(
            &mut conn,
            "t",
            1,
            "plan_add_substeps",
            &json!({"parent_id":"a","steps":[
                {"id":"a1","title":"Sketch","status":"pending"},
                {"id":"a2","title":"Review","status":"in_progress"}]}),
        )
        .unwrap();
        assert_eq!(value["added"], 2);
        assert_eq!(value["total"], 2);
        assert_eq!(value["completed"], 0);
        apply(
            &mut conn,
            "t",
            1,
            "plan_add_substeps",
            &json!({"parent_id":"b","steps":[{"id":"b1","title":"Wire","status":"pending"}]}),
        )
        .unwrap();
        let plan = stored(&conn, 1);
        assert_eq!(ids(&plan), vec!["a", "a1", "a2", "b", "b1"]);
        // A started sub-step lifts the pending parent and leaves siblings alone.
        assert_eq!(plan[0]["status"], "in_progress");
        assert_eq!(plan[3]["status"], "pending");
        // A later batch stays inside the same group instead of appending at the end.
        apply(
            &mut conn,
            "t",
            1,
            "plan_add_substeps",
            &json!({"parent_id":"a","steps":[{"id":"a3","title":"Handoff","status":"pending"}]}),
        )
        .unwrap();
        assert_eq!(ids(&stored(&conn, 1)), vec!["a", "a1", "a2", "a3", "b", "b1"]);
        // Sub-steps cannot become parents, and unknown parents or empty batches fail.
        for broken in [
            json!({"parent_id":"a1","steps":[{"id":"deep","title":"Deep","status":"pending"}]}),
            json!({"parent_id":"ghost","steps":[{"id":"x","title":"X","status":"pending"}]}),
            json!({"parent_id":"a","steps":[]}),
            json!({"parent_id":"a","steps":[{"id":"a1","title":"Repeat","status":"pending"}]}),
        ] {
            assert!(
                apply(&mut conn, "t", 1, "plan_add_substeps", &broken).is_err(),
                "{broken} should be rejected"
            );
        }
        assert_eq!(ids(&stored(&conn, 1)), vec!["a", "a1", "a2", "a3", "b", "b1"]);
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn child_updates_move_the_parent_and_guard_a_premature_done() {
        let (root, mut conn) = setup();
        apply(
            &mut conn,
            "t",
            1,
            "plan_set",
            &json!({"steps":[
                {"id":"p","title":"Phase","status":"pending"},
                {"id":"c1","title":"One","status":"pending","parent_id":"p"},
                {"id":"c2","title":"Two","status":"pending","parent_id":"p"}]}),
        )
        .unwrap();
        let value = apply(
            &mut conn,
            "t",
            1,
            "plan_update",
            &json!({"id":"c1","status":"in_progress"}),
        )
        .unwrap();
        assert_eq!(value["step"]["id"], "c1");
        assert_eq!(value["step"]["status"], "in_progress");
        assert_eq!(value["total"], 1);
        assert_eq!(value["completed"], 0);
        assert_eq!(stored(&conn, 1)[0]["status"], "in_progress");
        // A blocked sub-step reports itself without overwriting the parent.
        apply(
            &mut conn,
            "t",
            1,
            "plan_update",
            &json!({"id":"c1","status":"blocked","note":"waiting on keys"}),
        )
        .unwrap();
        let plan = stored(&conn, 1);
        assert_eq!(plan[0]["status"], "in_progress");
        assert_eq!(plan[1]["status"], "blocked");
        // A parent cannot go green while its sub-steps are still open.
        assert!(
            apply(
                &mut conn,
                "t",
                1,
                "plan_update",
                &json!({"id":"p","status":"done"})
            )
            .is_err()
        );
        assert_eq!(stored(&conn, 1)[0]["status"], "in_progress");
        // ...unless the caller closes them explicitly.
        let value = apply(
            &mut conn,
            "t",
            1,
            "plan_update",
            &json!({"id":"p","status":"done","complete_children":true}),
        )
        .unwrap();
        assert_eq!(value["completed"], 1);
        assert_eq!(value["total"], 1);
        let plan = stored(&conn, 1);
        assert_eq!(plan[0]["status"], "done");
        assert_eq!(plan[1]["status"], "done");
        assert_eq!(plan[2]["status"], "done");
        // plan_set enforces the same rule through its own flag.
        assert!(
            apply(
                &mut conn,
                "t",
                1,
                "plan_set",
                &json!({"steps":[
                    {"id":"p","title":"Phase","status":"done"},
                    {"id":"c1","title":"One","status":"pending","parent_id":"p"}]})
            )
            .is_err()
        );
        apply(
            &mut conn,
            "t",
            1,
            "plan_set",
            &json!({"steps":[
                {"id":"p","title":"Phase","status":"done"},
                {"id":"c1","title":"One","status":"pending","parent_id":"p"}],
                "complete_children":true}),
        )
        .unwrap();
        assert_eq!(stored(&conn, 1)[1]["status"], "done");
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn substeps_round_trip_through_sse_and_rewind() {
        let (root, mut conn) = setup();
        crate::context::rewind::snapshot(&conn, "t", 1, None).unwrap();
        let tx = conn.transaction().unwrap();
        crate::server::agent_service::append_event_tx(
            &tx,
            "t",
            "user/message",
            &json!({"turn":1,"message":{"role":"user","content":"start"}}),
            None,
        )
        .unwrap();
        tx.commit().unwrap();
        apply(
            &mut conn,
            "t",
            1,
            "plan_set",
            &json!({"steps":[
                {"id":"a","title":"Design","status":"in_progress"},
                {"id":"a1","title":"Sketch","status":"done","parent_id":"a","note":"kept"}]}),
        )
        .unwrap();
        apply(
            &mut conn,
            "t",
            1,
            "plan_add_substeps",
            &json!({"parent_id":"a","steps":[{"id":"a2","title":"Review","status":"pending"}]}),
        )
        .unwrap();
        let event: Value = serde_json::from_str(
            &conn
                .query_row(
                    "SELECT data FROM agent_task_events WHERE task_id='t' AND kind='plan/updated' ORDER BY seq DESC LIMIT 1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(event["steps"][2]["parent_id"], "a");
        assert_eq!(event["steps"][1]["note"], "kept");
        assert_eq!(event["total"], 1);
        assert_eq!(event["completed"], 0);
        crate::context::rewind::snapshot(&conn, "t", 2, None).unwrap();
        let tx = conn.transaction().unwrap();
        crate::server::agent_service::append_event_tx(
            &tx,
            "t",
            "user/message",
            &json!({"turn":2,"message":{"role":"user","content":"more"}}),
            None,
        )
        .unwrap();
        tx.commit().unwrap();
        apply(
            &mut conn,
            "t",
            2,
            "plan_update",
            &json!({"id":"a2","status":"done"}),
        )
        .unwrap();
        assert_eq!(stored(&conn, 2)[2]["status"], "done");
        // Rewinding turn 2 restores the snapshot taken before it: the sub-step
        // goes back to pending and keeps its parent link.
        crate::context::rewind::rewind(&root, "t", 2).unwrap();
        let plan = stored(&conn, 2);
        assert_eq!(ids(&plan), vec!["a", "a1", "a2"]);
        assert_eq!(plan[2]["status"], "pending");
        assert_eq!(plan[2]["parent_id"], "a");
        let event: Value = serde_json::from_str(
            &conn
                .query_row(
                    "SELECT data FROM agent_task_events WHERE task_id='t' AND kind='plan/updated' ORDER BY seq DESC LIMIT 1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(event["steps"][2]["status"], "pending");
        assert_eq!(event["steps"][2]["parent_id"], "a");
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn plan_context_is_host_issued() {
        assert!(
            execute("plan_set", json!({"task_id":"t","turn":1,"steps":[]}))
                .await
                .is_err()
        );
    }
}
