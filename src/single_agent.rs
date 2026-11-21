//! Context-as-Code v2 agent driver (Step 1: minimal pure-JS + Rust proxy loop).
//!
//! - Model ABI: raw JavaScript continuation inside `async function executeTask()`,
//!   zero JSON Schema `tools`, `.d.ts` host library declarations in `system`,
//!   and `return <<<` (or native JS `return <expr>;`) to finish a turn.
//! - Host ABI: Rust JS VM (`crate::agent_program::Runtime`) traps tool calls
//!   via `ActionKind::Tool` and proxies them into `host_tools` with full
//!   permission, sandbox, and file-baseline enforcement.
//! - Context layout: immutable Append-Only code + `// =>` receipts above, and
//!   one-step `EPHEMERAL_PEEK` at the bottom (100% KV-cache friendly).
//! - Per-turn isolation: every user turn starts with a brand-new empty VM and
//!   destroys it immediately upon emitting the final `assistant` reply.

#![allow(dead_code)]

use crate::agent_program::{self, Runtime, State};
use crate::agent_service::{self, AgentServiceState};
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

mod context_doc;
mod external_changes;
mod flow_runtime;
mod host_folder;
mod host_tools;
mod managed_files;
mod model_call;
pub(crate) mod modules;
mod task_frames;
#[cfg(test)]
mod tests;
mod tool_proxy;
mod virtual_frame;
mod vm_driver;

#[cfg(test)]
use host_tools::{business_tool, business_tool_inner};

/// Production loops have no step cap; tests pass small bounds of their own.
pub(crate) const NO_ROUND_LIMIT: usize = usize::MAX;
/// A completed turn is eligible for economic folding only after the provider's
/// short-lived prompt cache is expected to have expired.
pub(crate) const IDLE_CACHE_TTL_MS: u64 = 10 * 60 * 1000;

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis().min(u64::MAX as u128) as u64)
}

fn idle_soft_fold_due(
    now_ms: u64,
    last_model_request_at_ms: Option<u64>,
    active_turn_is_new: bool,
    has_unfolded_completed_turn: bool,
    estimated: usize,
    hard: usize,
    completed_turns: usize,
) -> bool {
    active_turn_is_new
        && last_model_request_at_ms.is_some_and(|last_ms| {
            now_ms.saturating_sub(last_ms) >= IDLE_CACHE_TTL_MS
        })
        && (has_unfolded_completed_turn
            || (estimated >= hard.saturating_div(2) && completed_turns > 1))
}

fn default_next_ref_id() -> usize {
    1
}

/// Durable runtime window for the v2 Context-as-Code driver.
#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct Window {
    #[serde(default)]
    pub(crate) context_doc: context_doc::ContextDocument,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) program: Option<Runtime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) program_task: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task_node_id: Option<String>,
    #[serde(default = "default_next_ref_id")]
    pub(crate) next_ref_id: usize,
    #[serde(default)]
    pub(crate) token_calibration: crate::context_window::TokenCalibration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pinned_system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_model_request_at_ms: Option<u64>,
}

/// Per-invocation host session context shared by the agent driver, host tool
/// backend, and flow libraries.
#[derive(Clone)]
pub(crate) struct Session<'a> {
    pub(super) agent_id: Option<String>,
    pub(super) task_node_id: Option<String>,
    pub(crate) plugin: Option<String>,
    pub(crate) state: &'a AgentServiceState,
    pub(crate) task: &'a str,
    pub(super) model: &'a str,
    pub(super) turn: usize,
    pub(super) cancel: &'a CancellationToken,
    pub(super) ban_execution: bool,
    pub(super) scope: String,
    pub(super) parent: Option<String>,
    pub(super) input_budget: usize,
}

impl Session<'_> {
    pub(super) fn delegate_id(&self) -> Option<&str> {
        self.agent_id.as_deref()
    }
}

async fn db<T: Send + 'static>(
    root: &Path,
    f: impl FnOnce(&mut rusqlite::Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    crate::db_writer::write_async(root, move |conn| {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_runtime_windows(
            task_id TEXT NOT NULL, scope TEXT NOT NULL, state TEXT NOT NULL,
            PRIMARY KEY(task_id,scope));",
        )?;
        f(conn)
    })
    .await
}

async fn save_window(s: &Session<'_>, w: &Window) -> Result<()> {
    let task = s.task.to_owned();
    let scope = s.scope.clone();
    let encoded = serde_json::to_string(w)?;
    db(s.state.workspace.root(), move |conn| {
        conn.execute(
            "INSERT OR REPLACE INTO agent_runtime_windows(task_id,scope,state) VALUES (?1,?2,?3)",
            params![task, scope, encoded],
        )?;
        Ok(())
    })
    .await
}

async fn append_event(s: &Session<'_>, kind: &str, mut data: Value, surface: bool) -> Result<i64> {
    if data.get("task_node_id").is_none()
        && let Some(id) = s.task_node_id.as_deref()
    {
        data["task_node_id"] = json!(id);
    }
    let task = s.task.to_owned();
    let kind = kind.to_owned();
    db(s.state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let seq =
            agent_service::append_event_tx(&tx, &task, &kind, &data, surface.then_some("append"))?;
        tx.commit()?;
        Ok(seq)
    })
    .await
}

/// Static host library scope available to a compiled JS program / flow library.
pub(crate) fn library_compile_scope(
    s: &Session<'_>,
    w: &Window,
) -> agent_program::libraries::Libraries {
    let allow_shell = s.plugin.is_none()
        && !s.ban_execution
        && w.program_task
            .as_ref()
            .is_none_or(|t| t["role"].as_str() != Some("leaf"));
    let mut libs = agent_program::libraries::builtin();
    if !allow_shell {
        libs.remove("process");
    } else if !crate::shell_process::read_only_available()
        && let Some(process_lib) = libs.get_mut("process")
    {
        process_lib.remove("run_readonly");
    }
    if s.plugin.is_some() {
        for methods in libs.values_mut() {
            methods.retain(|_, tool| host_tools::page_allows(s, tool));
        }
        libs.retain(|_, methods| !methods.is_empty());
    }
    if let Some(catalog) = s.state.tool_catalog.as_ref() {
        let mut packages = catalog.packages.catalog();
        if let Some(owner) = s.plugin.as_deref() {
            if let Some(entries) = packages.as_array_mut() {
                entries.retain(|package| package["id"] == owner);
            }
        }
        let active = packages
            .as_array()
            .into_iter()
            .flatten()
            .filter(|package| package["enabled"] == true)
            .filter_map(|package| package["id"].as_str().map(str::to_owned))
            .collect::<std::collections::BTreeSet<_>>();
        let plugin_definitions = catalog.packages.tools(&active);
        for (name, methods) in agent_program::libraries::catalog(&plugin_definitions, &packages) {
            if !libs.contains_key(&name) {
                libs.insert(name, methods);
            }
        }
    }
    libs
}

/// Cancel any persisted in-flight JS VM runtimes when a task is stopped.
pub(crate) async fn cancel_saved_programs(state: &AgentServiceState, task: &str) -> Result<()> {
    let task = task.to_owned();
    db(state.workspace.root(), move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let rows = tx
            .prepare("SELECT scope,state FROM agent_runtime_windows WHERE task_id=?1")?
            .query_map([&task], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (scope, raw) in rows {
            let Ok(mut window) = serde_json::from_str::<Window>(&raw) else {
                continue;
            };
            if let Some(program) = window.program.as_mut().filter(|p| !p.finished) {
                program.cancel();
                tx.execute(
                    "UPDATE agent_runtime_windows SET state=?3 WHERE task_id=?1 AND scope=?2",
                    params![task, scope, serde_json::to_string(&window)?],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

/// Primary agent entry point (convenience wrapper without explicit plugin resource).
pub(crate) async fn run_task(
    state: AgentServiceState,
    task: String,
    model: String,
    prompt: String,
    max_rounds: usize,
    cancel: CancellationToken,
    turn: usize,
    ban_execution: bool,
) -> Result<()> {
    run_task_with_resource(
        state,
        task,
        model,
        prompt,
        max_rounds,
        cancel,
        turn,
        ban_execution,
        None,
    )
    .await
}

/// Primary Context-as-Code v2 driver entry point (Step 1 minimal clean loop).
#[allow(unused_variables)]
pub(crate) async fn run_task_with_resource(
    state: AgentServiceState,
    task: String,
    model: String,
    prompt: String,
    max_rounds: usize,
    cancel: CancellationToken,
    turn: usize,
    ban_execution: bool,
    program_resource: Option<String>,
) -> Result<()> {
    let s = Session {
        agent_id: None,
        task_node_id: None,
        plugin: crate::plugin_sessions::owner(state.workspace.root(), &task)?,
        state: &state,
        task: &task,
        model: &model,
        turn,
        cancel: &cancel,
        ban_execution,
        scope: "main".into(),
        parent: None,
        input_budget: crate::model_settings::input_budget(&state, &model),
    };

    // Load prior conversation document (if any) and record rewind snapshot.
    let task_copy = task.clone();
    let saved_raw: Option<String> = db(state.workspace.root(), move |conn| {
        let main: Option<String> = conn
            .query_row(
                "SELECT state FROM agent_runtime_windows WHERE task_id=?1 AND scope='main'",
                [&task_copy],
                |r| r.get(0),
            )
            .optional()?;
        crate::rewind::snapshot(conn, &task_copy, turn, main.as_deref())?;
        Ok(main)
    })
    .await?;

    let mut w: Window = saved_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();

    // Between turns the user owns workspace changes. Advance managed-file
    // baselines before the new user message so the next edit sees the current
    // world and the immutable history remains untouched.
    let moved_baselines = managed_files::refresh_baselines(state.workspace.root(), &task)?;
    let prompt = if moved_baselines.is_empty() {
        prompt
    } else {
        format!(
            "{prompt}\n\n[Files changed between turns: {}]",
            moved_baselines.join(", ")
        )
    };
    let libs = library_compile_scope(&s, &w);
    if w.pinned_system_prompt.is_none() {
        w.pinned_system_prompt = Some(model_call::pinned_system_prompt(&s, &libs));
    }

    // Emit durable turn/start and user/message events for frontend timeline.
    append_event(
        &s,
        "turn/start",
        json!({
            "turn": turn,
            "runtime": "single_agent",
        }),
        false,
    )
    .await?;

    append_event(
        &s,
        "user/message",
        json!({
            "task_id": task,
            "turn": turn,
            "message": {
                "id": agent_service::uuid_like(),
                "role": "user",
                "content": [{"type": "text", "text": prompt}]
            }
        }),
        true,
    )
    .await?;

    // Every new user turn ALWAYS starts with a brand-new empty JS VM!
    w.program = Some(Runtime::new_repl(libs.clone())?);
    w.next_ref_id = 1;
    w.context_doc.begin_turn(turn, &prompt);
    save_window(&s, &w).await?;

    let allow_shell = s.plugin.is_none() && !s.ban_execution;

    for step in 1..=max_rounds {
        if cancel.is_cancelled() {
            let manifest =
                snapshot_turn_state(&s, &w).unwrap_or_else(|_| "{\"frozen\":true}".into());
            w.context_doc
                .finish_turn("[Turn aborted before final reply.]");
            let code_asset_id = fold_code_asset(&s, &w);
            if let Some(turn_doc) = w.context_doc.turns.iter_mut().find(|turn| turn.turn == s.turn) {
                turn_doc.snapshot_manifest = Some(manifest);
                turn_doc.snapshot_code_asset_id = code_asset_id;
            }
            w.program = None;
            save_window(&s, &w).await?;
            let _ = append_event(
                &s,
                "turn/end",
                json!({"turn": turn, "reason": {"kind": "aborted"}}),
                false,
            )
            .await;
            let _ = agent_service::finish(s.state, s.task, "cancelled").await;
            bail!("cancelled");
        }

        append_event(
            &s,
            "step/start",
            json!({
                "actor": "agent",
                "turn": s.turn,
                "step": step,
            }),
            false,
        )
        .await?;

        let estimated_request = maybe_fold_before_request(&s, &mut w, &libs).await?;

        let reply = match model_call::call_model(&s, &mut w, &libs, step).await {
            Ok(reply) => reply,
            Err(error) if is_context_length_error(&error) => {
                force_fold_active_prefix(&s, &mut w).await?;
                model_call::call_model(&s, &mut w, &libs, step).await?
            }
            Err(error) => return Err(error),
        };
        // The timestamp is persisted with the next window save and is deliberately
        // recorded only after a model response has completed successfully.
        w.last_model_request_at_ms = Some(unix_now_ms());
        #[cfg(test)]
        if std::env::var_os("LIVE_DEBUG_STEPS").is_some() {
            let head: String = reply.content.chars().take(160).collect();
            let tc = reply
                .tool_calls
                .first()
                .map(|c| format!("{}({})", c.name, c.arguments.len()));
            eprintln!(
                "[LIVE step={step} finish={:?} tc={tc:?} text_len={}] head={head:?}",
                reply.finish_reason,
                reply.content.len()
            );
        }
        let submission = model_call::parse_main_submission(&reply);
        let assistant_msg = Some(reply.assistant_message.clone());
        let call_id = reply.primary_call_id();
        let remaining_budget_chars = s
            .input_budget
            .saturating_sub(estimated_request)
            .saturating_mul(4);
        w.context_doc.set_inline_budget_chars(remaining_budget_chars);

        match submission {
            crate::model_stream::RunSubmission::Rejected { code, message } => {
                let raw_preview: String = reply.content.chars().take(600).collect();
                append_event(
                    &s,
                    "program/source_invalid",
                    json!({
                        "actor": "agent",
                        "turn": s.turn,
                        "step": step,
                        "code": code,
                        "error": message,
                        "diagnostic": {"error": message},
                        "raw_preview": raw_preview,
                    }),
                    false,
                )
                .await?;
                w.context_doc
                    .record_rejection(assistant_msg, call_id, code, &message);
                save_window(&s, &w).await?;
            }

            crate::model_stream::RunSubmission::Reply { markdown } => {
                finish_turn_with_reply(&s, &mut w, step, markdown).await?;
                return Ok(());
            }

            crate::model_stream::RunSubmission::Run { source } => {
                if execute_js_step(
                    &s,
                    &mut w,
                    &libs,
                    step,
                    allow_shell,
                    source,
                    None,
                    assistant_msg,
                    call_id,
                )
                .await?
                {
                    return Ok(());
                }
            }
            crate::model_stream::RunSubmission::CodeAndReply { source, markdown } => {
                if execute_js_step(
                    &s,
                    &mut w,
                    &libs,
                    step,
                    allow_shell,
                    source,
                    Some(markdown),
                    assistant_msg,
                    call_id,
                )
                .await?
                {
                    return Ok(());
                }
            }
        }
    }

    // Reached max_rounds (mainly test cap); persist current state cleanly.
    save_window(&s, &w).await?;
    Ok(())
}

async fn maybe_fold_before_request(
    s: &Session<'_>,
    w: &mut Window,
    libs: &agent_program::libraries::Libraries,
) -> Result<usize> {
    let body = model_call::build_request_body(s, w, libs);
    let route = format!("{}/{}", s.state.provider_name, s.model);
    let mut estimated = w.token_calibration.estimate(&body, &route);
    let hard = crate::context_window::hard_limit(
        s.model,
        s.input_budget,
        crate::model_settings::long_context_threshold(s.state, s.model),
    );
    let mut changed = false;
    let active_turn_is_new = w
        .context_doc
        .active_turn()
        .is_some_and(|turn| turn.segments.is_empty());
    let completed: Vec<usize> = w
        .context_doc
        .turns
        .iter()
        .filter(|turn| turn.turn < s.turn && turn.assistant_reply.is_some())
        .map(|turn| turn.turn)
        .collect();
    let has_unfolded_completed_turn = completed.iter().any(|turn_number| {
        w.context_doc
            .turns
            .iter()
            .find(|turn| turn.turn == *turn_number)
            .is_some_and(|turn| turn.folded.is_none())
    });
    if idle_soft_fold_due(
        unix_now_ms(),
        w.last_model_request_at_ms,
        active_turn_is_new,
        has_unfolded_completed_turn,
        estimated,
        hard,
        completed.len(),
    ) {
        let before_tokens = estimated;
        let mut folded_turns = 0usize;
        for turn_number in completed.iter().copied() {
            let Some(turn_doc) = w.context_doc.turns.iter().find(|turn| turn.turn == turn_number)
            else {
                continue;
            };
            if turn_doc.folded.is_some() {
                continue;
            }
            let manifest = turn_doc.snapshot_manifest.clone().unwrap_or_else(|| {
                json!({
                    "kind": "frozen_turn_snapshots",
                    "turn": turn_number,
                    "frozen": true,
                    "read": "session.snapshot({ asset_id })"
                })
                .to_string()
            });
            let code_asset_id = turn_doc.snapshot_code_asset_id.clone();
            if w.context_doc.fold_turn(turn_number, manifest, code_asset_id) {
                folded_turns += 1;
                changed = true;
            }
        }
        if changed {
            let refreshed = model_call::build_request_body(s, w, libs);
            estimated = w.token_calibration.estimate(&refreshed, &route);
        }

        // After an idle Level-1 fold, keep the conclusions of the latest two
        // completed turns in the hot rendering window. Older folded turns stay
        // durable and remain recoverable through the annotated manifest.
        let mut level2_changed = false;
        if completed.len() > 2 && estimated >= hard.saturating_div(2) {
            let keep_from = completed[completed.len() - 2];
            if w
                .context_doc
                .active_start_turn
                .map_or(true, |active_start| active_start < keep_from) {
                w.context_doc.active_start_turn = Some(keep_from);
                if let Some(retained) = w
                    .context_doc
                    .turns
                    .iter_mut()
                    .find(|turn| turn.turn == keep_from)
                    && let Some(folded) = retained.folded.as_mut()
                {
                    let mut manifest: Value =
                        serde_json::from_str(&folded.manifest).unwrap_or_else(|_| json!({}));
                    manifest["archived_before_turn"] = json!(keep_from);
                    manifest["archived_access"] =
                        json!("session.read_history({ turn: ... })");
                    folded.manifest = manifest.to_string();
                }
                level2_changed = true;
                changed = true;
                let refreshed = model_call::build_request_body(s, w, libs);
                estimated = w.token_calibration.estimate(&refreshed, &route);
            }
        }
        if folded_turns > 0 || level2_changed {
            let idle_ms = w
                .last_model_request_at_ms
                .map(|last_ms| unix_now_ms().saturating_sub(last_ms))
                .unwrap_or_default();
            tracing::info!(
                task = s.task,
                turn = s.turn,
                idle_ms,
                before_tokens,
                after_tokens = estimated,
                folded_turns,
                "idle soft-fold completed"
            );
        }
    }
    if estimated >= hard {
        let completed: Vec<usize> = w
            .context_doc
            .turns
            .iter()
            .filter(|turn| {
                turn.turn != s.turn && turn.assistant_reply.is_some() && turn.folded.is_none()
            })
            .map(|turn| turn.turn)
            .collect();
        for turn in completed {
            let (manifest, code_asset_id) = w.context_doc.turns.iter().find(|doc| doc.turn == turn)
                .map(|doc| (doc.snapshot_manifest.clone().unwrap_or_else(|| json!({"kind":"frozen_turn_snapshots","turn":turn,"frozen":true,"read":"session.snapshot({ asset_id })"}).to_string()), doc.snapshot_code_asset_id.clone()))
                .unwrap_or_else(|| (json!({"kind":"frozen_turn_snapshots","turn":turn,"frozen":true,"read":"session.snapshot({ asset_id })"}).to_string(), None));
            changed |= w.context_doc.fold_turn(turn, manifest, code_asset_id);
        }
    }
    if changed {
        let refreshed = model_call::build_request_body(s, w, libs);
        estimated = w.token_calibration.estimate(&refreshed, &route);
    }
    if estimated >= hard {
        let completed: Vec<usize> = w
            .context_doc
            .turns
            .iter()
            .filter(|turn| turn.turn < s.turn && turn.assistant_reply.is_some())
            .map(|turn| turn.turn)
            .collect();
        if completed.len() > 1 {
            let keep_from = *completed.last().unwrap();
            if w.context_doc.active_start_turn != Some(keep_from) {
                w.context_doc.active_start_turn = Some(keep_from);
                if let Some(retained) = w.context_doc.turns.iter_mut().find(|turn| turn.turn == keep_from)
                    && let Some(folded) = retained.folded.as_mut()
                {
                    let mut manifest: Value = serde_json::from_str(&folded.manifest).unwrap_or_else(|_| json!({}));
                    manifest["archived_before_turn"] = json!(keep_from);
                    manifest["archived_access"] = json!("session.read_history({ turn: ... })");
                    folded.manifest = manifest.to_string();
                }
                changed = true;
                let refreshed = model_call::build_request_body(s, w, libs);
                estimated = w.token_calibration.estimate(&refreshed, &route);
            }
        }
    }
    while estimated >= hard {
        let Some(turn) = w.context_doc.active_turn() else { break };
        let cutoff = active_prefix_cutoff(turn, 5);
        let existing_cutoff = turn.folded_prefix.as_ref().map_or(0, |fold| fold.cutoff);
        if cutoff == 0 || cutoff <= existing_cutoff {
            break;
        }
        let manifest = snapshot_turn_state(s, w).unwrap_or_else(|_| {
            json!({"kind":"frozen_prefix_snapshots","turn":s.turn,"frozen":true,"live_vm":true,"read":"session.snapshot({ asset_id })"}).to_string()
        });
        let code_asset_id = fold_prefix_code_asset(s, w, cutoff);
        if !w
            .context_doc
            .fold_active_prefix(s.turn, cutoff, manifest, code_asset_id)
        {
            break;
        }
        changed = true;
        let refreshed = model_call::build_request_body(s, w, libs);
        estimated = w.token_calibration.estimate(&refreshed, &route);
        if estimated < hard.saturating_mul(3) / 4 {
            break;
        }
    }
    if changed {
        save_window(s, w).await?;
    }
    Ok(estimated)
}

fn is_context_length_error(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    text.contains("context_length_exceeded")
        || text.contains("context length")
        || text.contains("maximum context")
}

fn active_prefix_cutoff(turn: &context_doc::TurnDoc, keep: usize) -> usize {
    let mut remaining = keep;
    for (index, segment) in turn.segments.iter().enumerate().rev() {
        if !segment.is_rejection && !segment.source.trim().is_empty() {
            remaining = remaining.saturating_sub(1);
            if remaining == 0 {
                return index;
            }
        }
    }
    0
}

fn fold_prefix_code_asset(s: &Session<'_>, w: &Window, cutoff: usize) -> Option<String> {
    let turn = w
        .context_doc
        .turns
        .iter()
        .find(|turn| turn.turn == s.turn)?;
    let code: String = turn.segments[..cutoff]
        .iter()
        .filter(|segment| !segment.is_rejection && !segment.source.trim().is_empty())
        .map(|segment| format!("{}\n", segment.source))
        .collect();
    (code.chars().count() > 16 * 1024).then(|| {
        crate::context_assets::register_pending(
            s.state.workspace.root(),
            s.task,
            "turn_folded_prefix_code",
            &json!({"turn":s.turn,"cutoff":cutoff}),
            &json!({"turn":s.turn,"cutoff":cutoff,"code":code}),
        )
        .ok()
        .and_then(|reference| reference["asset_id"].as_str().map(str::to_owned))
    })?
}

async fn force_fold_active_prefix(s: &Session<'_>, w: &mut Window) -> Result<()> {
    let completed: Vec<usize> = w
        .context_doc
        .turns
        .iter()
        .filter(|turn| turn.turn < s.turn && turn.assistant_reply.is_some())
        .map(|turn| turn.turn)
        .collect();
    for turn in &completed {
        let (manifest, code_asset_id) = w
            .context_doc
            .turns
            .iter()
            .find(|doc| doc.turn == *turn)
            .map(|doc| {
                (
                    doc.snapshot_manifest.clone().unwrap_or_else(|| {
                        json!({"kind":"frozen_turn_snapshots","turn":turn,"frozen":true,"read":"session.snapshot({ asset_id })"}).to_string()
                    }),
                    doc.snapshot_code_asset_id.clone(),
                )
            })
            .unwrap_or_else(|| {
                (
                    json!({"kind":"frozen_turn_snapshots","turn":turn,"frozen":true,"read":"session.snapshot({ asset_id })"}).to_string(),
                    None,
                )
            });
        let _ = w.context_doc.fold_turn(*turn, manifest, code_asset_id);
    }
    if completed.len() > 1 {
        w.context_doc.active_start_turn = completed.last().copied();
    }
    if let Some(turn) = w.context_doc.active_turn() {
        let existing_cutoff = turn.folded_prefix.as_ref().map_or(0, |fold| fold.cutoff);
        let cutoff = [5usize, 2, 1]
            .into_iter()
            .map(|keep| active_prefix_cutoff(turn, keep))
            .find(|&candidate| candidate > existing_cutoff)
            .unwrap_or(0);
        if cutoff > 0 {
            let manifest = snapshot_turn_state(s, w).unwrap_or_else(|_| {
                json!({"kind":"forced_context_guard","turn":s.turn,"frozen":true,"live_vm":true,"read":"session.snapshot({ asset_id })"}).to_string()
            });
            let code_asset_id = fold_prefix_code_asset(s, w, cutoff);
            let _ = w
                .context_doc
                .fold_active_prefix(s.turn, cutoff, manifest, code_asset_id);
        }
    }
    save_window(s, w).await
}

/// Execute one JS step (with optional trailing `return <<<` reply).
/// Returns `Ok(true)` if the turn finished and delivered the final reply.
#[allow(clippy::too_many_arguments)]
async fn execute_js_step(
    s: &Session<'_>,
    w: &mut Window,
    libs: &agent_program::libraries::Libraries,
    step: usize,
    allow_shell: bool,
    source: String,
    pending_reply: Option<String>,
    assistant_msg: Option<Value>,
    call_id: Option<String>,
) -> Result<bool> {
    let Some(mut runtime) = w.program.take() else {
        bail!("turn VM missing");
    };
    runtime.install_static_libraries(libs)?;
    let pre_fps = runtime.user_binding_fingerprints();
    let log_start = runtime.logs_len();

    // Main turn allows declaring top-level `@virtual` functions (`is_virtual_leaf = false`).
    if let Err(compile_err) = runtime.prepare_segment(&source, libs.clone(), false) {
        let msg = format!("{compile_err:#}");
        append_event(
            s,
            "program/source_invalid",
            json!({
                "actor": "agent",
                "turn": s.turn,
                "step": step,
                "code": "COMPILE_ERROR",
                "error": msg,
                "diagnostic": {"error": msg},
                "source": source,
            }),
            false,
        )
        .await?;
        w.program = Some(runtime);
        w.context_doc
            .record_rejection(assistant_msg, call_id, "COMPILE_ERROR", &msg);
        save_window(s, w).await?;
        return Ok(false);
    }

    let node = format!("step_{}_{}", s.turn, step);
    append_event(
        s,
        "program/start",
        json!({
            "actor": "agent",
            "turn": s.turn,
            "step": step,
            "source": source,
            "lines": source.lines().count(),
        }),
        false,
    )
    .await?;
    if w.next_ref_id == 0 {
        w.next_ref_id = 1;
    }
    let mut fallback_file_refs = std::collections::BTreeMap::new();
    let file_refs = w
        .context_doc
        .active_turn_mut()
        .map(|turn| &mut turn.file_handle_refs)
        .unwrap_or(&mut fallback_file_refs);
    let (final_state, external_tool_calls, hints) = vm_driver::drive_vm_segment(
        s,
        &mut runtime,
        libs,
        &node,
        step,
        allow_shell,
        &mut w.next_ref_id,
        file_refs,
    )
    .await?;
    let logs = runtime.logs_since(log_start);

    match final_state {
        State::Returned(val) => {
            if runtime.segment_returned_explicitly() && external_tool_calls == 0 {
                let answer = match val {
                    Value::String(text) => text,
                    Value::Null => String::new(),
                    other => serde_json::to_string_pretty(&other).unwrap_or_default(),
                };
                w.context_doc.record_completed_segment(
                    source,
                    &runtime,
                    &pre_fps,
                    &logs,
                    None,
                    &hints,
                    assistant_msg,
                    call_id,
                );
                finish_turn_with_reply(s, w, step, answer).await?;
                return Ok(true);
            }
            if let Some(reply_md) = pending_reply {
                if external_tool_calls == 0 {
                    // Pure in-memory JS projection + final reply: record tool output and finish!
                    w.context_doc.record_completed_segment(
                        source,
                        &runtime,
                        &pre_fps,
                        &logs,
                        None,
                        &hints,
                        assistant_msg,
                        call_id,
                    );
                    finish_turn_with_reply(s, w, step, reply_md).await?;
                    return Ok(true);
                }
                // First-Trap Truncation: external host tools/virtual calls ran, so the trailing
                // `return <<<` was written before observing real results — discard it!
                let note = format!(
                    "// [Host note: executed your JS above ({external_tool_calls} host/virtual call(s)) and discarded the premature `return <<<` so you can inspect the real result below before replying.]"
                );
                w.context_doc.record_completed_segment(
                    source,
                    &runtime,
                    &pre_fps,
                    &logs,
                    Some(note),
                    &hints,
                    assistant_msg,
                    call_id,
                );
                w.program = Some(runtime);
                save_window(s, w).await?;
                return Ok(false);
            }
            let note = (runtime.segment_returned_explicitly() && external_tool_calls > 0).then(|| {
                format!(
                    "// [Host note: executed your JS above ({external_tool_calls} host/virtual call(s)); inspect the real output below before returning your final answer.]"
                )
            });
            w.context_doc.record_completed_segment(
                source,
                &runtime,
                &pre_fps,
                &logs,
                note,
                &hints,
                assistant_msg,
                call_id,
            );
            w.program = Some(runtime);
            save_window(s, w).await?;
            Ok(false)
        }
        State::Failed(err) => {
            if agent_program::runtime::is_user_blocker(&err) {
                let question = err["question"]
                    .as_str()
                    .or_else(|| err["message"].as_str())
                    .map(str::trim)
                    .filter(|q| !q.is_empty())
                    .unwrap_or("需要你的确认后才能继续。")
                    .to_owned();
                w.context_doc.record_paused_for_user(
                    source,
                    &runtime,
                    &pre_fps,
                    &logs,
                    &hints,
                    assistant_msg,
                    call_id,
                );
                append_event(
                    s,
                    "agent/needs_user",
                    json!({
                        "actor": "agent",
                        "turn": s.turn,
                        "step": step,
                        "question": question,
                    }),
                    false,
                )
                .await?;
                finish_turn_with_reply(s, w, step, question).await?;
                return Ok(true);
            }
            append_event(
                s,
                "program/error",
                json!({
                    "actor": "agent",
                    "turn": s.turn,
                    "step": step,
                    "error": err,
                }),
                false,
            )
            .await?;
            w.context_doc.record_failed_segment(
                source,
                &runtime,
                &pre_fps,
                &logs,
                &err,
                assistant_msg,
                call_id,
            );
            w.program = Some(runtime);
            save_window(s, w).await?;
            Ok(false)
        }
        State::Action(_) | State::Waiting => {
            w.program = Some(runtime);
            save_window(s, w).await?;
            bail!("VM suspended in an unexpected waiting state");
        }
    }
}

async fn finish_turn_with_reply(
    s: &Session<'_>,
    w: &mut Window,
    step: usize,
    markdown: String,
) -> Result<()> {
    let manifest = snapshot_turn_state(s, w).unwrap_or_else(|_| {
        "// Frozen turn snapshots unavailable; no live VM state was re-read.\n".to_owned()
    });
    // Archive clean turn JS trace into `host_folder` (best-effort) when JS ran.
    if let Some(turn_js) = w.context_doc.render_turn_js(s.turn) {
        let _ = host_folder::write_program(
            s.state.workspace.root(),
            s.task,
            &format!("turn-{:02}", s.turn),
            &turn_js,
        );
    }
    // Strip EPHEMERAL_PEEK, pair `(user #N, assistant #N)`, and unconditionally destroy VM!
    w.context_doc.finish_turn(&markdown);
    let code_asset_id = fold_code_asset(s, w);
    if let Some(turn_doc) = w.context_doc.turns.iter_mut().find(|turn| turn.turn == s.turn) {
        turn_doc.snapshot_manifest = Some(manifest);
        turn_doc.snapshot_code_asset_id = code_asset_id;
    }
    w.program = None;
    append_event(
        s,
        "assistant/message",
        json!({
            "actor": "agent",
            "turn": s.turn,
            "step": step,
            "message": {
                "role": "assistant",
                "content": markdown,
            }
        }),
        true,
    )
    .await?;
    save_window(s, w).await?;
    append_event(
        s,
        "turn/end",
        json!({
            "turn": s.turn,
            "reason": {"kind": "completed"},
        }),
        false,
    )
    .await?;
    let _ = agent_service::finish(s.state, s.task, "completed").await;
    Ok(())
}

fn fold_code_asset(s: &Session<'_>, w: &Window) -> Option<String> {
    let turn = w
        .context_doc
        .turns
        .iter()
        .find(|turn| turn.turn == s.turn)?;
    let code: String = turn
        .segments
        .iter()
        .filter(|segment| !segment.is_rejection && !segment.source.trim().is_empty())
        .map(|segment| format!("{}\n", segment.source))
        .collect();
    (code.chars().count() > 16 * 1024).then(|| {
        crate::context_assets::register_pending(
            s.state.workspace.root(),
            s.task,
            "turn_folded_code",
            &json!({"turn":s.turn}),
            &json!({"turn":s.turn,"code":code}),
        )
        .ok()
        .and_then(|reference| reference["asset_id"].as_str().map(str::to_owned))
    })?
}

/// Freeze the runtime bindings and all receipts before the VM is destroyed.
/// The assets are content-addressed by `context_assets` and are subsequently
/// read only through `session.snapshot`, never through their source paths.
fn snapshot_turn_state(s: &Session<'_>, w: &Window) -> Result<String> {
    let turn = w.context_doc.turns.iter().find(|turn| turn.turn == s.turn);
    let mut asset_refs = std::collections::BTreeSet::new();
    let mut notes = Vec::new();
    if let Some(turn) = turn {
        for segment in &turn.segments {
            for receipt in &segment.inline_receipts {
                for token in receipt.split_whitespace() {
                    let id =
                        token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-');
                    if id.starts_with("asset-") {
                        asset_refs.insert(format!("asset: {id}"));
                    }
                }
            }
            if let Some(error) = &segment.error_note {
                let mut note: String = error.chars().take(600).collect();
                if error.chars().count() > 600 {
                    note.push_str("…");
                }
                notes.push(note);
            }
        }
    }
    let bindings = w
        .program
        .as_ref()
        .map(|runtime| {
            let mut values = serde_json::Map::new();
            for (name, value) in runtime.named_bindings() {
                if !name.starts_with('\0') && name != "$_" {
                    values.insert(name, value);
                }
            }
            Value::Object(values)
        })
        .unwrap_or_else(|| json!({}));
    struct BindingCandidate {
        names: Vec<String>,
        value: Value,
        kind: &'static str,
        bytes: usize,
        path: Option<String>,
        exit_code: Option<Value>,
        content_key: String,
    }
    let mut candidates = Vec::<BindingCandidate>::new();
    let mut by_content = std::collections::HashMap::<String, usize>::new();
    let mut latest_by_path = std::collections::HashMap::<String, usize>::new();
    let mut small_bindings = serde_json::Map::new();
    if let Some(object) = bindings.as_object() {
        for (name, value) in object {
            let serialized = serde_json::to_vec(value)?;
            let object = value.as_object();
            let is_file = object.is_some_and(|o| {
                o.get("path").and_then(Value::as_str).is_some()
                    && o.get("content").and_then(Value::as_str).is_some()
            });
            let is_command = object.is_some_and(|o| o.get("exit_code").is_some());
            let is_large = serialized.len() > 4 * 1024;
            if is_file || is_command || is_large {
                let kind = if is_file { "file" } else if is_command { "command" } else { "json" };
                let path = object.and_then(|o| o.get("path")).and_then(Value::as_str).map(str::to_owned);
                let content_key = crate::visual_artifacts::hash(&serialized);
                if let Some(previous) = path.as_ref().and_then(|path| latest_by_path.get(path).copied()) {
                    if candidates[previous].content_key != content_key {
                        // A later binding for the same path is the current version.
                        let previous_key = candidates[previous].content_key.clone();
                        if by_content.get(&previous_key) == Some(&previous) {
                            by_content.remove(&previous_key);
                        }
                        for previous_name in candidates[previous].names.clone() {
                            small_bindings.insert(previous_name, candidates[previous].value.clone());
                        }
                        candidates[previous].names.clear();
                    }
                }
                if let Some(&index) = by_content.get(&content_key) {
                    candidates[index].names.push(name.clone());
                    if let Some(path) = path.as_ref() {
                        latest_by_path.insert(path.clone(), index);
                    }
                    continue;
                }
                let index = candidates.len();
                candidates.push(BindingCandidate {
                    names: vec![name.clone()],
                    value: value.clone(),
                    kind,
                    bytes: serialized.len(),
                    path: path.clone(),
                    exit_code: object.and_then(|o| o.get("exit_code")).cloned(),
                    content_key: content_key.clone(),
                });
                by_content.insert(content_key, index);
                if let Some(path) = path {
                    latest_by_path.insert(path, index);
                }
            } else {
                small_bindings.insert(name.clone(), value.clone());
            }
        }
    }
    let mut selected = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| !candidate.names.is_empty())
        .collect::<Vec<_>>();
    selected.sort_by_key(|(_, candidate)| {
        let priority = match candidate.kind {
            "file" => 0,
            "command" => 1,
            _ => 2,
        };
        (priority, std::cmp::Reverse(candidate.names.first().cloned().unwrap_or_default()))
    });
    let selected_indices = selected.iter().take(16).map(|(index, _)| *index).collect::<std::collections::HashSet<_>>();
    let mut binding_assets = Vec::new();
    for (index, candidate) in candidates.into_iter().enumerate() {
        if candidate.names.is_empty() {
            continue;
        }
        if !selected_indices.contains(&index) {
            for name in candidate.names {
                small_bindings.insert(name, candidate.value.clone());
            }
            continue;
        }
        let asset = crate::context_assets::register_pending(
            s.state.workspace.root(),
            s.task,
            &format!("turn_binding_{}", candidate.kind),
            &json!({"turn":s.turn,"name":candidate.names.first().cloned().unwrap_or_default()}),
            &candidate.value,
        )?;
        let mut entry = json!({
            "name": candidate.names.first().cloned().unwrap_or_default(),
            "asset_id": asset["asset_id"],
            "kind": candidate.kind,
            "bytes": candidate.bytes,
            "lines": candidate.value.get("content").and_then(Value::as_str).or_else(|| candidate.value.get("stdout").and_then(Value::as_str)).map(|text| text.lines().count()),
        });
        if candidate.names.len() > 1 {
            entry["aliases"] = json!(candidate.names.into_iter().skip(1).collect::<Vec<_>>());
        }
        if let Some(path) = candidate.path {
            entry["path"] = json!(path);
        }
        if let Some(exit_code) = candidate.exit_code {
            entry["exit_code"] = exit_code;
        }
        binding_assets.push(entry);
    }
    let small_bindings_ref = crate::context_assets::register_pending(
        s.state.workspace.root(),
        s.task,
        "turn_bindings",
        &json!({"turn":s.turn}),
        &Value::Object(small_bindings),
    )?;
    let outputs = json!({
        "turn": s.turn,
        "step_outputs": w.context_doc.turns.iter().find(|turn| turn.turn == s.turn)
            .map(|turn| turn.segments.iter().map(|segment| json!({
                "inline_receipts": segment.inline_receipts.clone(),
                "error": segment.error_note.clone(),
            })).collect::<Vec<_>>()).unwrap_or_default(),
    });
    notes.truncate(4);
    for note in &mut notes {
        if note.chars().count() > 240 {
            *note = note.chars().take(240).collect::<String>() + "…";
        }
    }
    let binding_asset_ids = binding_assets
        .iter()
        .filter_map(|entry| entry["asset_id"].as_str().map(|id| format!("asset: {id}")))
        .collect::<std::collections::BTreeSet<_>>();
    let asset_refs = asset_refs
        .into_iter()
        .filter(|asset| !binding_asset_ids.contains(asset))
        .take(16)
        .collect::<Vec<_>>();
    let outputs_ref = crate::context_assets::register_pending(
        s.state.workspace.root(),
        s.task,
        "step_outputs",
        &json!({"turn":s.turn}),
        &outputs,
    )?;
    Ok(json!({
        "kind":"frozen_turn_snapshots",
        "turn":s.turn,
        "turn_bindings":{"asset_id":small_bindings_ref["asset_id"],"read":"session.snapshot({ asset_id: ... })"},
        "binding_assets":binding_assets,
        "step_outputs":{"asset_id":outputs_ref["asset_id"],"read":"session.snapshot({ asset_id: ... })"},
        "asset_refs":asset_refs,
        "notes":notes,
        "frozen":true,
    }).to_string())
}

#[cfg(test)]
mod context_guard_tests {
    use super::*;

    #[test]
    fn context_length_errors_trigger_the_single_retry_path() {
        assert!(is_context_length_error(&anyhow::anyhow!(
            "400 context_length_exceeded"
        )));
        assert!(is_context_length_error(&anyhow::anyhow!(
            "maximum context window reached"
        )));
        assert!(!is_context_length_error(&anyhow::anyhow!("network timeout")));
    }

    #[test]
    fn idle_soft_fold_respects_hot_cache_and_turn_boundary() {
        assert!(!idle_soft_fold_due(
            IDLE_CACHE_TTL_MS - 1,
            Some(0),
            true,
            true,
            1,
            100,
            1,
        ));
        assert!(!idle_soft_fold_due(
            IDLE_CACHE_TTL_MS,
            Some(0),
            false,
            true,
            1,
            100,
            1,
        ));
        assert!(idle_soft_fold_due(
            IDLE_CACHE_TTL_MS,
            Some(0),
            true,
            true,
            1,
            100,
            1,
        ));
    }

    #[test]
    fn idle_soft_fold_keeps_folded_reply_and_snapshot_recall() {
        let mut doc = context_doc::ContextDocument::default();
        doc.begin_turn(1, "old topic");
        doc.active_turn_mut().unwrap().segments.push(context_doc::TraceSegment {
            source: "const old = 1;".into(),
            inline_receipts: vec!["// => old = 1".into()],
            error_note: None,
            assistant_message: Some(json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "call-old",
                    "type": "function",
                    "function": {
                        "name": "run_js",
                        "arguments": "{\"code\":\"const old = 1;\"}"
                    }
                }]
            })),
            tool_call_id: Some("call-old".into()),
            is_rejection: false,
        });
        doc.finish_turn("old assistant reply");
        doc.begin_turn(2, "new topic");
        let before = serde_json::to_string(&doc.render_conversation_messages()).unwrap();
        assert!(idle_soft_fold_due(
            IDLE_CACHE_TTL_MS,
            Some(0),
            true,
            true,
            10,
            100,
            1,
        ));
        assert!(doc.fold_turn(
            1,
            r#"{"frozen":true,"read":"session.snapshot({ asset_id })"}"#,
            None,
        ));
        let after = serde_json::to_string(&doc.render_conversation_messages()).unwrap();
        assert_ne!(after, before, "folding must change the rendered wire");
        let wire = doc.render_conversation_messages();
        assert!(wire.iter().any(|message| message["content"] == "old assistant reply"));
        assert!(wire.iter().any(|message| {
            message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("session.snapshot"))
        }));
    }
}
