//! Context-as-Code v2 document & three-tier value formatter.
//!
//! Layout of the active `user #N` message:
//! 1. **Immutable Append-Only prefix** (100% KV-cache friendly):
//!    - User's request (`// === USER GOAL ===`)
//!    - `async function executeTask() {`
//!    - Executed JS segments, each followed immediately by permanent, compact
//!      `// =>` receipts (short values `<= 2,000` chars verbatim; medium/large
//!      values as one-line live signatures).
//! 2. **Ephemeral tail (`EPHEMERAL_PEEK`, lives for 1 step only)**:
//!    - Full line-numbered view of medium results (`2,000..=24,000` chars) so
//!      the model sees 100% truth and can pin only the critical lines via JS
//!      `.slice()` / `.filter()` into permanent `// =>`.
//!    - High-entropy index (symbol lines / error lines + tail / schema + sample)
//!      for large results (`> 24,000` chars).
//!    - Any compile/protocol rejection or runtime error from the last attempt.
//!    - Completely dropped on the next step or when the turn seals, leaving the
//!      prior byte prefix 100% unchanged.

use crate::agent_program::Runtime;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Max chars printed verbatim in permanent Append-Only `// =>` receipts.
/// Also acts as the anti-cheating threshold lock: slicing > 2KB will NOT dump
/// verbatim into the permanent trace.
pub const INLINE_LIMIT_CHARS: usize = 2_000;

/// Max chars expanded verbatim (with line numbers) inside `EPHEMERAL_PEEK`.
pub const PEEK_FULL_LIMIT_CHARS: usize = 24_000;

/// Multi-turn history budget before dropping oldest completed turns in a batch.
pub const HISTORY_BUDGET_CHARS: usize = 48_000;

/// Minimum length at which a duplicate inline binding in the same step folds
/// into `(same as <earlierVar>)` to avoid wasting permanent trace tokens.
const INLINE_DEDUP_MIN_CHARS: usize = 120;

/// Context hints collected during one VM segment execution:
/// - `virtual_receipts`: factual `[virtual ...]` summary lines emitted by child frames.
/// - `delegated_refs` / `delegated_content_hashes`: Resource Handles (and large text
///   payloads) passed into `@virtual` calls in the SAME step, so the outer parent skips
///   expanding their raw bodies into `EPHEMERAL_PEEK` (Pass-by-Handle zero outer bloat).
#[derive(Clone, Debug, Default)]
pub struct SegmentContextHints {
    pub virtual_receipts: Vec<String>,
    pub delegated_refs: BTreeSet<String>,
    pub delegated_content_hashes: BTreeSet<u64>,
}

/// Fast deterministic FNV-1a 64-bit hash used for content deduplication & handle matching.
pub(super) fn fnv1a64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Attach a monotonic `"__ref": "Ref#N"` handle tag to file-read results and
/// large command outputs so they act as first-class Lazy Resource Handles.
pub(super) fn tag_resource_handle(value: &mut Value, next_ref_id: &mut usize) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    if obj.contains_key("__ref") {
        return;
    }
    let is_file = obj.get("path").and_then(Value::as_str).is_some()
        && obj.get("content").and_then(Value::as_str).is_some();
    let is_large_cmd = obj.contains_key("exit_code")
        && (obj
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .count()
            + obj
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .count()
            > INLINE_LIMIT_CHARS);
    if is_file || is_large_cmd {
        let id = *next_ref_id;
        *next_ref_id += 1;
        obj.insert("__ref".into(), Value::String(format!("Ref#{id}")));
    }
}

/// Recursively collect any `"__ref"` handle IDs and large string hashes (`> 2KB`)
/// passed into a `@virtual` call (via args or `@uses` captures).
pub(super) fn collect_delegated_markers(
    value: &Value,
    refs: &mut BTreeSet<String>,
    hashes: &mut BTreeSet<u64>,
) {
    match value {
        Value::String(s) => {
            if s.chars().count() > INLINE_LIMIT_CHARS {
                hashes.insert(fnv1a64(s));
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_delegated_markers(item, refs, hashes);
            }
        }
        Value::Object(map) => {
            if let Some(r) = map.get("__ref").and_then(Value::as_str) {
                refs.insert(r.to_owned());
            }
            for v in map.values() {
                collect_delegated_markers(v, refs, hashes);
            }
        }
        _ => {}
    }
}

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
    /// Single-line permanent abort note if the segment threw a runtime error.
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
    /// Small params/captures print inline; large ones (> 2KB) expand in Step 1's
    /// `EPHEMERAL_PEEK` while keeping a compact signature in the function header.
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
                "Fix and resubmit JavaScript statements, or finish with native JS `return { ..., summary: \"...\" };`."
            } else {
                "Fix and resubmit JavaScript statements, or start with `return <<<` to give your final Markdown reply."
            };
            turn.ephemeral_peek = Some(format!(
                "// [REJECTED ({code}) — executed: false; nothing ran]\n// {message}\n// {hint}"
            ));
        }
    }

    /// Record a completed JS segment from the live `Runtime`:
    /// - Short new/updated bindings (`<= 2KB`) go verbatim into `inline_receipts`.
    /// - Medium (`2KB..=24KB`) & large (`> 24KB`) bindings leave a compact
    ///   one-line signature in `inline_receipts` and populate `ephemeral_peek`.
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

    /// Seal the active turn with the final user-facing Markdown reply and strip
    /// `ephemeral_peek` completely.
    pub fn finish_turn(&mut self, assistant_markdown: impl Into<String>) {
        if let Some(turn) = self.active_turn_mut() {
            turn.ephemeral_peek = None;
            turn.assistant_reply = Some(assistant_markdown.into());
        }
    }

    /// Exact stable byte prefix of the active `user #N` message (everything
    /// before `EPHEMERAL_PEEK`). Used by tests to verify 100% KV-cache stability.
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
    // Note: intentionally leave `executeTask()` open without a closing `}`
    // while in progress (and keep the exact same prefix when sealed!) so the
    // bytes before `EPHEMERAL_PEEK` never mutate across steps or across turns.
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

/// Per-step tracker for `EPHEMERAL_PEEK` deduplication and Pass-by-Handle suppression.
struct PeekTracker<'a> {
    delegated_refs: Option<&'a BTreeSet<String>>,
    delegated_hashes: Option<&'a BTreeSet<u64>>,
    seen_refs: BTreeSet<String>,
    seen_hashes: BTreeSet<u64>,
    is_virtual_param: bool,
}

impl<'a> PeekTracker<'a> {
    fn from_hints(hints: &'a SegmentContextHints) -> Self {
        Self {
            delegated_refs: Some(&hints.delegated_refs),
            delegated_hashes: Some(&hints.delegated_content_hashes),
            seen_refs: BTreeSet::new(),
            seen_hashes: BTreeSet::new(),
            is_virtual_param: false,
        }
    }

    fn for_virtual_param() -> Self {
        Self {
            delegated_refs: None,
            delegated_hashes: None,
            seen_refs: BTreeSet::new(),
            seen_hashes: BTreeSet::new(),
            is_virtual_param: true,
        }
    }

    fn is_delegated(&self, ref_id: Option<&str>, text_hash: Option<u64>) -> bool {
        if let Some(r) = ref_id
            && self.delegated_refs.is_some_and(|set| set.contains(r))
        {
            return true;
        }
        if let Some(h) = text_hash
            && self.delegated_hashes.is_some_and(|set| set.contains(&h))
        {
            return true;
        }
        false
    }

    /// Returns `true` if this resource should be expanded in `EPHEMERAL_PEEK`
    /// (neither delegated to `@virtual` in the same step nor already peeked by an alias).
    fn claim_peek(&mut self, ref_id: Option<&str>, text_hash: Option<u64>) -> bool {
        if self.is_delegated(ref_id, text_hash) {
            return false;
        }
        if let Some(r) = ref_id
            && !self.seen_refs.insert(r.to_owned())
        {
            return false;
        }
        if let Some(h) = text_hash
            && !self.seen_hashes.insert(h)
        {
            return false;
        }
        true
    }

    /// Returns `true` if `value` contains at least one large resource (`__ref` or
    /// `> 2KB` text) and EVERY such resource inside it was already delegated to
    /// `@virtual` or already expanded in this step's `EPHEMERAL_PEEK`.
    fn container_already_covered(&self, value: &Value) -> bool {
        fn visit(v: &Value, t: &PeekTracker<'_>, found_any: &mut bool, all_covered: &mut bool) {
            if !*all_covered {
                return;
            }
            match v {
                Value::String(s) if s.chars().count() > INLINE_LIMIT_CHARS => {
                    *found_any = true;
                    let h = fnv1a64(s);
                    let covered = t.is_delegated(None, Some(h)) || t.seen_hashes.contains(&h);
                    if !covered {
                        *all_covered = false;
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        visit(item, t, found_any, all_covered);
                    }
                }
                Value::Object(map) => {
                    if let Some(r) = map.get("__ref").and_then(Value::as_str) {
                        *found_any = true;
                        let h = map
                            .get("content")
                            .and_then(Value::as_str)
                            .map(fnv1a64);
                        let covered = t.is_delegated(Some(r), h)
                            || t.seen_refs.contains(r)
                            || h.is_some_and(|hh| t.seen_hashes.contains(&hh));
                        if !covered {
                            *all_covered = false;
                        }
                        return;
                    }
                    for val in map.values() {
                        visit(val, t, found_any, all_covered);
                    }
                }
                _ => {}
            }
        }
        let mut found_any = false;
        let mut all_covered = true;
        visit(value, self, &mut found_any, &mut all_covered);
        found_any && all_covered
    }
}

fn format_segment_feedback(
    runtime: &Runtime,
    pre_fingerprints: &BTreeMap<String, String>,
    console_logs: &[String],
    hints: &SegmentContextHints,
) -> (Vec<String>, Vec<String>) {
    let mut receipts = Vec::new();
    let mut peek_sections = Vec::new();
    let mut tracker = PeekTracker::from_hints(hints);

    if !console_logs.is_empty() {
        let joined = console_logs.join("\n");
        let chars = joined.chars().count();
        if chars <= INLINE_LIMIT_CHARS {
            receipts.push(format_comment_block("console", &joined));
        } else {
            let line_count = joined.lines().count();
            receipts.push(format!(
                "// => console = [{line_count} lines, {chars} chars]"
            ));
            let h = fnv1a64(&joined);
            if tracker.claim_peek(None, Some(h)) {
                peek_sections.push(format_peek_string("console", &joined, None));
            }
        }
    }

    let current_fps = runtime.user_binding_fingerprints();
    let live_values = runtime.named_bindings();
    let show_last_expr = runtime.segment_evaluated_expr() || runtime.segment_returned_explicitly();

    // Emit newly created / changed bindings in deterministic order.
    let mut changed_names: Vec<String> = current_fps
        .iter()
        .filter(|(name, fp)| pre_fingerprints.get(*name) != Some(*fp))
        .map(|(name, _)| name.clone())
        .collect();

    if show_last_expr && live_values.contains_key("$_") && !changed_names.contains(&"$_".to_owned()) {
        // Avoid duplicating `$_` if it merely mirrors the single newly assigned variable.
        let suppress_dup = changed_names.len() == 1
            && live_values.get("$_") == live_values.get(&changed_names[0]);
        if !suppress_dup {
            changed_names.push("$_".to_owned());
        }
    }

    // Fold duplicate large inline values in the same step (e.g. `auditResults = audits`
    // or `$_ = JSON.stringify(summaries)`) so permanent `// =>` stays clean and concise.
    let mut seen_inline: BTreeMap<String, String> = BTreeMap::new();

    for name in changed_names {
        let Some(value) = live_values.get(&name) else {
            continue;
        };
        let canonical = canonical_inline_key(value);
        if let Some(key) = &canonical
            && let Some(orig) = seen_inline.get(key)
        {
            receipts.push(format!("// => {name} = (same as {orig})"));
            continue;
        }
        let (receipt, peek) = format_binding_three_tier(&name, value, &mut tracker);
        if peek.is_none()
            && let Some(key) = canonical
        {
            seen_inline.entry(key).or_insert_with(|| name.clone());
        }
        receipts.push(receipt);
        if let Some(p) = peek {
            peek_sections.push(p);
        }
    }

    (receipts, peek_sections)
}

/// Canonical string key for same-step duplicate inline folding.
/// Normalizes `JSON.stringify(x)` strings to the same canonical JSON as `x`
/// when `x` is a JSON array/object so `JSON.stringify(summaries)` folds onto `summaries`.
fn canonical_inline_key(value: &Value) -> Option<String> {
    let key = match value {
        Value::String(s) => {
            if s.chars().count() < INLINE_DEDUP_MIN_CHARS || s.chars().count() > INLINE_LIMIT_CHARS {
                return None;
            }
            if (s.starts_with('{') || s.starts_with('['))
                && let Ok(parsed) = serde_json::from_str::<Value>(s)
            {
                serde_json::to_string(&parsed).unwrap_or_else(|_| s.clone())
            } else {
                s.clone()
            }
        }
        other => {
            let s = serde_json::to_string(other).ok()?;
            if s.chars().count() < INLINE_DEDUP_MIN_CHARS || s.chars().count() > INLINE_LIMIT_CHARS {
                return None;
            }
            s
        }
    };
    Some(key)
}

/// Three-tier value formatter with Lazy Resource Handle awareness:
/// - Tier 1 (`<= 2,000` chars): verbatim in permanent `// =>` receipt.
/// - Tier 2 (`2,000 < chars <= 24,000` chars): one-line handle signature in `// =>`,
///   full line-numbered content in `EPHEMERAL_PEEK` (suppressed if delegated to `@virtual`
///   or already expanded by an alias in the same step!).
/// - Tier 3 (`> 24,000` chars): one-line handle signature in `// =>`, high-entropy
///   structural index in `EPHEMERAL_PEEK` (for demand paging via `.slice()` / `.grep()`).
fn format_binding_three_tier(
    name: &str,
    value: &Value,
    tracker: &mut PeekTracker<'_>,
) -> (String, Option<String>) {
    // 1. File Resource Handle `{ path, content: string, __ref?: string, ... }`
    //    (Inside `@virtual` bound params, also auto-dereference `{ file, content }` pairs!)
    if let Some(obj) = value.as_object()
        && let Some(content) = obj.get("content").and_then(Value::as_str)
        && let Some(path) = obj
            .get("path")
            .and_then(Value::as_str)
            .or_else(|| tracker.is_virtual_param.then(|| obj.get("file").and_then(Value::as_str)).flatten())
    {
        let ref_id = obj.get("__ref").and_then(Value::as_str);
        let content_chars = content.chars().count();
        let line_count = content.lines().count().max(usize::from(!content.is_empty()));
        if content_chars <= INLINE_LIMIT_CHARS {
            let ref_part = ref_id.map(|r| format!("{r}, ")).unwrap_or_default();
            let label = format!("{name} (File {path:?}, {ref_part}{line_count} lines)");
            return (format_comment_block(&label, &add_line_numbers(content, 1)), None);
        }
        let ref_part = ref_id.map(|r| format!(" | {r}")).unwrap_or_default();
        let sig = format!(
            "// => {name} = [File({path:?}){ref_part} | {line_count} lines, {content_chars} chars | .slice() .grep() .outline()]"
        );
        let h = fnv1a64(content);
        let peek = if tracker.claim_peek(ref_id, Some(h)) {
            Some(format_peek_string(
                &format!("{name}.content ({path})"),
                content,
                Some(path),
            ))
        } else {
            None
        };
        return (sig, peek);
    }

    // 2. Command Resource Handle `{ exit_code, stdout, stderr, __ref?: string, ... }`
    if let Some(obj) = value.as_object()
        && obj.contains_key("exit_code")
        && (obj.contains_key("stdout") || obj.contains_key("stderr"))
    {
        let ref_id = obj.get("__ref").and_then(Value::as_str);
        let exit = obj["exit_code"].clone();
        let stdout = obj.get("stdout").and_then(Value::as_str).unwrap_or("");
        let stderr = obj.get("stderr").and_then(Value::as_str).unwrap_or("");
        let combined = if stderr.is_empty() {
            stdout.to_owned()
        } else if stdout.is_empty() {
            stderr.to_owned()
        } else {
            format!("{stdout}\n--- stderr ---\n{stderr}")
        };
        let chars = combined.chars().count();
        let lines = combined.lines().count();
        if chars <= INLINE_LIMIT_CHARS {
            let label = format!("{name} (exit_code={exit})");
            return (format_comment_block(&label, &combined), None);
        }
        let ref_part = ref_id.map(|r| format!(" | {r}")).unwrap_or_default();
        let sig = format!(
            "// => {name} = [CommandResult(exit_code={exit}){ref_part} | {lines} lines, {chars} chars | .slice() .grep() .tail()]"
        );
        let h = fnv1a64(&combined);
        let peek = if tracker.claim_peek(ref_id, Some(h)) {
            if chars <= PEEK_FULL_LIMIT_CHARS {
                Some(format!(
                    "// [{name} FULL OUTPUT (exit_code={exit}, {lines} lines)]:\n{}",
                    add_line_numbers(&combined, 1)
                ))
            } else {
                Some(format_large_log_index(name, &exit, &combined))
            }
        } else {
            None
        };
        return (sig, peek);
    }

    // 3. Plain string
    if let Some(text) = value.as_str() {
        let chars = text.chars().count();
        let lines = text.lines().count().max(usize::from(!text.is_empty()));
        if text.is_empty() {
            return (format!("// => {name} = \"\" (Empty string)"), None);
        }
        if chars <= INLINE_LIMIT_CHARS {
            if text.contains('\n') {
                return (format_comment_block(name, text), None);
            }
            return (format!("// => {name} = {text:?}"), None);
        }
        let sig = format!(
            "// => {name} = [string | {lines} lines, {chars} chars | > 2KB not inlined]"
        );
        let h = fnv1a64(text);
        let peek = if tracker.claim_peek(None, Some(h)) {
            Some(format_peek_string(name, text, None))
        } else {
            None
        };
        return (sig, peek);
    }

    // 4. Array (with compact summary for Array of File Resource Handles!)
    if let Some(arr) = value.as_array() {
        if arr.is_empty() {
            return (format!("// => {name} = [] (Empty: 0 items)"), None);
        }
        // Check if every element is a File Resource Handle (`{ path, content }`).
        let all_file_handles = arr.iter().all(|item| {
            item.as_object().is_some_and(|o| {
                o.get("path").and_then(Value::as_str).is_some()
                    && o.get("content").and_then(Value::as_str).is_some()
            })
        });
        if all_file_handles {
            let summaries: Vec<String> = arr
                .iter()
                .map(|item| {
                    let p = item["path"].as_str().unwrap_or("");
                    match item.get("__ref").and_then(Value::as_str) {
                        Some(r) => format!("{r}({p:?})"),
                        None => format!("File({p:?})"),
                    }
                })
                .collect();
            let sig = format!(
                "// => {name} = [Array({}) of Handles: {}]",
                arr.len(),
                summaries.join(", ")
            );
            // Expand only handles that are neither delegated to `@virtual` nor already peeked.
            let mut peeks = Vec::new();
            for (idx, item) in arr.iter().enumerate() {
                let p = item["path"].as_str().unwrap_or("");
                let c = item["content"].as_str().unwrap_or("");
                let r = item.get("__ref").and_then(Value::as_str);
                if c.chars().count() > INLINE_LIMIT_CHARS && tracker.claim_peek(r, Some(fnv1a64(c))) {
                    peeks.push(format_peek_string(&format!("{name}[{idx}].content ({p})"), c, Some(p)));
                }
            }
            let peek = if peeks.is_empty() { None } else { Some(peeks.join("\n\n")) };
            return (sig, peek);
        }

        let serialized = serde_json::to_string(value).unwrap_or_default();
        let chars = serialized.chars().count();
        if chars <= INLINE_LIMIT_CHARS {
            return (format!("// => {name} = {serialized}"), None);
        }
        let shape = describe_item_shape(&arr[0]);
        let sig = format!(
            "// => {name} = [Array({}) | itemShape: {shape} | {chars} chars]",
            arr.len()
        );
        if tracker.container_already_covered(value) {
            return (sig, None);
        }
        let peek = if chars <= PEEK_FULL_LIMIT_CHARS {
            let pretty = serde_json::to_string_pretty(value).unwrap_or(serialized);
            format!("// [{name} FULL Array({})]:\n{pretty}", arr.len())
        } else {
            format_large_array_index(name, arr, &shape)
        };
        return (sig, Some(peek));
    }

    // 5. Generic Object / Primitive
    let serialized = serde_json::to_string(value).unwrap_or_default();
    let chars = serialized.chars().count();
    if chars <= INLINE_LIMIT_CHARS {
        return (format!("// => {name} = {serialized}"), None);
    }
    let keys_summary = value
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{k}({}c)", v.to_string().chars().count()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|| "value".into());
    let sig = format!(
        "// => {name} = [Object {{ {keys_summary} }} | {chars} chars]"
    );
    if tracker.container_already_covered(value) {
        return (sig, None);
    }
    let peek = if chars <= PEEK_FULL_LIMIT_CHARS {
        let pretty = serde_json::to_string_pretty(value).unwrap_or(serialized);
        format!("// [{name} FULL Object]:\n{pretty}")
    } else {
        let head: String = serialized.chars().take(1_500).collect();
        format!(
            "// [{name} LARGE Object Index ({chars} chars) — keys: {keys_summary}]:\n\
             // Slice/filter `{name}` directly in JS (or pass `{name}` to a `@virtual` helper).\n\
             // head preview: {head}..."
        )
    };
    (sig, Some(peek))
}

fn format_peek_string(label: &str, text: &str, path_hint: Option<&str>) -> String {
    let chars = text.chars().count();
    let lines = text.lines().count().max(usize::from(!text.is_empty()));
    if chars <= PEEK_FULL_LIMIT_CHARS {
        format!(
            "// [{label} FULL VIEW ({lines} lines, {chars} chars)]:\n{}",
            add_line_numbers(text, 1)
        )
    } else {
        format_large_source_index(label, text, path_hint)
    }
}

fn format_large_source_index(label: &str, text: &str, path_hint: Option<&str>) -> String {
    let all_lines: Vec<&str> = text.lines().collect();
    let total = all_lines.len();
    let chars = text.chars().count();
    let mut symbols = Vec::new();
    for (idx, line) in all_lines.iter().enumerate() {
        let t = line.trim_start();
        let is_sym = t.starts_with("pub fn ")
            || t.starts_with("pub async fn ")
            || t.starts_with("fn ")
            || t.starts_with("async fn ")
            || t.starts_with("pub struct ")
            || t.starts_with("struct ")
            || t.starts_with("pub enum ")
            || t.starts_with("enum ")
            || t.starts_with("impl ")
            || t.starts_with("export ")
            || t.starts_with("function ")
            || t.starts_with("async function ")
            || t.starts_with("class ")
            || t.starts_with("interface ")
            || t.starts_with("def ")
            || t.starts_with("# ");
        if is_sym {
            symbols.push(format!("  L{}: {}", idx + 1, compact_single_line(t, 120)));
            if symbols.len() >= 80 {
                break;
            }
        }
    }
    let head = all_lines
        .iter()
        .take(15)
        .enumerate()
        .map(|(i, l)| format!("{}: {l}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let tail_start = total.saturating_sub(15);
    let tail = all_lines
        .iter()
        .skip(tail_start)
        .enumerate()
        .map(|(i, l)| format!("{}: {l}", tail_start + i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let title = path_hint.unwrap_or(label);
    let sym_block = if symbols.is_empty() {
        "  (no top-level symbol lines matched; use `.grep(pattern)` or `.slice(start, end)` on the handle)".into()
    } else {
        symbols.join("\n")
    };
    format!(
        "// [{label} LARGE INDEX ({title}, {total} lines, {chars} chars) — too large for full peek]\n\
         // Page in exact lines on demand via handle methods: `.slice(start, end)`, `.slice([[s1, e1], [s2, e2]])`, `.grep(pattern, ctx)`, or `.outline()`:\n\
         // Symbols / Sections:\n{sym_block}\n\
         // Head (L1..L{}):\n{head}\n\
         // Tail (L{}..L{total}):\n{tail}",
        total.min(15),
        tail_start + 1
    )
}

fn format_large_log_index(name: &str, exit: &Value, combined: &str) -> String {
    let lines: Vec<&str> = combined.lines().collect();
    let total = lines.len();
    let mut hits = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("error")
            || lower.contains("panic")
            || lower.contains("failed")
            || lower.contains("exception")
        {
            hits.push(format!("  L{}: {}", idx + 1, compact_single_line(line, 140)));
            if hits.len() >= 30 {
                break;
            }
        }
    }
    let tail_start = total.saturating_sub(30);
    let tail = lines
        .iter()
        .skip(tail_start)
        .enumerate()
        .map(|(i, l)| format!("{}: {l}", tail_start + i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let hit_block = if hits.is_empty() {
        "  (no error/panic/failed lines matched)".into()
    } else {
        hits.join("\n")
    };
    format!(
        "// [{name} LARGE COMMAND INDEX (exit_code={exit}, {total} lines)]:\n\
         // Matched error/panic/failed lines:\n{hit_block}\n\
         // Tail (L{}..L{total}):\n{tail}",
        tail_start + 1
    )
}

fn format_large_array_index(name: &str, arr: &[Value], shape: &str) -> String {
    let first = compact_single_line(&arr[0].to_string(), 400);
    let last = compact_single_line(&arr[arr.len() - 1].to_string(), 400);
    format!(
        "// [{name} LARGE Array({}) INDEX | itemShape: {shape}]:\n\
         // Filter/slice `{name}` directly in JS.\n\
         // sample[0]: {first}\n\
         // sample[{}]: {last}",
        arr.len(),
        arr.len() - 1
    )
}

fn describe_item_shape(sample: &Value) -> String {
    match sample {
        Value::Object(map) => {
            let fields = map
                .iter()
                .take(10)
                .map(|(k, v)| {
                    let ty = match v {
                        Value::Null => "null",
                        Value::Bool(_) => "boolean",
                        Value::Number(_) => "number",
                        Value::String(_) => "string",
                        Value::Array(_) => "Array",
                        Value::Object(_) => "Object",
                    };
                    format!("{k}: {ty}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {fields} }}")
        }
        Value::String(_) => "string".into(),
        Value::Number(_) => "number".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Array(_) => "Array".into(),
        Value::Null => "null".into(),
    }
}

fn add_line_numbers(text: &str, start_line: usize) -> String {
    text.lines()
        .enumerate()
        .map(|(idx, line)| format!("{}: {line}", start_line + idx))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_comment_block(label: &str, body: &str) -> String {
    if !body.contains('\n') {
        return format!("// => {label} = {body}");
    }
    let mut out = format!("// => {label}:\n");
    for line in body.lines() {
        out.push_str("//   ");
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end_matches('\n').to_owned()
}

fn compact_single_line(text: &str, max_chars: usize) -> String {
    let flat = text.replace(['\r', '\n'], " ");
    let count = flat.chars().count();
    if count <= max_chars {
        flat
    } else {
        let mut s: String = flat.chars().take(max_chars).collect();
        s.push_str("...");
        s
    }
}
