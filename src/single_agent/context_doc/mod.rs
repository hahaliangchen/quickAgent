//! Context-as-Code v2 document & native multi-turn wire renderer.
//!
//! Wire layout (100% Append-Only, strict byte-exact KV-cache prefix):
//! - `system`: static rules + `.d.ts` host SDK declarations.
//! - `user`: immutable turn goal (or isolated `@virtual` contract + bound params/prefix locals).
//! - For each `run_js({ code })` step:
//!   - `assistant`: exact provider assistant message (`tool_calls`, optional brief progress text,
//!     `reasoning_content`, `_reasoning_items`, `_thinking_blocks`) for native reasoning replay.
//!   - `tool`: concise final-statement execution output (up to 64KB inline, or compact
//!     `ResourceHandle` signature + structural index if > 64KB), plus explicit `console.log`
//!     and `[virtual ...]` / `[library ...]` factual receipts.
//! - Turn seal: plain `assistant` Markdown reply (no `run_js`).

mod format;
mod indexers;

pub use format::SegmentContextHints;
pub(super) use format::{
    collect_delegated_markers, tag_resource_handle, tag_resource_handle_in_turn,
};

use crate::agent_program::Runtime;
use format::{DisplayTracker, format_binding, format_segment_feedback};
use indexers::compact_single_line;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Max chars printed inline permanently in Append-Only `// =>` receipts (64 KiB).
/// Anything `<= 64KB` stays permanently visible for the entire turn (zero amnesia).
/// Anything `> 64KB` stays live in VM memory as a `ResourceHandle` (`Ref#N`) with a
/// compact structural index and durable `asset_id` archive.
pub const INLINE_LIMIT_CHARS: usize = 65_536;

/// Alias for `INLINE_LIMIT_CHARS` used by `tool_proxy` for automatic durable asset archiving.
pub const PEEK_FULL_LIMIT_CHARS: usize = INLINE_LIMIT_CHARS;

/// Clamp one tool receipt to a quarter of the remaining input budget. Keeping
/// this calculation in the document module makes all receipt producers use the
/// same safe bound.
pub fn inline_limit_for_budget(remaining_budget_chars: usize) -> usize {
    INLINE_LIMIT_CHARS.min(remaining_budget_chars.saturating_div(4).max(1))
}

/// A completed turn's immutable native `run_js` fold.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FoldedTurn {
    pub code: String,
    pub manifest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_asset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_message: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// An immutable prefix fold of the active turn. The live VM is deliberately
/// outside this value and remains owned by `Window.program`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FoldedPrefix {
    pub cutoff: usize,
    pub code: String,
    pub manifest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_asset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_message: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContextDocument {
    pub turns: Vec<TurnDoc>,
    #[serde(skip)]
    pub inline_budget_chars: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_start_turn: Option<usize>,
}

/// Record of a file handle whose body or structural index was already expanded
/// in the current turn's visible wire history, enabling turn-scoped dedup.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShownFileHandle {
    pub ref_id: String,
    pub content_hash: u64,
    /// 0-based `TurnDoc.segments` index where this version was expanded.
    pub segment_index: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnDoc {
    pub turn: usize,
    pub user_text: String,
    /// Optional header replacing `// === USER GOAL === ... async function executeTask() {`
    /// when this document represents an isolated `@virtual` child frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_header: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<TraceSegment>,
    /// Live `Ref#N` handles already recorded in this turn so incremental append-only
    /// `// [live handles added: ...]` notes list only newly created handles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub live_handles: Vec<String>,
    /// Turn-scoped `(content_hash, Ref#N)` registry by path so re-reading an
    /// unchanged file reuses its existing `Ref#N` handle identity.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub file_handle_refs: BTreeMap<String, (u64, String)>,
    /// Files whose full body or >64KB structural index is currently visible in
    /// this turn's unfolded segments (evicted automatically on `fold_active_prefix`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shown_file_handles: BTreeMap<String, ShownFileHandle>,
    /// Transient note for a rejected/uncompiled submission (`executed: false`);
    /// cleared as soon as the next valid segment executes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rejection: Option<String>,
    /// Permanently replaces all completed segments once a turn is folded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folded: Option<FoldedTurn>,
    /// Permanently replaces `segments[..cutoff]` while the active VM stays live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folded_prefix: Option<FoldedPrefix>,
    /// Snapshots captured when the VM ended, waiting for the next economic fold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_manifest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_code_asset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_reply: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceSegment {
    pub source: String,
    /// Permanent `// => ...` lines recorded right after this segment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline_receipts: Vec<String>,
    /// Permanent abort/pause/host/rejection note if the segment stopped early or was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_note: Option<String>,
    /// Native provider assistant message for this step (including `tool_calls`,
    /// `reasoning_content`, `_reasoning_items`, `_thinking_blocks`) for strict
    /// multi-turn KV-cache stability and encrypted reasoning replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_message: Option<Value>,
    /// Matching `tool_call_id` for this step's `tool` response message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// True when this step was rejected before execution (e.g. `COMPILE_ERROR`,
    /// `UNKNOWN_TOOL`, `VIRTUAL_TOOL_REQUIRED`): replayed append-only in wire history
    /// to preserve KV-cache, but omitted from clean archived `.js` files.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_rejection: bool,
}

impl ContextDocument {
    pub fn set_inline_budget_chars(&mut self, remaining_budget_chars: usize) {
        self.inline_budget_chars = inline_limit_for_budget(remaining_budget_chars);
    }

    fn inline_budget_chars(&self) -> usize {
        if self.inline_budget_chars == 0 {
            INLINE_LIMIT_CHARS
        } else {
            self.inline_budget_chars
        }
    }

    pub fn active_turn(&self) -> Option<&TurnDoc> {
        self.turns.last().filter(|t| t.assistant_reply.is_none())
    }

    pub fn active_turn_mut(&mut self) -> Option<&mut TurnDoc> {
        self.turns
            .last_mut()
            .filter(|t| t.assistant_reply.is_none())
    }

    /// Start a brand-new user turn. If a previous turn was interrupted without
    /// a final reply, seal it first so `(user, assistant)` pairs stay aligned.
    pub fn begin_turn(&mut self, turn: usize, user_text: impl Into<String>) {
        if let Some(unfinished) = self.active_turn_mut() {
            unfinished.live_handles.clear();
            unfinished.file_handle_refs.clear();
            unfinished.shown_file_handles.clear();
            unfinished.pending_rejection = None;
            unfinished.assistant_reply = Some("[Turn ended before final reply.]".into());
        }
        self.turns.retain(|t| t.turn < turn);
        self.turns.push(TurnDoc {
            turn,
            user_text: user_text.into(),
            virtual_header: None,
            segments: Vec::new(),
            live_handles: Vec::new(),
            file_handle_refs: BTreeMap::new(),
            shown_file_handles: BTreeMap::new(),
            pending_rejection: None,
            folded: None,
            folded_prefix: None,
            snapshot_manifest: None,
            snapshot_code_asset_id: None,
            assistant_reply: None,
        });
    }

    /// Initialize an isolated single-turn document for a `@virtual` child frame.
    pub fn begin_virtual_frame(
        &mut self,
        signature: &str,
        contract: &str,
        params: &[(String, Value)],
        captures: &BTreeMap<String, Value>,
        partial_locals: &[(String, Value)],
    ) {
        self.turns.clear();
        let mut header = String::from("// === @virtual CONTRACT ===\n");
        for line in contract.lines() {
            header.push_str("// ");
            header.push_str(line);
            header.push('\n');
        }
        header.push('\n');
        header.push_str(signature);
        header.push_str(" {");

        let mut tracker = DisplayTracker::for_virtual_param();
        if !params.is_empty() {
            header.push_str("\n  // bound params:");
            for (name, val) in params {
                let receipt = format_binding(name, val, &mut tracker);
                for line in receipt.lines() {
                    header.push_str("\n  ");
                    header.push_str(line);
                }
            }
        }
        if !captures.is_empty() {
            header.push_str("\n  // captured (@uses):");
            for (name, val) in captures {
                let receipt = format_binding(name, val, &mut tracker);
                for line in receipt.lines() {
                    header.push_str("\n  ");
                    header.push_str(line);
                }
            }
        }
        if !partial_locals.is_empty() {
            header.push_str(
                "\n  // precomputed by JS prefix (already executed — live in scope, do NOT re-run):",
            );
            for (name, val) in partial_locals {
                let receipt = format_binding(name, val, &mut tracker);
                for line in receipt.lines() {
                    header.push_str("\n  ");
                    header.push_str(line);
                }
            }
        }

        self.turns.push(TurnDoc {
            turn: 1,
            user_text: contract.to_owned(),
            virtual_header: Some(header),
            segments: Vec::new(),
            live_handles: Vec::new(),
            file_handle_refs: BTreeMap::new(),
            shown_file_handles: BTreeMap::new(),
            pending_rejection: None,
            folded: None,
            folded_prefix: None,
            snapshot_manifest: None,
            snapshot_code_asset_id: None,
            assistant_reply: None,
        });
    }

    /// Record a rejected/uncompiled submission append-only so previous messages stay
    /// 100% byte-identical for KV-cache and every `tool_call_id` receives a matching `tool` error.
    pub fn record_rejection(
        &mut self,
        assistant_message: Option<Value>,
        tool_call_id: Option<String>,
        code: &str,
        message: &str,
    ) {
        if let Some(turn) = self.active_turn_mut() {
            let hint = if turn.virtual_header.is_some() {
                "Fix and call `run_js({ code })` with valid JavaScript statements, finishing with native JS `return { ..., summary: \"...\" };`."
            } else {
                "Fix and call `run_js({ code })` with valid JavaScript statements, or reply directly in Markdown (without calling `run_js`) if you have all observed evidence."
            };
            let note = format!(
                "// [REJECTED ({code}) — executed: false; nothing ran]\n// {message}\n// {hint}"
            );
            turn.segments.push(TraceSegment {
                source: String::new(),
                inline_receipts: Vec::new(),
                error_note: Some(note),
                assistant_message,
                tool_call_id,
                is_rejection: true,
            });
            turn.pending_rejection = None;
        }
    }

    /// Record a completed JS segment from the live `Runtime`.
    #[allow(clippy::too_many_arguments)]
    pub fn record_completed_segment(
        &mut self,
        source: String,
        runtime: &Runtime,
        pre_fingerprints: &BTreeMap<String, String>,
        console_logs: &[String],
        extra_note: Option<String>,
        hints: &SegmentContextHints,
        assistant_message: Option<Value>,
        tool_call_id: Option<String>,
    ) {
        let inline_budget = self.inline_budget_chars();
        let live_handles = collect_live_handles(runtime);
        if let Some(turn) = self.active_turn_mut() {
            let segment_index = turn.segments.len();
            let mut receipts = format_segment_feedback(
                &source,
                runtime,
                pre_fingerprints,
                console_logs,
                hints,
                extra_note.is_some(),
                inline_budget,
                Some(&mut turn.shown_file_handles),
                segment_index,
            );
            if !hints.virtual_receipts.is_empty() {
                let mut combined = hints.virtual_receipts.clone();
                combined.extend(receipts);
                receipts = combined;
            }
            append_new_handle_note(&mut receipts, &turn.live_handles, &live_handles);
            turn.segments.push(TraceSegment {
                source,
                inline_receipts: receipts,
                error_note: extra_note,
                assistant_message,
                tool_call_id,
                is_rejection: false,
            });
            turn.live_handles = live_handles;
            turn.pending_rejection = None;
        }
    }

    /// Record a segment that threw an uncaught runtime error after starting.
    #[allow(clippy::too_many_arguments)]
    pub fn record_failed_segment(
        &mut self,
        source: String,
        runtime: &Runtime,
        pre_fingerprints: &BTreeMap<String, String>,
        console_logs: &[String],
        error: &Value,
        assistant_message: Option<Value>,
        tool_call_id: Option<String>,
    ) {
        let inline_budget = self.inline_budget_chars();
        let empty_hints = SegmentContextHints::default();
        let err_summary = error["message"]
            .as_str()
            .or_else(|| error["error"].as_str())
            .map(|s| s.lines().next().unwrap_or(s).to_owned())
            .unwrap_or_else(|| compact_single_line(&error.to_string(), 200));
        let err_detail = format!(
            "// [RUNTIME ERROR — completed side effects remain applied; do NOT replay writes]\n// [aborted: {err_summary}]\n// detail: {}",
            compact_single_line(&error.to_string(), 600)
        );
        let live_handles = collect_live_handles(runtime);
        if let Some(turn) = self.active_turn_mut() {
            let segment_index = turn.segments.len();
            let mut receipts = format_segment_feedback(
                &source,
                runtime,
                pre_fingerprints,
                console_logs,
                &empty_hints,
                false,
                inline_budget,
                Some(&mut turn.shown_file_handles),
                segment_index,
            );
            append_new_handle_note(&mut receipts, &turn.live_handles, &live_handles);
            turn.segments.push(TraceSegment {
                source,
                inline_receipts: receipts,
                error_note: Some(err_detail),
                assistant_message,
                tool_call_id,
                is_rejection: false,
            });
            turn.live_handles = live_handles;
            turn.pending_rejection = None;
        }
    }

    /// Record a segment that paused cleanly to ask the user a clarifying question (`host.ask_user`).
    #[allow(clippy::too_many_arguments)]
    pub fn record_paused_for_user(
        &mut self,
        source: String,
        runtime: &Runtime,
        pre_fingerprints: &BTreeMap<String, String>,
        console_logs: &[String],
        hints: &SegmentContextHints,
        assistant_message: Option<Value>,
        tool_call_id: Option<String>,
    ) {
        let inline_budget = self.inline_budget_chars();
        let live_handles = collect_live_handles(runtime);
        if let Some(turn) = self.active_turn_mut() {
            let segment_index = turn.segments.len();
            let mut receipts = format_segment_feedback(
                &source,
                runtime,
                pre_fingerprints,
                console_logs,
                hints,
                false,
                inline_budget,
                Some(&mut turn.shown_file_handles),
                segment_index,
            );
            if !hints.virtual_receipts.is_empty() {
                let mut combined = hints.virtual_receipts.clone();
                combined.extend(receipts);
                receipts = combined;
            }
            append_new_handle_note(&mut receipts, &turn.live_handles, &live_handles);
            turn.segments.push(TraceSegment {
                source,
                inline_receipts: receipts,
                error_note: Some("// [paused for user input via host.ask_user]".into()),
                assistant_message,
                tool_call_id,
                is_rejection: false,
            });
            turn.live_handles = live_handles;
            turn.pending_rejection = None;
        }
    }

    /// Seal the active turn with the final user-facing Markdown reply.
    pub fn finish_turn(&mut self, assistant_markdown: impl Into<String>) {
        if let Some(turn) = self.active_turn_mut() {
            turn.live_handles.clear();
            turn.file_handle_refs.clear();
            turn.shown_file_handles.clear();
            turn.pending_rejection = None;
            turn.assistant_reply = Some(assistant_markdown.into());
        }
    }

    /// Fold a completed turn exactly once. The original segments remain in the
    /// persisted document for audit/rewind, while rendering switches to the
    /// immutable native protocol representation.
    pub fn fold_turn(
        &mut self,
        turn_number: usize,
        manifest: impl Into<String>,
        code_asset_id: Option<String>,
    ) -> bool {
        let Some(turn) = self.turns.iter_mut().find(|turn| turn.turn == turn_number) else {
            return false;
        };
        if turn.folded.is_some() || turn.assistant_reply.is_none() {
            return false;
        }
        let steps = turn
            .segments
            .iter()
            .filter(|segment| !segment.is_rejection && !segment.source.trim().is_empty())
            .count();
        if steps == 0 {
            return false;
        }
        let mut code = format!(
            "// [Turn {turn_number} folded from {steps} steps; Turn {turn_number} VM ended]\n"
        );
        for segment in &turn.segments {
            if !segment.is_rejection && !segment.source.trim().is_empty() {
                code.push_str(&segment.source);
                code.push_str("\n");
            }
        }
        if code.chars().count() > 16 * 1024 {
            if let Some(asset_id) = code_asset_id.as_deref() {
                let prefix: String = code.chars().take(12 * 1024).collect();
                code = format!("{prefix}\n// [full folded code snapshot: {asset_id}]\n");
            }
        }
        let (assistant_message, tool_call_id) = fold_message(turn, turn_number);
        turn.folded = Some(FoldedTurn {
            code,
            manifest: manifest.into(),
            code_asset_id,
            assistant_message,
            tool_call_id,
        });
        true
    }

    /// Fold an old prefix of the active turn while retaining the live runtime.
    /// Later calls may extend the immutable prefix to a larger cutoff.
    pub fn fold_active_prefix(
        &mut self,
        turn_number: usize,
        cutoff: usize,
        manifest: impl Into<String>,
        code_asset_id: Option<String>,
    ) -> bool {
        let Some(turn) = self.turns.iter_mut().find(|turn| turn.turn == turn_number) else {
            return false;
        };
        if turn.assistant_reply.is_some() || cutoff == 0 || cutoff > turn.segments.len() {
            return false;
        }
        if let Some(existing) = &turn.folded_prefix {
            if cutoff <= existing.cutoff {
                return false;
            }
        }
        if !turn.segments[..cutoff]
            .iter()
            .any(|segment| !segment.is_rejection && !segment.source.trim().is_empty())
        {
            return false;
        }
        let existing_cutoff = turn.folded_prefix.as_ref().map_or(0, |fold| fold.cutoff);
        let total_steps = turn.segments[..cutoff]
            .iter()
            .filter(|segment| !segment.is_rejection && !segment.source.trim().is_empty())
            .count();
        let mut code = turn
            .folded_prefix
            .as_ref()
            .map(|fold| fold.code.clone())
            .unwrap_or_else(|| {
                format!(
                    "// [Turn {turn_number} folded from {total_steps} steps; live VM retained]\n"
                )
            });
        let segment_start = if turn.folded_prefix.is_some() {
            existing_cutoff
        } else {
            0
        };
        for segment in &turn.segments[segment_start..cutoff] {
            if !segment.is_rejection && !segment.source.trim().is_empty() {
                if !code.ends_with('\n') {
                    code.push('\n');
                }
                code.push_str(&segment.source);
                code.push_str("\n");
            }
        }
        if code.chars().count() > 16 * 1024 {
            if let Some(asset_id) = code_asset_id.as_deref() {
                let prefix: String = code.chars().take(12 * 1024).collect();
                code = format!("{prefix}\n// [full folded code snapshot: {asset_id}]\n");
            }
        }
        let assistant_message = turn
            .folded_prefix
            .as_ref()
            .and_then(|fold| fold.assistant_message.clone())
            .or_else(|| fold_message(turn, turn_number).0);
        let tool_call_id = turn
            .folded_prefix
            .as_ref()
            .and_then(|fold| fold.tool_call_id.clone())
            .or_else(|| fold_message(turn, turn_number).1);
        turn.folded_prefix = Some(FoldedPrefix {
            cutoff,
            code,
            manifest: manifest.into(),
            code_asset_id,
            assistant_message,
            tool_call_id,
        });
        // Any file whose inline body/index lived in segments[..cutoff] is no
        // longer visible in the wire history; evict it so a later re-read expands.
        turn.shown_file_handles
            .retain(|_, entry| entry.segment_index >= cutoff);
        true
    }

    /// Render the clean executed JS trace of `turn` (for durable `.js` archiving in `host_folder`).
    pub fn render_turn_js(&self, turn: usize) -> Option<String> {
        let doc = self.turns.iter().find(|t| t.turn == turn)?;
        if !doc.segments.iter().any(|s| !s.is_rejection) {
            return None;
        }
        let mut out = render_archive_js(doc);
        out.push_str("\n}\n");
        Some(out)
    }

    /// Render native multi-turn wire messages:
    /// `[user #1, assistant(run_js), tool(output), ..., assistant_reply #1, user #2, ...]`.
    /// Every earlier message remains 100% byte-identical as new steps append, preserving
    /// KV-cache prefix hits and native encrypted reasoning replay across providers.
    pub fn render_conversation_messages(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for turn in &self.turns {
            if self.active_start_turn.is_some_and(|start| turn.turn < start) {
                continue;
            }
            let active = turn.assistant_reply.is_none();
            let opening_user = match &turn.virtual_header {
                Some(vh) => vh.clone(),
                None => turn.user_text.clone(),
            };
            out.push(json!({"role": "user", "content": opening_user}));

            let mut rendered_fold = false;
            if let Some(folded) = &turn.folded {
                out.push(folded_assistant_message(folded, turn.turn));
                out.push(json!({"role":"tool","tool_call_id":folded_call_id(folded, turn.turn),"content":folded.manifest}));
                rendered_fold = true;
            } else if let Some(prefix) = &turn.folded_prefix {
                out.push(folded_assistant_message(prefix, turn.turn));
                out.push(json!({"role":"tool","tool_call_id":folded_call_id(prefix, turn.turn),"content":prefix.manifest}));
            }

            for (seg_idx, seg) in turn.segments.iter().enumerate() {
                if rendered_fold
                    || turn
                        .folded_prefix
                        .as_ref()
                        .is_some_and(|prefix| seg_idx < prefix.cutoff)
                {
                    continue;
                }
                let tool_output = format_tool_output(seg);
                let has_native_tool_calls = seg
                    .assistant_message
                    .as_ref()
                    .and_then(|m| m["tool_calls"].as_array())
                    .is_some_and(|arr| !arr.is_empty());

                if has_native_tool_calls {
                    let msg = seg.assistant_message.as_ref().unwrap();
                    out.push(msg.clone());
                    let calls = msg["tool_calls"].as_array().unwrap();
                    let primary_id = seg
                        .tool_call_id
                        .as_deref()
                        .filter(|id| !id.is_empty())
                        .or_else(|| calls[0]["id"].as_str())
                        .unwrap_or("call_run_js");
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": primary_id,
                        "content": tool_output,
                    }));
                    // Defensive protocol guard: if a provider ignored `parallel_tool_calls: false`
                    // and emitted extra calls, answer every remaining `tool_call_id` so the wire
                    // history remains strictly valid for OpenAI / Anthropic / Responses API.
                    for extra in calls.iter().skip(1) {
                        if let Some(extra_id) = extra["id"].as_str().filter(|id| !id.is_empty()) {
                            out.push(json!({
                                "role": "tool",
                                "tool_call_id": extra_id,
                                "content": "[Ignored: only one run_js call per step is executed; submit one run_js call at a time.]",
                            }));
                        }
                    }
                } else if seg.is_rejection {
                    // Non-tool rejection (e.g. plain text inside `@virtual` or empty response).
                    if let Some(msg) = &seg.assistant_message
                        && msg["content"]
                            .as_str()
                            .is_some_and(|s| !s.trim().is_empty())
                    {
                        out.push(msg.clone());
                    }
                    out.push(json!({"role": "user", "content": tool_output}));
                } else {
                    // Legacy persisted segment without saved `assistant_message`: synthesize
                    // a valid native `(assistant(run_js), tool)` pair.
                    let synthetic_id = seg
                        .tool_call_id
                        .clone()
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| format!("call_t{}_s{}", turn.turn, seg_idx + 1));
                    out.push(json!({
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [{
                            "id": synthetic_id,
                            "type": "function",
                            "function": {
                                "name": "run_js",
                                "arguments": json!({"code": seg.source}).to_string(),
                            }
                        }]
                    }));
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": synthetic_id,
                        "content": tool_output,
                    }));
                }
            }

            if active && let Some(rejection) = &turn.pending_rejection {
                out.push(json!({"role": "user", "content": rejection}));
            }
            if let Some(reply) = &turn.assistant_reply {
                out.push(json!({"role": "assistant", "content": reply}));
            }
        }
        out
    }
}

fn fold_message(turn: &TurnDoc, turn_number: usize) -> (Option<Value>, Option<String>) {
    let original = turn
        .segments
        .iter()
        .find(|segment| !segment.is_rejection && segment.assistant_message.is_some())
        .and_then(|segment| segment.assistant_message.clone());
    let call_id = turn
        .segments
        .iter()
        .find(|segment| !segment.is_rejection)
        .and_then(|segment| segment.tool_call_id.clone())
        .unwrap_or_else(|| format!("call_folded_t{turn_number}"));
    (original, Some(call_id))
}

fn folded_call_id(fold: &impl FoldMessage, turn: usize) -> std::borrow::Cow<'_, str> {
    fold.tool_call_id()
        .filter(|id| !id.is_empty())
        .map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| format!("call_folded_t{turn}").into())
}

fn folded_assistant_message(fold: &impl FoldMessage, turn: usize) -> Value {
    let call_id = folded_call_id(fold, turn);
    let mut message = fold
        .assistant_message()
        .cloned()
        .unwrap_or_else(|| json!({"role":"assistant","content":Value::Null}));
    if let Some(obj) = message.as_object_mut() {
        obj.remove("_reasoning_items");
        obj.remove("reasoning_content");
        if !fold.keep_thinking_blocks() {
            obj.remove(crate::model_stream::THINKING_BLOCKS);
        }
        obj.insert("role".into(), json!("assistant"));
        obj.insert("content".into(), Value::Null);
        obj.insert("tool_calls".into(), json!([{"id":call_id,"type":"function","function":{"name":"run_js","arguments":json!({"code":fold.code()}).to_string()}}]));
    }
    message
}

trait FoldMessage {
    fn code(&self) -> &str;
    fn assistant_message(&self) -> Option<&Value>;
    fn tool_call_id(&self) -> Option<&str>;
    fn keep_thinking_blocks(&self) -> bool;
}
impl FoldMessage for FoldedTurn {
    fn code(&self) -> &str {
        &self.code
    }
    fn assistant_message(&self) -> Option<&Value> {
        self.assistant_message.as_ref()
    }
    fn tool_call_id(&self) -> Option<&str> {
        self.tool_call_id.as_deref()
    }
    fn keep_thinking_blocks(&self) -> bool { false }
}
impl FoldMessage for FoldedPrefix {
    fn code(&self) -> &str {
        &self.code
    }
    fn assistant_message(&self) -> Option<&Value> {
        self.assistant_message.as_ref()
    }
    fn tool_call_id(&self) -> Option<&str> {
        self.tool_call_id.as_deref()
    }
    fn keep_thinking_blocks(&self) -> bool { true }
}

fn collect_live_handles(runtime: &Runtime) -> Vec<String> {
    let mut entries = Vec::new();
    for (name, val) in runtime.named_bindings() {
        if name.starts_with('\0') || name == "$_" {
            continue;
        }
        let Some(obj) = val.as_object() else {
            continue;
        };
        let Some(ref_id) = obj.get("__ref").and_then(Value::as_str) else {
            continue;
        };
        if let (Some(path), Some(content)) = (
            obj.get("path").and_then(Value::as_str),
            obj.get("content").and_then(Value::as_str),
        ) {
            let lines = content
                .lines()
                .count()
                .max(usize::from(!content.is_empty()));
            entries.push(format!("{name}={ref_id}({path:?},{lines}L)"));
        } else if obj.contains_key("exit_code") {
            let stdout = obj.get("stdout").and_then(Value::as_str).unwrap_or("");
            let stderr = obj.get("stderr").and_then(Value::as_str).unwrap_or("");
            let lines = stdout.lines().count() + stderr.lines().count();
            entries.push(format!("{name}={ref_id}(cmd,{lines}L)"));
        }
    }
    entries
}

/// Append a compact, permanent 1-line note listing ONLY newly bound silent
/// `Ref#N` handles created in this step (i.e. not already recorded in earlier steps
/// and not already explicitly printed with their `Ref#` in `receipts`).
/// Because this is written once into `inline_receipts`, the wire history stays
/// 100% strictly byte-exact Append-Only (zero KV-cache invalidation).
fn append_new_handle_note(
    receipts: &mut Vec<String>,
    previous_handles: &[String],
    current_handles: &[String],
) {
    fn extract_ref(entry: &str) -> &str {
        entry
            .split_once('=')
            .and_then(|(_, rest)| rest.split('(').next())
            .unwrap_or("")
    }
    let prev_refs: std::collections::BTreeSet<&str> = previous_handles
        .iter()
        .map(|e| extract_ref(e))
        .filter(|r| !r.is_empty())
        .collect();
    let mut seen_new_refs: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let added: Vec<&str> = current_handles
        .iter()
        .filter(|entry| {
            let ref_token = extract_ref(entry);
            if ref_token.is_empty() {
                return !previous_handles.contains(entry);
            }
            if prev_refs.contains(ref_token) || !seen_new_refs.insert(ref_token) {
                return false;
            }
            // Skip if the handle's `Ref#N` was already explicitly printed in this step's receipts.
            !receipts.iter().any(|r| r.contains(ref_token))
        })
        .map(String::as_str)
        .collect();
    if !added.is_empty() {
        receipts.push(format!("// [live handles added: {}]", added.join(", ")));
    }
}

fn format_tool_output(seg: &TraceSegment) -> String {
    let mut parts = Vec::new();
    for receipt in &seg.inline_receipts {
        if !receipt.trim().is_empty() {
            parts.push(receipt.clone());
        }
    }
    if let Some(err) = &seg.error_note
        && !err.trim().is_empty()
    {
        parts.push(err.clone());
    }
    if parts.is_empty() {
        "(ok)".to_owned()
    } else {
        parts.join("\n")
    }
}

fn render_archive_js(turn: &TurnDoc) -> String {
    let mut out = match &turn.virtual_header {
        Some(vh) => vh.clone(),
        None => format!(
            "// === USER GOAL ===\n{}\n\n// === EXECUTION TRACE (Append-Only) ===\nasync function executeTask() {{",
            turn.user_text
        ),
    };
    for seg in &turn.segments {
        if seg.is_rejection {
            continue;
        }
        out.push('\n');
        for line in seg.source.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
        for receipt in &seg.inline_receipts {
            for line in receipt.lines() {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
        if let Some(err) = &seg.error_note {
            for line in err.lines() {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out.trim_end_matches('\n').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(id: &str, thinking: bool) -> Value {
        let mut value = json!({"role":"assistant","content":Value::Null,"reasoning_content":"stale","_reasoning_items":[{"type":"reasoning","id":"r"}],"tool_calls":[{"id":id,"type":"function","function":{"name":"run_js","arguments":"{}"}}]});
        if thinking {
            value[crate::model_stream::THINKING_BLOCKS] =
                json!([{"thinking":"keep","signature":"sig"}]);
        }
        value
    }

    #[test]
    fn completed_fold_has_strict_native_wire_shape_and_keeps_prefix_bytes() {
        let mut doc = ContextDocument::default();
        doc.begin_turn(1, "inspect file");
        if let Some(turn) = doc.active_turn_mut() {
            turn.segments.push(TraceSegment {
                source: "const a = 1;".into(),
                inline_receipts: vec!["// => a = 1".into()],
                error_note: None,
                assistant_message: Some(assistant("call-1", true)),
                tool_call_id: Some("call-1".into()),
                is_rejection: false,
            });
            turn.segments.push(TraceSegment {
                source: "const b = a + 1;".into(),
                inline_receipts: vec!["// => b = 2".into()],
                error_note: None,
                assistant_message: Some(assistant("call-2", false)),
                tool_call_id: Some("call-2".into()),
                is_rejection: false,
            });
        }
        doc.finish_turn("done");
        assert!(doc.fold_turn(1, "{\"frozen\":true}", None));
        let first = doc.render_conversation_messages();
        assert_eq!(
            first
                .iter()
                .map(|m| m["role"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            ["user", "assistant", "tool", "assistant"]
        );
        assert_eq!(first[1]["tool_calls"][0]["function"]["name"], "run_js");
        assert!(
            first[1]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap()
                .contains("const a = 1;")
        );
        assert!(!first[0]["content"].as_str().unwrap().contains("const a"));
        assert!(first[1].get(crate::model_stream::THINKING_BLOCKS).is_none());
        assert!(first[1].get("_reasoning_items").is_none());
        assert!(first[1].get("reasoning_content").is_none());
        let prefix = first[..3].to_vec();
        doc.begin_turn(2, "next");
        let second = doc.render_conversation_messages();
        assert_eq!(&second[..3], &prefix[..]);
    }

    #[test]
    fn active_prefix_fold_keeps_recent_steps_and_thinking_metadata() {
        let mut doc = ContextDocument::default();
        doc.begin_turn(1, "work");
        for i in 0..6 {
            doc.active_turn_mut().unwrap().segments.push(TraceSegment {
                source: format!("let s{i} = {i};"),
                inline_receipts: vec![],
                error_note: None,
                assistant_message: Some(assistant(&format!("c{i}"), i == 0)),
                tool_call_id: Some(format!("c{i}")),
                is_rejection: false,
            });
        }
        assert!(doc.fold_active_prefix(1, 2, "{\"live_vm\":true}", None));
        let wire = doc.render_conversation_messages();
        assert!(
            wire.iter()
                .any(|message| message["role"] == "tool"
                    && message["content"] == "{\"live_vm\":true}")
        );
        assert!(wire.iter().any(|message| {
            message["role"] == "assistant"
                && message["tool_calls"][0]["function"]["arguments"]
                    .as_str()
                    .is_some_and(|code| code.contains("let s0 = 0;"))
        }));
        assert!(wire.iter().any(|message| message["role"] == "assistant"
            && message["tool_calls"].is_array()
            && message["tool_calls"][0]["id"] == "c5"));
        assert!(doc.fold_active_prefix(1, 4, "rewritten", None));
        let second = doc.render_conversation_messages();
        assert!(second.iter().any(|message| {
            message["role"] == "assistant"
                && message["tool_calls"][0]["function"]["arguments"]
                    .as_str()
                    .is_some_and(|code| code.contains("let s3 = 3;"))
        }));
        assert_eq!(
            second[1][crate::model_stream::THINKING_BLOCKS][0]["thinking"],
            "keep"
        );
        assert!(second[1].get("_reasoning_items").is_none());
        assert!(second[1].get("reasoning_content").is_none());
    }

    #[test]
    fn inline_budget_is_clamped_to_a_quarter_of_remaining_chars() {
        assert_eq!(inline_limit_for_budget(200_000), 50_000);
        assert_eq!(inline_limit_for_budget(400_000), INLINE_LIMIT_CHARS);
        assert_eq!(inline_limit_for_budget(3), 1);
    }

    #[test]
    fn anthropic_messages_wire_validates_completed_and_active_folds() {
        let mut doc = ContextDocument::default();
        doc.begin_turn(1, "completed goal");
        for turn in 1..=2 {
            if turn == 2 {
                doc.begin_turn(turn, "active goal");
            }
            for step in 0..3 {
                let id = format!("call_t{turn}_s{step}");
                doc.active_turn_mut().unwrap().segments.push(TraceSegment {
                    source: format!("const step{step} = {step};"),
                    inline_receipts: vec![],
                    error_note: None,
                    assistant_message: Some(assistant(&id, true)),
                    tool_call_id: Some(id),
                    is_rejection: false,
                });
            }
            if turn == 1 {
                doc.finish_turn("done");
                assert!(doc.fold_turn(turn, "completed manifest", None));
            } else {
                assert!(doc.fold_active_prefix(turn, 2, "active manifest", None));
                // Older persisted prefix folds could omit this field.
                doc.active_turn_mut().unwrap().folded_prefix.as_mut().unwrap().tool_call_id = None;
            }
        }
        let temp = crate::single_agent::tests::TestWorkspace::new("cac-v2-anthropic-fold");
        let mut state = crate::agent_service::tests::test_state(temp.path(), "127.0.0.1:1".parse().unwrap());
        state.provider_name = "deepseek-self".into();
        state.provider_routes.insert(state.provider_name.clone(), crate::agent_service::AgentProviderRoute {
            responses: false,
            messages: true,
            messages_capabilities: [("deepseek-flash".into(), crate::anthropic_messages::Capabilities {
                tool_update: true, system_prompt_update: true,
            })].into_iter().collect(),
            url: state.provider_url.clone(), api_key: String::new(), models: Default::default(),
            max_output_tokens: Default::default(), context_windows: Default::default(),
            long_context_thresholds: Default::default(), model_costs: Default::default(),
        });
        let mut messages = vec![json!({"role":"system","content":"static rules"})];
        messages.extend(doc.render_conversation_messages());
        let chat = json!({"model":"deepseek-flash","messages":messages,
            "tools":[{"type":"function","function":{"name":"run_js","parameters":{"type":"object"}}}]});
        let wire = crate::anthropic_messages::wire_body(&state, "deepseek-flash", &chat);
        let history = wire["messages"].as_array().unwrap();
        assert_eq!(history.iter().map(|m| m["role"].as_str().unwrap()).collect::<Vec<_>>(),
            ["user", "assistant", "user", "assistant", "user", "assistant", "user", "assistant", "user"]);
        assert_eq!(history[1]["content"][0]["type"], "tool_use");
        assert_eq!(history[5]["content"][0], json!({"type":"thinking","thinking":"keep","signature":"sig"}));
        assert_eq!(history[7]["content"][0], history[5]["content"][0]);
        for (index, expected_id) in [(1, "call_t1_s0"), (5, "call_folded_t2"), (7, "call_t2_s2")] {
            let calls = history[index]["content"].as_array().unwrap();
            let results = history[index + 1]["content"].as_array().unwrap();
            let call = calls.last().unwrap();
            assert_eq!(calls.iter().filter(|block| block["type"] == "tool_use").count(), 1);
            assert_eq!(results.len(), 1);
            assert_eq!(call["type"], "tool_use");
            assert_eq!(call["name"], "run_js");
            assert_eq!(call["id"], expected_id);
            assert_eq!(results[0]["type"], "tool_result");
            assert_eq!(results[0]["tool_use_id"], call["id"]);
        }
        assert!(wire["tools"][0].get("defer_loading").is_none());
        assert!(wire.to_string().matches("cache_control").count() <= 4);
    }

    #[test]
    fn active_start_turn_filters_archived_older_turns_from_rendering() {
        let mut doc = ContextDocument::default();
        doc.begin_turn(1, "old");
        doc.active_turn_mut().unwrap().segments.push(TraceSegment {
            source: "let old = 1;".into(),
            inline_receipts: vec![],
            error_note: None,
            assistant_message: Some(assistant("old", false)),
            tool_call_id: Some("old".into()),
            is_rejection: false,
        });
        doc.finish_turn("old reply");
        doc.begin_turn(2, "recent");
        doc.finish_turn("recent reply");
        doc.active_start_turn = Some(2);
        let wire = doc.render_conversation_messages();
        assert!(!wire.iter().any(|message| message["content"] == "old"));
        assert!(wire.iter().any(|message| message["content"] == "recent"));
    }

    #[test]
    fn tag_resource_handle_in_turn_reuses_ref_for_unchanged_file_and_mints_new_on_edit() {
        let mut next_ref_id = 1usize;
        let mut file_refs = BTreeMap::new();

        let mut read1 = json!({"path": "src/demo.rs", "content": "fn a() {}\n"});
        tag_resource_handle_in_turn(&mut read1, &mut next_ref_id, &mut file_refs);
        assert_eq!(read1["__ref"], "Ref#1");
        assert_eq!(next_ref_id, 2);

        // Re-reading the exact same unchanged content reuses Ref#1 without bumping next_ref_id.
        let mut read2 = json!({"path": "src/demo.rs", "content": "fn a() {}\n"});
        tag_resource_handle_in_turn(&mut read2, &mut next_ref_id, &mut file_refs);
        assert_eq!(read2["__ref"], "Ref#1");
        assert_eq!(next_ref_id, 2);

        // Once the file changes, a fresh Ref#2 is minted.
        let mut read3 = json!({"path": "src/demo.rs", "content": "fn a() { let x = 1; }\n"});
        tag_resource_handle_in_turn(&mut read3, &mut next_ref_id, &mut file_refs);
        assert_eq!(read3["__ref"], "Ref#2");
        assert_eq!(next_ref_id, 3);
    }

    #[test]
    fn turn_scoped_handle_dedup_collapses_unchanged_reread_and_reexpands_on_edit_or_fold() {
        use crate::agent_program::{Runtime, State, libraries::Libraries, runtime::ActionKind};

        let mut libs: Libraries = BTreeMap::new();
        libs.insert(
            "io".to_owned(),
            BTreeMap::from([("read".to_owned(), "read_file".to_owned())]),
        );

        let mut doc = ContextDocument::default();
        doc.begin_turn(1, "inspect demo.rs");
        let mut rt = Runtime::new_repl(libs.clone()).unwrap();
        let mut next_ref_id = 1usize;
        let hints = SegmentContextHints::default();
        let empty_fps = BTreeMap::new();

        let mut run_read_step = |doc: &mut ContextDocument,
                                 rt: &mut Runtime,
                                 code: &str,
                                 content: &str,
                                 call_id: &str| {
            rt.prepare_segment(code, libs.clone(), false).unwrap();
            loop {
                match rt.next() {
                    State::Action(action) => {
                        let ActionKind::Tool { name, .. } = action.kind else {
                            panic!("unexpected action");
                        };
                        assert_eq!(name, "read_file");
                        let mut res = json!({
                            "path": "src/demo.rs",
                            "content": content,
                        });
                        let file_refs = &mut doc.active_turn_mut().unwrap().file_handle_refs;
                        tag_resource_handle_in_turn(&mut res, &mut next_ref_id, file_refs);
                        rt.resume_action(&action.id, res, false).unwrap();
                    }
                    State::Returned(_) => break,
                    other => panic!("unexpected VM state: {other:?}"),
                }
            }
            doc.record_completed_segment(
                code.to_owned(),
                rt,
                &empty_fps,
                &[],
                None,
                &hints,
                Some(assistant(call_id, false)),
                Some(call_id.to_owned()),
            );
        };

        // Step 1: first read expands full line-numbered content inline.
        run_read_step(
            &mut doc,
            &mut rt,
            "const f1 = io.read({ path: \"src/demo.rs\" });",
            "fn alpha() {}\nfn beta() {}\n",
            "c1",
        );
        let seg1 = doc.active_turn().unwrap().segments[0]
            .inline_receipts
            .join("\n");
        assert!(seg1.contains("1|fn alpha() {}"), "{seg1}");

        // Step 2: unchanged re-read reuses Ref#1 and emits a 1-line compact reuse receipt
        // instead of repeating the full file body.
        run_read_step(
            &mut doc,
            &mut rt,
            "const f2 = io.read({ path: \"src/demo.rs\" });",
            "fn alpha() {}\nfn beta() {}\n",
            "c2",
        );
        let seg2 = doc.active_turn().unwrap().segments[1]
            .inline_receipts
            .join("\n");
        assert!(
            seg2.contains("Ref#1") && seg2.contains("unchanged since step 1"),
            "{seg2}"
        );
        assert!(!seg2.contains("1|fn alpha() {}"), "{seg2}");

        // Step 3: explicit .slice() on the live handle is NEVER suppressed by handle dedup.
        rt.prepare_segment("f2.slice(1, 2);", libs.clone(), false)
            .unwrap();
        assert!(matches!(rt.next(), State::Returned(_)));
        doc.record_completed_segment(
            "f2.slice(1, 2);".to_owned(),
            &rt,
            &empty_fps,
            &[],
            None,
            &hints,
            Some(assistant("c3", false)),
            Some("c3".to_owned()),
        );
        let seg3 = doc.active_turn().unwrap().segments[2]
            .inline_receipts
            .join("\n");
        assert!(seg3.contains("1|fn alpha() {}"), "{seg3}");

        // Step 4: after folding away segments[..2] (where Step 1 lived), re-reading the same
        // unchanged file MUST expand again so the model never suffers amnesia.
        assert!(doc.fold_active_prefix(1, 2, "{\"folded\":true}", None));
        run_read_step(
            &mut doc,
            &mut rt,
            "const f3 = io.read({ path: \"src/demo.rs\" });",
            "fn alpha() {}\nfn beta() {}\n",
            "c4",
        );
        let seg4 = doc.active_turn().unwrap().segments[3]
            .inline_receipts
            .join("\n");
        assert!(seg4.contains("1|fn alpha() {}"), "{seg4}");

        // Step 5: once the file is modified on disk, reading it mints Ref#2 and expands new content.
        run_read_step(
            &mut doc,
            &mut rt,
            "const f4 = io.read({ path: \"src/demo.rs\" });",
            "fn alpha_v2() {}\nfn beta() {}\n",
            "c5",
        );
        let seg5 = doc.active_turn().unwrap().segments[4]
            .inline_receipts
            .join("\n");
        assert!(
            seg5.contains("Ref#2") && seg5.contains("1|fn alpha_v2() {}"),
            "{seg5}"
        );
    }
}
