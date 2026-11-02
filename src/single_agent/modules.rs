//! Modules: functions declared `@module`. AI writes a module's code once per
//! contract, and later calls in the same chat run that code instead of asking
//! again. Every version is a row of `agent_modules`, kept per task and turn, so
//! rewinding a turn or deleting the chat removes it. The chat's folder holds
//! the latest version of each module as a file, which a fixer edits when a
//! call fails.
use super::*;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Notify;

/// The message that makes a window a fixer's.
pub(super) const FIX_INPUT: &str = "module_fix_input";

fn schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_modules(
            task_id TEXT NOT NULL, turn INTEGER NOT NULL, contract TEXT NOT NULL,
            name TEXT NOT NULL, signature TEXT NOT NULL, description TEXT NOT NULL,
            version INTEGER NOT NULL, source TEXT NOT NULL, fixed INTEGER NOT NULL,
            created_at INTEGER NOT NULL, PRIMARY KEY(task_id, contract, version));",
    )?;
    Ok(())
}

/// What identifies a module: its name, signature and contract comment.
pub(super) struct Contract {
    pub key: String,
    pub name: String,
    pub signature: String,
    pub description: String,
}

pub(super) fn contract(task: &Value) -> Result<Contract> {
    let view = program_flow::call_view(task)?;
    let signature = view["signature"].as_str().context("module signature missing")?.to_owned();
    let name = task["name"].as_str().context("module name missing")?.to_owned();
    let description = task["description"].as_str().unwrap_or("").to_owned();
    let key = crate::visual_artifacts::hash(format!("{signature}\n{description}").as_bytes())[..16].to_owned();
    Ok(Contract { key, name, signature, description })
}

/// A call of a module, as opposed to a @virtual or an ordinary function.
pub(super) fn is_module(w: &Window) -> bool {
    w.program_task.as_ref().is_some_and(|task| task["module"] == true)
}

fn file_name(name: &str, key: &str) -> String {
    format!("{name}-{key}.js")
}

/// The module file, relative to the workspace, as the fixer is told it.
pub(super) fn relative_path(root: &Path, task: &str, contract: &Contract) -> Result<String> {
    let path = host_folder::chat_dir(root, task)?.join("modules").join(file_name(&contract.name, &contract.key));
    Ok(path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/"))
}

fn write_file(root: &Path, task: &str, name: &str, key: &str, source: &str) -> Result<()> {
    let dir = host_folder::chat_dir(root, task)?.join("modules");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(file_name(name, key)), source)?;
    Ok(())
}

/// The latest version of a module, if this chat has one.
pub(super) fn latest(root: &Path, task: &str, key: &str) -> Result<Option<(i64, String)>> {
    let conn = agent_service::open_db(root)?;
    schema(&conn)?;
    Ok(conn
        .query_row(
            "SELECT version,source FROM agent_modules WHERE task_id=?1 AND contract=?2 ORDER BY version DESC LIMIT 1",
            params![task, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Record a new version and write it as the module's file.
async fn save(s: &Session<'_>, contract: &Contract, source: &str, fixed: bool) -> Result<i64> {
    let (task, turn) = (s.task.to_owned(), s.turn as i64);
    let row = (contract.key.clone(), contract.name.clone(), contract.signature.clone(), contract.description.clone(), source.to_owned());
    let version = db(s.state.workspace.root(), move |conn| {
        schema(conn)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version),0)+1 FROM agent_modules WHERE task_id=?1 AND contract=?2",
            params![task, row.0],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO agent_modules VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![task, turn, row.0, row.1, row.2, row.3, version, row.4, fixed as i64, agent_service::now()],
        )?;
        tx.commit()?;
        Ok(version)
    })
    .await?;
    write_file(s.state.workspace.root(), s.task, &contract.name, &contract.key, source)?;
    append(s, "module/saved", json!({"function":contract.name,"contract":contract.key,"version":version,
        "path":relative_path(s.state.workspace.root(), s.task, contract)?,"fixed":fixed}), false).await?;
    Ok(version)
}

/// The latest version of each module in this chat.
fn latest_all(conn: &rusqlite::Connection, task: &str) -> Result<Vec<(String, String, String, String, String)>> {
    schema(conn)?;
    Ok(conn
        .prepare(
            "SELECT m.contract,m.name,m.signature,m.description,m.source FROM agent_modules m
             WHERE m.task_id=?1 AND m.version=(SELECT MAX(version) FROM agent_modules WHERE task_id=m.task_id AND contract=m.contract)
             ORDER BY m.created_at, m.name",
        )?
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The chat's modules, listed for an AI that writes code: declaring one again
/// with the same name, parameters and contract reuses it.
pub(super) fn system_message(root: &Path, task: &str) -> Option<Value> {
    let conn = agent_service::open_db(root).ok()?;
    let modules = latest_all(&conn, task).ok()?;
    if modules.is_empty() {
        return None;
    }
    let declarations = modules
        .iter()
        .map(|(_, _, signature, description, _)| {
            let comment = description.lines().map(|line| format!("  // {line}")).collect::<Vec<_>>().join("\n");
            format!("/** @module */\n{signature} {{\n{comment}\n}}")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(json!({"role":"system","name":"chat_modules","content":format!(
        "Modules this chat already has. Declaring one again with the same name, parameters and contract comment runs its existing code instead of writing it again.\n\n{declarations}"
    )}))
}

/// Make the chat folder hold the latest surviving version of each module,
/// as after a rewind dropped later versions.
pub(crate) fn rebuild(root: &Path, task: &str) -> Result<()> {
    if let Some(dir) = host_folder::existing_chat_dir(root, task).map(|dir| dir.join("modules"))
        && dir.exists()
    {
        std::fs::remove_dir_all(&dir)?;
    }
    let conn = agent_service::open_db(root)?;
    for (key, name, _, _, source) in latest_all(&conn, task)? {
        write_file(root, task, &name, &key, &source)?;
    }
    Ok(())
}

/// The chat is gone, and so is its folder.
pub(crate) fn remove(root: &Path, task: &str) {
    if let Some(dir) = host_folder::existing_chat_dir(root, task) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Calls that write or fix the same module take turns: one does it, the
/// others wait for it. Claims live in this process; a claim whose owner frame
/// is no longer running is stale and taken over.
struct Claim {
    owner: String,
    done: Arc<Notify>,
}
static CLAIMS: LazyLock<Mutex<HashMap<String, Claim>>> = LazyLock::new(Default::default);

enum Turn {
    Mine,
    Wait(Arc<Notify>),
}

fn claim_key(s: &Session<'_>, what: &str) -> String {
    format!("{}|{}|{what}", s.state.workspace.root().display(), s.task)
}

fn owner(w: &Window) -> String {
    w.task_node_id.clone().unwrap_or_default()
}

fn running(s: &Session<'_>, node: &str) -> bool {
    agent_service::open_db(s.state.workspace.root())
        .and_then(|conn| crate::function_frames::get(&conn, s.task, node))
        .is_ok_and(|node| matches!(node.status.as_str(), "running" | "suspended" | "current"))
}

fn claim(s: &Session<'_>, key: &str, me: &str) -> Turn {
    let mut claims = CLAIMS.lock().unwrap();
    if let Some(claim) = claims.get(key)
        && claim.owner != me
        && running(s, &claim.owner)
    {
        return Turn::Wait(claim.done.clone());
    }
    claims.insert(key.to_owned(), Claim { owner: me.to_owned(), done: Arc::new(Notify::new()) });
    Turn::Mine
}

fn release(key: &str, me: &str) {
    let mut claims = CLAIMS.lock().unwrap();
    if claims.get(key).is_some_and(|claim| claim.owner == me)
        && let Some(claim) = claims.remove(key)
    {
        claim.done.notify_waiters();
    }
}

/// A frame that returns lets go of whatever it was writing or fixing.
pub(super) fn release_frame(w: &Window) {
    let me = owner(w);
    let mut claims = CLAIMS.lock().unwrap();
    let mine = claims.iter().filter(|(_, claim)| claim.owner == me).map(|(key, _)| key.clone()).collect::<Vec<_>>();
    for key in mine {
        if let Some(claim) = claims.remove(&key) {
            claim.done.notify_waiters();
        }
    }
}

async fn wait(s: &Session<'_>, done: &Notify) -> Result<()> {
    tokio::select! {
        _ = done.notified() => {}
        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        _ = s.cancel.cancelled() => anyhow::bail!("cancelled"),
    }
    Ok(())
}

/// Every host library: a module may import any authorized one, whatever tools
/// the window publishes to its AI. Third-party ones it requires as it runs.
fn libraries(s: &Session<'_>, w: &Window) -> crate::agent_program::libraries::Libraries {
    program_flow::static_libraries(s, &super::available_tools(s, w))
}

fn compile(s: &Session<'_>, w: &Window, name: &str, source: &str) -> Result<()> {
    crate::agent_program::compile_body_with_scope(source, name, libraries(s, w), Default::default()).map(|_| ())
}

/// Run this version of the module for the window's call, from its start.
fn install(s: &Session<'_>, w: &mut Window, version: i64, source: &str) -> Result<()> {
    w.program = None;
    w.program_value = None;
    let libraries = libraries(s, w);
    program_flow::install_body_with(w, &json!({"source":source}), libraries)?;
    if let Some(task) = w.program_task.as_mut() {
        task["module_version"] = json!(version);
        if let Some(task) = task.as_object_mut() {
            task.remove("fix");
        }
    }
    Ok(())
}

/// Before an AI writes a module, run the code this chat already has for its
/// contract; when another call is writing it right now, wait for that one.
/// True when the window now runs saved code.
pub(super) async fn prepare(s: &Session<'_>, w: &mut Window) -> Result<bool> {
    let Some(task) = w.program_task.clone() else { return Ok(false) };
    let writing = w.messages.iter().any(|m| m["role"] == "assistant");
    if task["module"] != true || task["module_version"].is_number() || task["fix"].is_object()
        || w.program.is_some() || w.program_value.is_some() || writing
    {
        return Ok(false);
    }
    let contract = contract(&task)?;
    let key = claim_key(s, &format!("write:{}", contract.key));
    let me = owner(w);
    loop {
        if let Some((version, source)) = latest(s.state.workspace.root(), s.task, &contract.key)?
            && install(s, w, version, &source).is_ok()
        {
            append(s, "module/loaded", json!({"function":contract.name,"contract":contract.key,"version":version}), false).await?;
            save_window(s, w).await?;
            return Ok(true);
        }
        match claim(s, &key, &me) {
            Turn::Mine => return Ok(false),
            Turn::Wait(done) => wait(s, &done).await?,
        }
    }
}

/// The AI that wrote a module replied with code that compiled: it is the
/// module's first version, and calls waiting for it can run it.
pub(super) async fn authored(s: &Session<'_>, w: &mut Window, source: &str) -> Result<()> {
    let Some(task) = w.program_task.clone() else { return Ok(()) };
    if task["module"] != true || task["module_version"].is_number() || task["fix"].is_object() {
        return Ok(());
    }
    let contract = contract(&task)?;
    let version = save(s, &contract, source, false).await?;
    if let Some(task) = w.program_task.as_mut() {
        task["module_version"] = json!(version);
    }
    release(&claim_key(s, &format!("write:{}", contract.key)), &owner(w));
    Ok(())
}

/// What a failed module call does next.
pub(super) enum AfterFailure {
    /// Another call already fixed the module: this call runs the new version.
    Rerun,
    /// A fixer now works in this window.
    Fixing,
    /// The fix this call waited for gave up: the call fails with its error.
    Fail,
}

/// A module call failed. A fixer gets the module file and this call's
/// problem, unless the module was fixed meanwhile or another call is already
/// fixing this version, in which case this call waits for that fix.
pub(super) async fn failed(s: &Session<'_>, w: &mut Window, error: &Value, console: Vec<String>) -> Result<AfterFailure> {
    let task = w.program_task.clone().context("module call has no function")?;
    let contract = contract(&task)?;
    let version = task["module_version"].as_i64().context("module call has no version")?;
    let completed = program_flow::completed_writes(s, w)?;
    let key = claim_key(s, &format!("fix:{}:{version}", contract.key));
    let me = owner(w);
    let mut waited = false;
    loop {
        let (latest_version, source) = latest(s.state.workspace.root(), s.task, &contract.key)?
            .context("module version missing")?;
        if latest_version > version {
            install(s, w, latest_version, &source)?;
            append(s, "module/loaded", json!({"function":contract.name,"contract":contract.key,"version":latest_version,"after_fix":true}), false).await?;
            save_window(s, w).await?;
            return Ok(AfterFailure::Rerun);
        }
        match claim(s, &key, &me) {
            // The fix this call waited for let go without a new version: it gave up.
            Turn::Mine if waited => {
                release(&key, &me);
                return Ok(AfterFailure::Fail);
            }
            Turn::Mine => break,
            // Still being fixed: keep waiting.
            Turn::Wait(done) => {
                wait(s, &done).await?;
                waited = true;
            }
        }
    }
    let (_, source) = latest(s.state.workspace.root(), s.task, &contract.key)?.context("module version missing")?;
    // The file is what the fixer edits; it starts as the version that failed.
    write_file(s.state.workspace.root(), s.task, &contract.name, &contract.key, &source)?;
    let path = relative_path(s.state.workspace.root(), s.task, &contract)?;
    let bindings = program_flow::call_view(&task)?["bindings"].clone();
    let input = json!({
        "module":{"path":path,"signature":contract.signature,"contract":contract.description,"version":version,"source":source},
        "call":{"bindings":bindings},
        "error":error,
        "console":console,
        "completed_writes":completed,
        "instruction":format!("Fix {path} with edit_file, then reply with one short sentence saying what you changed."),
    });
    w.program = None;
    w.program_value = None;
    // A fresh context: the fixer's prompt, then the module and this call's problem.
    w.messages = vec![
        json!({"role":"system","content":program_flow::FIX_PROMPT}),
        json!({"role":"user","name":FIX_INPUT,"content":input.to_string()}),
    ];
    if let Some(task) = w.program_task.as_mut() {
        task["fix"] = json!({"path":path,"version":version,"contract":contract.key});
    }
    append(s, "module/fix_started", json!({"function":contract.name,"contract":contract.key,"version":version,"error":error}), false).await?;
    save_window(s, w).await?;
    Ok(AfterFailure::Fixing)
}

/// The fixer's edit_file: exact text edits to its module file only. The
/// result says whether the module now compiles.
pub(super) fn edit(s: &Session<'_>, w: &Window, args: &Value) -> Result<Value> {
    let task = w.program_task.as_ref().context("no module is being fixed")?;
    let path = task["fix"]["path"].as_str().context("no module is being fixed")?;
    let asked = args["path"].as_str().context("path is required")?.replace('\\', "/");
    ensure!(asked.trim_start_matches("./") == path, "only the module file {path} can be edited here");
    let edits: Vec<crate::tools::TextEdit> = serde_json::from_value(args["edits"].clone()).context("edits must be a list of {old_text, new_text}")?;
    let full = s.state.workspace.root().join(path);
    let original = std::fs::read_to_string(&full)?;
    let updated = crate::file_edit::text_edits(&original, &edits)?;
    std::fs::write(&full, &updated)?;
    let name = task["name"].as_str().context("module name missing")?;
    Ok(match compile(s, w, name, &updated) {
        Ok(()) => json!({"path":path,"changed":updated != original,"compiles":true}),
        Err(error) => {
            let diagnostic = program_flow::source_error("edit_file", &json!({"source":updated}), &error);
            json!({"path":path,"changed":updated != original,"compiles":false,"diagnostics":diagnostic["diagnostics"]})
        }
    })
}

/// The fixer says it is done. The file must compile; it becomes the module's
/// next version and the call runs again from its start. None when it ran;
/// otherwise the diagnostic to give back to the fixer.
pub(super) async fn fixed(s: &Session<'_>, w: &mut Window) -> Result<Option<Value>> {
    let task = w.program_task.clone().context("no module is being fixed")?;
    let contract = contract(&task)?;
    let path = task["fix"]["path"].as_str().context("no module is being fixed")?.to_owned();
    let failed_version = task["fix"]["version"].as_i64().unwrap_or(0);
    let source = std::fs::read_to_string(s.state.workspace.root().join(&path))?;
    if let Err(error) = compile(s, w, &contract.name, &source) {
        let mut diagnostic = program_flow::source_error("module", &json!({"source":source}), &error);
        diagnostic["instruction"] = json!(format!("{path} does not compile yet. Keep fixing it with edit_file, then reply again."));
        return Ok(Some(diagnostic));
    }
    let version = save(s, &contract, &source, true).await?;
    append(s, "program/start", json!({"turn":s.turn,"function":contract.name,"source":source,"module_version":version,"fixed":true}), false).await?;
    install(s, w, version, &source)?;
    release(&claim_key(s, &format!("fix:{}:{failed_version}", contract.key)), &owner(w));
    save_window(s, w).await?;
    Ok(None)
}

async fn save_window(s: &Session<'_>, w: &Window) -> Result<()> {
    super::save(s, w).await
}
