//! Task-scoped progress. A row is the latest full snapshot in a turn.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, future::Future, path::PathBuf};

pub const TOOLS: &[&str] = &["plan_set", "plan_update"];
const MAX_STEPS: usize = 100;

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
}

#[derive(Deserialize)]
struct Update {
    id: String,
    status: Status,
    note: Option<String>,
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

fn apply(
    conn: &mut Connection,
    task: &str,
    turn: usize,
    name: &str,
    args: &Value,
) -> Result<Value> {
    ensure!(turn > 0, "plan tools require an active turn");
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (steps, updated) = match name {
        "plan_set" => (
            serde_json::from_value::<Vec<Step>>(args["steps"].clone()).context("invalid steps")?,
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
            let updated = step.clone();
            (steps, Some(updated))
        }
        _ => anyhow::bail!("unknown plan tool"),
    };
    ensure!(
        steps.len() <= MAX_STEPS,
        "plan accepts at most {MAX_STEPS} steps"
    );
    let mut ids = BTreeSet::new();
    for step in &steps {
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
    }
    let completed = steps.iter().filter(|s| s.status == Status::Done).count();
    let total = steps.len();
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
    Ok(match updated {
        Some(step) => json!({"step":step,"completed":completed,"total":total}),
        None => json!({"steps":steps,"completed":completed,"total":total}),
    })
}

pub fn step_schema() -> Value {
    json!({"type":"object","required":["id","title","status"],"properties":{
        "id":{"type":"string","minLength":1,"maxLength":96},"title":{"type":"string","minLength":1,"maxLength":200},
        "status":{"enum":["pending","in_progress","done","blocked"]},"note":{"type":"string","maxLength":500}}})
}

pub fn definitions() -> Vec<Value> {
    let step = step_schema();
    vec![
        json!({"name":"plan_set","description":"Replace this task's progress plan. Pass an empty list to clear it.","inputSchema":{"type":"object","required":["steps"],"properties":{"steps":{"type":"array","maxItems":MAX_STEPS,"items":step}}}}),
        json!({"name":"plan_update","description":"Update a step by ID. Omit note to keep it or pass an empty string to clear it.","inputSchema":{"type":"object","required":["id","status"],"properties":{"id":step["properties"]["id"],"status":step["properties"]["status"],"note":step["properties"]["note"]}}}),
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

    #[tokio::test]
    async fn plan_context_is_host_issued() {
        assert!(
            execute("plan_set", json!({"task_id":"t","turn":1,"steps":[]}))
                .await
                .is_err()
        );
    }
}
