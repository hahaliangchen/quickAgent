//! Three-tier value formatter, Resource Handle tagging/delegation, `EPHEMERAL_PEEK`
//! deduplication (`PeekTracker`), and same-step inline alias folding.

use super::indexers::{
    add_line_numbers, describe_item_shape, format_comment_block, format_large_array_index,
    format_large_log_index, format_peek_string,
};
use super::{INLINE_LIMIT_CHARS, PEEK_FULL_LIMIT_CHARS};
use crate::agent_program::Runtime;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Minimum length at which a duplicate inline binding in the same step folds
/// into `(same as <earlierVar>)` to avoid wasting permanent trace tokens.
const INLINE_DEDUP_MIN_CHARS: usize = 120;

/// Context hints collected during one VM segment execution:
/// - `virtual_receipts`: factual `[virtual ...]` / `[library ...]` summary lines.
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
pub(in crate::single_agent) fn fnv1a64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Attach a monotonic `"__ref": "Ref#N"` handle tag to file-read results and
/// large command outputs so they act as first-class Lazy Resource Handles.
pub(in crate::single_agent) fn tag_resource_handle(value: &mut Value, next_ref_id: &mut usize) {
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
pub(in crate::single_agent) fn collect_delegated_markers(
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

/// Per-step tracker for `EPHEMERAL_PEEK` deduplication and Pass-by-Handle suppression.
pub(super) struct PeekTracker<'a> {
    delegated_refs: Option<&'a BTreeSet<String>>,
    delegated_hashes: Option<&'a BTreeSet<u64>>,
    seen_refs: BTreeSet<String>,
    seen_hashes: BTreeSet<u64>,
    is_virtual_param: bool,
}

impl<'a> PeekTracker<'a> {
    pub(super) fn from_hints(hints: &'a SegmentContextHints) -> Self {
        Self {
            delegated_refs: Some(&hints.delegated_refs),
            delegated_hashes: Some(&hints.delegated_content_hashes),
            seen_refs: BTreeSet::new(),
            seen_hashes: BTreeSet::new(),
            is_virtual_param: false,
        }
    }

    pub(super) fn for_virtual_param() -> Self {
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
                        let h = map.get("content").and_then(Value::as_str).map(fnv1a64);
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

pub(super) fn format_segment_feedback(
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

    let mut changed_names: Vec<String> = current_fps
        .iter()
        .filter(|(name, fp)| pre_fingerprints.get(*name) != Some(*fp))
        .map(|(name, _)| name.clone())
        .collect();

    if show_last_expr && live_values.contains_key("$_") && !changed_names.contains(&"$_".to_owned()) {
        let suppress_dup = changed_names.len() == 1
            && live_values.get("$_") == live_values.get(&changed_names[0]);
        if !suppress_dup {
            changed_names.push("$_".to_owned());
        }
    }

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

pub(super) fn format_binding_three_tier(
    name: &str,
    value: &Value,
    tracker: &mut PeekTracker<'_>,
) -> (String, Option<String>) {
    // 1. File Resource Handle `{ path, content: string, __ref?: string, ... }`
    if let Some(obj) = value.as_object()
        && let Some(content) = obj.get("content").and_then(Value::as_str)
        && let Some(path) = obj
            .get("path")
            .and_then(Value::as_str)
            .or_else(|| {
                tracker
                    .is_virtual_param
                    .then(|| obj.get("file").and_then(Value::as_str))
                    .flatten()
            })
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
        let asset_part = obj
            .get("__asset_id")
            .and_then(Value::as_str)
            .map(|id| format!(" | asset: {id}"))
            .unwrap_or_default();
        let sig = format!(
            "// => {name} = [File({path:?}){ref_part} | {line_count} lines, {content_chars} chars{asset_part} | .slice() .grep() .outline()]"
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
        let asset_part = obj
            .get("__asset_id")
            .and_then(Value::as_str)
            .map(|id| format!(" | asset: {id}"))
            .unwrap_or_default();
        let sig = format!(
            "// => {name} = [CommandResult(exit_code={exit}){ref_part} | {lines} lines, {chars} chars{asset_part} | .slice() .grep() .tail()]"
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

    // 4. Array (with compact summary for Array of File Resource Handles)
    if let Some(arr) = value.as_array() {
        if arr.is_empty() {
            return (format!("// => {name} = [] (Empty: 0 items)"), None);
        }
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
    let asset_part = value
        .as_object()
        .and_then(|o| o.get("__asset_id"))
        .and_then(Value::as_str)
        .map(|id| format!(" | asset: {id}"))
        .unwrap_or_default();
    let sig = format!(
        "// => {name} = [Object {{ {keys_summary} }} | {chars} chars{asset_part}]"
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
