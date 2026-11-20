//! Context-as-Code v2 document & wire renderer.
//!
//! Layout of the active `user #N` message:
//! 1. **Immutable Append-Only prefix** (100% KV-cache friendly):
//!    - User's request (`// === USER GOAL ===`) + `async function executeTask() {`
//!    - Executed JS segments, each followed immediately by permanent, neutral
//!      `// =>` receipts (short values `<= 2KB` verbatim; medium/large handles
//!      as compact one-line signatures).
//! 2. **Ephemeral tail (`EPHEMERAL_PEEK`, lives for 1 step only)**:
//!    - Full line-numbered view of medium results (`2KB..=24KB`) or high-entropy
//!      index for large results (`> 24KB`), deduplicated across aliases and
//!      suppressed when delegated to `@virtual`.
//!    - Dropped on the next step or when the turn seals, leaving the prior byte
//!      prefix 100% unchanged.

mod format;
mod indexers;

pub use format::SegmentContextHints;
pub(super) use format::{collect_delegated_markers, tag_resource_handle};

use crate::agent_program::Runtime;
use format::{PeekTracker, format_binding_three_tier, format_segment_feedback};
use indexers::compact_single_line;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Max chars printed verbatim in permanent Append-Only `// =>` receipts.
/// Also acts as the anti-cheating threshold lock: slicing > 2KB will NOT dump
/// verbatim into the permanent trace.
pub const INLINE_LIMIT_CHARS: usize = 2_000;

/// Max chars expanded verbatim (with line numbers) inside `EPHEMERAL_PEEK`.
pub const PEEK_FULL_LIMIT_CHARS: usize = 24_000;

/// Multi-turn history budget before dropping oldest completed turns in a batch.
pub const HISTORY_BUDGET_CHARS: usize = 48_000;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_peek: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_reply: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceSegment {
    pub source: String,
    /// Permanent compact `// => ...` lines recorded right after this segment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline_receipts: Vec<String>,
    /// Single-line permanent abort/pause note if the segment stopped early.
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
            unfinished.ephemeral_peek = None;
            unfinished.assistant_reply = Some("[Turn ended before final reply.]".into());
        }
        self.turns.retain(|t| t.turn < turn);
        self.turns.push(TurnDoc {
            turn,
            user_text: user_text.into(),
            virtual_header: None,
            segments: Vec::new(),
            ephemeral_peek: None,
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

        let mut peek_sections = Vec::new();
        let mut tracker = PeekTracker::for_virtual_param();
        if !params.is_empty() {
            header.push_str("\n  // bound params:");
            for (name, val) in params {
                let (receipt, peek) = format_binding_three_tier(name, val, &mut tracker);
                for line in receipt.lines() {
                    header.push_str("\n  ");
                    header.push_str(line);
                }
                if let Some(p) = peek {
                    peek_sections.push(p);
                }
            }
        }
        if !captures.is_empty() {
            header.push_str("\n  // captured (@uses):");
            for (name, val) in captures {
                let (receipt, peek) = format_binding_three_tier(name, val, &mut tracker);
                for line in receipt.lines() {
                    header.push_str("\n  ");
                    header.push_str(line);
                }
                if let Some(p) = peek {
                    peek_sections.push(p);
                }
            }
        }

        self.turns.push(TurnDoc {
            turn: 1,
            user_text: contract.to_owned(),
            virtual_header: Some(header),
            segments: Vec::new(),
            ephemeral_peek: build_ephemeral_peek(peek_sections, None),
            assistant_reply: None,
        });
    }

    /// Record a rejected/uncompiled submission strictly in `ephemeral_peek`
    /// without mutating the immutable `segments` prefix.
    pub fn set_rejection(&mut self, code: &str, message: &str) {
        if let Some(turn) = self.active_turn_mut() {
            let hint = if turn.virtual_header.is_some() {
                "Fix and call `run_js({ code })` with valid JavaScript statements, finishing with native JS `return { ..., summary: \"...\" };`."
            } else {
                "Fix and call `run_js({ code })` with valid JavaScript statements, or reply directly in Markdown (without calling `run_js`) if you have all observed evidence."
            };
            turn.ephemeral_peek = Some(format!(
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
        let (mut receipts, peek_sections) =
            format_segment_feedback(runtime, pre_fingerprints, console_logs, hints);
        if !hints.virtual_receipts.is_empty() {
            let mut combined = hints.virtual_receipts.clone();
            combined.extend(receipts);
            receipts = combined;
        }
        if let Some(turn) = self.active_turn_mut() {
            turn.segments.push(TraceSegment {
                source,
                inline_receipts: receipts,
                error_note: None,
            });
            turn.ephemeral_peek = build_ephemeral_peek(peek_sections, extra_note);
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
        let (receipts, peek_sections) =
            format_segment_feedback(runtime, pre_fingerprints, console_logs, &empty_hints);
        let err_summary = error["message"]
            .as_str()
            .or_else(|| error["error"].as_str())
            .map(|s| s.lines().next().unwrap_or(s).to_owned())
            .unwrap_or_else(|| compact_single_line(&error.to_string(), 200));
        let err_detail = format!(
            "// [RUNTIME ERROR — completed side effects remain applied; do NOT replay writes]\n// {}",
            error
        );
        if let Some(turn) = self.active_turn_mut() {
            turn.segments.push(TraceSegment {
                source,
                inline_receipts: receipts,
                error_note: Some(format!("// [aborted: {err_summary}]")),
            });
            turn.ephemeral_peek = build_ephemeral_peek(peek_sections, Some(err_detail));
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
        let (mut receipts, _) =
            format_segment_feedback(runtime, pre_fingerprints, console_logs, hints);
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
            turn.ephemeral_peek = None;
        }
    }

    /// Seal the active turn with the final user-facing Markdown reply and strip
    /// `ephemeral_peek` completely.
    pub fn finish_turn(&mut self, assistant_markdown: impl Into<String>) {
        if let Some(turn) = self.active_turn_mut() {
            turn.ephemeral_peek = None;
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
            if active && let Some(peek) = &turn.ephemeral_peek {
                user_content.push_str("\n\n");
                user_content.push_str(peek);
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
            out.push_str("  ");
            out.push_str(err);
            out.push('\n');
        }
    }
    out.trim_end_matches('\n').to_owned()
}

fn build_ephemeral_peek(sections: Vec<String>, error_block: Option<String>) -> Option<String> {
    if sections.is_empty() && error_block.is_none() {
        return None;
    }
    let mut out = String::from(
        "// === EPHEMERAL_PEEK (ONLY visible this step; wiped next step!) ===\n\
         // IMPORTANT: Anything shown below disappears on the next step.\n\
         // Pin any needed lines/fields into a JS variable (e.g. `.slice()`, `.filter()`, or object summary <= 2KB) so the exact values stay permanently in `// =>` above.",
    );
    if let Some(err) = error_block {
        out.push_str("\n\n");
        out.push_str(&err);
    }
    for sec in sections {
        out.push_str("\n\n");
        out.push_str(&sec);
    }
    Some(out)
}
