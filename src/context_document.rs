//! Authoritative Turn & Code-Segment Document for Context-as-Code.
//!
//! Each user turn pairs one `user` message (`user_text` + append-only executed
//! `CodeSegment`s + ephemeral `ExecutionOverlay` while active) with one final
//! native `assistant` Markdown reply once settled. Runtime values stay in the
//! live VM `Heap` for the duration of the turn and are wiped when the turn ends.

use crate::agent_program::{self, Runtime, State};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const CONTEXT_DOCUMENT_VERSION: u32 = 1;
pub const RECENT_CODE_CHARS: usize = 24_000;
pub const CHAT_CHARS: usize = 48_000;
pub const INLINE_BINDING_CHARS: usize = 16_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextDocument {
    pub version: u32,
    pub turns: Vec<TurnDocument>,
    /// True only when a `@virtual` leaf window was opened without a parent `ContextDocument`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub standalone_leaf: bool,
    /// Standalone VM slot used by unit harnesses; `Window` uses `w.repl_runtime` / `w.program`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime: Option<Runtime>,
    #[serde(default)]
    libraries: agent_program::libraries::Libraries,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnDocument {
    pub turn: usize,
    pub user_event_seq: i64,
    pub user_text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<CodeSegment>,
    pub next_segment_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_script_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<TurnCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rejection: Option<RejectedSubmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_reply: Option<AssistantTurnReply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<ExecutionOverlay>,
    /// Present only inside an active `@virtual` child window: keeps the caller's
    /// main code prefix untouched above while projecting the child stack below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_child: Option<Box<ChildFrameStack>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChildFrameStack {
    pub frame_id: String,
    pub function_name: String,
    pub contract: String,
    #[serde(default)]
    pub allowed_libraries: Vec<String>,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    #[serde(default)]
    pub captures: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_segments: Vec<CodeSegment>,
    pub next_sub_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rejection: Option<RejectedSubmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<ExecutionOverlay>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantTurnReply {
    pub event_seq: i64,
    pub markdown: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodeSegment {
    pub id: String,
    pub frame_id: String,
    pub source: String,
    pub status: SegmentStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_effects: Vec<EffectReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<SegmentFailure>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SegmentStatus {
    Running,
    WaitingForTool,
    Completed,
    FailedAfterEffect,
    EffectUnknown,
    BlockedByUser,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectReceipt {
    pub effect_id: String,
    pub event_seq: i64,
    pub call: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chars: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SegmentFailure {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub executed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RejectedSubmission {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub executed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnCheckpoint {
    pub through_segment_id: String,
    pub script_file: String,
    #[serde(default)]
    pub binding_signatures: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionOverlay {
    pub active_frame: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stack: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub older_locals: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_step_effects: Vec<EffectReceipt>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub last_step_bindings: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub console_output: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncaught_error: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rejection: Option<RejectedSubmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_program_hint: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Default for ContextDocument {
    fn default() -> Self {
        Self {
            version: CONTEXT_DOCUMENT_VERSION,
            turns: Vec::new(),
            standalone_leaf: false,
            runtime: None,
            libraries: agent_program::libraries::builtin(),
        }
    }
}

impl ContextDocument {
    pub fn new(libraries: agent_program::libraries::Libraries) -> Self {
        Self { libraries, ..Self::default() }
    }

    pub fn set_libraries(&mut self, libraries: agent_program::libraries::Libraries) {
        self.libraries = libraries;
    }

    pub fn active_turn(&self) -> Option<&TurnDocument> {
        self.turns.last().filter(|turn| turn.assistant_reply.is_none())
    }

    pub fn active_turn_mut(&mut self) -> Option<&mut TurnDocument> {
        self.turns.last_mut().filter(|turn| turn.assistant_reply.is_none())
    }

    /// Start a new user turn with a completely fresh VM state. If an earlier
    /// turn was interrupted without a final reply (e.g. cancelled/stopped),
    /// seal it cleanly with an interruption note first so turns stay paired.
    pub fn begin_turn_with_number(
        &mut self,
        turn_number: usize,
        user_event_seq: i64,
        user_text: impl Into<String>,
        attachments: Vec<Value>,
    ) -> Result<usize> {
        if let Some(unfinished) = self.active_turn_mut() {
            unfinished.overlay = None;
            unfinished.pending_rejection = None;
            unfinished.active_child = None;
            unfinished.assistant_reply = Some(AssistantTurnReply {
                event_seq: user_event_seq,
                markdown: "[Turn ended before final reply.]".into(),
            });
        }
        self.turns.retain(|t| t.turn < turn_number);
        self.turns.push(TurnDocument {
            turn: turn_number,
            user_event_seq,
            user_text: user_text.into(),
            attachments,
            segments: Vec::new(),
            next_segment_seq: 1,
            saved_script_file: None,
            checkpoint: None,
            pending_rejection: None,
            assistant_reply: None,
            overlay: None,
            active_child: None,
        });
        self.runtime = Some(Runtime::new_repl(self.libraries.clone())?);
        Ok(turn_number)
    }

    pub fn begin_turn(&mut self, user_event_seq: i64, user_text: impl Into<String>) -> Result<usize> {
        ensure!(self.active_turn().is_none(), "finish the active turn before starting another");
        let turn = self.turns.len() + 1;
        self.begin_turn_with_number(turn, user_event_seq, user_text, Vec::new())
    }

    pub fn append_interjection(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let Some(turn) = self.active_turn_mut() else { return };
        if turn.segments.is_empty() {
            turn.user_text.push_str("\n\n[User follow-up]: ");
            turn.user_text.push_str(trimmed);
        } else {
            let id = format!("u{}", turn.next_segment_seq);
            turn.next_segment_seq += 1;
            let comment = trimmed
                .lines()
                .map(|line| format!("// [User follow-up]: {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            turn.segments.push(CodeSegment {
                id,
                frame_id: "main#0".into(),
                source: comment,
                status: SegmentStatus::Completed,
                completed_effects: Vec::new(),
                failure: None,
            });
        }
    }

    pub fn append_async_note(&mut self, note: impl Into<String>) {
        let note = note.into();
        let Some(turn) = self.active_turn_mut() else { return };
        let overlay = if let Some(child) = turn.active_child.as_mut() {
            child.overlay.get_or_insert_with(|| ExecutionOverlay {
                active_frame: child.frame_id.clone(),
                ..Default::default()
            })
        } else {
            turn.overlay.get_or_insert_with(|| ExecutionOverlay {
                active_frame: "main#0".into(),
                ..Default::default()
            })
        };
        match &mut overlay.note {
            Some(existing) if !existing.is_empty() => {
                existing.push('\n');
                existing.push_str(&note);
            }
            slot => *slot = Some(note),
        }
    }

    /// Record a rejected submission (`executed: false`, such as syntax/compile
    /// errors, `MIXED_RUN_AND_REPLY`, or output truncation) WITHOUT polluting
    /// the immutable executed segment list.
    pub fn reject_submission(&mut self, code: impl Into<String>, message: impl Into<String>) {
        let rejection = RejectedSubmission {
            code: code.into(),
            message: message.into(),
            executed: false,
        };
        let Some(turn) = self.active_turn_mut() else { return };
        if let Some(child) = turn.active_child.as_mut() {
            child.pending_rejection = Some(rejection.clone());
            let overlay = child.overlay.get_or_insert_with(|| ExecutionOverlay {
                active_frame: child.frame_id.clone(),
                ..Default::default()
            });
            overlay.pending_rejection = Some(rejection);
        } else {
            turn.pending_rejection = Some(rejection.clone());
            let overlay = turn.overlay.get_or_insert_with(|| ExecutionOverlay {
                active_frame: "main#0".into(),
                ..Default::default()
            });
            overlay.pending_rejection = Some(rejection);
        }
    }

    /// Register a compiled segment right as it begins executing.
    pub fn record_segment_start(&mut self, source: impl Into<String>) -> Result<String> {
        let source = source.into();
        let turn = self.active_turn_mut().context("no active turn")?;
        if let Some(child) = turn.active_child.as_mut() {
            let id = format!("s{}", child.next_sub_seq);
            child.next_sub_seq += 1;
            child.pending_rejection = None;
            if let Some(overlay) = child.overlay.as_mut() {
                overlay.pending_rejection = None;
                overlay.uncaught_error = None;
            }
            child.sub_segments.push(CodeSegment {
                id: id.clone(),
                frame_id: child.frame_id.clone(),
                source,
                status: SegmentStatus::Running,
                completed_effects: Vec::new(),
                failure: None,
            });
            Ok(id)
        } else {
            let id = format!("c{}", turn.next_segment_seq);
            turn.next_segment_seq += 1;
            turn.pending_rejection = None;
            if let Some(overlay) = turn.overlay.as_mut() {
                overlay.pending_rejection = None;
                overlay.uncaught_error = None;
            }
            turn.segments.push(CodeSegment {
                id: id.clone(),
                frame_id: "main#0".into(),
                source,
                status: SegmentStatus::Running,
                completed_effects: Vec::new(),
                failure: None,
            });
            Ok(id)
        }
    }

    pub fn record_effect(&mut self, receipt: EffectReceipt) {
        let Some(turn) = self.active_turn_mut() else { return };
        let target = if let Some(child) = turn.active_child.as_mut() {
            child.sub_segments.last_mut()
        } else {
            turn.segments.last_mut()
        };
        if let Some(segment) = target {
            segment.completed_effects.push(receipt);
        }
    }

    pub fn mark_segment_waiting(&mut self) {
        let Some(turn) = self.active_turn_mut() else { return };
        let target = if let Some(child) = turn.active_child.as_mut() {
            child.sub_segments.last_mut()
        } else {
            turn.segments.last_mut()
        };
        if let Some(segment) = target {
            segment.status = SegmentStatus::WaitingForTool;
        }
    }

    pub fn settle_segment_completed(
        &mut self,
        runtime: &Runtime,
        pre_fingerprints: &BTreeMap<String, String>,
        console_output: Vec<String>,
        note: Option<String>,
    ) {
        let (inspected, older) = runtime.partition_overlay_bindings(pre_fingerprints, INLINE_BINDING_CHARS);
        let last_step_bindings = inspected
            .into_iter()
            .map(|(name, info)| {
                (
                    name,
                    json!({
                        "preview": info.preview,
                        "chars": info.chars,
                        "lines": info.lines,
                        "tier": info.tier,
                        "truncated": info.truncated,
                        "sha256": info.sha256,
                        "live in JS binding": info.live,
                    }),
                )
            })
            .collect();
        let stack = runtime.stack_snapshot();
        let Some(turn) = self.active_turn_mut() else { return };
        if let Some(child) = turn.active_child.as_mut() {
            let effects = if let Some(seg) = child.sub_segments.last_mut() {
                seg.status = SegmentStatus::Completed;
                seg.completed_effects.clone()
            } else {
                Vec::new()
            };
            child.overlay = Some(ExecutionOverlay {
                active_frame: child.frame_id.clone(),
                stack,
                older_locals: older,
                last_step_effects: effects,
                last_step_bindings,
                console_output,
                uncaught_error: None,
                pending_rejection: None,
                paused_program_hint: None,
                note,
            });
        } else {
            let effects = if let Some(seg) = turn.segments.last_mut() {
                seg.status = SegmentStatus::Completed;
                seg.completed_effects.clone()
            } else {
                Vec::new()
            };
            turn.overlay = Some(ExecutionOverlay {
                active_frame: "main#0".into(),
                stack,
                older_locals: older,
                last_step_effects: effects,
                last_step_bindings,
                console_output,
                uncaught_error: None,
                pending_rejection: None,
                paused_program_hint: None,
                note,
            });
        }
    }

    pub fn settle_segment_failed(
        &mut self,
        runtime: &Runtime,
        pre_fingerprints: &BTreeMap<String, String>,
        error: Value,
        console_output: Vec<String>,
        status: SegmentStatus,
    ) {
        let (inspected, older) = runtime.partition_overlay_bindings(pre_fingerprints, INLINE_BINDING_CHARS);
        let last_step_bindings = inspected
            .into_iter()
            .map(|(name, info)| {
                (
                    name,
                    json!({
                        "preview": info.preview,
                        "chars": info.chars,
                        "lines": info.lines,
                        "tier": info.tier,
                        "truncated": info.truncated,
                        "live in JS binding": info.live,
                    }),
                )
            })
            .collect();
        let stack = runtime.stack_snapshot();
        let code = error["code"].as_str().unwrap_or("RUNTIME_ERROR").to_owned();
        let message = error["message"]
            .as_str()
            .or_else(|| error["error"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| error.to_string());
        let Some(turn) = self.active_turn_mut() else { return };
        if let Some(child) = turn.active_child.as_mut() {
            let effects = if let Some(seg) = child.sub_segments.last_mut() {
                seg.status = status;
                seg.failure = Some(SegmentFailure { code, message, executed: true });
                seg.completed_effects.clone()
            } else {
                Vec::new()
            };
            child.overlay = Some(ExecutionOverlay {
                active_frame: child.frame_id.clone(),
                stack,
                older_locals: older,
                last_step_effects: effects,
                last_step_bindings,
                console_output,
                uncaught_error: Some(error),
                pending_rejection: None,
                paused_program_hint: None,
                note: None,
            });
        } else {
            let effects = if let Some(seg) = turn.segments.last_mut() {
                seg.status = status;
                seg.failure = Some(SegmentFailure { code, message, executed: true });
                seg.completed_effects.clone()
            } else {
                Vec::new()
            };
            turn.overlay = Some(ExecutionOverlay {
                active_frame: "main#0".into(),
                stack,
                older_locals: older,
                last_step_effects: effects,
                last_step_bindings,
                console_output,
                uncaught_error: Some(error),
                pending_rejection: None,
                paused_program_hint: None,
                note: None,
            });
        }
    }

    pub fn enter_child_frame(&mut self, child: ChildFrameStack) {
        if let Some(turn) = self.active_turn_mut() {
            turn.active_child = Some(Box::new(child));
        }
    }

    pub fn set_paused_program_hint(&mut self, hint: Option<Value>) {
        if let Some(turn) = self.active_turn_mut() {
            let overlay = turn.overlay.get_or_insert_with(|| ExecutionOverlay {
                active_frame: "main#0".into(),
                ..Default::default()
            });
            overlay.paused_program_hint = hint;
        }
    }

    /// Standalone append+execute helper for unit harnesses. Compile rejections
    /// (`executed: false`) never enter `turn.segments`.
    pub fn append_segment(&mut self, source: impl Into<String>, frame_id: impl Into<String>) -> Result<State> {
        let source = source.into();
        let frame_id = frame_id.into();
        ensure!(!source.trim().is_empty(), "code segment must not be empty");
        ensure!(self.active_turn().is_some(), "no active turn");
        let libraries = self.libraries.clone();
        let pre = self
            .runtime
            .as_ref()
            .context("active turn VM is missing")?
            .user_binding_fingerprints();
        let log_start = self.runtime.as_ref().unwrap().logs_len();
        if let Err(error) = self
            .runtime
            .as_mut()
            .unwrap()
            .prepare_segment(&source, libraries, frame_id != "main#0")
        {
            self.reject_submission("SOURCE_REJECTED", format!("{error:#}"));
            return Err(error);
        }
        let _ = self.record_segment_start(source);
        let state = self.runtime.as_mut().unwrap().next();
        let runtime_snapshot = self.runtime.as_ref().unwrap().clone();
        let logs = runtime_snapshot.logs_since(log_start);
        match &state {
            State::Action(_) | State::Waiting => self.mark_segment_waiting(),
            State::Returned(_) => self.settle_segment_completed(&runtime_snapshot, &pre, logs, None),
            State::Failed(err) => {
                self.settle_segment_failed(&runtime_snapshot, &pre, err.clone(), logs, SegmentStatus::FailedAfterEffect);
            }
        }
        Ok(state)
    }

    pub fn finish_turn(&mut self, event_seq: i64, markdown: impl Into<String>) -> Result<()> {
        let turn = self.active_turn_mut().context("no active turn")?;
        turn.overlay = None;
        turn.pending_rejection = None;
        turn.active_child = None;
        turn.assistant_reply = Some(AssistantTurnReply {
            event_seq,
            markdown: markdown.into(),
        });
        self.runtime = None;
        Ok(())
    }

    pub fn maybe_checkpoint(&mut self, script_file: &str, runtime: &Runtime, max_code_chars: usize) {
        let Some(turn) = self.active_turn_mut() else { return };
        let start_idx = turn
            .checkpoint
            .as_ref()
            .and_then(|cp| turn.segments.iter().position(|s| s.id == cp.through_segment_id).map(|i| i + 1))
            .unwrap_or(0);
        let active_code_chars: usize = turn.segments[start_idx..]
            .iter()
            .map(|s| s.source.chars().count())
            .sum();
        if active_code_chars > max_code_chars
            && let Some(last) = turn.segments.last()
        {
            turn.checkpoint = Some(TurnCheckpoint {
                through_segment_id: last.id.clone(),
                script_file: script_file.to_owned(),
                binding_signatures: runtime.user_binding_fingerprints(),
            });
        }
    }

    pub fn archive_script(&mut self, directory: &Path, file_name: impl Into<String>) -> Result<String> {
        let file_name = file_name.into();
        ensure!(
            file_name.ends_with(".js") && !file_name.contains('/') && !file_name.contains('\\'),
            "invalid script file name"
        );
        let index = self.turns.len().checked_sub(1).context("no turn to archive")?;
        let turn = &mut self.turns[index];
        if turn.segments.is_empty() {
            return Ok(file_name);
        }
        let body = turn
            .segments
            .iter()
            .map(|segment| format!("/* [seg {} | frame {}] */\n{}\n", segment.id, segment.frame_id, segment.source))
            .collect::<Vec<_>>()
            .join("\n");
        fs::create_dir_all(directory)?;
        fs::write(directory.join(&file_name), body)?;
        turn.saved_script_file = Some(file_name.clone());
        Ok(file_name)
    }

    /// Render the conversation turns `(user #1, assistant #1, ..., user #N)`
    /// without the `system` preamble so `single_agent::request()` can attach
    /// the session's static system headers cleanly.
    pub fn render_conversation_messages(&self) -> Vec<Value> {
        let start = self.history_start(0);
        let total_turns = self.turns.len();
        let mut recent_code_budget = RECENT_CODE_CHARS;
        let mut inline_code_flags = vec![false; total_turns];
        for idx in (start..total_turns).rev() {
            let turn = &self.turns[idx];
            if turn.assistant_reply.is_none() {
                inline_code_flags[idx] = true;
                continue;
            }
            let code_chars: usize = turn.segments.iter().map(|s| s.source.chars().count()).sum();
            if code_chars <= recent_code_budget {
                inline_code_flags[idx] = true;
                recent_code_budget = recent_code_budget.saturating_sub(code_chars);
            }
        }

        let mut messages = Vec::new();
        for (idx, turn) in self.turns.iter().enumerate().skip(start) {
            let active = turn.assistant_reply.is_none();
            messages.push(self.render_user(turn, active, inline_code_flags[idx]));
            if let Some(reply) = &turn.assistant_reply {
                messages.push(json!({"role": "assistant", "content": reply.markdown}));
            }
        }
        messages
    }

    pub fn render_wire_messages(&self, system: impl Into<String>) -> Vec<Value> {
        let mut messages = vec![json!({"role": "system", "content": system.into()})];
        messages.extend(self.render_conversation_messages());
        messages
    }

    /// Bytes of the active `user` message that remain immutable while the
    /// ephemeral execution stack overlay at the bottom is replaced.
    pub fn render_stable_user_prefix(&self) -> Option<String> {
        self.active_turn().map(|turn| self.render_stable_user(turn, true))
    }

    fn history_start(&self, initial_chars: usize) -> usize {
        let mut start = 0;
        let mut used = initial_chars;
        let completed_end = self.turns.len().saturating_sub(usize::from(self.active_turn().is_some()));
        for index in 0..completed_end {
            let turn = &self.turns[index];
            let size = turn.user_text.chars().count()
                + turn.segments.iter().map(|s| s.source.chars().count()).sum::<usize>()
                + turn.assistant_reply.as_ref().map_or(0, |r| r.markdown.chars().count());
            if index > start && used + size > CHAT_CHARS {
                start = index;
                used = 0;
            }
            used += size;
        }
        start
    }

    fn render_user(&self, turn: &TurnDocument, active: bool, inline_code: bool) -> Value {
        let mut content = self.render_stable_user(turn, inline_code);
        if active {
            if let Some(child) = &turn.active_child {
                content.push_str("\n\n// === Active Execution Stack (Ephemeral — DO NOT copy syntax) ===\n");
                content.push_str(&Self::format_child_stack(turn, child));
            } else if let Some(overlay) = &turn.overlay {
                content.push_str("\n\n// === Active Execution Stack (Ephemeral — DO NOT copy syntax) ===\n");
                content.push_str(&serde_json::to_string_pretty(overlay).unwrap_or_default());
            } else if let Some(rejection) = &turn.pending_rejection {
                content.push_str("\n\n// === Active Execution Stack (Ephemeral — DO NOT copy syntax) ===\n");
                content.push_str(&serde_json::to_string_pretty(&json!({"active_frame":"main#0","pending_rejection":rejection})).unwrap_or_default());
            }
        }
        if active && !turn.attachments.is_empty() {
            let mut blocks = turn.attachments.clone();
            blocks.push(json!({"type": "text", "text": content}));
            return json!({"role": "user", "content": blocks});
        }
        json!({"role": "user", "content": content})
    }

    fn format_child_stack(turn: &TurnDocument, child: &ChildFrameStack) -> String {
        let waiting_seg = turn.segments.last().map(|s| s.id.as_str()).unwrap_or("c1");
        let mut out = format!(
            "VIRTUAL CALL BOUNDARY: You are currently executing inside `@virtual` frame `{}` (called by `main#0` at `{}`).\n\
             - Execute JavaScript inside `<run>...</run>` using this frame's parameters, `@uses` captures, and allowed non-shell libraries.\n\
             - Finish this `@virtual` call by executing `<run>\\nreturn <json_value>;\\n</run>` (or `return {{\"$task_error\": {{\"message\": \"...\"}}}}` if impossible, or `host.ask_user({{\"question\": \"...\"}})`).\n\
             - Do NOT output plain user-facing Markdown from inside a `@virtual` frame.\n\n",
            child.frame_id, waiting_seg
        );
        let summary = json!({
            "call_stack": [
                format!("main#0 [waiting: {} at {}]", child.function_name, waiting_seg),
                format!("└─ {} [ACTIVE @virtual]", child.frame_id),
            ],
            "contract": child.contract,
            "allowed_libraries": child.allowed_libraries,
            "params": child.params,
            "captures (@uses)": child.captures,
        });
        out.push_str(&serde_json::to_string_pretty(&summary).unwrap_or_default());
        for sub in &child.sub_segments {
            out.push_str(&format!("\n\n/* [sub-seg {} | frame {}] */\n{}", sub.id, child.frame_id, sub.source));
        }
        if let Some(overlay) = &child.overlay {
            out.push_str("\n\n// --- Child Frame Live Bindings ---\n");
            out.push_str(&serde_json::to_string_pretty(overlay).unwrap_or_default());
        } else if let Some(rejection) = &child.pending_rejection {
            out.push_str("\n\n// --- Child Frame Rejection ---\n");
            out.push_str(&serde_json::to_string_pretty(rejection).unwrap_or_default());
        }
        out
    }

    fn render_stable_user(&self, turn: &TurnDocument, inline_code: bool) -> String {
        let mut content = turn.user_text.clone();
        if turn.segments.is_empty() {
            return content;
        }
        if !inline_code {
            let script = turn.saved_script_file.as_deref().unwrap_or("turn.js");
            content.push_str(&format!(
                "\n\n// [Archived turn-{} script: {} ({} segments) — inspect via session.read_session_history if needed]",
                turn.turn,
                script,
                turn.segments.len()
            ));
            return content;
        }
        let mut start_idx = 0;
        if let Some(cp) = &turn.checkpoint {
            if let Some(pos) = turn.segments.iter().position(|s| s.id == cp.through_segment_id) {
                start_idx = pos + 1;
                content.push_str(&format!(
                    "\n\n// --- Checkpoint through {} ({}) | live bindings: {:?} ---",
                    cp.through_segment_id,
                    cp.script_file,
                    cp.binding_signatures.keys().collect::<Vec<_>>()
                ));
            }
        }
        // Keep header byte-identical across intra-turn steps and turn completion
        // so sealing `saved_script_file` at turn end never mutates earlier tokens!
        content.push_str(&format!("\n\n// --- Executed Program (turn-{}) ---", turn.turn));
        for segment in &turn.segments[start_idx..] {
            content.push_str(&format!(
                "\n/* [seg {} | frame {}] */\n{}",
                segment.id, segment.frame_id, segment.source
            ));
            let settled = !matches!(
                segment.status,
                SegmentStatus::Running | SegmentStatus::WaitingForTool
            );
            if settled {
                for effect in &segment.completed_effects {
                    if effect.exit_code.is_some()
                        || (effect.asset_id.is_some()
                            && effect.chars.unwrap_or(0) > INLINE_BINDING_CHARS)
                    {
                        content.push_str(&format!(
                            "\n// [receipt {}: seq={}, outcome={}, exit_code={:?}, asset={:?}]",
                            effect.effect_id,
                            effect.event_seq,
                            effect.outcome,
                            effect.exit_code,
                            effect.asset_id
                        ));
                    }
                }
                if let Some(failure) = &segment.failure {
                    let single_line = failure.message.lines().next().unwrap_or(&failure.message);
                    content.push_str(&format!("\n// [aborted {}: {}]", failure.code, single_line));
                }
            }
        }
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_append_segments_and_keep_immutable_prefix_even_across_turn_seal() {
        let mut document = ContextDocument::default();
        document.begin_turn(1, "inspect files").unwrap();
        document.append_segment("const target = 'a.js'; target", "main#0").unwrap();
        let first = document.render_stable_user_prefix().unwrap();
        document.append_segment("const again = target; again", "main#0").unwrap();
        let second = document.render_stable_user_prefix().unwrap();
        assert!(second.starts_with(&first));

        // Sealing turn 1 and starting turn 2 keeps user #1 byte-identical to `second`!
        let directory = std::env::temp_dir().join("cac-prefix-test");
        let _ = document.archive_script(&directory, "turn-1-inspect.js");
        document.finish_turn(2, "All checked.").unwrap();
        document.begin_turn(3, "next request").unwrap();
        let wire = document.render_wire_messages("system");
        assert_eq!(wire[1]["content"].as_str().unwrap(), second);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn compile_rejection_does_not_pollute_immutable_segments() {
        let mut document = ContextDocument::default();
        document.begin_turn(1, "bad syntax").unwrap();
        assert!(document.append_segment("const = ;", "main#0").is_err());
        assert!(document.active_turn().unwrap().segments.is_empty(), "rejected source must not enter immutable segments");
        assert_eq!(
            document.active_turn().unwrap().pending_rejection.as_ref().unwrap().executed,
            false
        );
    }

    #[test]
    fn failed_redeclaration_restores_previous_binding_value() {
        let mut document = ContextDocument::default();
        document.begin_turn(1, "preserve prior value").unwrap();
        document.append_segment("const data = 'important';", "main#0").unwrap();
        let state = document
            .append_segment("function boom(){ throw new Error('fail'); } const data = boom();", "main#0")
            .unwrap();
        assert!(matches!(state, State::Failed(_)));
        let recovered = document.append_segment("data", "main#0").unwrap();
        assert!(matches!(recovered, State::Returned(_)));
        let overlay = document.turns[0].overlay.as_ref().unwrap();
        assert!(overlay.last_step_bindings["$_"]["preview"].as_str().unwrap().contains("important"));
    }

    #[test]
    fn older_locals_compact_while_new_step_bindings_show_full_inline() {
        let mut document = ContextDocument::default();
        document.begin_turn(1, "partition bindings").unwrap();
        document.append_segment("const first = 'a'.repeat(500);", "main#0").unwrap();
        document.append_segment("const second = 'hello';", "main#0").unwrap();
        let overlay = document.turns[0].overlay.as_ref().unwrap();
        assert!(overlay.last_step_bindings.contains_key("second"));
        assert!(!overlay.last_step_bindings.contains_key("first"), "unchanged binding moves to older_locals");
        assert!(overlay.older_locals.contains_key("first"));
        assert!(!overlay.older_locals.contains_key("io"), "builtin namespaces stay out of user locals");
    }
}
