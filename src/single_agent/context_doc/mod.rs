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
pub(super) use format::{collect_delegated_markers, tag_resource_handle};

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

/// Multi-turn history budget before dropping oldest completed turns in a batch.
pub const HISTORY_BUDGET_CHARS: usize = 256_000;

/// Marker prefix for the transient current-turn live VM script snapshot appended
/// ONLY to the very latest `tool` message of an active turn.
/// Cleared automatically from superseded steps and completely absent from completed
/// turns / subsequent user turns.
pub const LIVE_VM_SCRIPT_MARKER: &str = "// === LIVE VM SCRIPT (current turn, read-only) ===";

/// Hard cap (20 KiB) for the transient live VM script snapshot so extreme turns
/// never hurt KV-cache efficiency.
pub const LIVE_VM_SCRIPT_BUDGET_CHARS: usize = 20_480;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContextDocument {
    pub turns: Vec<TurnDoc>,
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
    /// Compact inventory of live `Ref#N` ResourceHandles currently bound in the active
    /// turn's VM (e.g. `authFile=Ref#1("src/auth.rs",120L)`). Cleared when the turn finishes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub live_handles: Vec<String>,
    /// Transient note for a rejected/uncompiled submission (`executed: false`);
    /// cleared as soon as the next valid segment executes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rejection: Option<String>,
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
    pub fn active_turn(&self) -> Option<&TurnDoc> {
        self.turns.last().filter(|t| t.assistant_reply.is_none())
    }

    pub fn active_turn_mut(&mut self) -> Option<&mut TurnDoc> {
        self.turns.last_mut().filter(|t| t.assistant_reply.is_none())
    }

    /// Start a brand-new user turn. If a previous turn was interrupted without
    /// a final reply, seal it first so `(user, assistant)` pairs stay aligned.
    pub fn begin_turn(&mut self, turn: usize, user_text: impl Into<String>) {
        if let Some(unfinished) = self.active_turn_mut() {
            unfinished.live_handles.clear();
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
            pending_rejection: None,
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
            pending_rejection: None,
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
        let mut receipts = format_segment_feedback(
            &source,
            runtime,
            pre_fingerprints,
            console_logs,
            hints,
            extra_note.is_some(),
        );
        if !hints.virtual_receipts.is_empty() {
            let mut combined = hints.virtual_receipts.clone();
            combined.extend(receipts);
            receipts = combined;
        }
        let live_handles = collect_live_handles(runtime);
        if let Some(turn) = self.active_turn_mut() {
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
        let empty_hints = SegmentContextHints::default();
        let receipts = format_segment_feedback(
            &source,
            runtime,
            pre_fingerprints,
            console_logs,
            &empty_hints,
            false,
        );
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
        let mut receipts = format_segment_feedback(
            &source,
            runtime,
            pre_fingerprints,
            console_logs,
            hints,
            false,
        );
        if !hints.virtual_receipts.is_empty() {
            let mut combined = hints.virtual_receipts.clone();
            combined.extend(receipts);
            receipts = combined;
        }
        let live_handles = collect_live_handles(runtime);
        if let Some(turn) = self.active_turn_mut() {
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
            turn.pending_rejection = None;
            turn.assistant_reply = Some(assistant_markdown.into());
        }
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
        let start = self.history_start();
        let mut out = Vec::new();
        for turn in self.turns.iter().skip(start) {
            let active = turn.assistant_reply.is_none();
            let opening_user = match &turn.virtual_header {
                Some(vh) => vh.clone(),
                None => turn.user_text.clone(),
            };
            out.push(json!({"role": "user", "content": opening_user}));

            let live_snapshot = if active {
                render_live_vm_snapshot(turn)
            } else {
                None
            };
            let last_seg_idx = turn.segments.len().saturating_sub(1);

            for (seg_idx, seg) in turn.segments.iter().enumerate() {
                let mut tool_output = format_tool_output(seg);
                if active
                    && seg_idx == last_seg_idx
                    && let Some(snapshot) = &live_snapshot
                {
                    tool_output.push_str("\n\n");
                    tool_output.push_str(snapshot);
                }
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

    fn history_start(&self) -> usize {
        let mut start = 0;
        let mut used = 0usize;
        let completed = self
            .turns
            .len()
            .saturating_sub(usize::from(self.active_turn().is_some()));
        for idx in 0..completed {
            let t = &self.turns[idx];
            let size = t.user_text.chars().count()
                + t.segments
                    .iter()
                    .map(|s| {
                        s.source.chars().count()
                            + s.inline_receipts
                                .iter()
                                .map(|r| r.chars().count())
                                .sum::<usize>()
                    })
                    .sum::<usize>()
                + t.assistant_reply
                    .as_ref()
                    .map_or(0, |r| r.chars().count());
            if idx > start && used + size > HISTORY_BUDGET_CHARS {
                start = idx;
                used = 0;
            }
            used += size;
        }
        start
    }
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
            let lines = content.lines().count().max(usize::from(!content.is_empty()));
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

/// Render the transient mutable code-view layer for the CURRENT active turn ONLY.
/// Contains strictly committed JS code from executed (non-rejection) segments +
/// compact live `Ref#N` handle inventory, capped at `LIVE_VM_SCRIPT_BUDGET_CHARS` (20 KiB).
/// Never persisted into completed turns or carried across user turns.
fn render_live_vm_snapshot(turn: &TurnDoc) -> Option<String> {
    let executed: Vec<(usize, &TraceSegment)> = turn
        .segments
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.is_rejection && !s.source.trim().is_empty())
        .collect();
    if executed.is_empty() {
        return None;
    }

    let mut header = String::from(LIVE_VM_SCRIPT_MARKER);
    header.push_str(
        "\n// Already executed in this turn's live VM (do NOT redeclare or re-read; cleared next turn).",
    );
    if !turn.live_handles.is_empty() {
        header.push_str("\n// Live handles: ");
        header.push_str(&turn.live_handles.join(", "));
    }

    let mut chunks: Vec<String> = Vec::with_capacity(executed.len());
    for (idx, (_, seg)) in executed.iter().enumerate() {
        let mut block = String::new();
        let is_runtime_err = seg.error_note.as_deref().is_some_and(|n| n.contains("[RUNTIME ERROR"));
        if is_runtime_err {
            block.push_str(&format!("// [step {} threw runtime error]\n", idx + 1));
        }
        block.push_str(seg.source.trim_end());
        chunks.push(block);
    }

    let footer = "\n// === END LIVE VM SCRIPT ===";
    let overhead = header.chars().count() + footer.chars().count() + 128;
    let body_budget = LIVE_VM_SCRIPT_BUDGET_CHARS.saturating_sub(overhead);
    let total_body_chars: usize = chunks.iter().map(|c| c.chars().count() + 1).sum();

    let body = if total_body_chars <= body_budget {
        chunks.join("\n")
    } else {
        // Keep the newest committed segments fitting within `body_budget`, folding older ones.
        let mut kept_rev: Vec<&str> = Vec::new();
        let mut used = 0usize;
        for chunk in chunks.iter().rev() {
            let cost = chunk.chars().count() + 1;
            if used + cost > body_budget {
                if kept_rev.is_empty() {
                    // Single chunk larger than budget: keep tail within budget.
                    let tail: String = chunk
                        .chars()
                        .rev()
                        .take(body_budget.saturating_sub(32))
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    let note = format!(
                        "// ... [earlier code folded to stay <= 20KB]\n...{tail}"
                    );
                    return Some(format!("{header}\n{note}{footer}"));
                }
                break;
            }
            kept_rev.push(chunk.as_str());
            used += cost;
        }
        let omitted = chunks.len().saturating_sub(kept_rev.len());
        kept_rev.reverse();
        format!(
            "// ... [earlier {omitted} committed step(s) folded to keep live script <= 20KB; their bindings & Live handles remain active]\n{}",
            kept_rev.join("\n")
        )
    };

    Some(format!("{header}\n{body}{footer}"))
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
