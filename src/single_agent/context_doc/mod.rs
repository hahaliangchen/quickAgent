//! Context-as-Code v2 document & wire renderer.
//!
//! Layout of the active `user #N` message:
//! - **100% Immutable Append-Only execution trace** (strict byte-exact KV-cache prefix):
//!   - User's request (`// === USER GOAL ===`) + `async function executeTask() {`
//!   - Executed JS segments, each followed immediately by permanent `// =>` receipts
//!     (results `<= 64KB` printed inline permanently; results `> 64KB` represented as
//!     compact `ResourceHandle` signatures + high-entropy outline/error indexes so
//!     the model can slice/grep any portion on demand without losing prior context).
//! - Rejected/uncompiled attempts attach a transient `pending_rejection` note until
//!   the next valid step executes (never mutating prior executed segments).

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
    /// Permanent abort/pause/host note if the segment stopped early or needs follow-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_note: Option<String>,
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
            unfinished.pending_rejection = None;
            unfinished.assistant_reply = Some("[Turn ended before final reply.]".into());
        }
        self.turns.retain(|t| t.turn < turn);
        self.turns.push(TurnDoc {
            turn,
            user_text: user_text.into(),
            virtual_header: None,
            segments: Vec::new(),
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

        self.turns.push(TurnDoc {
            turn: 1,
            user_text: contract.to_owned(),
            virtual_header: Some(header),
            segments: Vec::new(),
            pending_rejection: None,
            assistant_reply: None,
        });
    }

    /// Record a rejected/uncompiled submission strictly in `pending_rejection`
    /// without mutating the immutable `segments` prefix.
    pub fn set_rejection(&mut self, code: &str, message: &str) {
        if let Some(turn) = self.active_turn_mut() {
            let hint = if turn.virtual_header.is_some() {
                "Fix and call `run_js({ code })` with valid JavaScript statements, finishing with native JS `return { ..., summary: \"...\" };`."
            } else {
                "Fix and call `run_js({ code })` with valid JavaScript statements, or reply directly in Markdown (without calling `run_js`) if you have all observed evidence."
            };
            turn.pending_rejection = Some(format!(
                "// [REJECTED ({code}) — executed: false; nothing ran]\n// {message}\n// {hint}"
            ));
        }
    }

    /// Record a completed JS segment from the live `Runtime`.
    pub fn record_completed_segment(
        &mut self,
        source: String,
        runtime: &Runtime,
        pre_fingerprints: &BTreeMap<String, String>,
        console_logs: &[String],
        extra_note: Option<String>,
        hints: &SegmentContextHints,
    ) {
        let mut receipts = format_segment_feedback(runtime, pre_fingerprints, console_logs, hints);
        if !hints.virtual_receipts.is_empty() {
            let mut combined = hints.virtual_receipts.clone();
            combined.extend(receipts);
            receipts = combined;
        }
        if let Some(turn) = self.active_turn_mut() {
            turn.segments.push(TraceSegment {
                source,
                inline_receipts: receipts,
                error_note: extra_note,
            });
            turn.pending_rejection = None;
        }
    }

    /// Record a segment that threw an uncaught runtime error after starting.
    pub fn record_failed_segment(
        &mut self,
        source: String,
        runtime: &Runtime,
        pre_fingerprints: &BTreeMap<String, String>,
        console_logs: &[String],
        error: &Value,
    ) {
        let empty_hints = SegmentContextHints::default();
        let receipts = format_segment_feedback(runtime, pre_fingerprints, console_logs, &empty_hints);
        let err_summary = error["message"]
            .as_str()
            .or_else(|| error["error"].as_str())
            .map(|s| s.lines().next().unwrap_or(s).to_owned())
            .unwrap_or_else(|| compact_single_line(&error.to_string(), 200));
        let err_detail = format!(
            "// [RUNTIME ERROR — completed side effects remain applied; do NOT replay writes]\n// [aborted: {err_summary}]\n// detail: {}",
            compact_single_line(&error.to_string(), 600)
        );
        if let Some(turn) = self.active_turn_mut() {
            turn.segments.push(TraceSegment {
                source,
                inline_receipts: receipts,
                error_note: Some(err_detail),
            });
            turn.pending_rejection = None;
        }
    }

    /// Record a segment that paused cleanly to ask the user a clarifying question (`host.ask_user`).
    pub fn record_paused_for_user(
        &mut self,
        source: String,
        runtime: &Runtime,
        pre_fingerprints: &BTreeMap<String, String>,
        console_logs: &[String],
        hints: &SegmentContextHints,
    ) {
        let mut receipts = format_segment_feedback(runtime, pre_fingerprints, console_logs, hints);
        if !hints.virtual_receipts.is_empty() {
            let mut combined = hints.virtual_receipts.clone();
            combined.extend(receipts);
            receipts = combined;
        }
        if let Some(turn) = self.active_turn_mut() {
            turn.segments.push(TraceSegment {
                source,
                inline_receipts: receipts,
                error_note: Some("// [paused for user input via host.ask_user]".into()),
            });
            turn.pending_rejection = None;
        }
    }

    /// Seal the active turn with the final user-facing Markdown reply.
    pub fn finish_turn(&mut self, assistant_markdown: impl Into<String>) {
        if let Some(turn) = self.active_turn_mut() {
            turn.pending_rejection = None;
            turn.assistant_reply = Some(assistant_markdown.into());
        }
    }

    /// Render the clean executed JS trace of `turn` (for durable `.js` archiving in `host_folder`).
    pub fn render_turn_js(&self, turn: usize) -> Option<String> {
        let doc = self.turns.iter().find(|t| t.turn == turn)?;
        if doc.segments.is_empty() {
            return None;
        }
        let mut out = render_stable_user(doc);
        out.push_str("\n}\n");
        Some(out)
    }

    #[cfg(test)]
    pub fn stable_active_user_prefix(&self) -> Option<String> {
        self.active_turn().map(render_stable_user)
    }

    /// Render paired `(user #1, assistant #1, ..., user #N)` wire messages.
    pub fn render_conversation_messages(&self) -> Vec<Value> {
        let start = self.history_start();
        let mut out = Vec::new();
        for turn in self.turns.iter().skip(start) {
            let active = turn.assistant_reply.is_none();
            let mut user_content = render_stable_user(turn);
            if active && let Some(rejection) = &turn.pending_rejection {
                user_content.push_str("\n\n");
                user_content.push_str(rejection);
            }
            out.push(json!({"role": "user", "content": user_content}));
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

fn render_stable_user(turn: &TurnDoc) -> String {
    let mut out = match &turn.virtual_header {
        Some(vh) => vh.clone(),
        None => format!(
            "// === USER GOAL ===\n{}\n\n// === EXECUTION TRACE (Append-Only) ===\nasync function executeTask() {{",
            turn.user_text
        ),
    };
    for seg in &turn.segments {
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
