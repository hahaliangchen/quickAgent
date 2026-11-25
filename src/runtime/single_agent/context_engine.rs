//! Durable turn windows, context folding, and turn lifecycle.

use super::{
    IDLE_CACHE_TTL_MS, Session, context_doc, host_folder, library_compile_scope, managed_files,
    model_call,
};
use crate::{
    runtime::agent_program::{Runtime, libraries::Libraries},
    server::agent_service::{self, AgentServiceState},
};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u64::MAX as u128) as u64
        })
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
        && last_model_request_at_ms
            .is_some_and(|last_ms| now_ms.saturating_sub(last_ms) >= IDLE_CACHE_TTL_MS)
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
    pub(crate) token_calibration: crate::context::context_window::TokenCalibration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pinned_system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_model_request_at_ms: Option<u64>,
}

pub(super) struct ContextEngine<'s, 'a> {
    session: &'s Session<'a>,
}

impl<'s, 'a> ContextEngine<'s, 'a> {
    pub(super) fn new(session: &'s Session<'a>) -> Self {
        Self { session }
    }

    // Load the prior document and record the rewind snapshot before mutation.
    async fn load_window(&self) -> Result<Window> {
        let s = self.session;
        let task_copy = s.task.to_owned();
        let turn = s.turn;
        let root = s.state.workspace.root();
        let saved_raw: Option<String> = db(root, move |conn| {
            let main: Option<String> = conn
                .query_row(
                    "SELECT state FROM agent_runtime_windows WHERE task_id=?1 AND scope='main'",
                    [&task_copy],
                    |r| r.get(0),
                )
                .optional()?;
            crate::context::rewind::snapshot(conn, &task_copy, turn, main.as_deref())?;
            Ok(main)
        })
        .await?;

        Ok(saved_raw
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default())
    }

    pub(super) async fn begin_turn(&self, prompt: String) -> Result<(Window, Libraries)> {
        let s = self.session;
        let task = s.task;
        let turn = s.turn;
        let state = s.state;
        let mut w = self.load_window().await?;
        // Between turns the user owns workspace changes. Advance managed-file
        // baselines before the new user message so the next edit sees the current
        // world and the immutable history remains untouched.
        let moved_baselines = managed_files::refresh_baselines(state.workspace.root(), task)?;
        let prompt = if moved_baselines.is_empty() {
            prompt
        } else {
            format!(
                "{prompt}\n\n[Files changed between turns: {}]",
                moved_baselines.join(", ")
            )
        };
        let libs = library_compile_scope(s, &w);
        if w.pinned_system_prompt.is_none() {
            w.pinned_system_prompt = Some(model_call::pinned_system_prompt(s, &libs));
        }

        // Emit durable turn/start and user/message events for frontend timeline.
        append_event(
            s,
            "turn/start",
            json!({
                "turn": turn,
                "runtime": "single_agent",
            }),
            false,
        )
        .await?;

        // The admitted attachment parts own the frontend message bubble, so the
        // timeline shows the image/file the user actually sent instead of dropping it.
        let admitted = crate::media::attachments::admitted(state.workspace.root(), task, turn)?;
        let user_content = if admitted.is_empty() {
            json!([{"type": "text", "text": prompt}])
        } else {
            Value::Array(admitted)
        };
        append_event(
            s,
            "user/message",
            json!({
                "task_id": task,
                "turn": turn,
                "message": {
                    "id": agent_service::uuid_like(),
                    "role": "user",
                    "content": user_content
                }
            }),
            true,
        )
        .await?;

        // Every new user turn ALWAYS starts with a brand-new empty JS VM!
        w.program = Some(Runtime::new_repl(libs.clone())?);
        w.next_ref_id = 1;
        // The model only sees text: an admitted image is registered as durable
        // image_artifact_id visual material and a file keeps its attachment_id with
        // read instructions, so a picture-only turn with an empty prompt still
        // reaches the model with its attachment context intact.
        let turn_user_text =
            crate::media::attachments::model_content(state.workspace.root(), task, turn, &prompt)?
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
        let turn_user_text = match model_call::workspace_blueprint_hint(s, &libs) {
            Some(hint) => format!("{turn_user_text}\n\n{hint}"),
            None => turn_user_text,
        };
        w.context_doc.begin_turn(turn, &turn_user_text);
        self.save_window(&w).await?;
        Ok((w, libs))
    }

    pub(super) async fn abort_turn(&self, w: &mut Window) -> Result<()> {
        let s = self.session;
        let turn = s.turn;
        let manifest = self
            .snapshot_turn_state(w)
            .unwrap_or_else(|_| "{\"frozen\":true}".into());
        w.context_doc
            .finish_turn("[Turn aborted before final reply.]");
        let code_asset_id = self.fold_code_asset(w);
        if let Some(turn_doc) = w
            .context_doc
            .turns
            .iter_mut()
            .find(|turn| turn.turn == s.turn)
        {
            turn_doc.snapshot_manifest = Some(manifest);
            turn_doc.snapshot_code_asset_id = code_asset_id;
        }
        w.program = None;
        self.save_window(w).await?;
        let _ = append_event(
            s,
            "turn/end",
            json!({"turn": turn, "reason": {"kind": "aborted"}}),
            false,
        )
        .await;
        let _ = agent_service::finish(s.state, s.task, "cancelled").await;
        Ok(())
    }

    pub(super) async fn save_window(&self, w: &Window) -> Result<()> {
        let s = self.session;
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

    /// Atomically consume any pending user interjections for the main task,
    /// emit `user/interjection_delivered`, append them to `w.context_doc`
    /// without mutating prior KV-cache prefix bytes, and persist `w` in one tx.
    pub(super) async fn deliver_interjections(
        &self,
        w: &mut Window,
        preceding_assistant_reply: Option<&str>,
    ) -> Result<usize> {
        let s = self.session;
        if s.scope != "main" {
            return Ok(0);
        }
        let task = s.task.to_owned();
        let scope = s.scope.clone();
        let mut draft = w.clone();
        let mut preceding = preceding_assistant_reply.map(str::to_owned);
        let (delivered_count, updated_window) = db(s.state.workspace.root(), move |conn| {
            if !crate::context::interjections::pending(conn, &task)? {
                return Ok((0usize, None));
            }
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let messages = crate::context::interjections::take(&tx, &task)?;
            if messages.is_empty() {
                tx.commit()?;
                return Ok((0usize, None));
            }
            let mut count = 0usize;
            for msg in messages {
                if let Some(text) = msg["content"].as_str().filter(|t| !t.trim().is_empty()) {
                    draft
                        .context_doc
                        .record_interjection(text.to_owned(), preceding.take());
                    count += 1;
                }
            }
            if count > 0 {
                let encoded = serde_json::to_string(&draft)?;
                tx.execute(
                    "INSERT OR REPLACE INTO agent_runtime_windows(task_id,scope,state) VALUES (?1,?2,?3)",
                    params![task, scope, encoded],
                )?;
            }
            tx.commit()?;
            Ok((count, (count > 0).then_some(draft)))
        })
        .await?;
        if let Some(updated) = updated_window {
            *w = updated;
        }
        Ok(delivered_count)
    }

    pub(super) async fn maybe_fold_before_request(
        &self,
        w: &mut Window,
        libs: &Libraries,
    ) -> Result<usize> {
        let s = self.session;
        let body = model_call::build_request_body(s, w, libs);
        let route = format!("{}/{}", s.state.provider_name, s.model);
        let mut estimated = w.token_calibration.estimate(&body, &route);
        let hard = crate::context::context_window::hard_limit(
            s.model,
            s.input_budget,
            crate::model::model_settings::long_context_threshold(s.state, s.model),
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
                let Some(turn_doc) = w
                    .context_doc
                    .turns
                    .iter()
                    .find(|turn| turn.turn == turn_number)
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
                if w.context_doc
                    .fold_turn(turn_number, manifest, code_asset_id)
                {
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
                if w.context_doc
                    .active_start_turn
                    .map_or(true, |active_start| active_start < keep_from)
                {
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
                        manifest["archived_access"] = json!("session.read_history({ turn: ... })");
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
            let Some(turn) = w.context_doc.active_turn() else {
                break;
            };
            let existing_cutoff = turn.folded_prefix.as_ref().map_or(0, |fold| fold.cutoff);
            let target = hard.saturating_mul(60) / 100;
            let cutoff = self.choose_active_prefix_cutoff(w, libs, &route, existing_cutoff, hard, target);
            if cutoff == 0 || cutoff <= existing_cutoff {
                break;
            }
            let manifest = self.snapshot_turn_state(w).unwrap_or_else(|_| {
                json!({"kind":"frozen_prefix_snapshots","turn":s.turn,"frozen":true,"live_vm":true,"read":"session.snapshot({ asset_id })"}).to_string()
            });
            let code_asset_id = self.fold_prefix_code_asset(w, cutoff);
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
            self.save_window(w).await?;
        }
        Ok(estimated)
    }

    pub(super) async fn force_fold_active_prefix(&self, w: &mut Window) -> Result<()> {
        let s = self.session;
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
            let cutoff = [12usize, 8, 5, 2, 1]
                .into_iter()
                .map(|keep| active_prefix_cutoff(turn, keep))
                .find(|&candidate| candidate > existing_cutoff)
                .unwrap_or(0);
            if cutoff > 0 {
                let manifest = self.snapshot_turn_state(w).unwrap_or_else(|_| {
                    json!({"kind":"forced_context_guard","turn":s.turn,"frozen":true,"live_vm":true,"read":"session.snapshot({ asset_id })"}).to_string()
                });
                let code_asset_id = self.fold_prefix_code_asset(w, cutoff);
                let _ = w
                    .context_doc
                    .fold_active_prefix(s.turn, cutoff, manifest, code_asset_id);
            }
        }
        self.save_window(w).await
    }

    pub(super) async fn finish_turn_with_reply(
        &self,
        w: &mut Window,
        step: usize,
        markdown: String,
    ) -> Result<()> {
        let s = self.session;
        let manifest = self.snapshot_turn_state(w).unwrap_or_else(|_| {
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
        let code_asset_id = self.fold_code_asset(w);
        if let Some(turn_doc) = w
            .context_doc
            .turns
            .iter_mut()
            .find(|turn| turn.turn == s.turn)
        {
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
        self.save_window(w).await?;
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

    fn fold_prefix_code_asset(&self, w: &Window, cutoff: usize) -> Option<String> {
        let s = self.session;
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
            crate::context::context_assets::register_pending(
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

    fn fold_code_asset(&self, w: &Window) -> Option<String> {
        let s = self.session;
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
            crate::context::context_assets::register_pending(
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
    fn snapshot_turn_state(&self, w: &Window) -> Result<String> {
        let s = self.session;
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
                    let kind = if is_file {
                        "file"
                    } else if is_command {
                        "command"
                    } else {
                        "json"
                    };
                    let path = object
                        .and_then(|o| o.get("path"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let content_key = crate::media::visual_artifacts::hash(&serialized);
                    if let Some(previous) = path
                        .as_ref()
                        .and_then(|path| latest_by_path.get(path).copied())
                    {
                        if candidates[previous].content_key != content_key {
                            // A later binding for the same path is the current version.
                            let previous_key = candidates[previous].content_key.clone();
                            if by_content.get(&previous_key) == Some(&previous) {
                                by_content.remove(&previous_key);
                            }
                            for previous_name in candidates[previous].names.clone() {
                                small_bindings
                                    .insert(previous_name, candidates[previous].value.clone());
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
            (
                priority,
                std::cmp::Reverse(candidate.names.first().cloned().unwrap_or_default()),
            )
        });
        let selected_indices = selected
            .iter()
            .take(16)
            .map(|(index, _)| *index)
            .collect::<std::collections::HashSet<_>>();
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
            let asset = crate::context::context_assets::register_pending(
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
        let small_bindings_ref = crate::context::context_assets::register_pending(
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
        let outputs_ref = crate::context::context_assets::register_pending(
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

    /// Pick the smallest valid `cutoff > existing_cutoff` (retaining as many recent
    /// steps as possible) whose folded request stays at or below `target` tokens.
    /// Falls back to the smallest cutoff below `hard`, or the largest available
    /// cutoff that still preserves at least one recent step.
    fn choose_active_prefix_cutoff(
        &self,
        w: &Window,
        libs: &crate::runtime::agent_program::libraries::Libraries,
        route: &str,
        existing_cutoff: usize,
        hard: usize,
        target: usize,
    ) -> usize {
        let Some(turn) = w.context_doc.active_turn() else {
            return 0;
        };
        let meaningful: Vec<usize> = turn
            .segments
            .iter()
            .enumerate()
            .filter_map(|(idx, seg)| {
                (!seg.is_rejection && !seg.source.trim().is_empty()).then_some(idx)
            })
            .collect();
        // Candidate cutoffs fold `segments[..cutoff]`, keeping `segments[cutoff..]`.
        // Always keep at least the latest meaningful segment (never fold past the last one).
        let candidates: Vec<usize> = meaningful
            .iter()
            .copied()
            .skip(1)
            .filter(|&idx| idx > existing_cutoff)
            .collect();
        if candidates.is_empty() {
            return 0;
        }
        let estimate_after = |cutoff: usize| -> usize {
            let mut probe = Window {
                context_doc: w.context_doc.clone(),
                program: None,
                program_task: w.program_task.clone(),
                task_node_id: w.task_node_id.clone(),
                next_ref_id: w.next_ref_id,
                token_calibration: w.token_calibration.clone(),
                pinned_system_prompt: w.pinned_system_prompt.clone(),
                last_model_request_at_ms: w.last_model_request_at_ms,
            };
            let placeholder_manifest = json!({
                "kind": "frozen_prefix_snapshots",
                "turn": self.session.turn,
                "frozen": true,
                "live_vm": true,
            })
            .to_string();
            if !probe
                .context_doc
                .fold_active_prefix(self.session.turn, cutoff, placeholder_manifest, None)
            {
                return usize::MAX;
            }
            let body = model_call::build_request_body(self.session, &probe, libs);
            probe.token_calibration.estimate(&body, route)
        };
        // Binary search for the smallest cutoff (maximum recent segments kept)
        // whose estimated tokens <= target.
        let mut lo = 0usize;
        let mut hi = candidates.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if estimate_after(candidates[mid]) <= target {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        if lo < candidates.len() {
            return candidates[lo];
        }
        // Even keeping only 1 recent step exceeds `target`; find the smallest cutoff < `hard`.
        let mut lo = 0usize;
        let mut hi = candidates.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if estimate_after(candidates[mid]) < hard {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        if lo < candidates.len() {
            candidates[lo]
        } else {
            *candidates.last().unwrap()
        }
    }
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

async fn db<T: Send + 'static>(
    root: &Path,
    f: impl FnOnce(&mut rusqlite::Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    crate::context::db_writer::write_async(root, move |conn| {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_runtime_windows(
            task_id TEXT NOT NULL, scope TEXT NOT NULL, state TEXT NOT NULL,
            PRIMARY KEY(task_id,scope));",
        )?;
        f(conn)
    })
    .await
}

pub(super) async fn append_event(
    s: &Session<'_>,
    kind: &str,
    mut data: Value,
    surface: bool,
) -> Result<i64> {
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

#[cfg(test)]
mod context_guard_tests {
    use super::*;

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
        doc.active_turn_mut()
            .unwrap()
            .segments
            .push(context_doc::TraceSegment {
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
                interjection: None,
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
        assert!(
            wire.iter()
                .any(|message| message["content"] == "old assistant reply")
        );
        assert!(wire.iter().any(|message| {
            message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("session.snapshot"))
        }));
    }
}
